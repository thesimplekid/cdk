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
//! ```

use std::collections::HashMap;

use cdk_common::wallet::{
    MeltOperationData, MeltSagaState, OperationData, WalletSaga, WalletSagaState,
};
use tracing::instrument;

use crate::util::unix_time;

use self::compensation::{ReleaseMeltQuote, RevertProofReservation};
use self::state::{Initial, Prepared};
use crate::nuts::nut00::ProofsMethods;
use crate::nuts::{Proofs, State};
use crate::types::ProofInfo;
use crate::wallet::saga::{add_compensation, new_compensations, Compensations};
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
pub struct MeltSaga<'a, S> {
    /// Wallet reference
    wallet: &'a Wallet,
    /// Compensating actions in LIFO order (most recent first)
    compensations: Compensations,
    /// State-specific data
    state_data: S,
}

impl<'a> MeltSaga<'a, Initial> {
    /// Create a new melt saga in the Initial state.
    pub fn new(wallet: &'a Wallet) -> Self {
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
        _metadata: HashMap<String, String>,
    ) -> Result<MeltSaga<'a, Prepared>, Error> {
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

        // Reserve the quote to prevent concurrent operations from using it
        self.wallet
            .localstore
            .reserve_melt_quote(quote_id, &self.state_data.operation_id)
            .await?;

        // Register compensation to release quote on failure
        add_compensation(
            &self.compensations,
            Box::new(ReleaseMeltQuote {
                localstore: self.wallet.localstore.clone(),
                operation_id: self.state_data.operation_id,
            }),
        )
        .await;

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

        // Only reserve proofs_to_send - proofs_to_swap will be reserved by the swap saga
        // when it runs in confirm(). This is important because if the swap succeeds,
        // proofs_to_swap are deleted and replaced by send_proofs + change_proofs.
        // We track send_proofs via RevertSwappedProofs compensation.
        let proof_ys = split_result.proofs_to_send.ys()?;
        let operation_id = self.state_data.operation_id;

        // Reserve only proofs_to_send
        if !proof_ys.is_empty() {
            self.wallet
                .localstore
                .update_proofs_state(proof_ys.clone(), State::Reserved)
                .await?;
        }

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
            },
        })
    }

    /// Prepare the melt operation with specific proofs (no automatic selection).
    ///
    /// Unlike `prepare()`, this method uses the provided proofs directly without
    /// automatic proof selection. The caller is responsible for ensuring the proofs
    /// are sufficient to cover the quote amount plus fee reserve.
    ///
    /// This is useful when:
    /// - You have specific proofs you want to use (e.g., from a token)
    /// - The proofs are external (not in the wallet's database)
    ///
    /// # Compensation
    ///
    /// Registers a compensation action that will revert proof state
    /// if later steps fail.
    #[instrument(skip_all)]
    pub async fn prepare_with_proofs(
        self,
        quote_id: &str,
        proofs: Proofs,
        _metadata: HashMap<String, String>,
    ) -> Result<MeltSaga<'a, Prepared>, Error> {
        tracing::info!(
            "Preparing melt with specific proofs for quote {} with operation {}",
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

        // Validate proofs are sufficient
        let proofs_total = proofs.total_amount()?;
        let inputs_needed = quote_info.amount + quote_info.fee_reserve;
        if proofs_total < inputs_needed {
            return Err(Error::InsufficientFunds);
        }

        // Reserve the quote to prevent concurrent operations from using it
        self.wallet
            .localstore
            .reserve_melt_quote(quote_id, &self.state_data.operation_id)
            .await?;

        // Register compensation to release quote on failure
        add_compensation(
            &self.compensations,
            Box::new(ReleaseMeltQuote {
                localstore: self.wallet.localstore.clone(),
                operation_id: self.state_data.operation_id,
            }),
        )
        .await;

        let operation_id = self.state_data.operation_id;
        let proof_ys = proofs.ys()?;

        // Since proofs may be external (not in our database), add them first
        // Set to Reserved state like the regular prepare() does
        let proofs_info = proofs
            .clone()
            .into_iter()
            .map(|p| {
                ProofInfo::new(
                    p,
                    self.wallet.mint_url.clone(),
                    State::Reserved,
                    self.wallet.unit.clone(),
                )
            })
            .collect::<Result<Vec<ProofInfo>, _>>()?;

        self.wallet
            .localstore
            .update_proofs(proofs_info, vec![])
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
                change_blinded_messages: None,
            }),
        );

        self.wallet.localstore.add_saga(saga).await?;

        // Register compensation to revert proofs on failure
        add_compensation(
            &self.compensations,
            Box::new(RevertProofReservation {
                localstore: self.wallet.localstore.clone(),
                proof_ys,
                saga_id: operation_id,
            }),
        )
        .await;

        let input_fee = self.wallet.get_proofs_fee(&proofs).await?.total;

        Ok(MeltSaga {
            wallet: self.wallet,
            compensations: self.compensations,
            state_data: Prepared {
                operation_id: self.state_data.operation_id,
                quote: quote_info,
                proofs,
                proofs_to_swap: Proofs::new(), // No swap needed - caller provided exact proofs
                swap_fee: Amount::ZERO,
                input_fee,
            },
        })
    }
}

impl<'a> MeltSaga<'a, Prepared> {
    /// Get the operation ID
    pub fn operation_id(&self) -> uuid::Uuid {
        self.state_data.operation_id
    }

    /// Get the quote
    pub fn quote(&self) -> &MeltQuote {
        &self.state_data.quote
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
}

impl std::fmt::Debug for MeltSaga<'_, Prepared> {
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
