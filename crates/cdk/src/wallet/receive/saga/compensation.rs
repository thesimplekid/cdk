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

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use cdk_common::database::WalletDatabase;
    use cdk_common::nuts::{CurrencyUnit, Id, Proof, State};
    use cdk_common::secret::Secret;
    use cdk_common::wallet::{
        OperationData, SwapOperationData, SwapSagaState, WalletSaga, WalletSagaState,
    };
    use cdk_common::{Amount, SecretKey};

    use super::*;

    /// Create test database
    async fn create_test_db() -> Arc<dyn WalletDatabase<cdk_common::database::Error> + Send + Sync>
    {
        Arc::new(cdk_sqlite::wallet::memory::empty().await.unwrap())
    }

    /// Create a test keyset ID
    fn test_keyset_id() -> Id {
        Id::from_str("00916bbf7ef91a36").unwrap()
    }

    /// Create a test proof
    fn test_proof(keyset_id: Id, amount: u64) -> Proof {
        Proof {
            amount: Amount::from(amount),
            keyset_id,
            secret: Secret::generate(),
            c: SecretKey::generate().public_key(),
            witness: None,
            dleq: None,
        }
    }

    /// Create a test proof info
    fn test_proof_info(
        keyset_id: Id,
        amount: u64,
        mint_url: cdk_common::mint_url::MintUrl,
    ) -> crate::types::ProofInfo {
        let proof = test_proof(keyset_id, amount);
        crate::types::ProofInfo::new(proof, mint_url, State::Pending, CurrencyUnit::Sat).unwrap()
    }

    /// Create a test mint URL
    fn test_mint_url() -> cdk_common::mint_url::MintUrl {
        cdk_common::mint_url::MintUrl::from_str("https://test-mint.example.com").unwrap()
    }

    /// Create a test wallet saga
    fn test_wallet_saga(mint_url: cdk_common::mint_url::MintUrl) -> WalletSaga {
        WalletSaga::new(
            uuid::Uuid::new_v4(),
            WalletSagaState::Swap(SwapSagaState::ProofsReserved),
            Amount::from(1000),
            mint_url,
            CurrencyUnit::Sat,
            OperationData::Swap(SwapOperationData {
                input_amount: Amount::from(1000),
                output_amount: Amount::from(990),
                counter_start: Some(0),
                counter_end: Some(10),
                blinded_messages: None,
            }),
        )
    }

    #[tokio::test]
    async fn test_remove_pending_proofs_is_idempotent() {
        let db = create_test_db().await;
        let mint_url = test_mint_url();
        let keyset_id = test_keyset_id();

        let proof_info = test_proof_info(keyset_id, 100, mint_url.clone());
        let proof_y = proof_info.y;
        db.update_proofs(vec![proof_info], vec![]).await.unwrap();

        let saga = test_wallet_saga(mint_url);
        let saga_id = saga.id;
        db.add_saga(saga).await.unwrap();

        let compensation = RemovePendingProofs {
            localstore: db.clone(),
            proof_ys: vec![proof_y],
            saga_id,
        };

        // Execute twice - should succeed both times
        compensation.execute().await.unwrap();
        compensation.execute().await.unwrap();

        // Proofs should still be gone
        let all_proofs = db.get_proofs(None, None, None, None).await.unwrap();
        assert!(all_proofs.is_empty());
    }

    #[tokio::test]
    async fn test_remove_pending_proofs_handles_missing_saga() {
        let db = create_test_db().await;
        let mint_url = test_mint_url();
        let keyset_id = test_keyset_id();

        let proof_info = test_proof_info(keyset_id, 100, mint_url.clone());
        let proof_y = proof_info.y;
        db.update_proofs(vec![proof_info], vec![]).await.unwrap();

        // Use a saga_id that doesn't exist
        let saga_id = uuid::Uuid::new_v4();

        let compensation = RemovePendingProofs {
            localstore: db.clone(),
            proof_ys: vec![proof_y],
            saga_id,
        };

        // Should succeed even without saga
        compensation.execute().await.unwrap();

        // Proofs should be removed
        let all_proofs = db.get_proofs(None, None, None, None).await.unwrap();
        assert!(all_proofs.is_empty());
    }

    #[tokio::test]
    async fn test_remove_pending_proofs_only_affects_specified_proofs() {
        let db = create_test_db().await;
        let mint_url = test_mint_url();
        let keyset_id = test_keyset_id();

        // Create two pending proofs
        let proof_info_1 = test_proof_info(keyset_id, 100, mint_url.clone());
        let proof_info_2 = test_proof_info(keyset_id, 200, mint_url.clone());
        let proof_y_1 = proof_info_1.y;
        let proof_y_2 = proof_info_2.y;
        db.update_proofs(vec![proof_info_1, proof_info_2], vec![])
            .await
            .unwrap();

        let saga = test_wallet_saga(mint_url);
        let saga_id = saga.id;
        db.add_saga(saga).await.unwrap();

        // Only remove the first proof
        let compensation = RemovePendingProofs {
            localstore: db.clone(),
            proof_ys: vec![proof_y_1],
            saga_id,
        };
        compensation.execute().await.unwrap();

        // Second proof should still exist
        let remaining_proofs = db.get_proofs(None, None, None, None).await.unwrap();
        assert_eq!(remaining_proofs.len(), 1);
        assert_eq!(remaining_proofs[0].y, proof_y_2);
    }
}
