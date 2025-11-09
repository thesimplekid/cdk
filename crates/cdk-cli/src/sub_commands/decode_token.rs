use std::collections::HashMap;
use std::str::FromStr;

use anyhow::Result;
use cdk::fees::calculate_fee;
use cdk::nuts::{ProofsMethods, Token};
use cdk::util::serialize_to_cbor_diag;
use cdk::wallet::MintConnector;
use cdk::HttpClient;
use clap::Args;

#[derive(Args)]
pub struct DecodeTokenSubCommand {
    /// Cashu Token
    token: String,
    /// Show detailed token info
    #[arg(short, long)]
    info: bool,
}

pub async fn decode_token(sub_command_args: &DecodeTokenSubCommand) -> Result<()> {
    let token = Token::from_str(&sub_command_args.token)?;

    if sub_command_args.info {
        // Display token info
        println!("=== Token Information ===\n");

        // Mint URL
        let mint_url = token.mint_url()?;
        println!("Mint URL: {}", mint_url);

        // Unit
        if let Some(unit) = token.unit() {
            println!("Unit: {}", unit);
        }

        // Get proofs to count them and check DLEQ
        let client = HttpClient::new(mint_url.clone(), None);
        let keysets_response = client.get_mint_keysets().await?;
        let proofs = token.proofs(&keysets_response.keysets)?;

        // Number of proofs
        println!("Number of proofs: {}", proofs.len());

        // Total value
        let total_amount = proofs.total_amount()?;
        println!(
            "Total amount: {} {}",
            total_amount,
            token.unit().unwrap_or_default()
        );

        // Calculate fees to redeem using CDK's calculate_fee function
        let proofs_count = proofs.iter().fold(HashMap::new(), |mut acc, proof| {
            *acc.entry(proof.keyset_id).or_insert(0) += 1;
            acc
        });

        let keyset_fees =
            keysets_response
                .keysets
                .iter()
                .fold(HashMap::new(), |mut acc, keyset| {
                    acc.insert(keyset.id, keyset.input_fee_ppk);
                    acc
                });

        let total_fee = calculate_fee(&proofs_count, &keyset_fees)?;
        println!(
            "Fee to redeem: {} {}",
            total_fee,
            token.unit().unwrap_or_default()
        );

        // Verify DLEQ proofs
        // Get unique keyset IDs and fetch keysets
        let keyset_ids = proofs.keyset_ids();
        let mut keysets = HashMap::new();
        for keyset_id in keyset_ids {
            match client.get_mint_keyset(keyset_id).await {
                Ok(keyset) => {
                    keysets.insert(keyset_id, keyset);
                }
                Err(e) => {
                    eprintln!("Warning: Could not fetch keyset {}: {}", keyset_id, e);
                }
            }
        }

        // Verify all DLEQs
        match proofs.verify_dleqs(&keysets) {
            Ok(()) => {
                println!("DLEQ: ✓ All {} proofs verified successfully", proofs.len());
            }
            Err(e) => {
                println!("DLEQ: ✗ Verification failed - {}", e);
            }
        }
    } else {
        // Only print raw data if --info flag is not used
        println!("{}", serialize_to_cbor_diag(&token)?);
    }

    Ok(())
}
