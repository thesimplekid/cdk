//! Hash-based payment (Ehash) types

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
#[cfg(feature = "mint")]
use uuid::Uuid;

use super::CurrencyUnit;
use crate::nuts::nut23::QuoteState;
use crate::Amount;

/// Ehash mint quote request
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "swagger", derive(utoipa::ToSchema))]
pub struct MintQuoteEhashRequest {
    /// Hash nonce for mining
    pub nonce: String,
    /// Amount (optional for Ehash)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount: Option<Amount>,
    /// Unit wallet would like to pay with
    pub unit: CurrencyUnit,
    /// Description for the hash challenge
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Ehash mint quote response
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "swagger", derive(utoipa::ToSchema))]
#[serde(bound = "Q: Serialize + DeserializeOwned")]
pub struct MintQuoteEhashResponse<Q> {
    /// Quote Id
    pub quote: Q,
    /// Hash challenge to solve
    pub challenge: String,
    /// Amount
    pub amount: Option<Amount>,
    /// Unit
    pub unit: CurrencyUnit,
    /// Quote State
    pub state: QuoteState,
    /// Unix timestamp until the quote is valid
    pub expiry: Option<u64>,
    /// Nonce provided in request
    pub nonce: String,
}

impl<Q: ToString> MintQuoteEhashResponse<Q> {
    /// Convert the MintQuoteEhashResponse with a quote type Q to a String
    pub fn to_string_id(&self) -> MintQuoteEhashResponse<String> {
        MintQuoteEhashResponse {
            quote: self.quote.to_string(),
            challenge: self.challenge.clone(),
            state: self.state,
            expiry: self.expiry,
            nonce: self.nonce.clone(),
            amount: self.amount,
            unit: self.unit.clone(),
        }
    }
}

#[cfg(feature = "mint")]
impl From<MintQuoteEhashResponse<Uuid>> for MintQuoteEhashResponse<String> {
    fn from(value: MintQuoteEhashResponse<Uuid>) -> Self {
        Self {
            quote: value.quote.to_string(),
            challenge: value.challenge,
            state: value.state,
            expiry: value.expiry,
            nonce: value.nonce,
            amount: value.amount,
            unit: value.unit,
        }
    }
}
