use bitcoin::bip32::{ChildNumber, DerivationPath, Xpriv};
use bitcoin::hashes::{hmac, sha256, Hash, HashEngine, HmacEngine};
use bitcoin::Network;
use cashu::nuts::{Id, SecretKey};
use cashu::util::hex;

use super::{invalid, CryptoError};

// cashu's wallet helper accepts only 64-byte seeds and u32 counters. Preserve
// cashu-ts's broader input domain at this boundary without changing wallet APIs.
pub(super) fn derive(
    seed: &[u8],
    counter: u64,
    keyset: &str,
) -> Result<(Vec<u8>, SecretKey), CryptoError> {
    if !(16..=64).contains(&seed.len()) {
        return Err(invalid("seed length (16–64 bytes)"));
    }
    if counter > (1_u64 << 53) - 1 {
        return Err(invalid("counter"));
    }
    let id: Id = keyset.parse().map_err(|_| invalid("keyset ID"))?;
    if keyset.starts_with("00") {
        let counter = u32::try_from(counter).map_err(|_| invalid("BIP32 counter"))?;
        let mut children = Vec::new();
        for index in [129372, 0, u32::from(id), counter] {
            children
                .push(ChildNumber::from_hardened_idx(index).map_err(|_| invalid("BIP32 counter"))?);
        }
        let parent = Xpriv::new_master(Network::Bitcoin, seed)
            .and_then(|x| x.derive_priv(&cashu::SECP256K1, &DerivationPath::from(children)))
            .map_err(|_| invalid("BIP32 derivation"))?;
        let derive_child = |index| {
            parent
                .derive_priv(&cashu::SECP256K1, &[ChildNumber::Normal { index }])
                .map(|x| x.private_key)
                .map_err(|_| invalid("BIP32 child"))
        };
        return Ok((
            hex::encode(derive_child(0)?.secret_bytes()).into_bytes(),
            derive_child(1)?.into(),
        ));
    }
    let derive = |suffix| {
        let mut engine = HmacEngine::<sha256::Hash>::new(seed);
        engine.input(b"Cashu_KDF_HMAC_SHA256");
        engine.input(&id.to_bytes());
        engine.input(&counter.to_be_bytes());
        engine.input(&[suffix]);
        hmac::Hmac::<sha256::Hash>::from_engine(engine).to_byte_array()
    };
    Ok((
        hex::encode(derive(0)).into_bytes(),
        reduce_blinding_factor(derive(1))?,
    ))
}

fn reduce_blinding_factor(mut r: [u8; 32]) -> Result<SecretKey, CryptoError> {
    // cashu-ts reduces the HMAC output modulo the curve order (at most once).
    let order = bitcoin::secp256k1::constants::CURVE_ORDER;
    if r >= order {
        let mut borrow = 0_i16;
        for i in (0..32).rev() {
            let v = i16::from(r[i]) - i16::from(order[i]) - borrow;
            r[i] = v as u8;
            borrow = i16::from(v < 0);
        }
    }
    SecretKey::from_slice(&r).map_err(|_| invalid("derived blinding factor"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reduction_handles_curve_order_boundaries() {
        let order = bitcoin::secp256k1::constants::CURVE_ORDER;
        assert!(reduce_blinding_factor([0; 32]).is_err());
        assert!(reduce_blinding_factor(order).is_err());
        let mut next = order;
        next[31] += 1;
        assert_eq!(
            reduce_blinding_factor(next).unwrap().to_secret_hex(),
            format!("{:064x}", 1)
        );
        assert_eq!(
            reduce_blinding_factor([255; 32]).unwrap().to_secret_hex(),
            "000000000000000000000000000000014551231950b75fc4402da1732fc9bebe"
        );
    }
}
