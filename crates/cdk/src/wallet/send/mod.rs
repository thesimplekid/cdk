//! Send Module
//!
//! This module provides the send functionality for the wallet.
//!
//! Use [`Wallet::prepare_send`] to create a [`PreparedSend`], then call
//! [`confirm`](PreparedSend::confirm) to complete the send or
//! [`cancel`](PreparedSend::cancel) to release reserved proofs.

use std::collections::HashMap;
use std::fmt::Debug;

use cdk_common::Id;
use tracing::instrument;

use super::SendKind;
use crate::amount::SplitTarget;
use crate::fees::calculate_fee;
use crate::nuts::nut00::ProofsMethods;
use crate::nuts::{Proofs, SpendingConditions, Token};
use crate::{Amount, Error, Wallet};

pub(crate) mod saga;

use saga::SendSaga;

/// Prepared send transaction
///
/// Created by [`Wallet::prepare_send`]. Call [`confirm`](Self::confirm) to complete the send
/// and create a token, or [`cancel`](Self::cancel) to release reserved proofs.
pub struct PreparedSend {
    inner: SendSaga<saga::state::Prepared>,
}

impl PreparedSend {
    /// Amount to send
    pub fn amount(&self) -> Amount {
        self.inner.amount()
    }

    /// Send options
    pub fn options(&self) -> &SendOptions {
        self.inner.options()
    }

    /// Proofs that need to be swapped before sending
    pub fn proofs_to_swap(&self) -> &Proofs {
        self.inner.proofs_to_swap()
    }

    /// Fee for the swap operation
    pub fn swap_fee(&self) -> Amount {
        self.inner.swap_fee()
    }

    /// Proofs that will be sent directly
    pub fn proofs_to_send(&self) -> &Proofs {
        self.inner.proofs_to_send()
    }

    /// Fee the recipient will pay to redeem the token
    pub fn send_fee(&self) -> Amount {
        self.inner.send_fee()
    }

    /// All proofs (both to swap and to send)
    pub fn proofs(&self) -> Proofs {
        self.inner.proofs()
    }

    /// Total fee (swap + send)
    pub fn fee(&self) -> Amount {
        self.inner.fee()
    }

    /// Confirm the prepared send and create a token
    pub async fn confirm(self, memo: Option<SendMemo>) -> Result<Token, Error> {
        self.inner.confirm(memo).await
    }

    /// Cancel the prepared send and release reserved proofs
    pub async fn cancel(self) -> Result<(), Error> {
        self.inner.cancel().await
    }
}

impl Debug for PreparedSend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedSend")
            .field("amount", &self.inner.amount())
            .field("options", self.inner.options())
            .field(
                "proofs_to_swap",
                &self
                    .inner
                    .proofs_to_swap()
                    .iter()
                    .map(|p| p.amount)
                    .collect::<Vec<_>>(),
            )
            .field("swap_fee", &self.inner.swap_fee())
            .field(
                "proofs_to_send",
                &self
                    .inner
                    .proofs_to_send()
                    .iter()
                    .map(|p| p.amount)
                    .collect::<Vec<_>>(),
            )
            .field("send_fee", &self.inner.send_fee())
            .finish()
    }
}

impl Wallet {
    /// Prepare a send transaction
    ///
    /// This function prepares a send transaction by selecting proofs to send and proofs to swap.
    /// By doing so, it ensures that the wallet user is able to view the fees associated with the
    /// send transaction before confirming.
    ///
    /// # Example
    /// ```no_run
    /// # use cdk::wallet::{Wallet, SendOptions};
    /// # use cdk::Amount;
    /// # async fn example(wallet: &Wallet) -> Result<(), Box<dyn std::error::Error>> {
    /// let prepared = wallet.prepare_send(Amount::from(10), SendOptions::default()).await?;
    /// println!("Fee: {}", prepared.fee());
    /// let token = prepared.confirm(None).await?;
    /// # Ok(())
    /// # }
    /// ```
    #[instrument(skip(self), err)]
    pub async fn prepare_send(
        &self,
        amount: Amount,
        opts: SendOptions,
    ) -> Result<PreparedSend, Error> {
        let saga = SendSaga::new(self.clone());
        let inner = saga.prepare(amount, opts).await?;
        Ok(PreparedSend { inner })
    }
}

/// Send options
#[derive(Debug, Clone, Default)]
pub struct SendOptions {
    /// Memo
    pub memo: Option<SendMemo>,
    /// Spending conditions
    pub conditions: Option<SpendingConditions>,
    /// Amount split target
    pub amount_split_target: SplitTarget,
    /// Send kind
    pub send_kind: SendKind,
    /// Include fee
    ///
    /// When this is true the token created will include the amount of fees needed to redeem the token (amount + fee_to_redeem)
    pub include_fee: bool,
    /// Maximum number of proofs to include in the token
    /// Default is `None`, which means all selected proofs will be included.
    pub max_proofs: Option<usize>,
    /// Metadata
    pub metadata: HashMap<String, String>,
}

/// Send memo
#[derive(Debug, Clone)]
pub struct SendMemo {
    /// Memo
    pub memo: String,
    /// Include memo in token
    pub include_memo: bool,
}

impl SendMemo {
    /// Create a new send memo
    pub fn for_token(memo: &str) -> Self {
        Self {
            memo: memo.to_string(),
            include_memo: true,
        }
    }
}

/// Result of splitting proofs for a send operation
#[derive(Debug, Clone)]
pub struct ProofSplitResult {
    /// Proofs that can be sent directly (matching desired denominations)
    pub proofs_to_send: Proofs,
    /// Proofs that need to be swapped first
    pub proofs_to_swap: Proofs,
    /// Fee required for the swap operation
    pub swap_fee: Amount,
}

/// Split proofs between those to send directly and those requiring swap.
///
/// This is a pure function that implements the core logic of `internal_prepare_send`:
/// 1. Match proofs to desired send amounts
/// 2. Ensure proofs_to_swap can cover swap fees plus needed output
/// 3. Move proofs from send to swap if needed to cover fees
///
/// # Arguments
/// * `proofs` - All selected proofs to split
/// * `send_amounts` - Desired output denominations
/// * `amount` - Amount to send
/// * `send_fee` - Fee the recipient will pay to redeem
/// * `keyset_fees` - Map of keyset ID to fee_ppk
/// * `force_swap` - If true, all proofs go to swap
/// * `is_exact_or_offline` - If true (exact match or offline mode), all proofs go to send
// TODO: Consider making this pub(crate) - this function is also used by melt operations
pub fn split_proofs_for_send(
    proofs: Proofs,
    send_amounts: &[Amount],
    amount: Amount,
    send_fee: Amount,
    keyset_fees: &HashMap<Id, u64>,
    force_swap: bool,
    is_exact_or_offline: bool,
) -> Result<ProofSplitResult, Error> {
    let mut proofs_to_swap = Proofs::new();
    let mut proofs_to_send = Proofs::new();

    if force_swap {
        proofs_to_swap = proofs;
    } else if is_exact_or_offline {
        proofs_to_send = proofs;
    } else {
        let mut remaining_send_amounts: Vec<Amount> = send_amounts.to_vec();
        for proof in proofs {
            if let Some(idx) = remaining_send_amounts
                .iter()
                .position(|a| a == &proof.amount)
            {
                proofs_to_send.push(proof);
                remaining_send_amounts.remove(idx);
            } else {
                proofs_to_swap.push(proof);
            }
        }

        // Check if swap is actually needed
        if !proofs_to_swap.is_empty() {
            let swap_output_needed = (amount + send_fee)
                .checked_sub(proofs_to_send.total_amount()?)
                .unwrap_or(Amount::ZERO);

            if swap_output_needed == Amount::ZERO {
                // proofs_to_send already covers the full amount, no swap needed
                // Clear proofs_to_swap - these are just leftover proofs that don't match
                // any send denomination but aren't needed for the send
                proofs_to_swap.clear();
            } else {
                // Ensure proofs_to_swap can cover the swap's input fee plus the needed output
                loop {
                    let swap_input_fee =
                        calculate_fee(&proofs_to_swap.count_by_keyset(), keyset_fees)?.total;
                    let swap_total = proofs_to_swap.total_amount()?;

                    let swap_can_produce = swap_total.checked_sub(swap_input_fee);

                    match swap_can_produce {
                        Some(can_produce) if can_produce >= swap_output_needed => {
                            break;
                        }
                        _ => {
                            if proofs_to_send.is_empty() {
                                return Err(Error::InsufficientFunds);
                            }

                            // Move the smallest proof from send to swap
                            proofs_to_send.sort_by(|a, b| a.amount.cmp(&b.amount));
                            let proof_to_move = proofs_to_send.remove(0);
                            proofs_to_swap.push(proof_to_move);
                        }
                    }
                }
            }
        }
    }

    let swap_fee = calculate_fee(&proofs_to_swap.count_by_keyset(), keyset_fees)?.total;

    Ok(ProofSplitResult {
        proofs_to_send,
        proofs_to_swap,
        swap_fee,
    })
}

#[cfg(test)]
mod tests {
    use cdk_common::secret::Secret;
    use cdk_common::{Amount, Id, Proof, PublicKey};

    use super::*;

    fn id() -> Id {
        Id::from_bytes(&[0; 8]).unwrap()
    }

    fn proof(amount: u64) -> Proof {
        Proof::new(
            Amount::from(amount),
            id(),
            Secret::generate(),
            PublicKey::from_hex(
                "03deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
            )
            .unwrap(),
        )
    }

    fn proofs(amounts: &[u64]) -> Proofs {
        amounts.iter().map(|&a| proof(a)).collect()
    }

    fn keyset_fees_with_ppk(fee_ppk: u64) -> HashMap<Id, u64> {
        let mut fees = HashMap::new();
        fees.insert(id(), fee_ppk);
        fees
    }

    fn amounts(values: &[u64]) -> Vec<Amount> {
        values.iter().map(|&v| Amount::from(v)).collect()
    }

    #[test]
    fn test_split_exact_match_simple() {
        let input_proofs = proofs(&[8, 2]);
        let send_amounts = amounts(&[8, 2]);
        let keyset_fees = keyset_fees_with_ppk(200);

        let result = split_proofs_for_send(
            input_proofs,
            &send_amounts,
            Amount::from(10),
            Amount::from(1),
            &keyset_fees,
            false,
            true,
        )
        .unwrap();

        assert_eq!(result.proofs_to_send.len(), 2);
        assert!(result.proofs_to_swap.is_empty());
        assert_eq!(result.swap_fee, Amount::ZERO);
    }

    #[test]
    fn test_split_force_swap() {
        let input_proofs = proofs(&[2048, 1024, 512, 256, 128, 64, 32, 16]);
        let send_amounts = amounts(&[2048, 1024, 512, 256, 128, 32]);
        let keyset_fees = keyset_fees_with_ppk(200);

        let result = split_proofs_for_send(
            input_proofs,
            &send_amounts,
            Amount::from(3000),
            Amount::from(2),
            &keyset_fees,
            true,
            false,
        )
        .unwrap();

        assert!(result.proofs_to_send.is_empty());
        assert_eq!(result.proofs_to_swap.len(), 8);
    }
}
