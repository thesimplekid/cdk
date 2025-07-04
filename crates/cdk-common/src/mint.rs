//! Mint types

use bitcoin::bip32::DerivationPath;
use cashu::util::unix_time;
use cashu::{MeltQuoteBolt11Response, MintQuoteBolt11Response};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::nuts::{MeltQuoteState, MintQuoteState};
use crate::{Amount, CurrencyUnit, Id, KeySetInfo, PublicKey};

/// Mint Quote Info
#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct MintQuote {
    /// Quote id
    pub id: Uuid,
    /// Amount of quote
    pub amount: Amount,
    /// Unit of quote
    pub unit: CurrencyUnit,
    /// Quote payment request e.g. bolt11
    pub request: String,
    /// Quote state
    pub state: MintQuoteState,
    /// Expiration time of quote
    pub expiry: u64,
    /// Value used by ln backend to look up state of request
    pub request_lookup_id: String,
    /// Pubkey
    pub pubkey: Option<PublicKey>,
    /// Unix time quote was created
    #[serde(default)]
    pub created_time: u64,
    /// Unix time quote was paid
    pub paid_time: Option<u64>,
    /// Unix time quote was issued
    pub issued_time: Option<u64>,
}

impl MintQuote {
    /// Create new [`MintQuote`]
    pub fn new(
        request: String,
        unit: CurrencyUnit,
        amount: Amount,
        expiry: u64,
        request_lookup_id: String,
        pubkey: Option<PublicKey>,
    ) -> Self {
        let id = Uuid::new_v4();

        Self {
            id,
            amount,
            unit,
            request,
            state: MintQuoteState::Unpaid,
            expiry,
            request_lookup_id,
            pubkey,
            created_time: unix_time(),
            paid_time: None,
            issued_time: None,
        }
    }
}

/// Melt Quote Info
#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeltQuote {
    /// Quote id
    pub id: Uuid,
    /// Quote unit
    pub unit: CurrencyUnit,
    /// Quote amount
    pub amount: Amount,
    /// Quote Payment request e.g. bolt11
    pub request: String,
    /// Quote fee reserve
    pub fee_reserve: Amount,
    /// Quote state
    pub state: MeltQuoteState,
    /// Expiration time of quote
    pub expiry: u64,
    /// Payment preimage
    pub payment_preimage: Option<String>,
    /// Value used by ln backend to look up state of request
    pub request_lookup_id: String,
    /// Msat to pay
    ///
    /// Used for an amountless invoice
    pub msat_to_pay: Option<Amount>,
    /// Unix time quote was created
    #[serde(default)]
    pub created_time: u64,
    /// Unix time quote was paid
    pub paid_time: Option<u64>,
}

impl MeltQuote {
    /// Create new [`MeltQuote`]
    pub fn new(
        request: String,
        unit: CurrencyUnit,
        amount: Amount,
        fee_reserve: Amount,
        expiry: u64,
        request_lookup_id: String,
        msat_to_pay: Option<Amount>,
    ) -> Self {
        let id = Uuid::new_v4();

        Self {
            id,
            amount,
            unit,
            request,
            fee_reserve,
            state: MeltQuoteState::Unpaid,
            expiry,
            payment_preimage: None,
            request_lookup_id,
            msat_to_pay,
            created_time: unix_time(),
            paid_time: None,
        }
    }
}

/// Mint Keyset Info
#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct MintKeySetInfo {
    /// Keyset [`Id`]
    pub id: Id,
    /// Keyset [`CurrencyUnit`]
    pub unit: CurrencyUnit,
    /// Keyset active or inactive
    /// Mint will only issue new signatures on active keysets
    pub active: bool,
    /// Starting unix time Keyset is valid from
    pub valid_from: u64,
    /// [`DerivationPath`] keyset
    pub derivation_path: DerivationPath,
    /// DerivationPath index of Keyset
    pub derivation_path_index: Option<u32>,
    /// Max order of keyset
    pub max_order: u8,
    /// Input Fee ppk
    #[serde(default = "default_fee")]
    pub input_fee_ppk: u64,
    /// Final expiry
    pub final_expiry: Option<u64>,
}

/// Default fee
pub fn default_fee() -> u64 {
    0
}

impl From<MintKeySetInfo> for KeySetInfo {
    fn from(keyset_info: MintKeySetInfo) -> Self {
        Self {
            id: keyset_info.id,
            unit: keyset_info.unit,
            active: keyset_info.active,
            input_fee_ppk: keyset_info.input_fee_ppk,
            final_expiry: keyset_info.final_expiry,
        }
    }
}

impl From<MintQuote> for MintQuoteBolt11Response<Uuid> {
    fn from(mint_quote: crate::mint::MintQuote) -> MintQuoteBolt11Response<Uuid> {
        MintQuoteBolt11Response {
            quote: mint_quote.id,
            request: mint_quote.request,
            state: mint_quote.state,
            expiry: Some(mint_quote.expiry),
            pubkey: mint_quote.pubkey,
            amount: Some(mint_quote.amount),
            unit: Some(mint_quote.unit.clone()),
        }
    }
}

impl From<&MeltQuote> for MeltQuoteBolt11Response<Uuid> {
    fn from(melt_quote: &MeltQuote) -> MeltQuoteBolt11Response<Uuid> {
        MeltQuoteBolt11Response {
            quote: melt_quote.id,
            payment_preimage: None,
            change: None,
            state: melt_quote.state,
            paid: Some(melt_quote.state == MeltQuoteState::Paid),
            expiry: melt_quote.expiry,
            amount: melt_quote.amount,
            fee_reserve: melt_quote.fee_reserve,
            request: None,
            unit: Some(melt_quote.unit.clone()),
        }
    }
}

impl From<MeltQuote> for MeltQuoteBolt11Response<Uuid> {
    fn from(melt_quote: MeltQuote) -> MeltQuoteBolt11Response<Uuid> {
        let paid = melt_quote.state == MeltQuoteState::Paid;
        MeltQuoteBolt11Response {
            quote: melt_quote.id,
            amount: melt_quote.amount,
            fee_reserve: melt_quote.fee_reserve,
            paid: Some(paid),
            state: melt_quote.state,
            expiry: melt_quote.expiry,
            payment_preimage: melt_quote.payment_preimage,
            change: None,
            request: Some(melt_quote.request.clone()),
            unit: Some(melt_quote.unit.clone()),
        }
    }
}
