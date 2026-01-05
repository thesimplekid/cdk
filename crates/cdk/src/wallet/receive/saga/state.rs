//! State types for the Receive saga.
//!
//! Each state is a distinct type that holds the data relevant to that stage
//! of the receive operation. The type state pattern ensures that only valid
//! operations are available at each stage.

use uuid::Uuid;

use crate::nuts::{Id, Proofs};
use crate::wallet::receive::ReceiveOptions;
use crate::Amount;

/// Initial state - operation ID assigned but no work done yet.
///
/// The receive saga starts in this state. Only `validate()` is available.
pub struct Initial {
    /// Unique operation identifier for tracking and crash recovery
    pub operation_id: Uuid,
}

/// Validated state - token has been parsed and proofs extracted.
///
/// After successful validation, the saga transitions to this state.
/// Methods available: `execute()`
pub struct Validated {
    /// Unique operation identifier
    pub operation_id: Uuid,
    /// Options for the receive operation
    pub options: ReceiveOptions,
    /// Memo from the token (if any)
    pub memo: Option<String>,
    /// Proofs extracted from the token (potentially signed for P2PK/HTLC)
    pub proofs: Proofs,
    /// Total amount of the incoming proofs
    pub proofs_amount: Amount,
    /// Active keyset ID for the swap
    pub active_keyset_id: Id,
}

/// Finalized state - receive operation completed successfully.
///
/// After successful execution, the saga transitions to this state.
/// The received amount can be retrieved and the saga is complete.
pub struct Finalized {
    /// Total amount received (after fees)
    pub amount: Amount,
}
