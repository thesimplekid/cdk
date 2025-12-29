//! Melt Saga - Type State Pattern Implementation
//!
//! This module implements the saga pattern for melt operations using the typestate
//! pattern to enforce valid state transitions at compile-time.
//!
//! # Type State Flow
//!
//! ```text
//! MeltSaga<Initial>
//!   └─> prepare() -> MeltSaga<Prepared>
//!         ├─> confirm() -> MeltSaga<Confirmed>
//!         └─> cancel() -> ()
//! ```

use std::collections::HashMap;

use cdk_common::amount::SplitTarget;
use cdk_common::wallet::{
    MeltOperationData, MeltSagaState, OperationData, Transaction, TransactionDirection, WalletSaga,
    WalletSagaState,
};
use cdk_common::MeltQuoteState;
use tracing::instrument;

use self::compensation::RevertProofReservation;
use self::state::{Confirmed, Initial, Prepared};
use crate::dhke::construct_proofs;
use crate::nuts::nut00::ProofsMethods;
use crate::nuts::{MeltRequest, PreMintSecrets, Proofs, State};
use crate::types::{Melted, ProofInfo};
use crate::util::unix_time;
use crate::wallet::saga::{
    add_compensation, clear_compensations, execute_compensations, new_compensations, Compensations,
};
use crate::wallet::send::split_proofs_for_send;
use crate::wallet::MeltQuote;
use crate::{ensure_cdk, Amount, Error, Wallet};

pub mod compensation;
pub mod state;

/// Saga pattern implementation for melt operations.
///
/// Uses the typestate pattern to enforce valid state transitions at compile-time.
/// Each state (Initial, Prepared, Confirmed) is a distinct type, and operations
/// are only available on the appropriate type.
pub struct MeltSaga<S> {
    /// Wallet reference
    wallet: Wallet,
    /// Compensating actions in LIFO order (most recent first)
    compensations: Compensations,
    /// State-specific data
    state_data: S,
}

impl MeltSaga<Initial> {
    /// Create a new melt saga in the Initial state.
    pub fn new(wallet: Wallet) -> Self {
        let operation_id = uuid::Uuid::new_v4();

        Self {
            wallet,
            compensations: new_compensations(),
            state_data: Initial { operation_id },
        }
    }

    /// Prepare the melt operation by selecting and reserving proofs.
    ///
    /// This is the first step in the saga. It:
    /// 1. Loads the quote from the database
    /// 2. Selects proofs for the required amount
    /// 3. Reserves the selected proofs (sets state to Reserved)
    /// 4. Determines if a swap is needed
    ///
    /// # Compensation
    ///
    /// Registers a compensation action that will revert proof reservation
    /// if later steps fail.
    #[instrument(skip_all)]
    pub async fn prepare(
        self,
        quote_id: &str,
        metadata: HashMap<String, String>,
    ) -> Result<MeltSaga<Prepared>, Error> {
        tracing::info!(
            "Preparing melt for quote {} with operation {}",
            quote_id,
            self.state_data.operation_id
        );

        let quote_info = self
            .wallet
            .localstore
            .get_melt_quote(quote_id)
            .await?
            .ok_or(Error::UnknownQuote)?;

        ensure_cdk!(
            quote_info.expiry.gt(&unix_time()),
            Error::ExpiredQuote(quote_info.expiry, unix_time())
        );

        let inputs_needed_amount = quote_info.amount + quote_info.fee_reserve;

        let active_keyset_ids = self
            .wallet
            .get_mint_keysets()
            .await?
            .into_iter()
            .map(|k| k.id)
            .collect();
        let keyset_fees_and_amounts = self.wallet.get_keyset_fees_and_amounts().await?;

        let available_proofs = self.wallet.get_unspent_proofs().await?;

        // Try to select proofs that exactly match inputs_needed_amount
        let exact_input_proofs = Wallet::select_proofs(
            inputs_needed_amount,
            available_proofs.clone(),
            &active_keyset_ids,
            &keyset_fees_and_amounts,
            true,
        )?;
        let proofs_total = exact_input_proofs.total_amount()?;

        // If exact match, use proofs directly without swap
        if proofs_total == inputs_needed_amount {
            let proof_ys = exact_input_proofs.ys()?;
            let operation_id = self.state_data.operation_id;

            // Reserve proofs
            self.wallet
                .localstore
                .update_proofs_state(proof_ys.clone(), State::Reserved)
                .await?;

            // Persist saga state for crash recovery
            let saga = WalletSaga::new(
                operation_id,
                WalletSagaState::Melt(MeltSagaState::ProofsReserved),
                quote_info.amount,
                self.wallet.mint_url.clone(),
                self.wallet.unit.clone(),
                OperationData::Melt(MeltOperationData {
                    quote_id: quote_id.to_string(),
                    amount: quote_info.amount,
                    fee_reserve: quote_info.fee_reserve,
                    counter_start: None,
                    counter_end: None,
                    change_amount: None,
                    change_blinded_messages: None, // Will be set when melt is requested
                }),
            );

            self.wallet.localstore.add_saga(saga).await?;

            // Register compensation
            add_compensation(
                &self.compensations,
                Box::new(RevertProofReservation {
                    localstore: self.wallet.localstore.clone(),
                    proof_ys,
                    saga_id: operation_id,
                }),
            )
            .await;

            let input_fee = self.wallet.get_proofs_fee(&exact_input_proofs).await?.total;

            return Ok(MeltSaga {
                wallet: self.wallet,
                compensations: self.compensations,
                state_data: Prepared {
                    operation_id: self.state_data.operation_id,
                    quote: quote_info,
                    proofs: exact_input_proofs,
                    proofs_to_swap: Proofs::new(),
                    swap_fee: Amount::ZERO,
                    input_fee,
                    metadata,
                },
            });
        }

        // Need to swap to get exact denominations
        let active_keyset_id = self.wallet.get_active_keyset().await?.id;
        let fee_and_amounts = self
            .wallet
            .get_keyset_fees_and_amounts_by_id(active_keyset_id)
            .await?;

        // Calculate optimal denomination split and the fee for those proofs
        let initial_split = inputs_needed_amount.split(&fee_and_amounts);
        let target_fee = self
            .wallet
            .get_proofs_fee_by_count(
                vec![(active_keyset_id, initial_split.len() as u64)]
                    .into_iter()
                    .collect(),
            )
            .await?
            .total;

        // Include swap fee in selection
        let inputs_total_needed = inputs_needed_amount + target_fee;
        let target_amounts = inputs_total_needed.split(&fee_and_amounts);

        let input_proofs = Wallet::select_proofs(
            inputs_total_needed,
            available_proofs,
            &active_keyset_ids,
            &keyset_fees_and_amounts,
            true,
        )?;

        let keyset_fees: HashMap<cdk_common::Id, u64> = keyset_fees_and_amounts
            .iter()
            .map(|(key, values)| (*key, values.fee()))
            .collect();

        let split_result = split_proofs_for_send(
            input_proofs,
            &target_amounts,
            inputs_total_needed,
            target_fee,
            &keyset_fees,
            false,
            false,
        )?;

        // Combine all proofs for reservation
        let mut all_proofs = split_result.proofs_to_send.clone();
        all_proofs.extend(split_result.proofs_to_swap.clone());
        let proof_ys = all_proofs.ys()?;
        let operation_id = self.state_data.operation_id;

        // Reserve proofs
        self.wallet
            .localstore
            .update_proofs_state(proof_ys.clone(), State::Reserved)
            .await?;

        // Persist saga state for crash recovery
        let saga = WalletSaga::new(
            operation_id,
            WalletSagaState::Melt(MeltSagaState::ProofsReserved),
            quote_info.amount,
            self.wallet.mint_url.clone(),
            self.wallet.unit.clone(),
            OperationData::Melt(MeltOperationData {
                quote_id: quote_id.to_string(),
                amount: quote_info.amount,
                fee_reserve: quote_info.fee_reserve,
                counter_start: None,
                counter_end: None,
                change_amount: None,
                change_blinded_messages: None, // Will be set when melt is requested
            }),
        );

        self.wallet.localstore.add_saga(saga).await?;

        // Register compensation
        add_compensation(
            &self.compensations,
            Box::new(RevertProofReservation {
                localstore: self.wallet.localstore.clone(),
                proof_ys,
                saga_id: operation_id,
            }),
        )
        .await;

        let input_fee = self
            .wallet
            .get_proofs_fee(&split_result.proofs_to_send)
            .await?
            .total;

        Ok(MeltSaga {
            wallet: self.wallet,
            compensations: self.compensations,
            state_data: Prepared {
                operation_id: self.state_data.operation_id,
                quote: quote_info,
                proofs: split_result.proofs_to_send,
                proofs_to_swap: split_result.proofs_to_swap,
                swap_fee: split_result.swap_fee,
                input_fee,
                metadata,
            },
        })
    }
}

impl MeltSaga<Prepared> {
    /// Get the quote
    pub fn quote(&self) -> &MeltQuote {
        &self.state_data.quote
    }

    /// Get the amount to be melted
    pub fn amount(&self) -> Amount {
        self.state_data.quote.amount
    }

    /// Get the proofs that will be used
    pub fn proofs(&self) -> &Proofs {
        &self.state_data.proofs
    }

    /// Get the proofs that need to be swapped
    pub fn proofs_to_swap(&self) -> &Proofs {
        &self.state_data.proofs_to_swap
    }

    /// Get the swap fee
    pub fn swap_fee(&self) -> Amount {
        self.state_data.swap_fee
    }

    /// Get the input fee
    pub fn input_fee(&self) -> Amount {
        self.state_data.input_fee
    }

    /// Get the total fee
    pub fn total_fee(&self) -> Amount {
        self.state_data.total_fee()
    }

    /// Confirm the prepared melt and execute the payment.
    ///
    /// This completes the melt operation by:
    /// 1. Swapping proofs if necessary
    /// 2. Executing the melt request
    /// 3. Processing any change
    /// 4. Recording the transaction
    ///
    /// On success, compensations are cleared.
    #[instrument(skip_all)]
    pub async fn confirm(self) -> Result<MeltSaga<Confirmed>, Error> {
        tracing::info!(
            "Confirming melt for operation {}",
            self.state_data.operation_id
        );

        let active_keyset_id = self.wallet.fetch_active_keyset().await?.id;
        let mut quote_info = self.state_data.quote.clone();

        let inputs_needed_amount = quote_info.amount + quote_info.fee_reserve;

        let mut final_proofs = self.state_data.proofs.clone();

        // Swap if necessary
        if !self.state_data.proofs_to_swap.is_empty() {
            let swap_amount = inputs_needed_amount
                .checked_sub(final_proofs.total_amount()?)
                .ok_or(Error::AmountOverflow)?;

            tracing::debug!(
                "Swapping {} proofs to get {} (swap fee: {})",
                self.state_data.proofs_to_swap.len(),
                swap_amount,
                self.state_data.swap_fee
            );

            if let Some(swapped) = self
                .wallet
                .swap(
                    Some(swap_amount),
                    SplitTarget::None,
                    self.state_data.proofs_to_swap.clone(),
                    None,
                    false,
                )
                .await?
            {
                final_proofs.extend(swapped);
            }
        }

        let proofs_total = final_proofs.total_amount()?;
        if proofs_total < inputs_needed_amount {
            execute_compensations(&self.compensations).await?;
            return Err(Error::InsufficientFunds);
        }

        // Since the proofs may be external (not in our database), add them first
        let proofs_info = final_proofs
            .clone()
            .into_iter()
            .map(|p| {
                ProofInfo::new(
                    p,
                    self.wallet.mint_url.clone(),
                    State::Pending,
                    self.wallet.unit.clone(),
                )
            })
            .collect::<Result<Vec<ProofInfo>, _>>()?;

        self.wallet
            .localstore
            .update_proofs(proofs_info, vec![])
            .await?;

        // Calculate change accounting for input fees
        let input_fee = self.wallet.get_proofs_fee(&final_proofs).await?.total;
        let change_amount = proofs_total - quote_info.amount - input_fee;

        let premint_secrets = if change_amount <= Amount::ZERO {
            PreMintSecrets::new(active_keyset_id)
        } else {
            let num_secrets =
                ((u64::from(change_amount) as f64).log2().ceil() as u64).max(1) as u32;

            let new_counter = self
                .wallet
                .localstore
                .increment_keyset_counter(&active_keyset_id, num_secrets)
                .await?;

            let count = new_counter - num_secrets;

            PreMintSecrets::from_seed_blank(
                active_keyset_id,
                count,
                &self.wallet.seed,
                change_amount,
            )?
        };

        let request = MeltRequest::new(
            quote_info.id.clone(),
            final_proofs.clone(),
            Some(premint_secrets.blinded_messages()),
        );

        let operation_id = self.state_data.operation_id;

        // Get counter range for recovery
        let counter_end = self
            .wallet
            .localstore
            .increment_keyset_counter(&active_keyset_id, 0)
            .await?;
        let counter_start = counter_end.saturating_sub(premint_secrets.secrets.len() as u32);

        // Update saga state to MeltRequested BEFORE making the melt call
        // This is write-ahead logging - if we crash after this, recovery knows
        // the melt request may have been sent
        let updated_saga = WalletSaga::new(
            operation_id,
            WalletSagaState::Melt(MeltSagaState::MeltRequested),
            quote_info.amount,
            self.wallet.mint_url.clone(),
            self.wallet.unit.clone(),
            OperationData::Melt(MeltOperationData {
                quote_id: quote_info.id.clone(),
                amount: quote_info.amount,
                fee_reserve: quote_info.fee_reserve,
                counter_start: Some(counter_start),
                counter_end: Some(counter_end),
                change_amount: if change_amount > Amount::ZERO {
                    Some(change_amount)
                } else {
                    None
                },
                change_blinded_messages: if change_amount > Amount::ZERO {
                    Some(premint_secrets.blinded_messages())
                } else {
                    None
                },
            }),
        );

        // Update saga state - if this fails due to version conflict, another instance
        // is processing this saga, which should not happen during normal operation
        if !self.wallet.localstore.update_saga(updated_saga).await? {
            return Err(Error::Custom(
                "Saga version conflict during update - another instance may be processing this saga".to_string(),
            ));
        }

        let melt_response = match quote_info.payment_method {
            cdk_common::PaymentMethod::Bolt11 => self.wallet.client.post_melt(request).await?,
            cdk_common::PaymentMethod::Bolt12 => {
                self.wallet.client.post_melt_bolt12(request).await?
            }
            cdk_common::PaymentMethod::Custom(_) => {
                execute_compensations(&self.compensations).await?;
                return Err(Error::UnsupportedPaymentMethod);
            }
        };

        let active_keys = self.wallet.load_keyset_keys(active_keyset_id).await?;

        let change_proofs = match melt_response.change {
            Some(change) => {
                let num_change_proof = change.len();

                let num_change_proof = match (
                    premint_secrets.len() < num_change_proof,
                    premint_secrets.secrets().len() < num_change_proof,
                ) {
                    (true, _) | (_, true) => {
                        tracing::error!("Mismatch in change promises to change");
                        premint_secrets.len()
                    }
                    _ => num_change_proof,
                };

                Some(construct_proofs(
                    change,
                    premint_secrets.rs()[..num_change_proof].to_vec(),
                    premint_secrets.secrets()[..num_change_proof].to_vec(),
                    &active_keys,
                )?)
            }
            None => None,
        };

        let payment_preimage = melt_response.payment_preimage.clone();

        let melted = Melted::from_proofs(
            melt_response.state,
            melt_response.payment_preimage.clone(),
            quote_info.amount,
            final_proofs.clone(),
            change_proofs.clone(),
        )?;

        let change_proof_infos = match change_proofs {
            Some(change_proofs) => change_proofs
                .into_iter()
                .map(|proof| {
                    ProofInfo::new(
                        proof,
                        self.wallet.mint_url.clone(),
                        State::Unspent,
                        quote_info.unit.clone(),
                    )
                })
                .collect::<Result<Vec<ProofInfo>, _>>()?,
            None => Vec::new(),
        };

        quote_info.state = MeltQuoteState::Paid;

        let payment_request = quote_info.request.clone();
        let payment_method = quote_info.payment_method.clone();
        self.wallet.localstore.add_melt_quote(quote_info).await?;

        let deleted_ys = final_proofs.ys()?;

        self.wallet
            .localstore
            .update_proofs(change_proof_infos, deleted_ys)
            .await?;

        // Add transaction to store
        self.wallet
            .localstore
            .add_transaction(Transaction {
                mint_url: self.wallet.mint_url.clone(),
                direction: TransactionDirection::Outgoing,
                amount: melted.amount,
                fee: melted.fee_paid,
                unit: self.wallet.unit.clone(),
                ys: final_proofs.ys()?,
                timestamp: unix_time(),
                memo: None,
                metadata: self.state_data.metadata.clone(),
                quote_id: Some(self.state_data.quote.id.clone()),
                payment_request: Some(payment_request),
                payment_proof: payment_preimage.clone(),
                payment_method: Some(payment_method),
            })
            .await?;

        // Clear compensations - operation completed successfully
        clear_compensations(&self.compensations).await;

        // Delete saga record - melt completed successfully (best-effort)
        if let Err(e) = self.wallet.localstore.delete_saga(&operation_id).await {
            tracing::warn!(
                "Failed to delete melt saga {}: {}. Will be cleaned up on recovery.",
                operation_id,
                e
            );
            // Don't fail the melt if saga deletion fails - orphaned saga is harmless
        }

        Ok(MeltSaga {
            wallet: self.wallet,
            compensations: self.compensations,
            state_data: Confirmed {
                operation_id: self.state_data.operation_id,
                amount: melted.amount,
                fee: melted.fee_paid,
                payment_preimage,
            },
        })
    }

    /// Cancel the prepared melt and release reserved proofs.
    ///
    /// This rolls back the melt operation by executing all registered
    /// compensating actions in reverse order.
    #[instrument(skip_all)]
    pub async fn cancel(self) -> Result<(), Error> {
        tracing::info!(
            "Cancelling prepared melt for operation {}",
            self.state_data.operation_id
        );

        execute_compensations(&self.compensations).await?;

        Ok(())
    }
}

impl MeltSaga<Confirmed> {
    /// Get the operation ID
    pub fn operation_id(&self) -> uuid::Uuid {
        self.state_data.operation_id
    }

    /// Get the amount melted
    pub fn amount(&self) -> Amount {
        self.state_data.amount
    }

    /// Get the fee paid
    pub fn fee(&self) -> Amount {
        self.state_data.fee
    }

    /// Get the payment preimage
    pub fn payment_preimage(&self) -> Option<&String> {
        self.state_data.payment_preimage.as_ref()
    }
}

impl std::fmt::Debug for MeltSaga<Prepared> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MeltSaga<Prepared>")
            .field("operation_id", &self.state_data.operation_id)
            .field("quote_id", &self.state_data.quote.id)
            .field("amount", &self.state_data.quote.amount)
            .field(
                "proofs",
                &self
                    .state_data
                    .proofs
                    .iter()
                    .map(|p| p.amount)
                    .collect::<Vec<_>>(),
            )
            .field(
                "proofs_to_swap",
                &self
                    .state_data
                    .proofs_to_swap
                    .iter()
                    .map(|p| p.amount)
                    .collect::<Vec<_>>(),
            )
            .field("swap_fee", &self.state_data.swap_fee)
            .field("input_fee", &self.state_data.input_fee)
            .finish()
    }
}
