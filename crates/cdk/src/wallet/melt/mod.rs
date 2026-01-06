//! Melt Module
//!
//! This module provides the melt functionality for the wallet.
//!
//! # Usage
//!
//! ## Simple melt (convenience method)
//! ```rust,no_run
//! # async fn example(wallet: &cdk::wallet::Wallet) -> anyhow::Result<()> {
//! let quote = wallet.melt_quote("lnbc...".to_string(), None).await?;
//! let melted = wallet.melt(&quote.id).await?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Two-phase melt (prepare then confirm)
//! ```rust,no_run
//! # async fn example(wallet: &cdk::wallet::Wallet) -> anyhow::Result<()> {
//! use std::collections::HashMap;
//! let quote = wallet.melt_quote("lnbc...".to_string(), None).await?;
//!
//! // Prepare the melt - proofs are reserved but payment not yet executed
//! let prepared = wallet.prepare_melt(&quote.id, HashMap::new()).await?;
//!
//! // Inspect the prepared melt
//! println!("Amount: {}, Fee: {}", prepared.amount(), prepared.total_fee());
//!
//! // Either confirm or cancel
//! let confirmed = prepared.confirm().await?;
//! // Or: prepared.cancel().await?;
//! # Ok(())
//! # }
//! ```

use std::collections::HashMap;
use std::fmt::Debug;

use cdk_common::amount::SplitTarget;
use cdk_common::util::unix_time;
use cdk_common::wallet::{
    MeltOperationData, MeltQuote, MeltSagaState, OperationData, Transaction, TransactionDirection,
    WalletSaga, WalletSagaState,
};
use cdk_common::{Error, MeltQuoteBolt11Response, MeltQuoteState, ProofsMethods, State};
use tracing::instrument;
use uuid::Uuid;

use crate::dhke::construct_proofs;
use crate::nuts::{MeltRequest, PreMintSecrets, Proofs};
use crate::types::{Melted, ProofInfo};
use crate::{Amount, Wallet};

#[cfg(all(feature = "bip353", not(target_arch = "wasm32")))]
mod melt_bip353;
mod melt_bolt11;
mod melt_bolt12;
#[cfg(feature = "wallet")]
mod melt_lightning_address;
pub(crate) mod saga;

use saga::MeltSaga;

/// A prepared melt operation that can be confirmed or cancelled.
///
/// This is the result of calling [`Wallet::prepare_melt`]. The proofs are reserved
/// but the payment has not yet been executed.
///
/// Call [`confirm`](Self::confirm) to execute the melt, or [`cancel`](Self::cancel)
/// to release the reserved proofs.
pub struct PreparedMelt<'a> {
    wallet: &'a Wallet,
    operation_id: Uuid,
    // Cached data for display and confirm
    quote: MeltQuote,
    proofs: Proofs,
    proofs_to_swap: Proofs,
    swap_fee: Amount,
    input_fee: Amount,
    metadata: HashMap<String, String>,
}

impl<'a> PreparedMelt<'a> {
    /// Get the operation ID
    pub fn operation_id(&self) -> Uuid {
        self.operation_id
    }

    /// Get the quote
    pub fn quote(&self) -> &MeltQuote {
        &self.quote
    }

    /// Get the amount to be melted
    pub fn amount(&self) -> Amount {
        self.quote.amount
    }

    /// Get the proofs that will be used
    pub fn proofs(&self) -> &Proofs {
        &self.proofs
    }

    /// Get the proofs that need to be swapped
    pub fn proofs_to_swap(&self) -> &Proofs {
        &self.proofs_to_swap
    }

    /// Get the swap fee
    pub fn swap_fee(&self) -> Amount {
        self.swap_fee
    }

    /// Get the input fee
    pub fn input_fee(&self) -> Amount {
        self.input_fee
    }

    /// Get the total fee
    pub fn total_fee(&self) -> Amount {
        self.swap_fee + self.input_fee
    }

    /// Confirm the prepared melt and execute the payment
    pub async fn confirm(self) -> Result<ConfirmedMelt, Error> {
        self.wallet
            .confirm_melt(
                self.operation_id,
                self.quote,
                self.proofs,
                self.proofs_to_swap,
                self.input_fee,
                self.metadata,
            )
            .await
    }

    /// Cancel the prepared melt and release reserved proofs
    pub async fn cancel(self) -> Result<(), Error> {
        self.wallet
            .cancel_melt(self.operation_id, self.proofs, self.proofs_to_swap)
            .await
    }
}

impl Debug for PreparedMelt<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedMelt")
            .field("operation_id", &self.operation_id)
            .field("quote_id", &self.quote.id)
            .field("amount", &self.quote.amount)
            .field("total_fee", &self.total_fee())
            .finish()
    }
}

/// A confirmed melt operation.
///
/// This is the result of calling [`PreparedMelt::confirm`]. The payment has been
/// executed (or is pending).
pub struct ConfirmedMelt {
    state: MeltQuoteState,
    amount: Amount,
    fee: Amount,
    payment_preimage: Option<String>,
    change: Option<Proofs>,
}

impl ConfirmedMelt {
    /// Get the state of the melt (Paid, Pending, etc.)
    pub fn state(&self) -> MeltQuoteState {
        self.state
    }

    /// Get the amount melted
    pub fn amount(&self) -> Amount {
        self.amount
    }

    /// Get the fee paid
    pub fn fee(&self) -> Amount {
        self.fee
    }

    /// Get the payment preimage
    pub fn payment_preimage(&self) -> Option<&String> {
        self.payment_preimage.as_ref()
    }

    /// Get the change proofs returned from the melt
    pub fn change(&self) -> Option<&Proofs> {
        self.change.as_ref()
    }
}

impl Debug for ConfirmedMelt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfirmedMelt")
            .field("state", &self.state)
            .field("amount", &self.amount)
            .field("fee", &self.fee)
            .finish()
    }
}

impl Wallet {
    /// Prepare a melt operation without executing it.
    ///
    /// This reserves the proofs needed for the melt but does not execute the payment.
    /// The returned `PreparedMelt` can be:
    /// - Confirmed with `confirm()` to execute the payment
    /// - Cancelled with `cancel()` to release the reserved proofs
    ///
    /// This is useful for:
    /// - Inspecting the fee before committing to the melt
    /// - Building UIs that show a confirmation step
    /// - Implementing custom retry/cancellation logic
    ///
    /// # Example
    /// ```rust,no_run
    /// # async fn example(wallet: &cdk::wallet::Wallet) -> anyhow::Result<()> {
    /// use std::collections::HashMap;
    /// let quote = wallet.melt_quote("lnbc...".to_string(), None).await?;
    ///
    /// let prepared = wallet.prepare_melt(&quote.id, HashMap::new()).await?;
    /// println!("Fee will be: {}", prepared.total_fee());
    ///
    /// // Decide whether to proceed
    /// let confirmed = prepared.confirm().await?;
    /// # Ok(())
    /// # }
    /// ```
    #[instrument(skip(self, metadata))]
    pub async fn prepare_melt(
        &self,
        quote_id: &str,
        metadata: HashMap<String, String>,
    ) -> Result<PreparedMelt<'_>, Error> {
        let saga = MeltSaga::new(self);
        let prepared_saga = saga.prepare(quote_id, metadata.clone()).await?;

        // Extract data from the saga into PreparedMelt
        let prepared = PreparedMelt {
            wallet: self,
            operation_id: prepared_saga.operation_id(),
            quote: prepared_saga.quote().clone(),
            proofs: prepared_saga.proofs().clone(),
            proofs_to_swap: prepared_saga.proofs_to_swap().clone(),
            swap_fee: prepared_saga.swap_fee(),
            input_fee: prepared_saga.input_fee(),
            metadata,
        };

        Ok(prepared)
    }

    /// Melt tokens to pay a Lightning invoice.
    ///
    /// This is a convenience method that prepares and confirms the melt in one step.
    /// For more control, use `prepare_melt()` instead.
    ///
    /// # Example
    /// ```rust,no_run
    /// # async fn example(wallet: &cdk::wallet::Wallet) -> anyhow::Result<()> {
    /// let quote = wallet.melt_quote("lnbc...".to_string(), None).await?;
    /// let melted = wallet.melt(&quote.id).await?;
    /// println!("Paid {} sats, fee: {} sats", melted.amount, melted.fee_paid);
    /// # Ok(())
    /// # }
    /// ```
    #[instrument(skip(self))]
    pub async fn melt(&self, quote_id: &str) -> Result<Melted, Error> {
        self.melt_with_metadata(quote_id, HashMap::new()).await
    }

    /// Melt tokens with additional metadata saved with the transaction.
    ///
    /// Like `melt()`, but allows attaching custom metadata to the transaction record.
    ///
    /// Note: The returned `Melted` struct contains the actual state from the mint.
    /// If `state` is `Pending`, the payment is in-flight and will complete later.
    /// Call `check_pending_melt_quotes()` to finalize pending melts.
    #[instrument(skip(self, metadata))]
    pub async fn melt_with_metadata(
        &self,
        quote_id: &str,
        metadata: HashMap<String, String>,
    ) -> Result<Melted, Error> {
        let prepared = self.prepare_melt(quote_id, metadata).await?;
        let confirmed = prepared.confirm().await?;

        Ok(Melted {
            state: confirmed.state(),
            preimage: confirmed.payment_preimage().cloned(),
            amount: confirmed.amount(),
            fee_paid: confirmed.fee(),
            change: confirmed.change().cloned(),
        })
    }

    /// Prepare a melt operation with specific proofs.
    ///
    /// Unlike `prepare_melt()`, this method uses the provided proofs directly
    /// without automatic proof selection. The caller is responsible for ensuring
    /// the proofs are sufficient to cover the quote amount plus fee reserve.
    ///
    /// This is useful when:
    /// - You have specific proofs you want to use (e.g., from a received token)
    /// - The proofs are external (not already in the wallet's database)
    ///
    /// # Example
    /// ```rust,no_run
    /// # async fn example(wallet: &cdk::wallet::Wallet, proofs: cdk::nuts::Proofs) -> anyhow::Result<()> {
    /// use std::collections::HashMap;
    /// let quote = wallet.melt_quote("lnbc...".to_string(), None).await?;
    ///
    /// let prepared = wallet.prepare_melt_proofs(&quote.id, proofs, HashMap::new()).await?;
    /// let confirmed = prepared.confirm().await?;
    /// # Ok(())
    /// # }
    /// ```
    #[instrument(skip(self, proofs, metadata))]
    pub async fn prepare_melt_proofs(
        &self,
        quote_id: &str,
        proofs: crate::nuts::Proofs,
        metadata: HashMap<String, String>,
    ) -> Result<PreparedMelt<'_>, Error> {
        let saga = MeltSaga::new(self);
        let prepared_saga = saga
            .prepare_with_proofs(quote_id, proofs, metadata.clone())
            .await?;

        // Extract data from the saga into PreparedMelt
        let prepared = PreparedMelt {
            wallet: self,
            operation_id: prepared_saga.operation_id(),
            quote: prepared_saga.quote().clone(),
            proofs: prepared_saga.proofs().clone(),
            proofs_to_swap: prepared_saga.proofs_to_swap().clone(),
            swap_fee: prepared_saga.swap_fee(),
            input_fee: prepared_saga.input_fee(),
            metadata,
        };

        // Drop the saga - state is persisted in DB
        drop(prepared_saga);

        Ok(prepared)
    }

    /// Melt specific proofs to pay a Lightning invoice.
    ///
    /// Unlike `melt()`, this method uses the provided proofs directly without
    /// automatic proof selection.
    ///
    /// # Example
    /// ```rust,no_run
    /// # async fn example(wallet: &cdk::wallet::Wallet, proofs: cdk::nuts::Proofs) -> anyhow::Result<()> {
    /// let quote = wallet.melt_quote("lnbc...".to_string(), None).await?;
    /// let melted = wallet.melt_proofs(&quote.id, proofs).await?;
    /// # Ok(())
    /// # }
    /// ```
    #[instrument(skip(self, proofs))]
    pub async fn melt_proofs(
        &self,
        quote_id: &str,
        proofs: crate::nuts::Proofs,
    ) -> Result<Melted, Error> {
        self.melt_proofs_with_metadata(quote_id, proofs, HashMap::new())
            .await
    }

    /// Melt specific proofs with additional metadata saved with the transaction.
    #[instrument(skip(self, proofs, metadata))]
    pub async fn melt_proofs_with_metadata(
        &self,
        quote_id: &str,
        proofs: crate::nuts::Proofs,
        metadata: HashMap<String, String>,
    ) -> Result<Melted, Error> {
        let prepared = self.prepare_melt_proofs(quote_id, proofs, metadata).await?;
        let confirmed = prepared.confirm().await?;

        Ok(Melted {
            state: confirmed.state(),
            preimage: confirmed.payment_preimage().cloned(),
            amount: confirmed.amount(),
            fee_paid: confirmed.fee(),
            change: confirmed.change().cloned(),
        })
    }

    /// Check pending melt quotes
    #[instrument(skip_all)]
    pub async fn check_pending_melt_quotes(&self) -> Result<(), Error> {
        let quotes = self.get_pending_melt_quotes().await?;
        for quote in quotes {
            self.melt_quote_status(&quote.id).await?;
        }
        Ok(())
    }

    /// Confirm a prepared melt and execute the payment
    ///
    /// This is called by `PreparedMelt::confirm` with the cached data.
    #[instrument(skip(self, proofs, proofs_to_swap, metadata))]
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn confirm_melt(
        &self,
        operation_id: Uuid,
        quote: MeltQuote,
        proofs: Proofs,
        proofs_to_swap: Proofs,
        input_fee: Amount,
        metadata: HashMap<String, String>,
    ) -> Result<ConfirmedMelt, Error> {
        tracing::info!("Confirming melt for operation {}", operation_id);

        let active_keyset_id = self.fetch_active_keyset().await?.id;
        let mut quote_info = quote.clone();

        let mut final_proofs = proofs.clone();

        // Swap if necessary to get optimal denominations for the melt
        // We swap for a specific amount to ensure optimal output denominations,
        // which keeps the actual input_fee matching our estimate.
        if !proofs_to_swap.is_empty() {
            // Calculate the target amount we need after swap:
            // quote.amount + fee_reserve + input_fee (the estimated melt input fee)
            let target_swap_amount = quote_info.amount + quote_info.fee_reserve + input_fee;

            tracing::debug!(
                "Swapping {} proofs (total: {}) for target amount {}",
                proofs_to_swap.len(),
                proofs_to_swap.total_amount()?,
                target_swap_amount
            );

            if let Some(swapped) = self
                .swap(
                    Some(target_swap_amount),
                    SplitTarget::None,
                    proofs_to_swap.clone(),
                    None,
                    false,
                )
                .await?
            {
                final_proofs.extend(swapped);
            }
        }

        // Recalculate the actual input_fee based on final_proofs
        let actual_input_fee = self.get_proofs_fee(&final_proofs).await?.total;
        let inputs_needed_amount = quote_info.amount + quote_info.fee_reserve + actual_input_fee;

        let proofs_total = final_proofs.total_amount()?;
        if proofs_total < inputs_needed_amount {
            // Revert proofs on insufficient funds
            let all_ys = final_proofs.ys()?;
            self.localstore
                .update_proofs_state(all_ys, State::Unspent)
                .await?;
            let _ = self.localstore.release_melt_quote(&operation_id).await;
            let _ = self.localstore.delete_saga(&operation_id).await;
            return Err(Error::InsufficientFunds);
        }

        // Set proofs to Pending state before making melt request
        let operation_id_str = operation_id.to_string();
        let proofs_info = final_proofs
            .clone()
            .into_iter()
            .map(|p| {
                ProofInfo::new_with_operations(
                    p,
                    self.mint_url.clone(),
                    State::Pending,
                    self.unit.clone(),
                    Some(operation_id_str.clone()),
                    None,
                )
            })
            .collect::<Result<Vec<ProofInfo>, _>>()?;

        self.localstore.update_proofs(proofs_info, vec![]).await?;

        // Calculate change accounting for input fees
        // Note: actual_input_fee was already calculated above for checking if we have enough
        let change_amount = proofs_total - quote_info.amount - actual_input_fee;

        let premint_secrets = if change_amount <= Amount::ZERO {
            PreMintSecrets::new(active_keyset_id)
        } else {
            let num_secrets =
                ((u64::from(change_amount) as f64).log2().ceil() as u64).max(1) as u32;

            let new_counter = self
                .localstore
                .increment_keyset_counter(&active_keyset_id, num_secrets)
                .await?;

            let count = new_counter - num_secrets;

            PreMintSecrets::from_seed_blank(active_keyset_id, count, &self.seed, change_amount)?
        };

        let request = MeltRequest::new(
            quote_info.id.clone(),
            final_proofs.clone(),
            Some(premint_secrets.blinded_messages()),
        );

        // Get counter range for recovery
        let counter_end = self
            .localstore
            .increment_keyset_counter(&active_keyset_id, 0)
            .await?;
        let counter_start = counter_end.saturating_sub(premint_secrets.secrets.len() as u32);

        // Update saga state to MeltRequested BEFORE making the melt call
        let updated_saga = WalletSaga::new(
            operation_id,
            WalletSagaState::Melt(MeltSagaState::MeltRequested),
            quote_info.amount,
            self.mint_url.clone(),
            self.unit.clone(),
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

        if !self.localstore.update_saga(updated_saga).await? {
            return Err(Error::Custom(
                "Saga version conflict during update".to_string(),
            ));
        }

        // Make the melt request based on payment method
        let melt_result = match quote_info.payment_method {
            cdk_common::PaymentMethod::Bolt11 => self.client.post_melt(request).await,
            cdk_common::PaymentMethod::Bolt12 => self.client.post_melt_bolt12(request).await,
            cdk_common::PaymentMethod::Custom(_) => {
                // Revert on unsupported payment method
                let all_ys = final_proofs.ys()?;
                self.localstore
                    .update_proofs_state(all_ys, State::Unspent)
                    .await?;
                let _ = self.localstore.release_melt_quote(&operation_id).await;
                let _ = self.localstore.delete_saga(&operation_id).await;
                return Err(Error::UnsupportedPaymentMethod);
            }
        };

        // Handle the result
        let melt_response = match melt_result {
            Ok(response) => response,
            Err(e) => {
                // Check for known terminal errors first
                if matches!(e, Error::RequestAlreadyPaid) {
                    tracing::info!("Invoice already paid by another wallet - releasing proofs");
                    let all_ys = final_proofs.ys()?;
                    self.localstore
                        .update_proofs_state(all_ys, State::Unspent)
                        .await?;
                    let _ = self.localstore.release_melt_quote(&operation_id).await;
                    let _ = self.localstore.delete_saga(&operation_id).await;
                    return Err(e);
                }

                // On HTTP error, check quote status to determine if payment failed
                tracing::warn!(
                    "Melt request failed with error: {}. Checking quote status...",
                    e
                );

                match self.melt_quote_status(&quote_info.id).await {
                    Ok(status) => {
                        match status.state {
                            MeltQuoteState::Failed
                            | MeltQuoteState::Unknown
                            | MeltQuoteState::Unpaid => {
                                // Payment failed - release proofs
                                tracing::info!(
                                    "Quote {} status is {:?} - releasing proofs",
                                    quote_info.id,
                                    status.state
                                );
                                let all_ys = final_proofs.ys()?;
                                self.localstore
                                    .update_proofs_state(all_ys, State::Unspent)
                                    .await?;
                                let _ = self.localstore.release_melt_quote(&operation_id).await;
                                let _ = self.localstore.delete_saga(&operation_id).await;
                                return Err(Error::PaymentFailed);
                            }
                            MeltQuoteState::Pending | MeltQuoteState::Paid => {
                                // Payment is in flight or succeeded - keep proofs pending
                                tracing::info!(
                                    "Quote {} status is {:?} - keeping proofs pending",
                                    quote_info.id,
                                    status.state
                                );
                            }
                        }
                    }
                    Err(check_err) => {
                        // Can't check status - keep proofs pending
                        tracing::warn!(
                            "Failed to check quote {} status: {}. Keeping proofs pending.",
                            quote_info.id,
                            check_err
                        );
                    }
                }

                // Update saga state to PaymentPending for recovery
                let pending_saga = WalletSaga::new(
                    operation_id,
                    WalletSagaState::Melt(MeltSagaState::PaymentPending),
                    quote_info.amount,
                    self.mint_url.clone(),
                    self.unit.clone(),
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

                if let Err(saga_err) = self.localstore.update_saga(pending_saga).await {
                    tracing::warn!(
                        "Failed to update saga {} to PaymentPending state: {}",
                        operation_id,
                        saga_err
                    );
                }

                return Err(Error::PaymentPending);
            }
        };

        let active_keys = self.load_keyset_keys(active_keyset_id).await?;

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

        // Update quote state
        quote_info.state = melt_response.state;

        let payment_request = quote_info.request.clone();
        let payment_method = quote_info.payment_method.clone();
        self.localstore.add_melt_quote(quote_info.clone()).await?;

        // Handle based on melt response state
        match melt_response.state {
            MeltQuoteState::Paid => {
                // Payment completed - finalize
                let change_proof_infos = match change_proofs.clone() {
                    Some(change_proofs) => change_proofs
                        .into_iter()
                        .map(|proof| {
                            ProofInfo::new(
                                proof,
                                self.mint_url.clone(),
                                State::Unspent,
                                quote_info.unit.clone(),
                            )
                        })
                        .collect::<Result<Vec<ProofInfo>, _>>()?,
                    None => Vec::new(),
                };

                let deleted_ys = final_proofs.ys()?;

                self.localstore
                    .update_proofs(change_proof_infos, deleted_ys)
                    .await?;

                // Add transaction
                self.localstore
                    .add_transaction(Transaction {
                        mint_url: self.mint_url.clone(),
                        direction: TransactionDirection::Outgoing,
                        amount: melted.amount,
                        fee: melted.fee_paid,
                        unit: self.unit.clone(),
                        ys: final_proofs.ys()?,
                        timestamp: unix_time(),
                        memo: None,
                        metadata,
                        quote_id: Some(quote.id.clone()),
                        payment_request: Some(payment_request),
                        payment_proof: payment_preimage.clone(),
                        payment_method: Some(payment_method),
                    })
                    .await?;

                // Release quote reservation
                if let Err(e) = self.localstore.release_melt_quote(&operation_id).await {
                    tracing::warn!(
                        "Failed to release melt quote for operation {}: {}",
                        operation_id,
                        e
                    );
                }

                // Delete saga record
                if let Err(e) = self.localstore.delete_saga(&operation_id).await {
                    tracing::warn!(
                        "Failed to delete melt saga {}: {}. Will be cleaned up on recovery.",
                        operation_id,
                        e
                    );
                }

                Ok(ConfirmedMelt {
                    state: melt_response.state,
                    amount: melted.amount,
                    fee: melted.fee_paid,
                    payment_preimage,
                    change: change_proofs,
                })
            }
            MeltQuoteState::Pending => {
                // Payment in flight - keep proofs pending
                tracing::info!(
                    "Melt quote {} is pending - proofs kept in pending state",
                    quote_info.id
                );

                // Update saga to PaymentPending
                let pending_saga = WalletSaga::new(
                    operation_id,
                    WalletSagaState::Melt(MeltSagaState::PaymentPending),
                    quote_info.amount,
                    self.mint_url.clone(),
                    self.unit.clone(),
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

                if let Err(e) = self.localstore.update_saga(pending_saga).await {
                    tracing::warn!(
                        "Failed to update saga {} to PaymentPending state: {}",
                        operation_id,
                        e
                    );
                }

                Err(Error::PaymentPending)
            }
            MeltQuoteState::Failed => {
                // Payment failed - release proofs
                tracing::warn!("Melt quote {} failed - releasing proofs", quote_info.id);

                let all_ys = final_proofs.ys()?;
                self.localstore
                    .update_proofs_state(all_ys, State::Unspent)
                    .await?;
                let _ = self.localstore.release_melt_quote(&operation_id).await;
                let _ = self.localstore.delete_saga(&operation_id).await;

                Err(Error::PaymentFailed)
            }
            _ => {
                // Unknown state - keep proofs pending
                tracing::warn!(
                    "Melt quote {} returned unexpected state {:?} - keeping proofs pending",
                    quote_info.id,
                    melt_response.state
                );

                Ok(ConfirmedMelt {
                    state: melt_response.state,
                    amount: melted.amount,
                    fee: melted.fee_paid,
                    payment_preimage,
                    change: change_proofs,
                })
            }
        }
    }

    /// Cancel a prepared melt and release reserved proofs
    ///
    /// This is called by `PreparedMelt::cancel` with the cached data.
    #[instrument(skip(self, proofs, proofs_to_swap))]
    pub(crate) async fn cancel_melt(
        &self,
        operation_id: Uuid,
        proofs: Proofs,
        proofs_to_swap: Proofs,
    ) -> Result<(), Error> {
        tracing::info!("Cancelling prepared melt for operation {}", operation_id);

        // Collect all proof Ys
        let mut all_ys = proofs.ys()?;
        all_ys.extend(proofs_to_swap.ys()?);

        // Revert proof reservation (only proofs that were reserved - proofs_to_send)
        // proofs_to_swap are not reserved until confirm() swaps them
        let proofs_ys = proofs.ys()?;
        if !proofs_ys.is_empty() {
            self.localstore
                .update_proofs_state(proofs_ys, State::Unspent)
                .await?;
        }

        // Release melt quote reservation
        if let Err(e) = self.localstore.release_melt_quote(&operation_id).await {
            tracing::warn!(
                "Failed to release melt quote for operation {}: {}",
                operation_id,
                e
            );
        }

        // Delete saga record
        if let Err(e) = self.localstore.delete_saga(&operation_id).await {
            tracing::warn!(
                "Failed to delete melt saga {}: {}. Will be cleaned up on recovery.",
                operation_id,
                e
            );
        }

        Ok(())
    }

    /// Get all active melt quotes from the wallet
    pub async fn get_active_melt_quotes(&self) -> Result<Vec<MeltQuote>, Error> {
        let quotes = self.localstore.get_melt_quotes().await?;
        Ok(quotes
            .into_iter()
            .filter(|q| {
                q.state == MeltQuoteState::Pending
                    || (q.state == MeltQuoteState::Unpaid && q.expiry > unix_time())
            })
            .collect())
    }

    /// Get pending melt quotes
    pub async fn get_pending_melt_quotes(&self) -> Result<Vec<MeltQuote>, Error> {
        let quotes = self.localstore.get_melt_quotes().await?;
        Ok(quotes
            .into_iter()
            .filter(|q| q.state == MeltQuoteState::Pending)
            .collect())
    }

    pub(crate) async fn add_transaction_for_pending_melt(
        &self,
        quote: &MeltQuote,
        response: &MeltQuoteBolt11Response<String>,
    ) -> Result<(), Error> {
        if quote.state != response.state {
            tracing::info!(
                "Quote melt {} state changed from {} to {}",
                quote.id,
                quote.state,
                response.state
            );
            if response.state == MeltQuoteState::Paid {
                let pending_proofs = self
                    .get_proofs_with(None, Some(vec![State::Pending]), None)
                    .await?;
                let proofs_total = pending_proofs.total_amount().unwrap_or_default();
                let change_total = response.change_amount().unwrap_or_default();

                self.localstore
                    .add_transaction(Transaction {
                        mint_url: self.mint_url.clone(),
                        direction: TransactionDirection::Outgoing,
                        amount: response.amount,
                        fee: proofs_total
                            .checked_sub(response.amount)
                            .and_then(|amt| amt.checked_sub(change_total))
                            .unwrap_or_default(),
                        unit: quote.unit.clone(),
                        ys: pending_proofs.ys()?,
                        timestamp: unix_time(),
                        memo: None,
                        metadata: HashMap::new(),
                        quote_id: Some(quote.id.clone()),
                        payment_request: Some(quote.request.clone()),
                        payment_proof: response.payment_preimage.clone(),
                        payment_method: Some(quote.payment_method.clone()),
                    })
                    .await?;
            }
        }
        Ok(())
    }

    /// Get a melt quote for a human-readable address
    ///
    /// This method accepts a human-readable address that could be either a BIP353 address
    /// or a Lightning address. It intelligently determines which to try based on mint support:
    ///
    /// 1. If the mint supports Bolt12, it tries BIP353 first
    /// 2. Falls back to Lightning address only if BIP353 DNS resolution fails
    /// 3. If BIP353 resolves but fails at the mint, it does NOT fall back to Lightning address
    /// 4. If the mint doesn't support Bolt12, it tries Lightning address directly
    #[cfg(all(feature = "bip353", feature = "wallet", not(target_arch = "wasm32")))]
    pub async fn melt_human_readable_quote(
        &self,
        address: &str,
        amount_msat: impl Into<crate::Amount>,
    ) -> Result<MeltQuote, Error> {
        use cdk_common::nuts::PaymentMethod;

        let amount = amount_msat.into();

        // Get mint info from cache to check bolt12 support (no network call)
        let mint_info = &self
            .metadata_cache
            .load(&self.localstore, &self.client, {
                let ttl = self.metadata_cache_ttl.read();
                *ttl
            })
            .await?
            .mint_info;

        // Check if mint supports bolt12 by looking at nut05 methods
        let supports_bolt12 = mint_info
            .nuts
            .nut05
            .methods
            .iter()
            .any(|m| m.method == PaymentMethod::Bolt12);

        if supports_bolt12 {
            // Mint supports bolt12, try BIP353 first
            match self.melt_bip353_quote(address, amount).await {
                Ok(quote) => Ok(quote),
                Err(Error::Bip353Resolve(_)) => {
                    // DNS resolution failed, fall back to Lightning address
                    tracing::debug!(
                        "BIP353 DNS resolution failed for {}, trying Lightning address",
                        address
                    );
                    return self.melt_lightning_address_quote(address, amount).await;
                }
                Err(e) => {
                    // BIP353 resolved but failed for another reason (e.g., mint error)
                    // Don't fall back to Lightning address
                    Err(e)
                }
            }
        } else {
            // Mint doesn't support bolt12, use Lightning address directly
            self.melt_lightning_address_quote(address, amount).await
        }
    }
}
