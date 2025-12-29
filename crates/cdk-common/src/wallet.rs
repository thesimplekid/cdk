//! Wallet Types

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use bitcoin::hashes::{sha256, Hash, HashEngine};
use cashu::util::hex;
use cashu::{nut00, PaymentMethod, Proofs, PublicKey};
use serde::{Deserialize, Serialize};

use crate::mint_url::MintUrl;
use crate::nuts::{CurrencyUnit, MeltQuoteState, MintQuoteState, SecretKey};
use crate::{Amount, Error};

/// Wallet Key
#[derive(Debug, Clone, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct WalletKey {
    /// Mint Url
    pub mint_url: MintUrl,
    /// Currency Unit
    pub unit: CurrencyUnit,
}

impl fmt::Display for WalletKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "mint_url: {}, unit: {}", self.mint_url, self.unit,)
    }
}

impl WalletKey {
    /// Create new [`WalletKey`]
    pub fn new(mint_url: MintUrl, unit: CurrencyUnit) -> Self {
        Self { mint_url, unit }
    }
}

/// Mint Quote Info
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MintQuote {
    /// Quote id
    pub id: String,
    /// Mint Url
    pub mint_url: MintUrl,
    /// Payment method
    #[serde(default)]
    pub payment_method: PaymentMethod,
    /// Amount of quote
    pub amount: Option<Amount>,
    /// Unit of quote
    pub unit: CurrencyUnit,
    /// Quote payment request e.g. bolt11
    pub request: String,
    /// Quote state
    pub state: MintQuoteState,
    /// Expiration time of quote
    pub expiry: u64,
    /// Secretkey for signing mint quotes [NUT-20]
    pub secret_key: Option<SecretKey>,
    /// Amount minted
    #[serde(default)]
    pub amount_issued: Amount,
    /// Amount paid to the mint for the quote
    #[serde(default)]
    pub amount_paid: Amount,
}

/// Melt Quote Info
#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeltQuote {
    /// Quote id
    pub id: String,
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
    /// Payment method
    #[serde(default)]
    pub payment_method: PaymentMethod,
}

impl MintQuote {
    /// Create a new MintQuote
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: String,
        mint_url: MintUrl,
        payment_method: PaymentMethod,
        amount: Option<Amount>,
        unit: CurrencyUnit,
        request: String,
        expiry: u64,
        secret_key: Option<SecretKey>,
    ) -> Self {
        Self {
            id,
            mint_url,
            payment_method,
            amount,
            unit,
            request,
            state: MintQuoteState::Unpaid,
            expiry,
            secret_key,
            amount_issued: Amount::ZERO,
            amount_paid: Amount::ZERO,
        }
    }

    /// Calculate the total amount including any fees
    pub fn total_amount(&self) -> Amount {
        self.amount_paid
    }

    /// Check if the quote has expired
    pub fn is_expired(&self, current_time: u64) -> bool {
        current_time > self.expiry
    }

    /// Amount that can be minted
    pub fn amount_mintable(&self) -> Amount {
        if self.amount_issued > self.amount_paid {
            return Amount::ZERO;
        }

        let difference = self.amount_paid - self.amount_issued;

        if difference == Amount::ZERO && self.state != MintQuoteState::Issued {
            if let Some(amount) = self.amount {
                return amount;
            }
        }

        difference
    }
}

/// Send Kind
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SendKind {
    #[default]
    /// Allow online swap before send if wallet does not have exact amount
    OnlineExact,
    /// Prefer offline send if difference is less then tolerance
    OnlineTolerance(Amount),
    /// Wallet cannot do an online swap and selected proof must be exactly send amount
    OfflineExact,
    /// Wallet must remain offline but can over pay if below tolerance
    OfflineTolerance(Amount),
}

impl SendKind {
    /// Check if send kind is online
    pub fn is_online(&self) -> bool {
        matches!(self, Self::OnlineExact | Self::OnlineTolerance(_))
    }

    /// Check if send kind is offline
    pub fn is_offline(&self) -> bool {
        matches!(self, Self::OfflineExact | Self::OfflineTolerance(_))
    }

    /// Check if send kind is exact
    pub fn is_exact(&self) -> bool {
        matches!(self, Self::OnlineExact | Self::OfflineExact)
    }

    /// Check if send kind has tolerance
    pub fn has_tolerance(&self) -> bool {
        matches!(self, Self::OnlineTolerance(_) | Self::OfflineTolerance(_))
    }
}

/// Wallet Transaction
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Transaction {
    /// Mint Url
    pub mint_url: MintUrl,
    /// Transaction direction
    pub direction: TransactionDirection,
    /// Amount
    pub amount: Amount,
    /// Fee
    pub fee: Amount,
    /// Currency Unit
    pub unit: CurrencyUnit,
    /// Proof Ys
    pub ys: Vec<PublicKey>,
    /// Unix timestamp
    pub timestamp: u64,
    /// Memo
    pub memo: Option<String>,
    /// User-defined metadata
    pub metadata: HashMap<String, String>,
    /// Quote ID if this is a mint or melt transaction
    pub quote_id: Option<String>,
    /// Payment request (e.g., BOLT11 invoice, BOLT12 offer)
    pub payment_request: Option<String>,
    /// Payment proof (e.g., preimage for Lightning melt transactions)
    pub payment_proof: Option<String>,
    /// Payment method (e.g., Bolt11, Bolt12) for mint/melt transactions
    #[serde(default)]
    pub payment_method: Option<PaymentMethod>,
}

impl Transaction {
    /// Transaction ID
    pub fn id(&self) -> TransactionId {
        TransactionId::new(self.ys.clone())
    }

    /// Check if transaction matches conditions
    pub fn matches_conditions(
        &self,
        mint_url: &Option<MintUrl>,
        direction: &Option<TransactionDirection>,
        unit: &Option<CurrencyUnit>,
    ) -> bool {
        if let Some(mint_url) = mint_url {
            if &self.mint_url != mint_url {
                return false;
            }
        }
        if let Some(direction) = direction {
            if &self.direction != direction {
                return false;
            }
        }
        if let Some(unit) = unit {
            if &self.unit != unit {
                return false;
            }
        }
        true
    }
}

impl PartialOrd for Transaction {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Transaction {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.timestamp
            .cmp(&other.timestamp)
            .reverse()
            .then_with(|| self.id().cmp(&other.id()))
    }
}

/// Transaction Direction
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransactionDirection {
    /// Incoming transaction (i.e., receive or mint)
    Incoming,
    /// Outgoing transaction (i.e., send or melt)
    Outgoing,
}

impl std::fmt::Display for TransactionDirection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransactionDirection::Incoming => write!(f, "Incoming"),
            TransactionDirection::Outgoing => write!(f, "Outgoing"),
        }
    }
}

impl FromStr for TransactionDirection {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "Incoming" => Ok(Self::Incoming),
            "Outgoing" => Ok(Self::Outgoing),
            _ => Err(Error::InvalidTransactionDirection),
        }
    }
}

/// Transaction ID
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TransactionId([u8; 32]);

impl TransactionId {
    /// Create new [`TransactionId`]
    pub fn new(ys: Vec<PublicKey>) -> Self {
        let mut ys = ys;
        ys.sort();
        let mut hasher = sha256::Hash::engine();
        for y in ys {
            hasher.input(&y.to_bytes());
        }
        let hash = sha256::Hash::from_engine(hasher);
        Self(hash.to_byte_array())
    }

    /// From proofs
    pub fn from_proofs(proofs: Proofs) -> Result<Self, nut00::Error> {
        let ys = proofs
            .iter()
            .map(|proof| proof.y())
            .collect::<Result<Vec<PublicKey>, nut00::Error>>()?;
        Ok(Self::new(ys))
    }

    /// From bytes
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// From hex string
    pub fn from_hex(value: &str) -> Result<Self, Error> {
        let bytes = hex::decode(value)?;
        if bytes.len() != 32 {
            return Err(Error::InvalidTransactionId);
        }
        let mut array = [0u8; 32];
        array.copy_from_slice(&bytes);
        Ok(Self(array))
    }

    /// From slice
    pub fn from_slice(slice: &[u8]) -> Result<Self, Error> {
        if slice.len() != 32 {
            return Err(Error::InvalidTransactionId);
        }
        let mut array = [0u8; 32];
        array.copy_from_slice(slice);
        Ok(Self(array))
    }

    /// Get inner value
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Get inner value as slice
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

impl std::fmt::Display for TransactionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", hex::encode(self.0))
    }
}

impl FromStr for TransactionId {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::from_hex(value)
    }
}

impl TryFrom<Proofs> for TransactionId {
    type Error = nut00::Error;

    fn try_from(proofs: Proofs) -> Result<Self, Self::Error> {
        Self::from_proofs(proofs)
    }
}

/// Wallet operation kind
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    /// Send operation
    Send,
    /// Receive operation
    Receive,
    /// Swap operation
    Swap,
    /// Mint operation
    Mint,
    /// Melt operation
    Melt,
}

impl fmt::Display for OperationKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OperationKind::Send => write!(f, "send"),
            OperationKind::Receive => write!(f, "receive"),
            OperationKind::Swap => write!(f, "swap"),
            OperationKind::Mint => write!(f, "mint"),
            OperationKind::Melt => write!(f, "melt"),
        }
    }
}

impl FromStr for OperationKind {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "send" => Ok(OperationKind::Send),
            "receive" => Ok(OperationKind::Receive),
            "swap" => Ok(OperationKind::Swap),
            "mint" => Ok(OperationKind::Mint),
            "melt" => Ok(OperationKind::Melt),
            _ => Err(Error::InvalidOperationKind),
        }
    }
}

/// Wallet operation state
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WalletOperationState {
    /// Operation initialized but not yet prepared
    Init,
    /// Operation prepared (resources reserved)
    Prepared,
    /// Operation executing (API call in progress)
    Executing,
    /// Operation pending (waiting for confirmation)
    Pending,
    /// Operation finalized successfully
    Finalized,
    /// Operation rolled back due to error
    RolledBack,
}

impl fmt::Display for WalletOperationState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WalletOperationState::Init => write!(f, "init"),
            WalletOperationState::Prepared => write!(f, "prepared"),
            WalletOperationState::Executing => write!(f, "executing"),
            WalletOperationState::Pending => write!(f, "pending"),
            WalletOperationState::Finalized => write!(f, "finalized"),
            WalletOperationState::RolledBack => write!(f, "rolled_back"),
        }
    }
}

impl FromStr for WalletOperationState {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "init" => Ok(WalletOperationState::Init),
            "prepared" => Ok(WalletOperationState::Prepared),
            "executing" => Ok(WalletOperationState::Executing),
            "pending" => Ok(WalletOperationState::Pending),
            "finalized" => Ok(WalletOperationState::Finalized),
            "rolled_back" => Ok(WalletOperationState::RolledBack),
            _ => Err(Error::InvalidOperationState),
        }
    }
}

/// Operation-specific data for Send operations
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendOperationData {
    /// Target amount to send
    pub amount: Amount,
    /// Memo for the send
    pub memo: Option<String>,
    /// Derivation counter start
    pub counter_start: Option<u32>,
    /// Derivation counter end
    pub counter_end: Option<u32>,
    /// Token data (when in Pending/Finalized state)
    pub token: Option<String>,
}

/// Operation-specific data for Receive operations
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiveOperationData {
    /// Token to receive
    pub token: String,
    /// Derivation counter start
    pub counter_start: Option<u32>,
    /// Derivation counter end
    pub counter_end: Option<u32>,
    /// Amount received
    pub amount: Option<Amount>,
}

/// Operation-specific data for Swap operations
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwapOperationData {
    /// Input amount
    pub input_amount: Amount,
    /// Output amount
    pub output_amount: Amount,
    /// Derivation counter start
    pub counter_start: Option<u32>,
    /// Derivation counter end
    pub counter_end: Option<u32>,
}

/// Operation-specific data for Mint operations
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MintOperationData {
    /// Quote ID
    pub quote_id: String,
    /// Amount to mint
    pub amount: Amount,
    /// Derivation counter start
    pub counter_start: Option<u32>,
    /// Derivation counter end
    pub counter_end: Option<u32>,
}

/// Operation-specific data for Melt operations
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeltOperationData {
    /// Quote ID
    pub quote_id: String,
    /// Amount to melt
    pub amount: Amount,
    /// Fee reserve
    pub fee_reserve: Amount,
    /// Derivation counter start
    pub counter_start: Option<u32>,
    /// Derivation counter end
    pub counter_end: Option<u32>,
    /// Change amount (if any)
    pub change_amount: Option<Amount>,
}

/// Operation data enum
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum OperationData {
    /// Send operation data
    Send(SendOperationData),
    /// Receive operation data
    Receive(ReceiveOperationData),
    /// Swap operation data
    Swap(SwapOperationData),
    /// Mint operation data
    Mint(MintOperationData),
    /// Melt operation data
    Melt(MeltOperationData),
}

impl OperationData {
    /// Get the operation kind
    pub fn kind(&self) -> OperationKind {
        match self {
            OperationData::Send(_) => OperationKind::Send,
            OperationData::Receive(_) => OperationKind::Receive,
            OperationData::Swap(_) => OperationKind::Swap,
            OperationData::Mint(_) => OperationKind::Mint,
            OperationData::Melt(_) => OperationKind::Melt,
        }
    }
}

/// Wallet operation
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalletOperation {
    /// Unique operation ID
    pub id: String,
    /// Operation kind
    pub kind: OperationKind,
    /// Operation state
    pub state: WalletOperationState,
    /// Amount involved in the operation
    pub amount: Amount,
    /// Mint URL
    pub mint_url: MintUrl,
    /// Currency unit
    pub unit: CurrencyUnit,
    /// Creation timestamp (unix seconds)
    pub created_at: u64,
    /// Last update timestamp (unix seconds)
    pub updated_at: u64,
    /// Operation-specific data
    pub data: OperationData,
}

impl WalletOperation {
    /// Create a new wallet operation
    pub fn new(
        id: String,
        kind: OperationKind,
        amount: Amount,
        mint_url: MintUrl,
        unit: CurrencyUnit,
        data: OperationData,
    ) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Self {
            id,
            kind,
            state: WalletOperationState::Init,
            amount,
            mint_url,
            unit,
            created_at: now,
            updated_at: now,
            data,
        }
    }

    /// Update the operation state
    pub fn update_state(&mut self, state: WalletOperationState) {
        self.state = state;
        self.updated_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
    }

    /// Check if operation is complete
    pub fn is_complete(&self) -> bool {
        matches!(
            self.state,
            WalletOperationState::Finalized | WalletOperationState::RolledBack
        )
    }

    /// Check if operation is incomplete
    pub fn is_incomplete(&self) -> bool {
        !self.is_complete()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transaction_id_from_hex() {
        let hex_str = "a1b2c3d4e5f60718293a0b1c2d3e4f506172839a0b1c2d3e4f506172839a0b1c";
        let transaction_id = TransactionId::from_hex(hex_str).unwrap();
        assert_eq!(transaction_id.to_string(), hex_str);
    }

    #[test]
    fn test_transaction_id_from_hex_empty_string() {
        let hex_str = "";
        let res = TransactionId::from_hex(hex_str);
        assert!(matches!(res, Err(Error::InvalidTransactionId)));
    }

    #[test]
    fn test_transaction_id_from_hex_longer_string() {
        let hex_str = "a1b2c3d4e5f60718293a0b1c2d3e4f506172839a0b1c2d3e4f506172839a0b1ca1b2";
        let res = TransactionId::from_hex(hex_str);
        assert!(matches!(res, Err(Error::InvalidTransactionId)));
    }
}
