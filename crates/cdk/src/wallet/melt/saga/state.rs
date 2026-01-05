//! State types for the Melt saga.
//!
//! Each state is a distinct type that holds the data relevant to that stage
//! of the melt operation. The type state pattern ensures that only valid
//! operations are available at each stage.

use std::collections::HashMap;

use cdk_common::MeltQuoteState;
use uuid::Uuid;

use crate::nuts::Proofs;
use crate::wallet::MeltQuote;
use crate::Amount;

/// Initial state - operation ID assigned but no work done yet.
///
/// The melt saga starts in this state. Only `prepare()` is available.
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
    /// The melt quote
    pub quote: MeltQuote,
    /// Proofs that will be used for the melt
    pub proofs: Proofs,
    /// Proofs that need to be swapped first (if any)
    pub proofs_to_swap: Proofs,
    /// Fee for the swap operation
    pub swap_fee: Amount,
    /// Input fee for the melt
    pub input_fee: Amount,
    /// Additional metadata for the transaction
    pub metadata: HashMap<String, String>,
}

impl Prepared {
    /// Get the total fee (swap + input)
    pub fn total_fee(&self) -> Amount {
        self.swap_fee + self.input_fee
    }
}

/// Confirmed state - melt has been completed (or is pending).
///
/// After confirmation attempt, the saga transitions to this state.
/// The `state` field indicates whether payment is Paid, Pending, etc.
/// The result can be retrieved and the saga is complete.
pub struct Confirmed {
    /// Unique operation identifier
    pub operation_id: Uuid,
    /// The actual state of the melt (Paid, Pending, etc.)
    pub state: MeltQuoteState,
    /// Amount melted
    pub amount: Amount,
    /// Total fee paid
    pub fee: Amount,
    /// Payment preimage (if available)
    pub payment_preimage: Option<String>,
    /// Change proofs returned from the melt (if any)
    pub change: Option<Proofs>,
}
