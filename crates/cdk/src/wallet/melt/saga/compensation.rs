//! Compensation actions for the melt saga.
//!
//! When a saga step fails, compensating actions are executed in reverse order (LIFO)
//! to undo all completed steps and restore the database to its pre-saga state.

use std::sync::Arc;

use async_trait::async_trait;
use cdk_common::database::{self, WalletDatabase};
use tracing::instrument;

use crate::nuts::{PublicKey, State};
use crate::wallet::saga::CompensatingAction;
use crate::Error;

/// Compensation action to revert proof reservation.
///
/// This compensation is used when melt fails after proofs have been reserved.
/// It sets the proofs back to Unspent state and deletes the saga record.
pub struct RevertProofReservation {
    /// Database reference
    pub localstore: Arc<dyn WalletDatabase<database::Error> + Send + Sync>,
    /// Y values (public keys) of the reserved proofs
    pub proof_ys: Vec<PublicKey>,
    /// Saga ID for cleanup
    pub saga_id: uuid::Uuid,
}

#[async_trait]
impl CompensatingAction for RevertProofReservation {
    #[instrument(skip_all)]
    async fn execute(&self) -> Result<(), Error> {
        tracing::info!(
            "Compensation: Reverting {} proofs from Reserved to Unspent",
            self.proof_ys.len()
        );

        self.localstore
            .update_proofs_state(self.proof_ys.clone(), State::Unspent)
            .await
            .map_err(Error::Database)?;

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
        "RevertProofReservation"
    }
}
