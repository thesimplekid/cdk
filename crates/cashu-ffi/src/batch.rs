//! Batched output creation keeps loops and seed material on the Rust side.
use serde::{Deserialize, Serialize};

use super::{
    blind_locking_keys, create_locked_output, create_random_output, invalid, output,
    random_secret_key, CryptoError, NativeOutput,
};

/// Normalized locking data shared by an output batch, before key blinding.
#[derive(Debug, Clone, Serialize, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct NativeLock {
    /// Compressed primary public key or HTLC hash.
    pub data: String,
    /// Ordered NUT-10 tags, including any additional and refund keys.
    pub tags: Vec<Vec<String>>,
    /// Whether data is an HTLC hash.
    pub htlc: bool,
    /// Blind signing keys using NUT-28.
    pub blind_keys: bool,
}

/// Create random outputs in denomination order in one native call.
#[uniffi::export]
pub fn create_random_outputs(
    amounts: Vec<u64>,
    keyset_id: String,
) -> Result<Vec<NativeOutput>, CryptoError> {
    amounts
        .into_iter()
        .map(|amount| create_random_output(amount, keyset_id.clone()))
        .collect()
}

/// Create consecutive NUT-13 outputs, copying the seed across FFI only once.
#[uniffi::export]
pub fn create_deterministic_outputs(
    amounts: Vec<u64>,
    seed: Vec<u8>,
    counter: u64,
    keyset_id: String,
) -> Result<Vec<NativeOutput>, CryptoError> {
    if let Some(last) = amounts.len().checked_sub(1) {
        let last = counter
            .checked_add(last as u64)
            .ok_or_else(|| invalid("counter range"))?;
        // Reject an invalid range before computing any outputs.
        let max = if keyset_id.starts_with("00") {
            (1_u64 << 31) - 1
        } else {
            (1_u64 << 53) - 1
        };
        if last > max {
            return Err(invalid("counter range"));
        }
    }
    amounts
        .into_iter()
        .enumerate()
        .map(|(i, amount)| {
            let (secret, r) = super::derivation::derive(&seed, counter + i as u64, &keyset_id)?;
            output(amount, keyset_id.clone(), secret, Some(r), None)
        })
        .collect()
}

/// Create blank NUT-09 restore outputs for consecutive counters.
///
/// Each synchronous call accepts at most 10,000 outputs; callers chunk larger scans.
#[uniffi::export]
pub fn create_restore_outputs(
    seed: Vec<u8>,
    keyset_id: String,
    counter: u64,
    count: u32,
) -> Result<Vec<NativeOutput>, CryptoError> {
    if count > 10_000 {
        return Err(invalid("restore batch size"));
    }
    create_deterministic_outputs(vec![0; count as usize], seed, counter, keyset_id)
}

/// Create locked outputs, sharing a NUT-28 ephemeral key only for SIG_ALL.
/// An explicitly supplied ephemeral secret is shared, matching the single-output API.
#[uniffi::export]
pub fn create_locked_outputs(
    amounts: Vec<u64>,
    keyset_id: String,
    locking: NativeLock,
    ephemeral_secret: Option<Vec<u8>>,
) -> Result<Vec<NativeOutput>, CryptoError> {
    let shared = if locking.blind_keys
        && locking.tags.iter().any(|t| {
            t.first().map(String::as_str) == Some("sigflag")
                && t.get(1).map(String::as_str) == Some("SIG_ALL")
        }) {
        Some(ephemeral_secret.unwrap_or_else(random_secret_key))
    } else {
        ephemeral_secret
    };
    amounts
        .into_iter()
        .map(|amount| {
            let mut data = locking.data.clone();
            let mut tags = locking.tags.clone();
            let mut ephemeral_e = None;
            if locking.blind_keys {
                let mut keys = if locking.htlc {
                    vec![]
                } else {
                    vec![data.clone()]
                };
                // Canonical NUT-28 slot order: main key, additional keys, refund keys.
                for name in ["pubkeys", "refund"] {
                    for tag in &tags {
                        if tag.first().map(String::as_str) == Some(name) {
                            keys.extend_from_slice(&tag[1..]);
                        }
                    }
                }
                let blinded = blind_locking_keys(keys, shared.clone(), locking.htlc)?;
                let mut keys = blinded.keys.into_iter();
                if !locking.htlc {
                    data = keys.next().ok_or_else(|| invalid("locking public key"))?;
                }
                for name in ["pubkeys", "refund"] {
                    for tag in &mut tags {
                        if tag.first().map(String::as_str) == Some(name) {
                            for key in &mut tag[1..] {
                                *key = keys.next().ok_or_else(|| invalid("locking key count"))?;
                            }
                        }
                    }
                }
                ephemeral_e = blinded.ephemeral_e;
            }
            create_locked_output(
                amount,
                keyset_id.clone(),
                data,
                tags,
                locking.htlc,
                ephemeral_e,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_overflow_and_oversized_restore_before_work() {
        let id = "009a1f293253e41e".to_owned();
        assert!(
            create_deterministic_outputs(vec![1, 2], vec![3; 32], u64::MAX, id.clone()).is_err()
        );
        assert!(create_restore_outputs(vec![3; 32], id, 0, 10_001).is_err());
    }
}
