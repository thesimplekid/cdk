//! State types for the Send saga.
//!
//! Each state is a distinct type that holds the data relevant to that stage
//! of the send operation. The type state pattern ensures that only valid
//! operations are available at each stage.

use uuid::Uuid;

use crate::nuts::Proofs;
use crate::wallet::send::SendOptions;
use crate::Amount;

/// Initial state - operation ID assigned but no work done yet.
///
/// The send saga starts in this state. Only `prepare()` is available.
pub struct Initial {
    /// Unique operation identifier for tracking and crash recovery
    pub operation_id: Uuid,
}

/// Prepared state - proofs have been selected and reserved.
///
/// After successful preparation, the saga transitions to this state.
/// Methods available: `confirm()`, `cancel()`
pub struct Prepared {
    /// Unique operation identifier
    pub operation_id: Uuid,
    /// Amount to send
    pub amount: Amount,
    /// Send options
    pub options: SendOptions,
    /// Proofs that need to be swapped before sending
    pub proofs_to_swap: Proofs,
    /// Fee for the swap operation
    pub swap_fee: Amount,
    /// Proofs that will be included in the token directly
    pub proofs_to_send: Proofs,
    /// Fee the recipient will pay to redeem the token
    pub send_fee: Amount,
}

impl Prepared {
    /// Get all proofs (both to swap and to send)
    pub fn all_proofs(&self) -> Proofs {
        let mut proofs = self.proofs_to_swap.clone();
        proofs.extend(self.proofs_to_send.clone());
        proofs
    }

    /// Total fee (swap + send)
    pub fn total_fee(&self) -> Amount {
        self.swap_fee + self.send_fee
    }
}

