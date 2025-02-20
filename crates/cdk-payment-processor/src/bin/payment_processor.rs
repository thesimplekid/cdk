use std::collections::{HashMap, HashSet};
use std::env;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::bail;
use cdk_common::common::FeeReserve;
use cdk_common::payment::{self, MintPayment};
use cdk_common::Amount;
use cdk_fake_wallet::FakeWallet;
use serde::{Deserialize, Serialize};
use tokio::signal;
use tracing_subscriber::EnvFilter;

pub const ENV_LN_BACKEND: &str = "CDK_PAYMENT_PROCESSOR_LN_BACKEND";
pub const ENV_LISTEN_HOST: &str = "CDK_PAYMENT_PROCESSOR_LISTEN_HOST";
pub const ENV_LISTEN_PORT: &str = "CDK_PAYMENT_PROCESSOR_LISTEN_PORT";
pub const ENV_PAYMENT_PROCESSOR_TLS_DIR: &str = "CDK_PAYMENT_PROCESSOR_TLS_DIR";

// CLN
pub const ENV_CLN_RPC_PATH: &str = "CDK_PAYMENT_PROCESSOR_CLN_RPC_PATH";
pub const ENV_CLN_BOLT12: &str = "CDK_PAYMENT_PROCESSOR_CLN_BOLT12";
pub const ENV_CLN_FEE_PERCENT: &str = "CDK_PAYMENT_PROCESSOR_CLN_FEE_PERCENT";
pub const ENV_CLN_RESERVE_FEE_MIN: &str = "CDK_PAYMENT_PROCESSOR_CLN_RESERVE_FEE_MIN";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let default_filter = "debug";

    let sqlx_filter = "sqlx=warn";
    let hyper_filter = "hyper=warn";
    let h2_filter = "h2=warn";
    let rustls_filter = "rustls=warn";

    let env_filter = EnvFilter::new(format!(
        "{},{},{},{},{}",
        default_filter, sqlx_filter, hyper_filter, h2_filter, rustls_filter
    ));

    tracing_subscriber::fmt().with_env_filter(env_filter).init();

    let ln_backend: String = env::var(ENV_LN_BACKEND)?;
    let listen_addr: String = env::var(ENV_LISTEN_HOST)?;
    let listen_port: u16 = env::var(ENV_LISTEN_PORT)?.parse()?;
    let tls_dir: Option<PathBuf> = env::var(ENV_PAYMENT_PROCESSOR_TLS_DIR)
        .ok()
        .map(|p| PathBuf::from(p));

    let ln_backed: Arc<dyn MintPayment<Err = payment::Error> + Send + Sync> =
        match ln_backend.as_str() {
            #[cfg(feature = "cln")]
            "CLN" => {
                let cln_settings = Cln::default().from_env();
                let fee_reserve = FeeReserve {
                    min_fee_reserve: cln_settings.reserve_fee_min,
                    percent_fee_reserve: cln_settings.fee_percent,
                };

                println!("{}", cln_settings.rpc_path.display());

                Arc::new(
                    cdk_cln::Cln::new(cln_settings.rpc_path, fee_reserve)
                        .await
                        .expect("Could not create cln"),
                )
            }
            "fakewallet" => {
                let fee_reserve = FeeReserve {
                    min_fee_reserve: 1.into(),
                    percent_fee_reserve: 0.0,
                };

                let fake_wallet =
                    FakeWallet::new(fee_reserve, HashMap::default(), HashSet::default(), 0);

                Arc::new(fake_wallet)
            }
            _ => {
                bail!("Unknown payment processor");
            }
        };

    let mut server =
        cdk_payment_processor::PaymentProcessorServer::new(ln_backed, &listen_addr, listen_port)?;

    server.start(tls_dir).await?;

    // Wait for shutdown signal
    signal::ctrl_c().await?;

    server.stop().await?;

    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Cln {
    pub rpc_path: PathBuf,
    #[serde(default)]
    pub bolt12: bool,
    pub fee_percent: f32,
    pub reserve_fee_min: Amount,
}

impl Cln {
    pub fn from_env(mut self) -> Self {
        // RPC Path
        if let Ok(path) = env::var(ENV_CLN_RPC_PATH) {
            self.rpc_path = PathBuf::from(path);
        }

        // BOLT12 flag
        if let Ok(bolt12_str) = env::var(ENV_CLN_BOLT12) {
            if let Ok(bolt12) = bolt12_str.parse() {
                self.bolt12 = bolt12;
            }
        }

        // Fee percent
        if let Ok(fee_str) = env::var(ENV_CLN_FEE_PERCENT) {
            if let Ok(fee) = fee_str.parse() {
                self.fee_percent = fee;
            }
        }

        // Reserve fee minimum
        if let Ok(reserve_fee_str) = env::var(ENV_CLN_RESERVE_FEE_MIN) {
            if let Ok(reserve_fee) = reserve_fee_str.parse::<u64>() {
                self.reserve_fee_min = reserve_fee.into();
            }
        }

        self
    }
}
