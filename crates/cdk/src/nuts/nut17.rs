//! NUT-17: Minting tokens Onchain
//!
//! <https://github.com/cashubtc/nuts/blob/main/17.md>

use serde::{Deserialize, Serialize};

use super::{BlindSignature, BlindedMessage, CurrencyUnit, MintMethodSettings, MintQuoteState};
use crate::Amount;

/// Mint quote request [NUT-17]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MintQuoteBtcOnchainRequest {
    /// Amount
    pub amount: Amount,
    /// Unit wallet would like to pay with
    pub unit: CurrencyUnit,
}

/// Mint quote response [NUT-17]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MintQuoteBtcOnchainResponse {
    /// Quote Id
    pub quote: String,
    /// Payment request to fulfill
    pub address: String,
    /// Whether the the request has been paid
    pub state: MintQuoteState,
    /// Payjoin
    pub payjoin: Option<PayjoinInfo>,
}

/// Payjoin information
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayjoinInfo {
    /// Origin Directory in v2
    pub origin: String,
    /// Ohttp keys
    pub ohttp_relay: Option<String>,
    /// PJO
    pub pjos: bool,
}

/// Mint request [NUT-17]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MintBtcOnchainRequest {
    /// Quote id
    pub quote: String,
    /// Outputs
    pub outputs: Vec<BlindedMessage>,
}

impl MintBtcOnchainRequest {
    /// Total amount of outputs in request
    pub fn total_amount(&self) -> Amount {
        self.outputs
            .iter()
            .map(|BlindedMessage { amount, .. }| *amount)
            .sum()
    }
}

/// Mint response [NUT-17]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MintBtcOnchainResponse {
    /// Blind Signatures
    pub signatures: Vec<BlindSignature>,
}

/// Mint Settings
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    methods: Vec<MintMethodSettings>,
    disabled: bool,
}