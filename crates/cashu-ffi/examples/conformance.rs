//! Host-side JSON transport used only by the TypeScript conformance suite.
use std::io::{self, Read};

use cashu_ffi::*;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
#[serde(tag = "method", content = "args", rename_all = "camelCase")]
enum Request {
    CreateRandomOutput(u64, String),
    CreateRandomOutputs(Vec<u64>, String),
    CreateDeterministicOutputs(Vec<u64>, Vec<u8>, u64, String),
    CreateRestoreOutputs(Vec<u8>, String, u64, u32),
    CreateLockedOutputs(Vec<u64>, String, NativeLock, Option<Vec<u8>>),
    CreateDeterministicOutput(u64, Vec<u8>, u64, String),
    CreateLockedOutput(u64, String, String, Vec<Vec<String>>, bool, Option<String>),
    RandomSecretKey,
    NormalizePublicKeys(Vec<String>),
    BlindLockingKeys(Vec<String>, Option<Vec<u8>>, bool),
    UnblindOutput(NativeOutput, NativeSignature, String),
}

fn run(request: Request) -> Result<Value, Box<dyn std::error::Error>> {
    let result = match request {
        Request::CreateRandomOutputs(a, id) => json!(create_random_outputs(a, id)?),
        Request::CreateDeterministicOutputs(a, seed, counter, id) => {
            json!(create_deterministic_outputs(a, seed, counter, id)?)
        }
        Request::CreateRestoreOutputs(seed, id, counter, count) => {
            json!(create_restore_outputs(seed, id, counter, count)?)
        }
        Request::CreateLockedOutputs(a, id, lock, e) => {
            json!(create_locked_outputs(a, id, lock, e)?)
        }
        Request::CreateRandomOutput(a, id) => json!(create_random_output(a, id)?),
        Request::CreateDeterministicOutput(a, seed, counter, id) => {
            json!(create_deterministic_output(a, seed, counter, id)?)
        }
        Request::CreateLockedOutput(a, id, data, tags, htlc, e) => {
            json!(create_locked_output(a, id, data, tags, htlc, e)?)
        }
        Request::NormalizePublicKeys(keys) => json!(normalize_public_keys(keys)?),
        Request::RandomSecretKey => json!(random_secret_key()),
        Request::BlindLockingKeys(keys, e, htlc) => json!(blind_locking_keys(keys, e, htlc)?),
        Request::UnblindOutput(output, signature, key) => {
            json!(unblind_output(output, signature, key)?)
        }
    };
    Ok(result)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    match run(serde_json::from_str(&input)?) {
        Ok(value) => println!("{value}"),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
    Ok(())
}
