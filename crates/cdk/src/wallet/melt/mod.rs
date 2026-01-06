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

use cdk_common::util::unix_time;
use cdk_common::wallet::{MeltQuote, Transaction, TransactionDirection};
use cdk_common::{Error, MeltQuoteBolt11Response, MeltQuoteState, ProofsMethods, State};
use tracing::instrument;
use uuid::Uuid;

use crate::nuts::Proofs;
use crate::types::FinalizedMelt;
use crate::{Amount, Wallet};

#[cfg(all(feature = "bip353", not(target_arch = "wasm32")))]
mod melt_bip353;
mod melt_bolt11;
mod melt_bolt12;
#[cfg(feature = "wallet")]
mod melt_lightning_address;
pub(crate) mod saga;

use saga::{state::Prepared, MeltSaga};

/// A prepared melt operation that can be confirmed or cancelled.
///
/// This is the result of calling [`Wallet::prepare_melt`]. The proofs are reserved
/// but the payment has not yet been executed.
///
/// Call [`confirm`](Self::confirm) to execute the melt, or [`cancel`](Self::cancel)
/// to release the reserved proofs.
pub struct PreparedMelt<'a> {
    /// The saga in the Prepared state
    saga: MeltSaga<'a, Prepared>,
    /// Metadata for the transaction
    metadata: HashMap<String, String>,
}

impl<'a> PreparedMelt<'a> {
    /// Get the operation ID
    pub fn operation_id(&self) -> Uuid {
        self.saga.operation_id()
    }

    /// Get the quote
    pub fn quote(&self) -> &MeltQuote {
        self.saga.quote()
    }

    /// Get the amount to be melted
    pub fn amount(&self) -> Amount {
        self.saga.quote().amount
    }

    /// Get the proofs that will be used
    pub fn proofs(&self) -> &Proofs {
        self.saga.proofs()
    }

    /// Get the proofs that need to be swapped
    pub fn proofs_to_swap(&self) -> &Proofs {
        self.saga.proofs_to_swap()
    }

    /// Get the swap fee
    pub fn swap_fee(&self) -> Amount {
        self.saga.swap_fee()
    }

    /// Get the input fee
    pub fn input_fee(&self) -> Amount {
        self.saga.input_fee()
    }

    /// Get the total fee
    pub fn total_fee(&self) -> Amount {
        self.saga.swap_fee() + self.saga.input_fee()
    }

    /// Confirm the prepared melt and execute the payment.
    ///
    /// This transitions the saga through: Prepared -> MeltRequested -> Finalized
    pub async fn confirm(self) -> Result<ConfirmedMelt, Error> {
        // Transition to MeltRequested state (handles swap if needed)
        let melt_requested = self.saga.request_melt().await?;

        // Execute the melt request and get Finalized saga
        let finalized = melt_requested.execute(self.metadata).await?;

        Ok(ConfirmedMelt {
            state: finalized.state(),
            amount: finalized.amount(),
            fee: finalized.fee(),
            payment_preimage: finalized.payment_preimage().map(|s| s.to_string()),
            change: finalized.into_change(),
        })
    }

    /// Cancel the prepared melt and release reserved proofs
    pub async fn cancel(self) -> Result<(), Error> {
        self.saga.cancel().await
    }
}

impl Debug for PreparedMelt<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedMelt")
            .field("operation_id", &self.saga.operation_id())
            .field("quote_id", &self.saga.quote().id)
            .field("amount", &self.saga.quote().amount)
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

        Ok(PreparedMelt {
            saga: prepared_saga,
            metadata,
        })
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
    pub async fn melt(&self, quote_id: &str) -> Result<FinalizedMelt, Error> {
        self.melt_with_metadata(quote_id, HashMap::new()).await
    }

    /// Melt tokens with additional metadata saved with the transaction.
    ///
    /// Like `melt()`, but allows attaching custom metadata to the transaction record.
    ///
    /// Note: The returned `FinalizedMelt` struct contains the actual state from the mint.
    /// If `state` is `Pending`, the payment is in-flight and will complete later.
    /// Call `finalize_pending_melts()` to finalize pending melts.
    #[instrument(skip(self, metadata))]
    pub async fn melt_with_metadata(
        &self,
        quote_id: &str,
        metadata: HashMap<String, String>,
    ) -> Result<FinalizedMelt, Error> {
        let prepared = self.prepare_melt(quote_id, metadata).await?;
        let confirmed = prepared.confirm().await?;

        Ok(FinalizedMelt {
            quote_id: quote_id.to_string(),
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

        Ok(PreparedMelt {
            saga: prepared_saga,
            metadata,
        })
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
    ) -> Result<FinalizedMelt, Error> {
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
    ) -> Result<FinalizedMelt, Error> {
        let prepared = self.prepare_melt_proofs(quote_id, proofs, metadata).await?;
        let confirmed = prepared.confirm().await?;

        Ok(FinalizedMelt {
            quote_id: quote_id.to_string(),
            state: confirmed.state(),
            preimage: confirmed.payment_preimage().cloned(),
            amount: confirmed.amount(),
            fee_paid: confirmed.fee(),
            change: confirmed.change().cloned(),
        })
    }

    /// Finalize pending melt operations.
    ///
    /// This checks all incomplete melt sagas where payment may be pending,
    /// queries the mint for the current quote status, and:
    /// - **Paid**: Marks proofs as spent, recovers change, returns `FinalizedMelt` with state `Paid`
    /// - **Failed/Unpaid**: Compensates by releasing reserved proofs, returns `FinalizedMelt` with state `Unpaid`/`Failed`
    /// - **Pending/Unknown**: Skips (payment still in flight), not included in result
    ///
    /// Call this periodically or after receiving a notification that a
    /// pending payment may have settled.
    ///
    /// # Returns
    ///
    /// A vector of finalized melt results. Check the `state` field to determine
    /// if each melt succeeded (`Paid`) or failed (`Unpaid`/`Failed`).
    /// Melts that are still pending are not included in the result.
    #[instrument(skip_all)]
    pub async fn finalize_pending_melts(&self) -> Result<Vec<FinalizedMelt>, Error> {
        use cdk_common::wallet::{MeltSagaState, WalletSagaState};

        let sagas = self.localstore.get_incomplete_sagas().await?;

        // Filter to only melt sagas in states that need checking
        let melt_sagas: Vec<_> = sagas
            .into_iter()
            .filter(|s| {
                matches!(
                    &s.state,
                    WalletSagaState::Melt(MeltSagaState::MeltRequested | MeltSagaState::PaymentPending)
                )
            })
            .collect();

        if melt_sagas.is_empty() {
            return Ok(Vec::new());
        }

        tracing::info!("Found {} pending melt(s) to check", melt_sagas.len());

        let mut results = Vec::new();

        for saga in melt_sagas {
            match self.resume_melt_saga(&saga).await {
                Ok(Some(melted)) => {
                    tracing::info!(
                        "Melt {} finalized with state {:?}",
                        saga.id,
                        melted.state
                    );
                    results.push(melted);
                }
                Ok(None) => {
                    tracing::debug!("Melt {} still pending or compensated early", saga.id);
                }
                Err(e) => {
                    tracing::error!("Failed to finalize melt {}: {}", saga.id, e);
                    // Continue with other sagas instead of failing entirely
                }
            }
        }

        Ok(results)
    }

    /// Confirm a prepared melt with already-reserved proofs (internal helper for Arc<Wallet>).
    ///
    /// This is used by `MultiMintPreparedMelt::confirm` which holds an `Arc<Wallet>`
    /// and has already prepared/reserved proofs. For the normal API path, use
    /// `PreparedMelt::confirm()` which uses the typestate saga.
    ///
    /// The `operation_id` and `quote` must correspond to an existing prepared saga.
    #[instrument(skip(self, proofs, proofs_to_swap, metadata))]
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn confirm_prepared_melt(
        &self,
        operation_id: Uuid,
        quote: MeltQuote,
        proofs: Proofs,
        proofs_to_swap: Proofs,
        input_fee: Amount,
        metadata: HashMap<String, String>,
    ) -> Result<ConfirmedMelt, Error> {
        // Create a saga in Prepared state and continue from there
        // We reconstruct the Prepared state from the stored data
        let saga = MeltSaga::from_prepared(
            self,
            operation_id,
            quote,
            proofs,
            proofs_to_swap,
            input_fee,
        );

        let melt_requested = saga.request_melt().await?;
        let finalized = melt_requested.execute(metadata).await?;

        Ok(ConfirmedMelt {
            state: finalized.state(),
            amount: finalized.amount(),
            fee: finalized.fee(),
            payment_preimage: finalized.payment_preimage().map(|s| s.to_string()),
            change: finalized.into_change(),
        })
    }

    /// Cancel a prepared melt and release reserved proofs (internal helper).
    ///
    /// This is used by `MultiMintPreparedMelt::cancel` which holds an `Arc<Wallet>`.
    #[instrument(skip(self, proofs, proofs_to_swap))]
    pub(crate) async fn cancel_prepared_melt(
        &self,
        operation_id: Uuid,
        proofs: Proofs,
        proofs_to_swap: Proofs,
    ) -> Result<(), Error> {
        tracing::info!("Cancelling prepared melt for operation {}", operation_id);

        // Revert proof reservation
        let mut all_ys = proofs.ys()?;
        all_ys.extend(proofs_to_swap.ys()?);

        if !all_ys.is_empty() {
            self.localstore
                .update_proofs_state(all_ys, State::Unspent)
                .await?;
        }

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
