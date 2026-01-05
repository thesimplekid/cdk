//! Send Saga - Type State Pattern Implementation
//!
//! This module implements the saga pattern for send operations using the typestate
//! pattern to enforce valid state transitions at compile-time.
//!
//! # Type State Flow
//!
//! ```text
//! SendSaga<Initial>
//!   └─> prepare() -> SendSaga<Prepared>
//!         ├─> confirm() -> SendSaga<Confirmed>
//!         └─> cancel() -> ()
//! ```
//!
//! # Persistence
//!
//! The saga state is persisted to the database for crash recovery:
//! - After `prepare()`: State = ProofsReserved
//! - Before swap/token creation in `confirm()`: State = TokenCreated
//! - After successful completion: Saga is deleted

use std::collections::HashMap;

use cdk_common::nut02::KeySetInfosMethods;
use cdk_common::util::unix_time;
use cdk_common::wallet::{
    OperationData, SendOperationData, SendSagaState, Transaction, TransactionDirection, WalletSaga,
    WalletSagaState,
};
use cdk_common::Id;
use tracing::instrument;

use self::compensation::{RevertProofReservation, RevertSwappedProofs};
use self::state::{Initial, Prepared};
use super::{split_proofs_for_send, SendMemo, SendOptions};
use crate::amount::SplitTarget;
use crate::nuts::nut00::ProofsMethods;
use crate::nuts::{Proofs, State, Token};
use crate::wallet::saga::{
    add_compensation, clear_compensations, execute_compensations, new_compensations, Compensations,
};
use crate::wallet::SendKind;
use crate::{Amount, Error, Wallet};

pub mod compensation;
pub mod state;

/// Saga pattern implementation for send operations.
///
/// Uses the typestate pattern to enforce valid state transitions at compile-time.
/// Each state (Initial, Prepared, Confirmed) is a distinct type, and operations
/// are only available on the appropriate type.
pub struct SendSaga<S> {
    /// Wallet reference
    wallet: Wallet,
    /// Compensating actions in LIFO order (most recent first)
    compensations: Compensations,
    /// State-specific data
    state_data: S,
}

impl SendSaga<Initial> {
    /// Create a new send saga in the Initial state.
    pub fn new(wallet: Wallet) -> Self {
        let operation_id = uuid::Uuid::new_v4();

        Self {
            wallet,
            compensations: new_compensations(),
            state_data: Initial { operation_id },
        }
    }

    /// Prepare the send operation by selecting and reserving proofs.
    ///
    /// This is the first step in the saga. It:
    /// 1. Refreshes keysets if online mode
    /// 2. Selects proofs for the requested amount
    /// 3. Reserves the selected proofs (sets state to Reserved)
    /// 4. Splits proofs between direct send and swap
    ///
    /// # Compensation
    ///
    /// Registers a compensation action that will revert proof reservation
    /// if later steps fail.
    #[instrument(skip_all)]
    pub async fn prepare(
        self,
        amount: Amount,
        opts: SendOptions,
    ) -> Result<SendSaga<Prepared>, Error> {
        tracing::info!(
            "Preparing send for {} with operation {}",
            amount,
            self.state_data.operation_id
        );

        // If online send check mint for current keysets fees
        if opts.send_kind.is_online() {
            if let Err(e) = self.wallet.refresh_keysets().await {
                tracing::error!("Error refreshing keysets: {:?}. Using stored keysets", e);
            }
        }

        // Get keyset fees from localstore
        let keyset_fees = self.wallet.get_keyset_fees_and_amounts().await?;

        // Get available proofs matching conditions
        let mut available_proofs = self
            .wallet
            .get_proofs_with(
                None,
                Some(vec![State::Unspent]),
                opts.conditions.clone().map(|c| vec![c]),
            )
            .await?;

        // Check if sufficient proofs are available
        let mut force_swap = false;
        let available_sum = available_proofs.total_amount()?;
        if available_sum < amount {
            if opts.conditions.is_none() || opts.send_kind.is_offline() {
                return Err(Error::InsufficientFunds);
            } else {
                // Swap is required for send
                tracing::debug!("Insufficient proofs matching conditions");
                force_swap = true;
                available_proofs = self
                    .wallet
                    .localstore
                    .get_proofs(
                        Some(self.wallet.mint_url.clone()),
                        Some(self.wallet.unit.clone()),
                        Some(vec![State::Unspent]),
                        Some(vec![]),
                    )
                    .await?
                    .into_iter()
                    .map(|p| p.proof)
                    .collect();
            }
        }

        // Select proofs
        let active_keyset_ids = self
            .wallet
            .get_mint_keysets()
            .await?
            .active()
            .map(|k| k.id)
            .collect();

        // Calculate selection amount including fees if needed
        let active_keyset_id = self.wallet.get_active_keyset().await?.id;
        let fee_and_amounts = self
            .wallet
            .get_keyset_fees_and_amounts_by_id(active_keyset_id)
            .await?;

        let selection_amount = if opts.include_fee {
            let send_split = amount.split_with_fee(&fee_and_amounts)?;
            let send_fee = self
                .wallet
                .get_proofs_fee_by_count(
                    vec![(active_keyset_id, send_split.len() as u64)]
                        .into_iter()
                        .collect(),
                )
                .await?;
            amount + send_fee.total
        } else {
            amount
        };

        let selected_proofs = Wallet::select_proofs(
            selection_amount,
            available_proofs,
            &active_keyset_ids,
            &keyset_fees,
            opts.include_fee,
        )?;
        let selected_total = selected_proofs.total_amount()?;

        // Check if selected proofs are exact
        let send_fee = if opts.include_fee {
            self.wallet.get_proofs_fee(&selected_proofs).await?.total
        } else {
            Amount::ZERO
        };

        // Early return for exact match
        if selected_total == amount + send_fee {
            return self
                .internal_prepare(amount, opts, selected_proofs, force_swap)
                .await;
        } else if opts.send_kind == SendKind::OfflineExact {
            return Err(Error::InsufficientFunds);
        }

        // Check if selected proofs are sufficient for tolerance
        let tolerance = match opts.send_kind {
            SendKind::OfflineTolerance(tolerance) => Some(tolerance),
            SendKind::OnlineTolerance(tolerance) => Some(tolerance),
            _ => None,
        };
        if let Some(tolerance) = tolerance {
            if selected_total - amount > tolerance && opts.send_kind.is_offline() {
                return Err(Error::InsufficientFunds);
            }
        }

        self.internal_prepare(amount, opts, selected_proofs, force_swap)
            .await
    }

    async fn internal_prepare(
        self,
        amount: Amount,
        opts: SendOptions,
        proofs: Proofs,
        force_swap: bool,
    ) -> Result<SendSaga<Prepared>, Error> {
        // Split amount with fee if necessary
        let active_keyset_id = self.wallet.get_active_keyset().await?.id;
        let fee_and_amounts = self
            .wallet
            .get_keyset_fees_and_amounts_by_id(active_keyset_id)
            .await?;

        let (send_amounts, send_fee) = if opts.include_fee {
            let send_split = amount.split_with_fee(&fee_and_amounts)?;
            let send_fee = self
                .wallet
                .get_proofs_fee_by_count(
                    vec![(active_keyset_id, send_split.len() as u64)]
                        .into_iter()
                        .collect(),
                )
                .await?;
            (send_split, send_fee)
        } else {
            let send_split = amount.split(&fee_and_amounts);
            let send_fee = crate::fees::ProofsFeeBreakdown {
                total: Amount::ZERO,
                per_keyset: std::collections::HashMap::new(),
            };
            (send_split, send_fee)
        };

        // Get proof Y values for reservation
        let proof_ys = proofs.ys()?;

        // Reserve proofs (atomic operation)
        self.wallet
            .localstore
            .update_proofs_state(proof_ys.clone(), State::Reserved)
            .await?;

        // Persist saga state for crash recovery
        let memo_text = opts.memo.as_ref().map(|m| m.memo.clone());
        let saga = WalletSaga::new(
            self.state_data.operation_id,
            WalletSagaState::Send(SendSagaState::ProofsReserved),
            amount,
            self.wallet.mint_url.clone(),
            self.wallet.unit.clone(),
            OperationData::Send(SendOperationData {
                amount,
                memo: memo_text.clone(),
                counter_start: None, // Will be set if swap is needed
                counter_end: None,
                token: None,
            }),
        );

        self.wallet.localstore.add_saga(saga).await?;

        // Register compensation to revert reservation and delete saga on failure
        add_compensation(
            &self.compensations,
            Box::new(RevertProofReservation {
                localstore: self.wallet.localstore.clone(),
                proof_ys,
                saga_id: self.state_data.operation_id,
            }),
        )
        .await;

        // Check if proofs are exact send amount
        let mut exact_proofs = proofs.total_amount()? == amount + send_fee.total;
        if let Some(max_proofs) = opts.max_proofs {
            exact_proofs &= proofs.len() <= max_proofs;
        }

        // Determine if we should send all proofs directly
        let is_exact_or_offline =
            exact_proofs || opts.send_kind.is_offline() || opts.send_kind.has_tolerance();

        // Get keyset fees for the split function
        let keyset_fees_and_amounts = self.wallet.get_keyset_fees_and_amounts().await?;
        let keyset_fees: HashMap<Id, u64> = keyset_fees_and_amounts
            .iter()
            .map(|(key, values)| (*key, values.fee()))
            .collect();

        // Split proofs between send and swap
        let split_result = split_proofs_for_send(
            proofs,
            &send_amounts,
            amount,
            send_fee.total,
            &keyset_fees,
            force_swap,
            is_exact_or_offline,
        )?;

        // Transition to Prepared state
        Ok(SendSaga {
            wallet: self.wallet,
            compensations: self.compensations,
            state_data: Prepared {
                operation_id: self.state_data.operation_id,
                amount,
                options: opts,
                proofs_to_swap: split_result.proofs_to_swap,
                swap_fee: split_result.swap_fee,
                proofs_to_send: split_result.proofs_to_send,
                send_fee: send_fee.total,
            },
        })
    }
}

impl SendSaga<Prepared> {
    /// Get the amount to be sent
    pub fn amount(&self) -> Amount {
        self.state_data.amount
    }

    /// Get the send options
    pub fn options(&self) -> &SendOptions {
        &self.state_data.options
    }

    /// Get the proofs that will be swapped
    pub fn proofs_to_swap(&self) -> &Proofs {
        &self.state_data.proofs_to_swap
    }

    /// Get the swap fee
    pub fn swap_fee(&self) -> Amount {
        self.state_data.swap_fee
    }

    /// Get the proofs that will be sent directly
    pub fn proofs_to_send(&self) -> &Proofs {
        &self.state_data.proofs_to_send
    }

    /// Get the send fee
    pub fn send_fee(&self) -> Amount {
        self.state_data.send_fee
    }

    /// Get all proofs (both to swap and to send)
    pub fn all_proofs(&self) -> Proofs {
        self.state_data.all_proofs()
    }

    /// Get all proofs (alias for backward compatibility with PreparedSend)
    pub fn proofs(&self) -> Proofs {
        self.all_proofs()
    }

    /// Get the total fee
    pub fn total_fee(&self) -> Amount {
        self.state_data.total_fee()
    }

    /// Get the total fee (alias for backward compatibility with PreparedSend)
    pub fn fee(&self) -> Amount {
        self.total_fee()
    }

    /// Confirm the prepared send and create a token.
    ///
    /// This completes the send operation by:
    /// 1. Updating saga state to TokenCreated
    /// 2. Swapping proofs if necessary
    /// 3. Creating the token
    /// 4. Marking proofs as PendingSpent
    /// 5. Recording the transaction
    /// 6. Deleting the saga
    ///
    /// On success, compensations are cleared and the token is returned.
    #[instrument(skip_all)]
    pub async fn confirm(self, memo: Option<SendMemo>) -> Result<Token, Error> {
        tracing::info!(
            "Confirming prepared send for operation {}",
            self.state_data.operation_id
        );

        let operation_id = self.state_data.operation_id;
        let total_send_fee = self.total_fee();
        let mut proofs_to_send = self.state_data.proofs_to_send.clone();

        // Get active keyset ID
        let active_keyset_id = self.wallet.fetch_active_keyset().await?.id;

        // Get keyset fees (used for future fee verification)
        let _keyset_fee_ppk = self
            .wallet
            .get_keyset_fees_and_amounts_by_id(active_keyset_id)
            .await?;

        // Calculate total send amount
        let total_send_amount = self.state_data.amount + self.state_data.send_fee;

        // Update saga state to TokenCreated BEFORE making external calls
        // This is write-ahead logging - if we crash after this, recovery knows
        // the operation may have progressed
        let memo_text = self
            .state_data
            .options
            .memo
            .as_ref()
            .map(|m| m.memo.clone());
        let updated_saga = WalletSaga::new(
            operation_id,
            WalletSagaState::Send(SendSagaState::TokenCreated),
            self.state_data.amount,
            self.wallet.mint_url.clone(),
            self.wallet.unit.clone(),
            OperationData::Send(SendOperationData {
                amount: self.state_data.amount,
                memo: memo_text,
                counter_start: None,
                counter_end: None,
                token: None, // Will be populated if we need to store it
            }),
        );

        // Update saga state - if this fails due to version conflict, another instance
        // is processing this saga, which should not happen during normal operation
        if !self.wallet.localstore.update_saga(updated_saga).await? {
            return Err(Error::Custom(
                "Saga version conflict during update - another instance may be processing this saga".to_string(),
            ));
        }

        // Swap proofs if necessary
        if !self.state_data.proofs_to_swap.is_empty() {
            let swap_amount = total_send_amount
                .checked_sub(proofs_to_send.total_amount()?)
                .unwrap_or(Amount::ZERO);

            tracing::debug!("Swapping proofs; swap_amount={:?}", swap_amount);

            if let Some(proofs) = self
                .wallet
                .swap(
                    Some(swap_amount),
                    SplitTarget::None,
                    self.state_data.proofs_to_swap.clone(),
                    self.state_data.options.conditions.clone(),
                    false, // already included in swap_amount
                )
                .await?
            {
                // Track swapped proofs for compensation if send fails later.
                // The nested swap saga has completed and deleted its own tracking,
                // so we must register compensation for these new proofs.
                let swapped_ys = proofs.ys()?;

                // Register compensation to revert swapped proofs on failure
                add_compensation(
                    &self.compensations,
                    Box::new(RevertSwappedProofs {
                        localstore: self.wallet.localstore.clone(),
                        proof_ys: swapped_ys,
                    }),
                )
                .await;

                proofs_to_send.extend(proofs);
            }
        }

        // Check if sufficient proofs are available
        if self.state_data.amount > proofs_to_send.total_amount()? {
            execute_compensations(&self.compensations).await?;
            return Err(Error::InsufficientFunds);
        }

        // Check if proofs are reserved or unspent
        let sendable_proofs = self
            .wallet
            .localstore
            .get_proofs(
                Some(self.wallet.mint_url.clone()),
                Some(self.wallet.unit.clone()),
                Some(vec![State::Reserved, State::Unspent]),
                self.state_data.options.conditions.clone().map(|c| vec![c]),
            )
            .await?;

        let sendable_proof_ys = sendable_proofs.into_iter().map(|p| p.y).collect::<Vec<_>>();

        if proofs_to_send
            .ys()?
            .iter()
            .any(|y| !sendable_proof_ys.contains(y))
        {
            tracing::warn!("Proofs to send are not reserved or unspent");
            execute_compensations(&self.compensations).await?;
            return Err(Error::UnexpectedProofState);
        }

        // Update proofs state to pending spent
        self.wallet
            .localstore
            .update_proofs_state(proofs_to_send.ys()?, State::PendingSpent)
            .await?;

        // Include token memo
        let send_memo = self.state_data.options.memo.clone().or(memo);
        let token_memo = send_memo.and_then(|m| if m.include_memo { Some(m.memo) } else { None });

        // Add transaction to store
        self.wallet
            .localstore
            .add_transaction(Transaction {
                mint_url: self.wallet.mint_url.clone(),
                direction: TransactionDirection::Outgoing,
                amount: self.state_data.amount,
                fee: total_send_fee,
                unit: self.wallet.unit.clone(),
                ys: proofs_to_send.ys()?,
                timestamp: unix_time(),
                memo: token_memo.clone(),
                metadata: self.state_data.options.metadata.clone(),
                quote_id: None,
                payment_request: None,
                payment_proof: None,
                payment_method: None,
            })
            .await?;

        // Create token
        let token = Token::new(
            self.wallet.mint_url.clone(),
            proofs_to_send,
            token_memo,
            self.wallet.unit.clone(),
        );

        // Clear compensations - operation completed successfully
        clear_compensations(&self.compensations).await;

        // Delete saga record - send completed successfully (best-effort)
        if let Err(e) = self.wallet.localstore.delete_saga(&operation_id).await {
            tracing::warn!(
                "Failed to delete send saga {}: {}. Will be cleaned up on recovery.",
                operation_id,
                e
            );
            // Don't fail the send if saga deletion fails - orphaned saga is harmless
        }

        Ok(token)
    }

    /// Cancel the prepared send and release reserved proofs.
    ///
    /// This rolls back the send operation by executing all registered
    /// compensating actions in reverse order.
    #[instrument(skip_all)]
    pub async fn cancel(self) -> Result<(), Error> {
        tracing::info!(
            "Cancelling prepared send for operation {}",
            self.state_data.operation_id
        );

        // Execute compensations (which will revert proof reservation)
        execute_compensations(&self.compensations).await?;

        Ok(())
    }
}

impl std::fmt::Debug for SendSaga<Prepared> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SendSaga<Prepared>")
            .field("operation_id", &self.state_data.operation_id)
            .field("amount", &self.state_data.amount)
            .field("options", &self.state_data.options)
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
            .field(
                "proofs_to_send",
                &self
                    .state_data
                    .proofs_to_send
                    .iter()
                    .map(|p| p.amount)
                    .collect::<Vec<_>>(),
            )
            .field("send_fee", &self.state_data.send_fee)
            .finish()
    }
}
