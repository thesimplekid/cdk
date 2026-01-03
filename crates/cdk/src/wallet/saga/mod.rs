//! Wallet Saga Pattern Implementation
//!
//! This module provides the type state pattern infrastructure for wallet operations.
//! It mirrors the mint's saga pattern to ensure consistency across the codebase.
//!
//! # Type State Pattern
//!
//! The type state pattern uses Rust's type system to enforce valid state transitions
//! at compile-time. Each operation state is a distinct type, and operations are only
//! available on the appropriate type.
//!
//! # Compensation Pattern
//!
//! When a saga step fails, compensating actions are executed in reverse order (LIFO)
//! to undo all completed steps and restore the database to its pre-saga state.

use std::collections::VecDeque;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::Error;

/// Trait for compensating actions in the saga pattern.
///
/// Compensating actions are registered as steps complete and executed in reverse
/// order (LIFO) if the saga fails. Each action should be idempotent.
#[async_trait]
pub trait CompensatingAction: Send + Sync {
    /// Execute the compensating action
    async fn execute(&self) -> Result<(), Error>;

    /// Get the name of this compensating action for logging
    fn name(&self) -> &'static str;
}

/// A queue of compensating actions for saga rollback.
///
/// Actions are stored in LIFO order (most recent first) and executed
/// in that order during compensation.
pub type Compensations = Arc<Mutex<VecDeque<Box<dyn CompensatingAction>>>>;

/// Create a new empty compensations queue
pub fn new_compensations() -> Compensations {
    Arc::new(Mutex::new(VecDeque::new()))
}

/// Execute all compensating actions in the queue.
///
/// Actions are executed in LIFO order (most recent first).
/// Errors during compensation are logged but don't stop the process.
pub async fn execute_compensations(compensations: &Compensations) -> Result<(), Error> {
    let mut queue = compensations.lock().await;

    if queue.is_empty() {
        return Ok(());
    }

    tracing::warn!("Running {} compensating actions", queue.len());

    while let Some(compensation) = queue.pop_front() {
        tracing::debug!("Running compensation: {}", compensation.name());
        if let Err(e) = compensation.execute().await {
            tracing::error!(
                "Compensation {} failed: {}. Continuing...",
                compensation.name(),
                e
            );
        }
    }

    Ok(())
}

/// Clear all compensating actions from the queue.
///
/// Called when an operation completes successfully.
pub async fn clear_compensations(compensations: &Compensations) {
    compensations.lock().await.clear();
}

/// Add a compensating action to the front of the queue (LIFO order).
pub async fn add_compensation(compensations: &Compensations, action: Box<dyn CompensatingAction>) {
    compensations.lock().await.push_front(action);
}
