//! Compensation actions for the melt saga.
//!
//! When a saga step fails, compensating actions are executed in reverse order (LIFO)
//! to undo all completed steps and restore the database to its pre-saga state.

use std::sync::Arc;

use async_trait::async_trait;
use cdk_common::database::{self, WalletDatabase};
use tracing::instrument;
use uuid::Uuid;

use crate::nuts::{PublicKey, State};
use crate::wallet::saga::CompensatingAction;
use crate::Error;

/// Compensation action to release a melt quote reservation.
///
/// This compensation is used when melt fails after the quote has been reserved
/// but before it has been used. It clears the used_by_operation field on the quote.
pub struct ReleaseMeltQuote {
    /// Database reference
    pub localstore: Arc<dyn WalletDatabase<database::Error> + Send + Sync>,
    /// Operation ID that reserved the quote
    pub operation_id: Uuid,
}

#[async_trait]
impl CompensatingAction for ReleaseMeltQuote {
    #[instrument(skip_all)]
    async fn execute(&self) -> Result<(), Error> {
        tracing::info!(
            "Compensation: Releasing melt quote reserved by operation {}",
            self.operation_id
        );

        self.localstore
            .release_melt_quote(&self.operation_id)
            .await
            .map_err(Error::Database)?;

        Ok(())
    }

    fn name(&self) -> &'static str {
        "ReleaseMeltQuote"
    }
}

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

/// Compensation action to revert swapped proofs.
///
/// This compensation is used when melt fails after a nested swap has succeeded.
/// The swap created new proofs that need to be released back to Unspent state.
/// Unlike RevertProofReservation, this does NOT delete the saga (that's handled
/// by RevertProofReservation).
pub struct RevertSwappedProofs {
    /// Database reference
    pub localstore: Arc<dyn WalletDatabase<database::Error> + Send + Sync>,
    /// Y values (public keys) of the swapped proofs
    pub proof_ys: Vec<PublicKey>,
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

#[async_trait]
impl CompensatingAction for RevertSwappedProofs {
    #[instrument(skip_all)]
    async fn execute(&self) -> Result<(), Error> {
        tracing::info!(
            "Compensation: Reverting {} swapped proofs to Unspent",
            self.proof_ys.len()
        );

        self.localstore
            .update_proofs_state(self.proof_ys.clone(), State::Unspent)
            .await
            .map_err(Error::Database)?;

        Ok(())
    }

    fn name(&self) -> &'static str {
        "RevertSwappedProofs"
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use cdk_common::database::WalletDatabase;
    use cdk_common::nuts::{CurrencyUnit, Id, MeltQuoteState, Proof, State};
    use cdk_common::secret::Secret;
    use cdk_common::wallet::{
        MeltQuote, OperationData, SwapOperationData, SwapSagaState, WalletSaga, WalletSagaState,
    };
    use cdk_common::{Amount, PaymentMethod, SecretKey};

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
        crate::types::ProofInfo::new(proof, mint_url, State::Unspent, CurrencyUnit::Sat).unwrap()
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

    /// Create a test melt quote
    fn test_melt_quote() -> MeltQuote {
        MeltQuote {
            id: format!("test_melt_quote_{}", uuid::Uuid::new_v4()),
            unit: CurrencyUnit::Sat,
            amount: Amount::from(1000),
            request: "lnbc1000...".to_string(),
            fee_reserve: Amount::from(10),
            state: MeltQuoteState::Unpaid,
            expiry: 9999999999,
            payment_preimage: None,
            payment_method: PaymentMethod::Bolt11,
            used_by_operation: None,
        }
    }

    // =========================================================================
    // RevertProofReservation Tests
    // =========================================================================

    #[tokio::test]
    async fn test_revert_proof_reservation_is_idempotent() {
        let db = create_test_db().await;
        let mint_url = test_mint_url();
        let keyset_id = test_keyset_id();

        // Create and store proof in Reserved state
        let mut proof_info = test_proof_info(keyset_id, 100, mint_url.clone());
        proof_info.state = State::Reserved;
        let proof_y = proof_info.y;
        db.update_proofs(vec![proof_info], vec![]).await.unwrap();

        let saga = test_wallet_saga(mint_url);
        let saga_id = saga.id;
        db.add_saga(saga).await.unwrap();

        let compensation = RevertProofReservation {
            localstore: db.clone(),
            proof_ys: vec![proof_y],
            saga_id,
        };

        // Execute twice - should succeed both times
        compensation.execute().await.unwrap();
        compensation.execute().await.unwrap();

        // Proof should still be Unspent
        let proofs = db
            .get_proofs(None, None, Some(vec![State::Unspent]), None)
            .await
            .unwrap();
        assert_eq!(proofs.len(), 1);
    }

    #[tokio::test]
    async fn test_revert_proof_reservation_handles_missing_saga() {
        let db = create_test_db().await;
        let mint_url = test_mint_url();
        let keyset_id = test_keyset_id();

        // Create and store proof
        let mut proof_info = test_proof_info(keyset_id, 100, mint_url.clone());
        proof_info.state = State::Reserved;
        let proof_y = proof_info.y;
        db.update_proofs(vec![proof_info], vec![]).await.unwrap();

        // Use a saga_id that doesn't exist
        let saga_id = uuid::Uuid::new_v4();

        let compensation = RevertProofReservation {
            localstore: db.clone(),
            proof_ys: vec![proof_y],
            saga_id,
        };

        // Should succeed even though saga doesn't exist
        compensation.execute().await.unwrap();

        // Proof should be Unspent
        let proofs = db
            .get_proofs(None, None, Some(vec![State::Unspent]), None)
            .await
            .unwrap();
        assert_eq!(proofs.len(), 1);
    }

    // =========================================================================
    // RevertSwappedProofs Tests
    // =========================================================================

    #[tokio::test]
    async fn test_revert_swapped_proofs_is_idempotent() {
        let db = create_test_db().await;
        let mint_url = test_mint_url();
        let keyset_id = test_keyset_id();

        let mut proof_info = test_proof_info(keyset_id, 100, mint_url.clone());
        proof_info.state = State::Reserved;
        let proof_y = proof_info.y;
        db.update_proofs(vec![proof_info], vec![]).await.unwrap();

        let compensation = RevertSwappedProofs {
            localstore: db.clone(),
            proof_ys: vec![proof_y],
        };

        // Execute twice
        compensation.execute().await.unwrap();
        compensation.execute().await.unwrap();

        let proofs = db
            .get_proofs(None, None, Some(vec![State::Unspent]), None)
            .await
            .unwrap();
        assert_eq!(proofs.len(), 1);
    }

    // =========================================================================
    // ReleaseMeltQuote Tests
    // =========================================================================

    #[tokio::test]
    async fn test_release_melt_quote_is_idempotent() {
        let db = create_test_db().await;
        let operation_id = uuid::Uuid::new_v4();

        let mut quote = test_melt_quote();
        quote.used_by_operation = Some(operation_id.to_string());
        db.add_melt_quote(quote.clone()).await.unwrap();

        let compensation = ReleaseMeltQuote {
            localstore: db.clone(),
            operation_id,
        };

        // Execute twice
        compensation.execute().await.unwrap();
        compensation.execute().await.unwrap();

        let retrieved_quote = db.get_melt_quote(&quote.id).await.unwrap().unwrap();
        assert!(retrieved_quote.used_by_operation.is_none());
    }

    #[tokio::test]
    async fn test_release_melt_quote_handles_no_matching_quote() {
        let db = create_test_db().await;
        let operation_id = uuid::Uuid::new_v4();

        // Don't add any quote - compensation should still succeed
        let compensation = ReleaseMeltQuote {
            localstore: db.clone(),
            operation_id,
        };

        // Should not error even with no matching quote
        let result = compensation.execute().await;
        assert!(result.is_ok());
    }

    // =========================================================================
    // Isolation Tests
    // =========================================================================

    #[tokio::test]
    async fn test_compensation_only_affects_specified_proofs() {
        let db = create_test_db().await;
        let mint_url = test_mint_url();
        let keyset_id = test_keyset_id();

        // Create two proofs, both Reserved
        let mut proof_info_1 = test_proof_info(keyset_id, 100, mint_url.clone());
        let mut proof_info_2 = test_proof_info(keyset_id, 200, mint_url.clone());
        proof_info_1.state = State::Reserved;
        proof_info_2.state = State::Reserved;

        let proof_y_1 = proof_info_1.y;
        let proof_y_2 = proof_info_2.y;

        db.update_proofs(vec![proof_info_1, proof_info_2], vec![])
            .await
            .unwrap();

        let saga = test_wallet_saga(mint_url);
        let saga_id = saga.id;
        db.add_saga(saga).await.unwrap();

        // Only revert the first proof
        let compensation = RevertProofReservation {
            localstore: db.clone(),
            proof_ys: vec![proof_y_1],
            saga_id,
        };
        compensation.execute().await.unwrap();

        // First proof should be Unspent
        let unspent = db
            .get_proofs(None, None, Some(vec![State::Unspent]), None)
            .await
            .unwrap();
        assert_eq!(unspent.len(), 1);
        assert_eq!(unspent[0].y, proof_y_1);

        // Second proof should still be Reserved
        let reserved = db
            .get_proofs(None, None, Some(vec![State::Reserved]), None)
            .await
            .unwrap();
        assert_eq!(reserved.len(), 1);
        assert_eq!(reserved[0].y, proof_y_2);
    }
}
