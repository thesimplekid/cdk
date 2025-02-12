//! CDK Fake LN Backend
//!
//! Used for testing where quotes are auto filled

#![warn(missing_docs)]
#![warn(rustdoc::bare_urls)]

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use bitcoin::hashes::{sha256, Hash};
use bitcoin::secp256k1::rand::{thread_rng, Rng};
use bitcoin::secp256k1::{Secp256k1, SecretKey};
use cdk::amount::{Amount, MSAT_IN_SAT};
use cdk::cdk_lightning::PayInvoiceResponse;
use cdk::mint::FeeReserve;
use cdk::nuts::{CurrencyUnit, MeltQuoteBolt11Request, MeltQuoteState};
use cdk::proto::cdk_payment_processor_server::{CdkPaymentProcessor, CdkPaymentProcessorServer};
use cdk::proto::*;
use cdk::tonic::transport::{Certificate, Identity, Server, ServerTlsConfig};
use cdk::tonic::{async_trait, Request, Response, Status};
use cdk::util::unix_time;
use lightning_invoice::{Bolt11Invoice, Currency, InvoiceBuilder, PaymentSecret};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, Notify};
use tokio::task::JoinHandle;
use tokio::time;
use tokio_util::sync::CancellationToken;

pub mod error;

/// Fake Wallet
#[derive(Clone)]
pub struct FakeWallet {
    fee_reserve: FeeReserve,
    sender: tokio::sync::mpsc::Sender<String>,
    _receiver: Arc<Mutex<Option<tokio::sync::mpsc::Receiver<String>>>>,
    payment_states: Arc<Mutex<HashMap<String, MeltQuoteState>>>,
    failed_payment_check: Arc<Mutex<HashSet<String>>>,
    payment_delay: u64,
    _wait_invoice_cancel_token: CancellationToken,
    _wait_invoice_is_active: Arc<AtomicBool>,
    socket_addr: SocketAddr,
    shutdown: Arc<Notify>,
    handle: Option<Arc<JoinHandle<Result<()>>>>,
}

impl FakeWallet {
    /// Create new [`FakeWallet`]
    pub fn new(
        fee_reserve: FeeReserve,
        payment_states: HashMap<String, MeltQuoteState>,
        fail_payment_check: HashSet<String>,
        payment_delay: u64,
        addr: &str,
        port: u16,
    ) -> Result<Self> {
        let (sender, receiver) = tokio::sync::mpsc::channel(8);

        Ok(Self {
            fee_reserve,
            sender,
            _receiver: Arc::new(Mutex::new(Some(receiver))),
            payment_states: Arc::new(Mutex::new(payment_states)),
            failed_payment_check: Arc::new(Mutex::new(fail_payment_check)),
            payment_delay,
            _wait_invoice_cancel_token: CancellationToken::new(),
            _wait_invoice_is_active: Arc::new(AtomicBool::new(false)),
            shutdown: Arc::new(Notify::new()),
            handle: None,
            socket_addr: format!("{addr}:{port}").parse()?,
        })
    }

    /// Start fake wallet grpc server
    pub async fn start(&mut self, tls_dir: Option<PathBuf>) -> Result<()> {
        tracing::info!("Starting RPC server {}", self.socket_addr);

        let server = match tls_dir {
            Some(tls_dir) => {
                tracing::info!("TLS configuration found, starting secure server");
                let cert = std::fs::read_to_string(tls_dir.join("server.pem"))?;
                let key = std::fs::read_to_string(tls_dir.join("server.key"))?;
                let client_ca_cert = std::fs::read_to_string(tls_dir.join("ca.pem"))?;
                let client_ca_cert = Certificate::from_pem(client_ca_cert);
                let server_identity = Identity::from_pem(cert, key);
                let tls_config = ServerTlsConfig::new()
                    .identity(server_identity)
                    .client_ca_root(client_ca_cert);

                Server::builder()
                    .tls_config(tls_config)?
                    .add_service(CdkPaymentProcessorServer::new(self.clone()))
            }
            None => {
                tracing::warn!("No valid TLS configuration found, starting insecure server");
                Server::builder().add_service(CdkPaymentProcessorServer::new(self.clone()))
            }
        };

        let shutdown = self.shutdown.clone();
        let addr = self.socket_addr;

        self.handle = Some(Arc::new(tokio::spawn(async move {
            let server = server.serve_with_shutdown(addr, async {
                shutdown.notified().await;
            });

            server.await?;
            Ok(())
        })));

        Ok(())
    }

    /// Stop fake wallet grpc server
    pub async fn stop(&self) -> Result<()> {
        self.shutdown.notify_one();
        if let Some(handle) = &self.handle {
            while !handle.is_finished() {
                tracing::info!("Waitning for mint rpc server to stop");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }

        tracing::info!("Mint rpc server stopped");
        Ok(())
    }
}

impl Drop for FakeWallet {
    fn drop(&mut self) {
        tracing::debug!("Dropping fake wallet rpc server");
        self.shutdown.notify_one();
    }
}

/// Struct for signaling what methods should respond via invoice description
#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct FakeInvoiceDescription {
    /// State to be returned from pay invoice state
    pub pay_invoice_state: MeltQuoteState,
    /// State to be returned by check payment state
    pub check_payment_state: MeltQuoteState,
    /// Should pay invoice error
    pub pay_err: bool,
    /// Should check failure
    pub check_err: bool,
}

impl Default for FakeInvoiceDescription {
    fn default() -> Self {
        Self {
            pay_invoice_state: MeltQuoteState::Paid,
            check_payment_state: MeltQuoteState::Paid,
            pay_err: false,
            check_err: false,
        }
    }
}

#[async_trait]
impl CdkPaymentProcessor for FakeWallet {
    async fn get_settings(
        &self,
        _request: Request<SettingsRequest>,
    ) -> Result<Response<SettingsResponse>, Status> {
        Ok(Response::new(SettingsResponse {
            mpp: true,
            unit: CurrencyUnit::Msat.to_string(),
            invoice_description: true,
        }))
    }

    async fn create_payment(
        &self,
        request: Request<CreatePaymentRequest>,
    ) -> Result<Response<CreatePaymentResponse>, Status> {
        let CreatePaymentRequest {
            amount,
            unit,
            description,
            unix_expiry,
        } = request.into_inner();

        let time_now = unix_time();

        if unit != CurrencyUnit::Msat.to_string() {
            return Err(Status::invalid_argument("Unsupported unit"));
        }

        if let Some(unix_expiry) = unix_expiry {
            if time_now > unix_expiry {
                return Err(Status::invalid_argument("Invalid unix time"));
            }
        }

        // Since this is fake we just use the amount no matter the unit to create an invoice
        let amount_msat = amount;

        let invoice = create_fake_invoice(amount_msat, description);

        let sender = self.sender.clone();

        let payment_hash = invoice.payment_hash();

        let payment_hash_clone = payment_hash.to_string();

        let duration = time::Duration::from_secs(self.payment_delay);

        tokio::spawn(async move {
            // Wait for the random delay to elapse
            time::sleep(duration).await;

            // Send the message after waiting for the specified duration
            if sender.send(payment_hash_clone.clone()).await.is_err() {
                tracing::error!("Failed to send label: {}", payment_hash_clone);
            }
        });

        let expiry = invoice.expires_at().map(|t| t.as_secs());

        Ok(Response::new(CreatePaymentResponse {
            request_lookup_id: payment_hash.to_string(),
            request: invoice.to_string(),
            expiry,
        }))
    }

    async fn get_payment_quote(
        &self,
        request: Request<PaymentQuoteRequest>,
    ) -> Result<Response<PaymentQuoteResponse>, Status> {
        let request = request.into_inner();

        let melt_quote_request: MeltQuoteBolt11Request = request
            .try_into()
            .map_err(|_| Status::invalid_argument("invalid request"))?;
        let amount = melt_quote_request
            .amount_msat()
            .map_err(|_| Status::internal("Could not get amount"))?;

        let amount = amount / MSAT_IN_SAT.into();

        let relative_fee_reserve =
            (self.fee_reserve.percent_fee_reserve * u64::from(amount) as f32) as u64;

        let absolute_fee_reserve: u64 = self.fee_reserve.min_fee_reserve.into();

        let fee = match relative_fee_reserve > absolute_fee_reserve {
            true => relative_fee_reserve,
            false => absolute_fee_reserve,
        };

        Ok(Response::new(PaymentQuoteResponse {
            request_lookup_id: melt_quote_request.request.payment_hash().to_string(),
            amount: amount.into(),
            fee,
            state: QuoteState::from(MeltQuoteState::Unpaid).into(),
        }))
    }

    async fn make_payment(
        &self,
        request: Request<MakePaymentRequest>,
    ) -> Result<Response<MakePaymentResponse>, Status> {
        let MakePaymentRequest {
            melt_quote,
            partial_amount: _,
            max_fee_amount: _,
        } = request.into_inner();

        let melt_quote: MeltQuote = melt_quote.ok_or(Status::invalid_argument("No melt quote"))?;

        let bolt11 = Bolt11Invoice::from_str(&melt_quote.request)
            .map_err(|_| Status::internal("Invalid bolt11"))?;

        let payment_hash = bolt11.payment_hash().to_string();

        let description = bolt11.description().to_string();

        let status: Option<FakeInvoiceDescription> = serde_json::from_str(&description).ok();

        let mut payment_states = self.payment_states.lock().await;
        let payment_status = status
            .clone()
            .map(|s| s.pay_invoice_state)
            .unwrap_or(MeltQuoteState::Paid);

        let checkout_going_status = status
            .clone()
            .map(|s| s.check_payment_state)
            .unwrap_or(MeltQuoteState::Paid);

        payment_states.insert(payment_hash.clone(), checkout_going_status);

        if let Some(description) = status {
            if description.check_err {
                let mut fail = self.failed_payment_check.lock().await;
                fail.insert(payment_hash.clone());
            }

            if description.pay_err {
                return Err(Status::invalid_argument("Description not supported"));
            }
        }

        Ok(Response::new(
            PayInvoiceResponse {
                payment_preimage: Some("".to_string()),
                payment_lookup_id: payment_hash,
                status: payment_status,
                total_spent: melt_quote.amount.into(),
                unit: melt_quote
                    .unit
                    .parse()
                    .map_err(|_| Status::invalid_argument("Invalid unit"))?,
            }
            .into(),
        ))
    }

    async fn check_incoming_payment(
        &self,
        _request: Request<CheckIncomingPaymentRequest>,
    ) -> Result<Response<CheckIncomingPaymentResponse>, Status> {
        Ok(Response::new(CheckIncomingPaymentResponse {
            status: QuoteState::Paid.into(),
        }))
    }

    async fn check_outgoing_payment(
        &self,
        request: Request<CheckOutgoingPaymentRequest>,
    ) -> Result<Response<MakePaymentResponse>, Status> {
        let request = request.into_inner();
        let request_lookup_id = request.request_lookup_id;

        // For fake wallet if the state is not explicitly set default to paid
        let states = self.payment_states.lock().await;
        let status = states.get(&request_lookup_id).cloned();

        let status = status.unwrap_or(MeltQuoteState::Paid);

        let fail_payments = self.failed_payment_check.lock().await;

        if fail_payments.contains(&request_lookup_id) {
            return Err(Status::internal("Could not pay invoice"));
        }

        Ok(Response::new(
            PayInvoiceResponse {
                payment_preimage: Some("".to_string()),
                payment_lookup_id: request_lookup_id.to_string(),
                status,
                total_spent: Amount::ZERO,
                unit: self
                    .get_settings(Request::new(SettingsRequest {}))
                    .await?
                    .into_inner()
                    .unit
                    .parse()
                    .expect("Valid unit set"),
            }
            .into(),
        ))
    }
}

// #[async_trait]
// impl MintLightning for FakeWallet {
//     type Err = cdk_lightning::Error;

//     async fn get_settings(&self) -> Result<Settings, Self::Err> {
//         Ok(Settings {
//             mpp: true,
//             unit: CurrencyUnit::Msat,
//             invoice_description: true,
//         })
//     }

//     fn is_wait_invoice_active(&self) -> bool {
//         self.wait_invoice_is_active.load(Ordering::SeqCst)
//     }

//     fn cancel_wait_invoice(&self) {
//         self.wait_invoice_cancel_token.cancel()
//     }

//     async fn wait_any_invoice(
//         &self,
//     ) -> Result<Pin<Box<dyn Stream<Item = String> + Send>>, Self::Err> {
//         let receiver = self.receiver.lock().await.take().ok_or(Error::NoReceiver)?;
//         let receiver_stream = ReceiverStream::new(receiver);
//         Ok(Box::pin(receiver_stream.map(|label| label)))
//     }

//     async fn get_payment_quote(
//         &self,
//         melt_quote_request: &MeltQuoteBolt11Request,
//     ) -> Result<PaymentQuoteResponse, Self::Err> {
//         let amount = melt_quote_request.amount_msat()?;

//         let amount = amount / MSAT_IN_SAT.into();

//         let relative_fee_reserve =
//             (self.fee_reserve.percent_fee_reserve * u64::from(amount) as f32) as u64;

//         let absolute_fee_reserve: u64 = self.fee_reserve.min_fee_reserve.into();

//         let fee = match relative_fee_reserve > absolute_fee_reserve {
//             true => relative_fee_reserve,
//             false => absolute_fee_reserve,
//         };

//         Ok(PaymentQuoteResponse {
//             request_lookup_id: melt_quote_request.request.payment_hash().to_string(),
//             amount,
//             fee: fee.into(),
//             state: MeltQuoteState::Unpaid,
//         })
//     }

//     async fn pay_invoice(
//         &self,
//         melt_quote: mint::MeltQuote,
//         _partial_msats: Option<Amount>,
//         _max_fee_msats: Option<Amount>,
//     ) -> Result<PayInvoiceResponse, Self::Err> {
//         let bolt11 = Bolt11Invoice::from_str(&melt_quote.request)?;

//         let payment_hash = bolt11.payment_hash().to_string();

//         let description = bolt11.description().to_string();

//         let status: Option<FakeInvoiceDescription> = serde_json::from_str(&description).ok();

//         let mut payment_states = self.payment_states.lock().await;
//         let payment_status = status
//             .clone()
//             .map(|s| s.pay_invoice_state)
//             .unwrap_or(MeltQuoteState::Paid);

//         let checkout_going_status = status
//             .clone()
//             .map(|s| s.check_payment_state)
//             .unwrap_or(MeltQuoteState::Paid);

//         payment_states.insert(payment_hash.clone(), checkout_going_status);

//         if let Some(description) = status {
//             if description.check_err {
//                 let mut fail = self.failed_payment_check.lock().await;
//                 fail.insert(payment_hash.clone());
//             }

//             if description.pay_err {
//                 return Err(Error::UnknownInvoice.into());
//             }
//         }

//         Ok(PayInvoiceResponse {
//             payment_preimage: Some("".to_string()),
//             payment_lookup_id: payment_hash,
//             status: payment_status,
//             total_spent: melt_quote.amount,
//             unit: melt_quote.unit,
//         })
//     }

//     async fn create_invoice(
//         &self,
//         amount: Amount,
//         _unit: &CurrencyUnit,
//         description: String,
//         unix_expiry: u64,
//     ) -> Result<CreateInvoiceResponse, Self::Err> {
//         let time_now = unix_time();
//         assert!(unix_expiry > time_now);

//         // Since this is fake we just use the amount no matter the unit to create an invoice
//         let amount_msat = amount;

//         let invoice = create_fake_invoice(amount_msat.into(), description);

//         let sender = self.sender.clone();

//         let payment_hash = invoice.payment_hash();

//         let payment_hash_clone = payment_hash.to_string();

//         let duration = time::Duration::from_secs(self.payment_delay);

//         tokio::spawn(async move {
//             // Wait for the random delay to elapse
//             time::sleep(duration).await;

//             // Send the message after waiting for the specified duration
//             if sender.send(payment_hash_clone.clone()).await.is_err() {
//                 tracing::error!("Failed to send label: {}", payment_hash_clone);
//             }
//         });

//         let expiry = invoice.expires_at().map(|t| t.as_secs());

//         Ok(CreateInvoiceResponse {
//             request_lookup_id: payment_hash.to_string(),
//             request: invoice,
//             expiry,
//         })
//     }

//     async fn check_incoming_invoice_status(
//         &self,
//         _request_lookup_id: &str,
//     ) -> Result<MintQuoteState, Self::Err> {
//         Ok(MintQuoteState::Paid)
//     }

//     async fn check_outgoing_payment(
//         &self,
//         request_lookup_id: &str,
//     ) -> Result<PayInvoiceResponse, Self::Err> {
//         // For fake wallet if the state is not explicitly set default to paid
//         let states = self.payment_states.lock().await;
//         let status = states.get(request_lookup_id).cloned();

//         let status = status.unwrap_or(MeltQuoteState::Paid);

//         let fail_payments = self.failed_payment_check.lock().await;

//         if fail_payments.contains(request_lookup_id) {
//             return Err(cdk_lightning::Error::InvoicePaymentPending);
//         }

//         Ok(PayInvoiceResponse {
//             payment_preimage: Some("".to_string()),
//             payment_lookup_id: request_lookup_id.to_string(),
//             status,
//             total_spent: Amount::ZERO,
//             unit: self.get_settings().await?.unit,
//         })
//     }
// }

/// Create fake invoice
pub fn create_fake_invoice(amount_msat: u64, description: String) -> Bolt11Invoice {
    let private_key = SecretKey::from_slice(
        &[
            0xe1, 0x26, 0xf6, 0x8f, 0x7e, 0xaf, 0xcc, 0x8b, 0x74, 0xf5, 0x4d, 0x26, 0x9f, 0xe2,
            0x06, 0xbe, 0x71, 0x50, 0x00, 0xf9, 0x4d, 0xac, 0x06, 0x7d, 0x1c, 0x04, 0xa8, 0xca,
            0x3b, 0x2d, 0xb7, 0x34,
        ][..],
    )
    .unwrap();

    let mut rng = thread_rng();
    let mut random_bytes = [0u8; 32];
    rng.fill(&mut random_bytes);

    let payment_hash = sha256::Hash::from_slice(&random_bytes).unwrap();
    let payment_secret = PaymentSecret([42u8; 32]);

    InvoiceBuilder::new(Currency::Bitcoin)
        .description(description)
        .payment_hash(payment_hash)
        .payment_secret(payment_secret)
        .amount_milli_satoshis(amount_msat)
        .current_timestamp()
        .min_final_cltv_expiry_delta(144)
        .build_signed(|hash| Secp256k1::new().sign_ecdsa_recoverable(hash, &private_key))
        .unwrap()
}
