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

use crate::nuts::Proofs;
use crate::types::Melted;
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
pub struct PreparedMelt {
    inner: MeltSaga<saga::state::Prepared>,
}

impl PreparedMelt {
    /// Get the quote
    pub fn quote(&self) -> &MeltQuote {
        self.inner.quote()
    }

    /// Get the amount to be melted
    pub fn amount(&self) -> Amount {
        self.inner.amount()
    }

    /// Get the proofs that will be used
    pub fn proofs(&self) -> &Proofs {
        self.inner.proofs()
    }

    /// Get the proofs that need to be swapped
    pub fn proofs_to_swap(&self) -> &Proofs {
        self.inner.proofs_to_swap()
    }

    /// Get the swap fee
    pub fn swap_fee(&self) -> Amount {
        self.inner.swap_fee()
    }

    /// Get the input fee
    pub fn input_fee(&self) -> Amount {
        self.inner.input_fee()
    }

    /// Get the total fee
    pub fn total_fee(&self) -> Amount {
        self.inner.total_fee()
    }

    /// Confirm the prepared melt and execute the payment
    pub async fn confirm(self) -> Result<ConfirmedMelt, Error> {
        let inner = self.inner.confirm().await?;
        Ok(ConfirmedMelt { inner })
    }

    /// Cancel the prepared melt and release reserved proofs
    pub async fn cancel(self) -> Result<(), Error> {
        self.inner.cancel().await
    }
}

impl Debug for PreparedMelt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedMelt")
            .field("quote_id", &self.inner.quote().id)
            .field("amount", &self.inner.amount())
            .field("total_fee", &self.inner.total_fee())
            .finish()
    }
}

/// A confirmed melt operation.
///
/// This is the result of calling [`PreparedMelt::confirm`]. The payment has been
/// executed (or is pending).
pub struct ConfirmedMelt {
    inner: MeltSaga<saga::state::Confirmed>,
}

impl ConfirmedMelt {
    /// Get the state of the melt (Paid, Pending, etc.)
    pub fn state(&self) -> MeltQuoteState {
        self.inner.state()
    }

    /// Get the amount melted
    pub fn amount(&self) -> Amount {
        self.inner.amount()
    }

    /// Get the fee paid
    pub fn fee(&self) -> Amount {
        self.inner.fee()
    }

    /// Get the payment preimage
    pub fn payment_preimage(&self) -> Option<&String> {
        self.inner.payment_preimage()
    }

    /// Get the change proofs returned from the melt
    pub fn change(&self) -> Option<&Proofs> {
        self.inner.change()
    }
}

impl Debug for ConfirmedMelt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfirmedMelt")
            .field("state", &self.inner.state())
            .field("amount", &self.inner.amount())
            .field("fee", &self.inner.fee())
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
    ) -> Result<PreparedMelt, Error> {
        let inner = MeltSaga::new(self.clone())
            .prepare(quote_id, metadata)
            .await?;
        Ok(PreparedMelt { inner })
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
    ) -> Result<PreparedMelt, Error> {
        let inner = MeltSaga::new(self.clone())
            .prepare_with_proofs(quote_id, proofs, metadata)
            .await?;
        Ok(PreparedMelt { inner })
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
