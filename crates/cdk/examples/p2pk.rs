use std::str::FromStr;
use std::sync::Arc;

use cdk::amount::SplitTarget;
use cdk::error::Error;
use cdk::nuts::{
    CurrencyUnit, HTLCCondition, Id, MintQuoteState, NotificationPayload, P2PKCondition, SecretKey, 
    SpendingCondition,
};
use cdk::wallet::{SendOptions, Wallet, WalletSubscription};
use cdk::Amount;
use cdk_sqlite::wallet::memory;
use rand::Rng;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Error> {
    let default_filter = "debug";

    let sqlx_filter = "sqlx=warn,hyper_util=warn,reqwest=warn,rustls=warn";

    let env_filter = EnvFilter::new(format!("{},{}", default_filter, sqlx_filter));

    // Parse input
    tracing_subscriber::fmt().with_env_filter(env_filter).init();

    // Initialize the memory store for the wallet
    let localstore = memory::empty().await?;

    // Generate a random seed for the wallet
    let seed = rand::thread_rng().gen::<[u8; 32]>();

    // Define the mint URL and currency unit
    let mint_url = "https://testnut.cashu.space";
    let unit = CurrencyUnit::Sat;
    let amount = Amount::from(100);

    // Create a new wallet
    let wallet = Wallet::new(mint_url, unit, Arc::new(localstore), &seed, Some(1))?;

    // Request a mint quote from the wallet
    let quote = wallet.mint_quote(amount, None).await?;

    println!("Minting nuts ...");

    // Subscribe to updates on the mint quote state
    let mut subscription = wallet
        .subscribe(WalletSubscription::Bolt11MintQuoteState(vec![quote
            .id
            .clone()]))
        .await;

    // Wait for the mint quote to be paid
    while let Some(msg) = subscription.recv().await {
        if let NotificationPayload::MintQuoteBolt11Response(response) = msg {
            if response.state == MintQuoteState::Paid {
                break;
            }
        }
    }

    // Mint the received amount
    let received_proofs = wallet.mint(&quote.id, SplitTarget::default(), None).await?;
    println!(
        "Minted nuts: {:?}",
        received_proofs
            .into_iter()
            .map(|p| p.amount)
            .collect::<Vec<_>>()
    );

    // Generate a secret key for spending conditions
    let secret = SecretKey::generate();

    // Create spending conditions using the generated public key (trait-based approach)
    let p2pk_condition = P2PKCondition::new(secret.public_key(), None);
    
    // Use the trait-based condition directly
    let condition: Box<dyn SpendingCondition> = Box::new(p2pk_condition.clone());

    // Get the total balance of the wallet
    let bal = wallet.total_balance().await?;
    println!("Total balance: {}", bal);

    // Send a token with the specified amount and spending conditions
    let prepared_send = wallet
        .prepare_send(
            10.into(),
            SendOptions {
                conditions: Some(condition),
                include_fee: true,
                ..Default::default()
            },
        )
        .await?;
    println!("Fee: {}", prepared_send.fee());
    let token = wallet.send(prepared_send, None).await?;

    println!("Created token locked to pubkey: {}", secret.public_key());
    println!("{}", token);

    // Receive the token using the secret key
    let amount = wallet
        .receive(&token.to_string(), SplitTarget::default(), &[secret], &[])
        .await?;

    println!("Redeemed locked token worth: {}", u64::from(amount));
    
    // Example of using the trait for generic programming
    println!("\nExample of trait-based usage:");
    print_condition_info(&p2pk_condition);
    
    // Create an HTLC condition
    if let Ok(htlc_condition) = cdk::nuts::HTLCCondition::new("deadbeef".into(), None) {
        print_condition_info(&htlc_condition);
    }
    
    // Example of dynamic dispatch with Box<dyn SpendingCondition>
    println!("\nExample of dynamic dispatch with trait objects:");
    let conditions: Vec<Box<dyn SpendingCondition>> = vec![
        Box::new(p2pk_condition.clone()), // P2PK condition created earlier
        // Create a new HTLC condition directly
        Box::new(HTLCCondition::new("deadbeef".into(), None).unwrap()),
    ];
    
    // Example of using PreMintSecrets::with_conditions with trait objects
    println!("\nExample of using with_conditions with the trait:");
    // The method now takes any type that implements SpendingCondition
    if let Ok(pre_mint) = cdk::nuts::PreMintSecrets::with_conditions(
        cdk::nuts::Id::from_str("deadbeef").unwrap(), 
        Amount::from(1000),
        &cdk::amount::SplitTarget::default(),
        &p2pk_condition // Pass the condition directly, no need for enum conversion
    ) {
        println!("Successfully created pre-mint secrets with trait object");
        println!("Number of pre-mint entries: {}", pre_mint.len());
    }
    
    for (i, condition) in conditions.iter().enumerate() {
        println!("Condition #{}", i + 1);
        println!("Kind: {:?}", condition.kind());
        println!("Num sigs: {:?}", condition.num_sigs());
        println!();
    }

    Ok(())
}

// This function can work with any type that implements SpendingCondition
fn print_condition_info<T: SpendingCondition + std::fmt::Debug>(condition: &T) {
    println!("Condition type: {:?}", condition.kind());
    println!("Required signatures: {:?}", condition.num_sigs());
    if let Some(pubkeys) = condition.pubkeys() {
        println!("Associated public keys: {}", pubkeys.len());
    }
    if let Some(locktime) = condition.locktime() {
        println!("Locktime: {}", locktime);
    }
    println!();
}
