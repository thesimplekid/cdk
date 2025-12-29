//! Compensation actions for the mint (issue) saga.
//!
//! When a saga step fails, compensating actions are executed in reverse order (LIFO)
//! to undo all completed steps and restore the database to its pre-saga state.
//!
//! Note: For mint operations, the primary side effect before the API call is
//! incrementing the keyset counter. Counter increments are not reversed because:
//! 1. They don't cause data loss (just potentially unused counter values)
//! 2. The secrets can be recovered via the restore process
//! 3. Reversing could cause issues if concurrent operations used adjacent counters

use std::sync::Arc;

use async_trait::async_trait;
use cdk_common::database::{self, WalletDatabase};
use tracing::instrument;

use crate::wallet::saga::CompensatingAction;
use crate::Error;

/// Placeholder compensation action for mint operations.
///
/// Currently, mint operations don't require compensation because:
/// - Counter increments are intentionally not reversed
/// - No proofs are stored until after successful mint
/// - Quote state is not modified until after successful mint
///
/// This struct exists for consistency with other sagas and for
/// potential future use if mint recovery logic changes.
pub struct MintCompensation {
    /// Database reference
    pub localstore: Arc<dyn WalletDatabase<database::Error> + Send + Sync>,
    /// Quote ID (for logging)
    pub quote_id: String,
    /// Saga ID for cleanup
    pub saga_id: uuid::Uuid,
}

#[async_trait]
impl CompensatingAction for MintCompensation {
    #[instrument(skip_all)]
    async fn execute(&self) -> Result<(), Error> {
        tracing::info!(
            "Compensation: Mint operation for quote {} failed, no rollback needed",
            self.quote_id
        );

        // No actual compensation needed for mint operations
        // Counter increments are not reversed (see module docs)
        // Quote and proofs are not modified until after successful mint

        // Delete saga record (best-effort)
        if let Err(e) = self.localstore.delete_saga(&self.saga_id).await {
            tracing::warn!(
                "Compensation: Failed to delete saga {}: {}. Will be cleaned up on recovery.",
                self.saga_id,
                e
            );
            // Don't fail compensation if saga deletion fails - orphaned saga is harmless
        }

        Ok(())
    }

    fn name(&self) -> &'static str {
        "MintCompensation"
    }
}
