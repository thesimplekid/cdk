pub mod amount;
#[cfg(feature = "wallet")]
pub mod dhke;
pub mod error;
pub mod nuts;
pub mod secret;
pub mod serde_utils;
pub mod types;
pub mod utils;

pub use amount::Amount;
pub use bitcoin::hashes::sha256::Hash as Sha256;
pub use lightning_invoice;
pub use lightning_invoice::Bolt11Invoice;

pub type Result<T, E = Box<dyn std::error::Error>> = std::result::Result<T, E>;
