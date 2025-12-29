//! Compensation actions for the receive saga.
//!
//! When a saga step fails, compensating actions are executed in reverse order (LIFO)
//! to undo all completed steps and restore the database to its pre-saga state.

use std::sync::Arc;

use async_trait::async_trait;
use cdk_common::database::{self, WalletDatabase};
use tracing::instrument;

use crate::nuts::PublicKey;
use crate::wallet::saga::CompensatingAction;
use crate::Error;

/// Compensation action to remove pending proofs that were stored during receive.
///
/// This compensation is used when receive fails after proofs have been stored
/// in Pending state (before the swap is executed). It removes those proofs
/// and deletes the saga record.
pub struct RemovePendingProofs {
    /// Database reference
    pub localstore: Arc<dyn WalletDatabase<database::Error> + Send + Sync>,
    /// Y values (public keys) of the pending proofs to remove
    pub proof_ys: Vec<PublicKey>,
    /// Saga ID for cleanup
    pub saga_id: uuid::Uuid,
}

#[async_trait]
impl CompensatingAction for RemovePendingProofs {
    #[instrument(skip_all)]
    async fn execute(&self) -> Result<(), Error> {
        tracing::info!(
            "Compensation: Removing {} pending proofs from receive",
            self.proof_ys.len()
        );

        self.localstore
            .update_proofs(vec![], self.proof_ys.clone())
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
        "RemovePendingProofs"
    }
}
