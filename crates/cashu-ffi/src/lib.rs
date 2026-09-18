//! UniFFI bindings for synchronous Cashu protocol cryptography.
//!
//! This crate deliberately depends on Cashu protocol primitives rather than the
//! CDK wallet, storage backends or an async runtime. Amounts cross UniFFI as u64,
//! scalars as hex, and secrets as bytes; no JavaScript number conversion is needed.

use std::fmt;
use std::str::FromStr;

use cashu::dhke::{blind_message, unblind_message};
use cashu::nuts::{BlindSignature, BlindSignatureDleq, Id, PublicKey, SecretKey};
use cashu::secret::Secret;
use cashu::util::hex;
use serde::{Deserialize, Serialize};

mod batch;
mod derivation;

pub use self::batch::{
    create_deterministic_outputs, create_locked_outputs, create_random_outputs,
    create_restore_outputs, NativeLock,
};

uniffi::setup_scaffolding!();

/// Invalid inputs and failed cryptographic checks. Messages never contain secrets.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum CryptoError {
    /// An input failed validation.
    #[error("Invalid {field}")]
    InvalidInput {
        /// Name of the invalid field, without its value.
        field: String,
    },
    /// The mint response does not belong to the requested output.
    #[error("Mint signature does not match output")]
    SignatureMismatch,
    /// A supplied DLEQ proof failed verification.
    #[error("Mint signature DLEQ verification failed")]
    InvalidDleq,
}

fn invalid(field: &str) -> CryptoError {
    CryptoError::InvalidInput {
        field: field.to_owned(),
    }
}

/// Portable output material. Treat this record as wallet secrets.
#[derive(Clone, Serialize, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct NativeOutput {
    /// Requested denomination; zero denotes a blank change/restore output.
    pub amount: u64,
    /// Full hex keyset ID (00 or 01).
    pub keyset_id: String,
    /// Compressed blinded message.
    pub blinded_message: String,
    /// Secret bytes, containing UTF-8 text.
    pub secret: Vec<u8>,
    /// Nonzero secp256k1 scalar, encoded as 32-byte hex.
    pub blinding_factor: String,
    /// Public P2BK ephemeral key, if applicable.
    pub ephemeral_e: Option<String>,
}

impl fmt::Debug for NativeOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeOutput")
            .field("material", &"[REDACTED]")
            .finish()
    }
}

/// Mint-supplied DLEQ scalars.
#[derive(Debug, Clone, Serialize, Deserialize, uniffi::Record)]
pub struct NativeDleq {
    /// Challenge scalar, hex encoded.
    pub e: String,
    /// Response scalar, hex encoded.
    pub s: String,
}

/// Mint response for one output.
#[derive(Debug, Clone, Serialize, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct NativeSignature {
    /// Signed denomination.
    pub amount: u64,
    /// Signing keyset ID.
    pub keyset_id: String,
    /// Compressed blinded signature.
    pub blinded_signature: String,
    /// Optional DLEQ proof, always verified when supplied.
    pub dleq: Option<NativeDleq>,
}

fn output(
    amount: u64,
    keyset_id: String,
    secret: Vec<u8>,
    r: Option<SecretKey>,
    ephemeral_e: Option<String>,
) -> Result<NativeOutput, CryptoError> {
    Id::from_str(&keyset_id).map_err(|_| invalid("keyset ID (expected full 00/01 hex ID)"))?;
    let text = std::str::from_utf8(&secret).map_err(|_| invalid("UTF-8 secret"))?;
    if text.is_empty() || text.chars().count() > 1024 {
        return Err(invalid("secret length"));
    }
    let (b, r) = blind_message(&secret, r).map_err(|_| invalid("blinded message"))?;
    Ok(NativeOutput {
        amount,
        keyset_id,
        blinded_message: b.to_string(),
        secret,
        blinding_factor: r.to_secret_hex(),
        ephemeral_e,
    })
}

/// Generate a random output using the operating system CSPRNG.
#[uniffi::export]
pub fn create_random_output(amount: u64, keyset_id: String) -> Result<NativeOutput, CryptoError> {
    output(amount, keyset_id, Secret::generate().to_bytes(), None, None)
}

/// Generate a NUT-13 output, accepting cashu-ts's 16–64 byte seeds and counters.
#[uniffi::export]
pub fn create_deterministic_output(
    amount: u64,
    seed: Vec<u8>,
    counter: u64,
    keyset_id: String,
) -> Result<NativeOutput, CryptoError> {
    let (secret, r) = self::derivation::derive(&seed, counter, &keyset_id)?;
    output(amount, keyset_id, secret, Some(r), None)
}

/// Generate secret key bytes for a shared SIG_ALL P2BK ephemeral key.
#[uniffi::export]
pub fn random_secret_key() -> Vec<u8> {
    SecretKey::generate().to_secret_bytes().to_vec()
}

/// Blinded locking keys and the corresponding public ephemeral key.
#[derive(Debug, Clone, Serialize, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct BlindedKeys {
    /// Blinded public keys in canonical slot order.
    pub keys: Vec<String>,
    /// Compressed public ephemeral key (absent for no signing keys).
    pub ephemeral_e: Option<String>,
}

/// Validate compressed or x-only locking keys and return compressed hex keys.
#[uniffi::export]
pub fn normalize_public_keys(keys: Vec<String>) -> Result<Vec<String>, CryptoError> {
    keys.iter()
        .map(|key| {
            let key = if key.len() == 64 {
                format!("02{key}")
            } else {
                key.clone()
            };
            if key.len() != 66 {
                return Err(invalid("locking public key"));
            }
            PublicKey::from_str(&key)
                .map(|p| p.to_string())
                .map_err(|_| invalid("locking public key"))
        })
        .collect()
}

/// Blind NUT-28 locking keys, reserving slot zero for an HTLC hashlock.
#[uniffi::export]
pub fn blind_locking_keys(
    keys: Vec<String>,
    ephemeral_secret: Option<Vec<u8>>,
    htlc: bool,
) -> Result<BlindedKeys, CryptoError> {
    let start = usize::from(htlc);
    if keys.len() + start > 11 {
        return Err(invalid("locking key count"));
    }
    if keys.is_empty() {
        return Ok(BlindedKeys {
            keys: vec![],
            ephemeral_e: None,
        });
    }
    let e = match ephemeral_secret {
        Some(bytes) => SecretKey::from_slice(&bytes).map_err(|_| invalid("ephemeral secret"))?,
        None => SecretKey::generate(),
    };
    let keys = keys
        .iter()
        .enumerate()
        .map(|(i, key)| {
            let key = PublicKey::from_str(key).map_err(|_| invalid("locking public key"))?;
            let r = cashu::nuts::nut28::ecdh_kdf(&e, &key, (start + i) as u8)
                .map_err(|_| invalid("locking key derivation"))?;
            cashu::nuts::nut28::blind_public_key(&key, &r)
                .map(|p| p.to_string())
                .map_err(|_| invalid("blinded locking key"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(BlindedKeys {
        keys,
        ephemeral_e: Some(e.public_key().to_string()),
    })
}

/// Construct and blind a NUT-10 secret from normalized cashu-ts locking data.
#[uniffi::export]
pub fn create_locked_output(
    amount: u64,
    keyset_id: String,
    data: String,
    tags: Vec<Vec<String>>,
    htlc: bool,
    ephemeral_e: Option<String>,
) -> Result<NativeOutput, CryptoError> {
    if htlc {
        if hex::decode(&data).map_err(|_| invalid("hashlock"))?.len() != 32 {
            return Err(invalid("hashlock"));
        }
    } else {
        PublicKey::from_str(&data).map_err(|_| invalid("locking public key"))?;
    }
    if let Some(e) = &ephemeral_e {
        PublicKey::from_str(e).map_err(|_| invalid("ephemeral public key"))?;
    }
    // Preserve the property order used by cashu-ts when encoding NUT-10 secrets.
    #[derive(Serialize)]
    struct Data<'a> {
        nonce: String,
        data: &'a str,
        tags: Vec<Vec<String>>,
    }
    let data = Data {
        nonce: Secret::generate().to_string(),
        data: &data,
        tags,
    };
    let secret = serde_json::to_vec(&(if htlc { "HTLC" } else { "P2PK" }, data))
        .map_err(|_| invalid("locking secret"))?;
    output(amount, keyset_id, secret, None, ephemeral_e)
}

/// Verify the response binding and DLEQ (when present), then unblind in Rust.
///
/// The caller selects the mint key for `signature.amount` from the matching
/// keyset. Missing DLEQ is accepted, matching cashu-ts/NUT-12 behavior.
#[uniffi::export]
pub fn unblind_output(
    output: NativeOutput,
    signature: NativeSignature,
    mint_key: String,
) -> Result<String, CryptoError> {
    if output.keyset_id != signature.keyset_id
        || (output.amount != 0 && output.amount != signature.amount)
    {
        return Err(CryptoError::SignatureMismatch);
    }
    let id = Id::from_str(&output.keyset_id).map_err(|_| invalid("keyset ID"))?;
    let r = SecretKey::from_hex(&output.blinding_factor).map_err(|_| invalid("blinding factor"))?;
    let (b, _) =
        blind_message(&output.secret, Some(r.clone())).map_err(|_| invalid("blinded message"))?;
    let stored_b =
        PublicKey::from_str(&output.blinded_message).map_err(|_| invalid("blinded message"))?;
    if b != stored_b {
        return Err(invalid("output secret/blinding factor binding"));
    }
    let key = PublicKey::from_str(&mint_key).map_err(|_| invalid("mint public key"))?;
    let c = PublicKey::from_str(&signature.blinded_signature)
        .map_err(|_| invalid("blinded signature"))?;
    let dleq = signature
        .dleq
        .map(|d| {
            Ok::<_, CryptoError>(BlindSignatureDleq {
                e: SecretKey::from_hex(d.e).map_err(|_| invalid("DLEQ challenge"))?,
                s: SecretKey::from_hex(d.s).map_err(|_| invalid("DLEQ response"))?,
            })
        })
        .transpose()?;
    let sig = BlindSignature {
        amount: signature.amount.into(),
        keyset_id: id,
        c,
        dleq,
    };
    if sig.dleq.is_some() {
        sig.verify_dleq(key, b)
            .map_err(|_| CryptoError::InvalidDleq)?;
    }
    unblind_message(&c, &r, &key)
        .map(|p| p.to_string())
        .map_err(|_| invalid("unblinded signature"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "009a1f293253e41e";

    fn signed(output: &NativeOutput) -> (NativeSignature, String, String) {
        let key = SecretKey::generate();
        let b = PublicKey::from_str(&output.blinded_message).unwrap();
        let c = cashu::dhke::sign_message(&key, &b).unwrap();
        let signature = BlindSignature::new(8.into(), c, ID.parse().unwrap(), &b, &key).unwrap();
        let dleq = signature.dleq.unwrap();
        let y = cashu::dhke::hash_to_curve(&output.secret).unwrap();
        let expected = cashu::dhke::sign_message(&key, &y).unwrap();
        (
            NativeSignature {
                amount: 8,
                keyset_id: ID.to_owned(),
                blinded_signature: c.to_string(),
                dleq: Some(NativeDleq {
                    e: dleq.e.to_secret_hex(),
                    s: dleq.s.to_secret_hex(),
                }),
            },
            key.public_key().to_string(),
            expected.to_string(),
        )
    }

    #[test]
    fn unblinds_and_checks_response_binding() {
        let output = create_random_output(8, ID.to_owned()).unwrap();
        let (sig, key, expected) = signed(&output);
        assert_eq!(
            unblind_output(output.clone(), sig.clone(), key.clone()).unwrap(),
            expected
        );
        let mut bad = sig.clone();
        bad.amount = 4;
        assert!(matches!(
            unblind_output(output.clone(), bad, key.clone()),
            Err(CryptoError::SignatureMismatch)
        ));
        let mut bad = sig.clone();
        bad.keyset_id = "0011111111111111".to_owned();
        assert!(unblind_output(output.clone(), bad, key.clone()).is_err());
        let mut tampered = output.clone();
        tampered.secret[0] ^= 1;
        assert!(unblind_output(tampered, sig.clone(), key.clone()).is_err());
        let mut bad = sig.clone();
        bad.dleq.as_mut().unwrap().s = SecretKey::generate().to_secret_hex();
        assert!(matches!(
            unblind_output(output.clone(), bad, key.clone()),
            Err(CryptoError::InvalidDleq)
        ));
        assert!(unblind_output(
            output.clone(),
            sig.clone(),
            SecretKey::generate().public_key().to_string()
        )
        .is_err());
        let mut no_dleq = sig;
        no_dleq.dleq = None;
        assert_eq!(unblind_output(output, no_dleq, key).unwrap(), expected);
    }

    #[test]
    fn blank_outputs_accept_the_signed_denomination() {
        let output = create_random_output(0, ID.to_owned()).unwrap();
        let (sig, key, expected) = signed(&output);
        assert_eq!(unblind_output(output, sig, key).unwrap(), expected);
    }

    #[test]
    fn deterministic_outputs_match_cdk_and_validate_inputs() {
        for id in [ID.to_owned(), format!("01{}", "ab".repeat(32))] {
            let seed = [42; 64];
            let output = create_deterministic_output(1, seed.to_vec(), 12, id.clone()).unwrap();
            let keyset: Id = id.parse().unwrap();
            assert_eq!(
                output.secret,
                Secret::from_seed(&seed, keyset, 12).unwrap().to_bytes()
            );
            assert_eq!(
                output.blinding_factor,
                SecretKey::from_seed(&seed, keyset, 12)
                    .unwrap()
                    .to_secret_hex()
            );
        }
        assert!(create_deterministic_output(1, vec![1; 15], 0, ID.to_owned()).is_err());
        assert!(create_deterministic_output(1, vec![1; 65], 0, ID.to_owned()).is_err());
        assert!(create_deterministic_output(1, vec![1; 32], 1 << 31, ID.to_owned()).is_err());
        assert!(create_random_output(1, format!("02{}", "ab".repeat(32))).is_err());
        let out = create_random_output(1, ID.to_owned()).unwrap();
        let debug = format!("{out:?}");
        assert!(!debug.contains(&out.blinding_factor));
        assert!(!debug.contains(std::str::from_utf8(&out.secret).unwrap()));
    }
}
