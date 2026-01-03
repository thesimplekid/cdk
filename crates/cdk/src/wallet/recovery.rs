//! Wallet saga recovery module.
//!
//! This module handles recovery from incomplete wallet sagas after a crash.
//! It follows the same pattern as the mint's `start_up_check.rs` module.
//!
//! # Usage
//!
//! Call `recover_incomplete_sagas()` after creating the wallet and before
//! performing normal operations:
//!
//! ```rust,ignore
//! let wallet = WalletBuilder::new()
//!     .mint_url(mint_url)
//!     .unit(CurrencyUnit::Sat)
//!     .localstore(localstore)
//!     .seed(&seed)
//!     .build()?;
//!
//! // Recover from any incomplete operations from a previous crash
//! let report = wallet.recover_incomplete_sagas().await?;
//! if report.recovered > 0 || report.compensated > 0 {
//!     tracing::info!("Recovered {} operations, compensated {}",
//!         report.recovered, report.compensated);
//! }
//!
//! // Now safe to use the wallet normally
//! ```
//!
//! # Recovery Strategy
//!
//! For each incomplete saga, the recovery logic examines the saga state and takes
//! appropriate action:
//!
//! - **ProofsReserved**: No external call was made. Safe to compensate by releasing proofs.
//! - **SwapRequested**: External call may have succeeded. Check mint for proof states
//!   and either reconstruct outputs or compensate.

use cdk_common::wallet::{
    IssueSagaState, MeltOperationData, MeltSagaState, OperationData, ReceiveSagaState,
    SendSagaState, SwapSagaState, WalletSagaState,
};
use tracing::instrument;

use crate::dhke::construct_proofs;
use crate::nuts::{CheckStateRequest, PreMintSecrets, RestoreRequest, State};
use crate::types::ProofInfo;
use crate::{Error, Wallet};

/// Report of recovery operations performed
#[derive(Debug, Default)]
pub struct RecoveryReport {
    /// Number of sagas that were successfully recovered
    pub recovered: usize,
    /// Number of sagas that were compensated (rolled back)
    pub compensated: usize,
    /// Number of sagas that were skipped (e.g., pending external state)
    pub skipped: usize,
    /// Number of sagas that failed to recover
    pub failed: usize,
}

impl Wallet {
    /// Recover from incomplete sagas.
    ///
    /// This method should be called on wallet initialization to recover from
    /// any incomplete operations that were interrupted by a crash.
    ///
    /// # Returns
    ///
    /// A report of the recovery operations performed.
    #[instrument(skip(self))]
    pub async fn recover_incomplete_sagas(&self) -> Result<RecoveryReport, Error> {
        // First, clean up any orphaned quote reservations.
        // These can occur if the wallet crashed after reserving a quote
        // but before creating the saga record.
        self.cleanup_orphaned_quote_reservations().await?;

        let sagas = self.localstore.get_incomplete_sagas().await?;

        if sagas.is_empty() {
            tracing::debug!("No incomplete sagas to recover");
            return Ok(RecoveryReport::default());
        }

        tracing::info!("Found {} incomplete saga(s) to recover", sagas.len());

        let mut report = RecoveryReport::default();

        for saga in sagas {
            tracing::info!(
                "Recovering saga {} (kind: {:?}, state: {})",
                saga.id,
                saga.kind,
                saga.state.state_str()
            );

            let result = match &saga.state {
                WalletSagaState::Swap(state) => {
                    self.recover_swap_saga(&saga.id, state, &saga.data).await
                }
                WalletSagaState::Send(state) => {
                    self.recover_send_saga(&saga.id, state, &saga.data).await
                }
                WalletSagaState::Receive(state) => {
                    self.recover_receive_saga(&saga.id, state, &saga.data).await
                }
                WalletSagaState::Issue(state) => {
                    self.recover_issue_saga(&saga.id, state, &saga.data).await
                }
                WalletSagaState::Melt(state) => {
                    self.recover_melt_saga(&saga.id, state, &saga.data).await
                }
            };

            match result {
                Ok(RecoveryAction::Recovered) => {
                    tracing::info!("Saga {} recovered successfully", saga.id);
                    report.recovered += 1;
                }
                Ok(RecoveryAction::Compensated) => {
                    tracing::info!("Saga {} compensated (rolled back)", saga.id);
                    report.compensated += 1;
                }
                Ok(RecoveryAction::Skipped) => {
                    tracing::info!("Saga {} skipped", saga.id);
                    report.skipped += 1;
                }
                Err(e) => {
                    tracing::error!("Failed to recover saga {}: {}", saga.id, e);
                    report.failed += 1;
                    // Continue with other sagas - don't fail the entire recovery
                }
            }
        }

        tracing::info!(
            "Recovery complete: {} recovered, {} compensated, {} skipped, {} failed",
            report.recovered,
            report.compensated,
            report.skipped,
            report.failed
        );

        Ok(report)
    }

    /// Recover a swap saga.
    ///
    /// # Recovery Logic
    ///
    /// - **ProofsReserved**: The swap request hasn't been sent to the mint yet.
    ///   Safe to compensate by releasing the reserved proofs.
    ///
    /// - **SwapRequested**: The swap request was sent but we don't know the outcome.
    ///   Need to check the mint to determine if the swap succeeded.
    #[instrument(skip(self, data))]
    async fn recover_swap_saga(
        &self,
        saga_id: &uuid::Uuid,
        state: &SwapSagaState,
        data: &OperationData,
    ) -> Result<RecoveryAction, Error> {
        match state {
            SwapSagaState::ProofsReserved => {
                // No external call was made - safe to compensate
                tracing::info!(
                    "Swap saga {} in ProofsReserved state - compensating",
                    saga_id
                );
                self.compensate_swap_saga(saga_id).await?;
                Ok(RecoveryAction::Compensated)
            }
            SwapSagaState::SwapRequested => {
                // External call may have succeeded - need to check mint
                tracing::info!(
                    "Swap saga {} in SwapRequested state - checking mint for proof states",
                    saga_id
                );

                // Get the reserved proofs for this operation
                let reserved_proofs = self.localstore.get_reserved_proofs(saga_id).await?;

                if reserved_proofs.is_empty() {
                    // No proofs found - saga may have already been cleaned up
                    tracing::warn!(
                        "No reserved proofs found for saga {} - cleaning up orphaned saga",
                        saga_id
                    );
                    self.localstore.delete_saga(saga_id).await?;
                    return Ok(RecoveryAction::Recovered);
                }

                let proof_ys: Vec<_> = reserved_proofs.iter().map(|p| p.y).collect();

                // Check proof states with the mint
                let check_request = CheckStateRequest { ys: proof_ys };
                let check_result = self.client.post_check_state(check_request).await;

                match check_result {
                    Ok(states) => {
                        // If all input proofs are spent, the swap succeeded
                        let all_spent = states.states.iter().all(|s| s.state == State::Spent);

                        if all_spent {
                            // Input proofs are spent - try to recover outputs
                            self.recover_swap_outputs(saga_id, data).await?;
                            Ok(RecoveryAction::Recovered)
                        } else {
                            // Proofs not spent - swap failed, safe to compensate
                            tracing::info!(
                                "Swap saga {} - input proofs not spent, compensating",
                                saga_id
                            );
                            self.compensate_swap_saga(saga_id).await?;
                            Ok(RecoveryAction::Compensated)
                        }
                    }
                    Err(e) => {
                        // Can't reach mint - skip for now, retry on next recovery
                        tracing::warn!(
                            "Swap saga {} - can't check proof states (mint unreachable: {}), skipping",
                            saga_id,
                            e
                        );
                        Ok(RecoveryAction::Skipped)
                    }
                }
            }
        }
    }

    /// Compensate a swap saga by releasing reserved proofs and deleting the saga.
    #[instrument(skip(self))]
    async fn compensate_swap_saga(&self, saga_id: &uuid::Uuid) -> Result<(), Error> {
        // Release proofs reserved by this operation
        if let Err(e) = self.localstore.release_proofs(saga_id).await {
            tracing::warn!(
                "Failed to release proofs for saga {}: {}. Continuing with saga cleanup.",
                saga_id,
                e
            );
        }

        // Delete the saga record
        self.localstore.delete_saga(saga_id).await?;

        Ok(())
    }

    /// Recover swap outputs using stored blinded messages.
    ///
    /// When a swap succeeded (inputs are spent) but we crashed before saving outputs,
    /// we can recover by querying the mint with the stored blinded messages.
    #[instrument(skip(self, data))]
    async fn recover_swap_outputs(
        &self,
        saga_id: &uuid::Uuid,
        data: &OperationData,
    ) -> Result<(), Error> {
        let swap_data = match data {
            OperationData::Swap(d) => d,
            _ => {
                tracing::error!("Swap saga {} has wrong operation data type", saga_id);
                return Err(Error::Custom(
                    "Invalid operation data for swap saga".to_string(),
                ));
            }
        };

        // Check if we have blinded messages stored
        let blinded_messages = match &swap_data.blinded_messages {
            Some(bm) if !bm.is_empty() => bm.clone(),
            _ => {
                tracing::warn!(
                    "Swap saga {} - no blinded messages stored, cannot recover outputs. \
                     Run wallet.restore() to recover any missing proofs.",
                    saga_id
                );
                // Fall back to cleanup without recovery
                return self.cleanup_saga_without_recovery(saga_id).await;
            }
        };

        // Get counter range for re-deriving secrets
        let (counter_start, counter_end) = match (swap_data.counter_start, swap_data.counter_end) {
            (Some(start), Some(end)) => (start, end),
            _ => {
                tracing::warn!(
                    "Swap saga {} - no counter range stored, cannot recover outputs. \
                     Run wallet.restore() to recover any missing proofs.",
                    saga_id
                );
                return self.cleanup_saga_without_recovery(saga_id).await;
            }
        };

        tracing::info!(
            "Swap saga {} - attempting to recover {} outputs using stored blinded messages",
            saga_id,
            blinded_messages.len()
        );

        // Query the mint for signatures using the stored blinded messages
        let restore_request = RestoreRequest {
            outputs: blinded_messages.clone(),
        };

        let restore_response = match self.client.post_restore(restore_request).await {
            Ok(response) => response,
            Err(e) => {
                tracing::warn!(
                    "Swap saga {} - failed to restore from mint: {}. \
                     Run wallet.restore() to recover any missing proofs.",
                    saga_id,
                    e
                );
                return self.cleanup_saga_without_recovery(saga_id).await;
            }
        };

        if restore_response.signatures.is_empty() {
            tracing::warn!(
                "Swap saga {} - mint returned no signatures. \
                 Outputs may have already been saved or mint doesn't have them.",
                saga_id
            );
            return self.cleanup_saga_without_recovery(saga_id).await;
        }

        // Get keyset ID from the first blinded message
        let keyset_id = blinded_messages[0].keyset_id;

        // Re-derive premint secrets using the counter range
        let premint_secrets =
            PreMintSecrets::restore_batch(keyset_id, &self.seed, counter_start, counter_end)?;

        // Match the returned outputs to our premint secrets by B_ value
        let matched_secrets: Vec<_> = premint_secrets
            .secrets
            .iter()
            .filter(|p| restore_response.outputs.contains(&p.blinded_message))
            .collect();

        if matched_secrets.len() != restore_response.signatures.len() {
            tracing::warn!(
                "Swap saga {} - signature count mismatch: {} secrets, {} signatures",
                saga_id,
                matched_secrets.len(),
                restore_response.signatures.len()
            );
        }

        // Load keyset keys for proof construction
        let keys = self.load_keyset_keys(keyset_id).await?;

        // Construct proofs from signatures
        let proofs = construct_proofs(
            restore_response.signatures,
            matched_secrets.iter().map(|p| p.r.clone()).collect(),
            matched_secrets.iter().map(|p| p.secret.clone()).collect(),
            &keys,
        )?;

        tracing::info!("Swap saga {} - recovered {} proofs", saga_id, proofs.len());

        // Store the recovered proofs
        let proof_infos: Vec<ProofInfo> = proofs
            .into_iter()
            .map(|p| ProofInfo::new(p, self.mint_url.clone(), State::Unspent, self.unit.clone()))
            .collect::<Result<Vec<_>, _>>()?;

        // Remove the input proofs (they're spent) and add recovered proofs
        let reserved_proofs = self.localstore.get_reserved_proofs(saga_id).await?;
        let input_ys: Vec<_> = reserved_proofs.iter().map(|p| p.y).collect();

        self.localstore.update_proofs(proof_infos, input_ys).await?;

        // Delete the saga record
        self.localstore.delete_saga(saga_id).await?;

        Ok(())
    }

    /// Clean up a saga when we can't recover outputs.
    /// Marks inputs as spent and deletes the saga.
    #[instrument(skip(self))]
    async fn cleanup_saga_without_recovery(&self, saga_id: &uuid::Uuid) -> Result<(), Error> {
        // Remove the input proofs (they're spent)
        let reserved_proofs = self.localstore.get_reserved_proofs(saga_id).await?;
        let input_ys: Vec<_> = reserved_proofs.iter().map(|p| p.y).collect();

        if !input_ys.is_empty() {
            self.localstore
                .update_proofs_state(input_ys, State::Spent)
                .await?;
        }

        // Delete the saga record
        self.localstore.delete_saga(saga_id).await?;

        Ok(())
    }

    /// Recover a send saga.
    ///
    /// # Recovery Logic
    ///
    /// - **ProofsReserved**: Proofs reserved but token not created.
    ///   Safe to compensate by releasing the reserved proofs.
    ///
    /// - **TokenCreated**: Token was created. Check if proofs are still spendable.
    ///   If spent, the send succeeded. If not spent, compensate.
    #[instrument(skip(self, _data))]
    async fn recover_send_saga(
        &self,
        saga_id: &uuid::Uuid,
        state: &SendSagaState,
        _data: &cdk_common::wallet::OperationData,
    ) -> Result<RecoveryAction, Error> {
        match state {
            SendSagaState::ProofsReserved => {
                // No token was created - safe to compensate
                tracing::info!(
                    "Send saga {} in ProofsReserved state - compensating",
                    saga_id
                );
                self.compensate_proofs_saga(saga_id).await?;
                Ok(RecoveryAction::Compensated)
            }
            SendSagaState::TokenCreated => {
                // Token was created but we don't know if it was received
                // For send, the proofs are marked PendingSpent once token is created
                // If they're still in that state, the token wasn't redeemed yet
                tracing::info!(
                    "Send saga {} in TokenCreated state - checking proof states",
                    saga_id
                );

                // Get the reserved/pending proofs for this operation
                let reserved_proofs = self.localstore.get_reserved_proofs(saga_id).await?;

                if reserved_proofs.is_empty() {
                    // No proofs found - saga may have completed
                    tracing::warn!(
                        "No reserved proofs found for send saga {} - cleaning up orphaned saga",
                        saga_id
                    );
                    self.localstore.delete_saga(saga_id).await?;
                    return Ok(RecoveryAction::Recovered);
                }

                let proof_ys: Vec<_> = reserved_proofs.iter().map(|p| p.y).collect();

                // Check proof states with the mint
                let check_request = CheckStateRequest {
                    ys: proof_ys.clone(),
                };
                let check_result = self.client.post_check_state(check_request).await;

                match check_result {
                    Ok(states) => {
                        let all_spent = states.states.iter().all(|s| s.state == State::Spent);

                        if all_spent {
                            // Token was redeemed - mark proofs as spent and clean up
                            tracing::info!(
                                "Send saga {} - proofs are spent, marking as complete",
                                saga_id
                            );
                            self.localstore
                                .update_proofs_state(proof_ys, State::Spent)
                                .await?;
                            self.localstore.delete_saga(saga_id).await?;
                            Ok(RecoveryAction::Recovered)
                        } else {
                            // Token wasn't redeemed - leave proofs in PendingSpent state
                            // The user still has the token and could redeem it later
                            tracing::info!(
                                "Send saga {} - proofs not spent, token may still be valid",
                                saga_id
                            );
                            self.localstore.delete_saga(saga_id).await?;
                            Ok(RecoveryAction::Recovered)
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Send saga {} - can't check proof states (mint unreachable: {}), skipping",
                            saga_id,
                            e
                        );
                        Ok(RecoveryAction::Skipped)
                    }
                }
            }
        }
    }

    /// Recover a receive saga.
    ///
    /// # Recovery Logic
    ///
    /// - **ProofsPending**: Proofs stored in Pending state but swap not executed.
    ///   Safe to compensate by removing the pending proofs.
    ///
    /// - **SwapRequested**: Swap was requested. Check if input proofs are spent.
    ///   If spent, try to reconstruct outputs. If not spent, compensate.
    #[instrument(skip(self, data))]
    async fn recover_receive_saga(
        &self,
        saga_id: &uuid::Uuid,
        state: &ReceiveSagaState,
        data: &OperationData,
    ) -> Result<RecoveryAction, Error> {
        match state {
            ReceiveSagaState::ProofsPending => {
                // No swap was executed - safe to compensate by removing pending proofs
                tracing::info!(
                    "Receive saga {} in ProofsPending state - compensating",
                    saga_id
                );
                self.compensate_receive_saga(saga_id).await?;
                Ok(RecoveryAction::Compensated)
            }
            ReceiveSagaState::SwapRequested => {
                // Similar to swap saga - check if proofs were spent
                tracing::info!(
                    "Receive saga {} in SwapRequested state - checking mint for proof states",
                    saga_id
                );

                // Get the pending proofs for this operation
                let pending_proofs = self
                    .localstore
                    .get_proofs(
                        Some(self.mint_url.clone()),
                        Some(self.unit.clone()),
                        Some(vec![State::Pending]),
                        None,
                    )
                    .await?;

                if pending_proofs.is_empty() {
                    tracing::warn!(
                        "No pending proofs found for receive saga {} - cleaning up orphaned saga",
                        saga_id
                    );
                    self.localstore.delete_saga(saga_id).await?;
                    return Ok(RecoveryAction::Recovered);
                }

                let proof_ys: Vec<_> = pending_proofs.iter().map(|p| p.y).collect();

                let check_request = CheckStateRequest { ys: proof_ys };
                let check_result = self.client.post_check_state(check_request).await;

                match check_result {
                    Ok(states) => {
                        let all_spent = states.states.iter().all(|s| s.state == State::Spent);

                        if all_spent {
                            // Input proofs are spent - try to recover outputs
                            self.recover_receive_outputs(saga_id, data).await?;
                            Ok(RecoveryAction::Recovered)
                        } else {
                            // Proofs not spent - swap failed, compensate
                            tracing::info!(
                                "Receive saga {} - input proofs not spent, compensating",
                                saga_id
                            );
                            self.compensate_receive_saga(saga_id).await?;
                            Ok(RecoveryAction::Compensated)
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Receive saga {} - can't check proof states (mint unreachable: {}), skipping",
                            saga_id,
                            e
                        );
                        Ok(RecoveryAction::Skipped)
                    }
                }
            }
        }
    }

    /// Compensate a receive saga by removing pending proofs and deleting the saga.
    #[instrument(skip(self))]
    async fn compensate_receive_saga(&self, saga_id: &uuid::Uuid) -> Result<(), Error> {
        // Remove pending proofs for this operation
        let pending_proofs = self
            .localstore
            .get_proofs(
                Some(self.mint_url.clone()),
                Some(self.unit.clone()),
                Some(vec![State::Pending]),
                None,
            )
            .await?;
        if !pending_proofs.is_empty() {
            let proof_ys: Vec<_> = pending_proofs.iter().map(|p| p.y).collect();
            self.localstore.update_proofs(vec![], proof_ys).await?;
        }

        self.localstore.delete_saga(saga_id).await?;
        Ok(())
    }

    /// Recover receive outputs using stored blinded messages.
    ///
    /// When a receive swap succeeded (inputs are spent) but we crashed before saving outputs,
    /// we can recover by querying the mint with the stored blinded messages.
    #[instrument(skip(self, data))]
    async fn recover_receive_outputs(
        &self,
        saga_id: &uuid::Uuid,
        data: &OperationData,
    ) -> Result<(), Error> {
        let receive_data = match data {
            OperationData::Receive(d) => d,
            _ => {
                tracing::error!("Receive saga {} has wrong operation data type", saga_id);
                return Err(Error::Custom(
                    "Invalid operation data for receive saga".to_string(),
                ));
            }
        };

        // Check if we have blinded messages stored
        let blinded_messages = match &receive_data.blinded_messages {
            Some(bm) if !bm.is_empty() => bm.clone(),
            _ => {
                tracing::warn!(
                    "Receive saga {} - no blinded messages stored, cannot recover outputs. \
                     Run wallet.restore() to recover any missing proofs.",
                    saga_id
                );
                return self.cleanup_receive_saga_without_recovery(saga_id).await;
            }
        };

        // Get counter range for re-deriving secrets
        let (counter_start, counter_end) =
            match (receive_data.counter_start, receive_data.counter_end) {
                (Some(start), Some(end)) => (start, end),
                _ => {
                    tracing::warn!(
                        "Receive saga {} - no counter range stored, cannot recover outputs. \
                         Run wallet.restore() to recover any missing proofs.",
                        saga_id
                    );
                    return self.cleanup_receive_saga_without_recovery(saga_id).await;
                }
            };

        tracing::info!(
            "Receive saga {} - attempting to recover {} outputs using stored blinded messages",
            saga_id,
            blinded_messages.len()
        );

        // Query the mint for signatures using the stored blinded messages
        let restore_request = RestoreRequest {
            outputs: blinded_messages.clone(),
        };

        let restore_response = match self.client.post_restore(restore_request).await {
            Ok(response) => response,
            Err(e) => {
                tracing::warn!(
                    "Receive saga {} - failed to restore from mint: {}. \
                     Run wallet.restore() to recover any missing proofs.",
                    saga_id,
                    e
                );
                return self.cleanup_receive_saga_without_recovery(saga_id).await;
            }
        };

        if restore_response.signatures.is_empty() {
            tracing::warn!(
                "Receive saga {} - mint returned no signatures. \
                 Outputs may have already been saved or mint doesn't have them.",
                saga_id
            );
            return self.cleanup_receive_saga_without_recovery(saga_id).await;
        }

        // Get keyset ID from the first blinded message
        let keyset_id = blinded_messages[0].keyset_id;

        // Re-derive premint secrets using the counter range
        let premint_secrets =
            PreMintSecrets::restore_batch(keyset_id, &self.seed, counter_start, counter_end)?;

        // Match the returned outputs to our premint secrets by B_ value
        let matched_secrets: Vec<_> = premint_secrets
            .secrets
            .iter()
            .filter(|p| restore_response.outputs.contains(&p.blinded_message))
            .collect();

        if matched_secrets.len() != restore_response.signatures.len() {
            tracing::warn!(
                "Receive saga {} - signature count mismatch: {} secrets, {} signatures",
                saga_id,
                matched_secrets.len(),
                restore_response.signatures.len()
            );
        }

        // Load keyset keys for proof construction
        let keys = self.load_keyset_keys(keyset_id).await?;

        // Construct proofs from signatures
        let proofs = construct_proofs(
            restore_response.signatures,
            matched_secrets.iter().map(|p| p.r.clone()).collect(),
            matched_secrets.iter().map(|p| p.secret.clone()).collect(),
            &keys,
        )?;

        tracing::info!(
            "Receive saga {} - recovered {} proofs",
            saga_id,
            proofs.len()
        );

        // Store the recovered proofs
        let proof_infos: Vec<ProofInfo> = proofs
            .into_iter()
            .map(|p| ProofInfo::new(p, self.mint_url.clone(), State::Unspent, self.unit.clone()))
            .collect::<Result<Vec<_>, _>>()?;

        // Remove the input proofs (they're spent) and add recovered proofs
        let pending_proofs = self
            .localstore
            .get_proofs(
                Some(self.mint_url.clone()),
                Some(self.unit.clone()),
                Some(vec![State::Pending]),
                None,
            )
            .await?;
        let input_ys: Vec<_> = pending_proofs.iter().map(|p| p.y).collect();

        self.localstore.update_proofs(proof_infos, input_ys).await?;

        // Delete the saga record
        self.localstore.delete_saga(saga_id).await?;

        Ok(())
    }

    /// Clean up a receive saga when we can't recover outputs.
    /// Removes pending proofs (they're spent) and deletes the saga.
    #[instrument(skip(self))]
    async fn cleanup_receive_saga_without_recovery(
        &self,
        saga_id: &uuid::Uuid,
    ) -> Result<(), Error> {
        tracing::warn!(
            "Receive saga {} - inputs are spent but outputs may not be saved. \
             Run wallet.restore() to recover any missing proofs.",
            saga_id
        );

        // Remove the pending input proofs (they're spent)
        let pending_proofs = self
            .localstore
            .get_proofs(
                Some(self.mint_url.clone()),
                Some(self.unit.clone()),
                Some(vec![State::Pending]),
                None,
            )
            .await?;
        if !pending_proofs.is_empty() {
            let input_ys: Vec<_> = pending_proofs.iter().map(|p| p.y).collect();
            self.localstore.update_proofs(vec![], input_ys).await?;
        }

        self.localstore.delete_saga(saga_id).await?;
        Ok(())
    }

    /// Recover an issue (mint) saga.
    ///
    /// # Recovery Logic
    ///
    /// - **SecretsPrepared**: Secrets created but mint request not sent.
    ///   Safe to compensate (no proofs to revert, just delete saga).
    ///
    /// - **MintRequested**: Mint request was sent. Try to recover outputs
    ///   using stored blinded messages.
    #[instrument(skip(self, data))]
    async fn recover_issue_saga(
        &self,
        saga_id: &uuid::Uuid,
        state: &IssueSagaState,
        data: &OperationData,
    ) -> Result<RecoveryAction, Error> {
        match state {
            IssueSagaState::SecretsPrepared => {
                // No mint request was sent - safe to delete saga
                // Counter increments are not reversed (by design)
                tracing::info!(
                    "Issue saga {} in SecretsPrepared state - cleaning up",
                    saga_id
                );

                // Release the mint quote reservation
                if let Err(e) = self.localstore.release_mint_quote(saga_id).await {
                    tracing::warn!(
                        "Failed to release mint quote for saga {}: {}. Continuing with saga cleanup.",
                        saga_id,
                        e
                    );
                }

                self.localstore.delete_saga(saga_id).await?;
                Ok(RecoveryAction::Compensated)
            }
            IssueSagaState::MintRequested => {
                // Mint request was sent - try to recover outputs
                tracing::info!(
                    "Issue saga {} in MintRequested state - attempting recovery",
                    saga_id
                );
                self.recover_issue_outputs(saga_id, data).await?;
                Ok(RecoveryAction::Recovered)
            }
        }
    }

    /// Recover issue outputs using stored blinded messages.
    ///
    /// When a mint request succeeded but we crashed before saving outputs,
    /// we can recover by querying the mint with the stored blinded messages.
    #[instrument(skip(self, data))]
    async fn recover_issue_outputs(
        &self,
        saga_id: &uuid::Uuid,
        data: &OperationData,
    ) -> Result<(), Error> {
        let issue_data = match data {
            OperationData::Mint(d) => d,
            _ => {
                tracing::error!("Issue saga {} has wrong operation data type", saga_id);
                return Err(Error::Custom(
                    "Invalid operation data for issue saga".to_string(),
                ));
            }
        };

        // Check if we have blinded messages stored
        let blinded_messages = match &issue_data.blinded_messages {
            Some(bm) if !bm.is_empty() => bm.clone(),
            _ => {
                tracing::warn!(
                    "Issue saga {} - no blinded messages stored, cannot recover outputs. \
                     Run wallet.restore() to recover any missing proofs.",
                    saga_id
                );
                self.localstore.delete_saga(saga_id).await?;
                return Ok(());
            }
        };

        // Get counter range for re-deriving secrets
        let (counter_start, counter_end) = match (issue_data.counter_start, issue_data.counter_end)
        {
            (Some(start), Some(end)) => (start, end),
            _ => {
                tracing::warn!(
                    "Issue saga {} - no counter range stored, cannot recover outputs. \
                         Run wallet.restore() to recover any missing proofs.",
                    saga_id
                );
                self.localstore.delete_saga(saga_id).await?;
                return Ok(());
            }
        };

        tracing::info!(
            "Issue saga {} - attempting to recover {} outputs using stored blinded messages",
            saga_id,
            blinded_messages.len()
        );

        // Query the mint for signatures using the stored blinded messages
        let restore_request = RestoreRequest {
            outputs: blinded_messages.clone(),
        };

        let restore_response = match self.client.post_restore(restore_request).await {
            Ok(response) => response,
            Err(e) => {
                tracing::warn!(
                    "Issue saga {} - failed to restore from mint: {}. \
                     Run wallet.restore() to recover any missing proofs.",
                    saga_id,
                    e
                );
                self.localstore.delete_saga(saga_id).await?;
                return Ok(());
            }
        };

        if restore_response.signatures.is_empty() {
            tracing::warn!(
                "Issue saga {} - mint returned no signatures. \
                 Outputs may have already been saved or mint doesn't have them.",
                saga_id
            );
            self.localstore.delete_saga(saga_id).await?;
            return Ok(());
        }

        // Get keyset ID from the first blinded message
        let keyset_id = blinded_messages[0].keyset_id;

        // Re-derive premint secrets using the counter range
        let premint_secrets =
            PreMintSecrets::restore_batch(keyset_id, &self.seed, counter_start, counter_end)?;

        // Match the returned outputs to our premint secrets by B_ value
        let matched_secrets: Vec<_> = premint_secrets
            .secrets
            .iter()
            .filter(|p| restore_response.outputs.contains(&p.blinded_message))
            .collect();

        if matched_secrets.len() != restore_response.signatures.len() {
            tracing::warn!(
                "Issue saga {} - signature count mismatch: {} secrets, {} signatures",
                saga_id,
                matched_secrets.len(),
                restore_response.signatures.len()
            );
        }

        // Load keyset keys for proof construction
        let keys = self.load_keyset_keys(keyset_id).await?;

        // Construct proofs from signatures
        let proofs = construct_proofs(
            restore_response.signatures,
            matched_secrets.iter().map(|p| p.r.clone()).collect(),
            matched_secrets.iter().map(|p| p.secret.clone()).collect(),
            &keys,
        )?;

        tracing::info!("Issue saga {} - recovered {} proofs", saga_id, proofs.len());

        // Store the recovered proofs
        let proof_infos: Vec<ProofInfo> = proofs
            .into_iter()
            .map(|p| ProofInfo::new(p, self.mint_url.clone(), State::Unspent, self.unit.clone()))
            .collect::<Result<Vec<_>, _>>()?;

        self.localstore.update_proofs(proof_infos, vec![]).await?;

        // Delete the saga record
        self.localstore.delete_saga(saga_id).await?;

        Ok(())
    }

    /// Recover a melt saga.
    ///
    /// # Recovery Logic
    ///
    /// - **ProofsReserved**: Proofs reserved but melt not executed.
    ///   Safe to compensate by releasing the reserved proofs.
    ///
    /// - **MeltRequested**: Melt request was sent. Check quote state
    ///   to determine if payment succeeded.
    ///
    /// - **PaymentPending**: Payment is pending. Check quote state
    ///   and wait for resolution.
    #[instrument(skip(self, data))]
    async fn recover_melt_saga(
        &self,
        saga_id: &uuid::Uuid,
        state: &MeltSagaState,
        data: &cdk_common::wallet::OperationData,
    ) -> Result<RecoveryAction, Error> {
        match state {
            MeltSagaState::ProofsReserved => {
                // No melt was executed - safe to compensate
                tracing::info!(
                    "Melt saga {} in ProofsReserved state - compensating",
                    saga_id
                );
                self.compensate_melt_saga(saga_id).await?;
                Ok(RecoveryAction::Compensated)
            }
            MeltSagaState::MeltRequested | MeltSagaState::PaymentPending => {
                // Melt was requested or payment is pending - check quote state
                tracing::info!(
                    "Melt saga {} in {:?} state - checking quote state",
                    saga_id,
                    state
                );

                let melt_data = match data {
                    cdk_common::wallet::OperationData::Melt(d) => d,
                    _ => {
                        return Err(Error::Custom(format!(
                            "Invalid operation data for melt saga {}",
                            saga_id
                        )))
                    }
                };

                // Check quote state with the mint
                match self.client.get_melt_quote_status(&melt_data.quote_id).await {
                    Ok(quote_status) => {
                        use cdk_common::MeltQuoteState;
                        match quote_status.state {
                            MeltQuoteState::Paid => {
                                // Payment succeeded - mark proofs as spent and clean up
                                tracing::info!(
                                    "Melt saga {} - payment succeeded, finalizing",
                                    saga_id
                                );
                                self.finalize_melt_saga(saga_id, melt_data).await?;
                                Ok(RecoveryAction::Recovered)
                            }
                            MeltQuoteState::Unpaid | MeltQuoteState::Failed => {
                                // Payment failed - compensate
                                tracing::info!(
                                    "Melt saga {} - payment failed, compensating",
                                    saga_id
                                );
                                self.compensate_melt_saga(saga_id).await?;
                                Ok(RecoveryAction::Compensated)
                            }
                            MeltQuoteState::Pending | MeltQuoteState::Unknown => {
                                // Still pending or unknown - skip and retry later
                                tracing::info!(
                                    "Melt saga {} - payment pending/unknown, skipping",
                                    saga_id
                                );
                                Ok(RecoveryAction::Skipped)
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Melt saga {} - can't check quote state (mint unreachable: {}), skipping",
                            saga_id,
                            e
                        );
                        Ok(RecoveryAction::Skipped)
                    }
                }
            }
        }
    }

    /// Finalize a melt saga after confirming payment succeeded.
    ///
    /// Marks input proofs as spent and tries to recover change proofs
    /// using stored blinded messages.
    #[instrument(skip(self, melt_data))]
    async fn finalize_melt_saga(
        &self,
        saga_id: &uuid::Uuid,
        melt_data: &MeltOperationData,
    ) -> Result<(), Error> {
        // Mark input proofs as spent
        let reserved_proofs = self.localstore.get_reserved_proofs(saga_id).await?;
        if !reserved_proofs.is_empty() {
            let proof_ys: Vec<_> = reserved_proofs.iter().map(|p| p.y).collect();
            self.localstore
                .update_proofs_state(proof_ys, State::Spent)
                .await?;
        }

        // Try to recover change proofs using stored blinded messages
        if let Some(ref change_blinded_messages) = melt_data.change_blinded_messages {
            if !change_blinded_messages.is_empty() {
                if let Err(e) = self
                    .recover_melt_change(saga_id, melt_data, change_blinded_messages)
                    .await
                {
                    tracing::warn!(
                        "Melt saga {} - failed to recover change: {}. \
                         Run wallet.restore() to recover any missing change.",
                        saga_id,
                        e
                    );
                }
            }
        } else {
            tracing::warn!(
                "Melt saga {} - payment succeeded but no change blinded messages stored. \
                 Run wallet.restore() to recover any missing change.",
                saga_id
            );
        }

        self.localstore.delete_saga(saga_id).await?;
        Ok(())
    }

    /// Recover melt change proofs using stored blinded messages.
    #[instrument(skip(self, melt_data, change_blinded_messages))]
    async fn recover_melt_change(
        &self,
        saga_id: &uuid::Uuid,
        melt_data: &MeltOperationData,
        change_blinded_messages: &[crate::nuts::BlindedMessage],
    ) -> Result<(), Error> {
        // Get counter range for re-deriving secrets
        let (counter_start, counter_end) = match (melt_data.counter_start, melt_data.counter_end) {
            (Some(start), Some(end)) => (start, end),
            _ => {
                return Err(Error::Custom(
                    "No counter range stored for melt change recovery".to_string(),
                ));
            }
        };

        tracing::info!(
            "Melt saga {} - attempting to recover {} change outputs",
            saga_id,
            change_blinded_messages.len()
        );

        // Query the mint for signatures using the stored blinded messages
        let restore_request = RestoreRequest {
            outputs: change_blinded_messages.to_vec(),
        };

        let restore_response = self.client.post_restore(restore_request).await?;

        if restore_response.signatures.is_empty() {
            tracing::info!(
                "Melt saga {} - mint returned no change signatures (change may have been 0)",
                saga_id
            );
            return Ok(());
        }

        // Get keyset ID from the first blinded message
        let keyset_id = change_blinded_messages[0].keyset_id;

        // Re-derive premint secrets using the counter range
        let premint_secrets =
            PreMintSecrets::restore_batch(keyset_id, &self.seed, counter_start, counter_end)?;

        // Match the returned outputs to our premint secrets by B_ value
        let matched_secrets: Vec<_> = premint_secrets
            .secrets
            .iter()
            .filter(|p| restore_response.outputs.contains(&p.blinded_message))
            .collect();

        if matched_secrets.len() != restore_response.signatures.len() {
            tracing::warn!(
                "Melt saga {} - change signature count mismatch: {} secrets, {} signatures",
                saga_id,
                matched_secrets.len(),
                restore_response.signatures.len()
            );
        }

        // Load keyset keys for proof construction
        let keys = self.load_keyset_keys(keyset_id).await?;

        // Construct proofs from signatures
        let proofs = construct_proofs(
            restore_response.signatures,
            matched_secrets.iter().map(|p| p.r.clone()).collect(),
            matched_secrets.iter().map(|p| p.secret.clone()).collect(),
            &keys,
        )?;

        tracing::info!(
            "Melt saga {} - recovered {} change proofs",
            saga_id,
            proofs.len()
        );

        // Store the recovered change proofs
        let proof_infos: Vec<ProofInfo> = proofs
            .into_iter()
            .map(|p| ProofInfo::new(p, self.mint_url.clone(), State::Unspent, self.unit.clone()))
            .collect::<Result<Vec<_>, _>>()?;

        self.localstore.update_proofs(proof_infos, vec![]).await?;

        Ok(())
    }

    /// Generic compensation for sagas with reserved proofs.
    #[instrument(skip(self))]
    async fn compensate_proofs_saga(&self, saga_id: &uuid::Uuid) -> Result<(), Error> {
        // Release proofs reserved by this operation
        if let Err(e) = self.localstore.release_proofs(saga_id).await {
            tracing::warn!(
                "Failed to release proofs for saga {}: {}. Continuing with saga cleanup.",
                saga_id,
                e
            );
        }

        self.localstore.delete_saga(saga_id).await?;
        Ok(())
    }

    /// Compensation for melt sagas - releases both proofs and the melt quote.
    #[instrument(skip(self))]
    async fn compensate_melt_saga(&self, saga_id: &uuid::Uuid) -> Result<(), Error> {
        // Release proofs reserved by this operation
        if let Err(e) = self.localstore.release_proofs(saga_id).await {
            tracing::warn!(
                "Failed to release proofs for saga {}: {}. Continuing with saga cleanup.",
                saga_id,
                e
            );
        }

        // Release the melt quote reservation
        if let Err(e) = self.localstore.release_melt_quote(saga_id).await {
            tracing::warn!(
                "Failed to release melt quote for saga {}: {}. Continuing with saga cleanup.",
                saga_id,
                e
            );
        }

        self.localstore.delete_saga(saga_id).await?;
        Ok(())
    }

    /// Clean up orphaned quote reservations.
    ///
    /// This handles the case where the wallet crashed after reserving a quote
    /// but before creating the saga record. In this case, the quote is stuck
    /// in a reserved state with no corresponding saga.
    ///
    /// This method:
    /// 1. Gets all quotes with `used_by_operation` set
    /// 2. Checks if a corresponding saga exists
    /// 3. If no saga exists, releases the quote reservation
    #[instrument(skip(self))]
    async fn cleanup_orphaned_quote_reservations(&self) -> Result<(), Error> {
        // Check melt quotes for orphaned reservations
        let melt_quotes = self.localstore.get_melt_quotes().await?;
        for quote in melt_quotes {
            if let Some(ref operation_id_str) = quote.used_by_operation {
                if let Ok(operation_id) = uuid::Uuid::parse_str(operation_id_str) {
                    // Check if saga exists
                    match self.localstore.get_saga(&operation_id).await {
                        Ok(Some(_)) => {
                            // Saga exists, this is not orphaned
                        }
                        Ok(None) => {
                            // No saga found - this is an orphaned reservation
                            tracing::warn!(
                                "Found orphaned melt quote reservation: quote={}, operation={}. Releasing.",
                                quote.id,
                                operation_id
                            );
                            if let Err(e) = self.localstore.release_melt_quote(&operation_id).await
                            {
                                tracing::error!(
                                    "Failed to release orphaned melt quote {}: {}",
                                    quote.id,
                                    e
                                );
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                "Failed to check saga for melt quote {}: {}",
                                quote.id,
                                e
                            );
                        }
                    }
                }
            }
        }

        // Check mint quotes for orphaned reservations
        let mint_quotes = self.localstore.get_mint_quotes().await?;
        for quote in mint_quotes {
            if let Some(ref operation_id_str) = quote.used_by_operation {
                if let Ok(operation_id) = uuid::Uuid::parse_str(operation_id_str) {
                    // Check if saga exists
                    match self.localstore.get_saga(&operation_id).await {
                        Ok(Some(_)) => {
                            // Saga exists, this is not orphaned
                        }
                        Ok(None) => {
                            // No saga found - this is an orphaned reservation
                            tracing::warn!(
                                "Found orphaned mint quote reservation: quote={}, operation={}. Releasing.",
                                quote.id,
                                operation_id
                            );
                            if let Err(e) = self.localstore.release_mint_quote(&operation_id).await
                            {
                                tracing::error!(
                                    "Failed to release orphaned mint quote {}: {}",
                                    quote.id,
                                    e
                                );
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                "Failed to check saga for mint quote {}: {}",
                                quote.id,
                                e
                            );
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

/// Result of a saga recovery operation
enum RecoveryAction {
    /// The saga was successfully recovered (outputs saved)
    Recovered,
    /// The saga was compensated (rolled back)
    Compensated,
    /// The saga was skipped (e.g., mint unreachable)
    Skipped,
}
