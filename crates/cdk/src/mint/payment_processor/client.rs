use std::path::PathBuf;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;

use anyhow::anyhow;
use cdk_common::lightning::{
    CreateInvoiceResponse, MintLightning, PayInvoiceResponse, PaymentQuoteResponse, Settings,
};
use cdk_common::proto::{
    CheckIncomingPaymentRequest, CheckOutgoingPaymentRequest, CreatePaymentRequest,
    MakePaymentRequest, SettingsRequest,
};
use cdk_common::{
    mint, Amount, CdkPaymentProcessorClient, CurrencyUnit, MeltQuoteBolt11Request, MintQuoteState,
};
use futures::Stream;
use tokio::sync::Mutex;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Identity};
use tonic::{async_trait, Request};

/// Payment Processor
#[derive(Clone)]
pub struct PaymentProcessor {
    inner: Arc<Mutex<CdkPaymentProcessorClient<Channel>>>,
}

impl PaymentProcessor {
    /// Payment Processor
    pub async fn new(addr: &str, port: u16, tls_dir: Option<PathBuf>) -> anyhow::Result<Self> {
        let addr = format!("{}:{}", addr, port);
        let channel = if let Some(tls_dir) = tls_dir {
            // TLS directory exists, configure TLS
            let server_root_ca_cert = std::fs::read_to_string(tls_dir.join("ca.pem"))?;
            let server_root_ca_cert = Certificate::from_pem(server_root_ca_cert);
            let client_cert = std::fs::read_to_string(tls_dir.join("client.pem"))?;
            let client_key = std::fs::read_to_string(tls_dir.join("client.key"))?;
            let client_identity = Identity::from_pem(client_cert, client_key);
            let tls = ClientTlsConfig::new()
                .ca_certificate(server_root_ca_cert)
                .identity(client_identity);

            Channel::from_shared(addr)?
                .tls_config(tls)?
                .connect()
                .await?
        } else {
            // No TLS directory, skip TLS configuration
            Channel::from_shared(addr)?.connect().await?
        };

        let client = CdkPaymentProcessorClient::new(channel);

        Ok(Self {
            inner: Arc::new(Mutex::new(client)),
        })
    }
}

#[async_trait]
impl MintLightning for PaymentProcessor {
    type Err = crate::cdk_lightning::Error;

    async fn get_settings(&self) -> Result<Settings, Self::Err> {
        let mut inner = self.inner.lock().await;
        let response = inner.get_settings(Request::new(SettingsRequest {})).await?;

        let settings = response.into_inner();

        Ok(Settings {
            mpp: settings.mpp,
            unit: settings.unit.parse().expect("Valid unit"),
            invoice_description: settings.invoice_description,
        })
    }

    /// Create a new invoice
    async fn create_invoice(
        &self,
        amount: Amount,
        unit: &CurrencyUnit,
        description: String,
        unix_expiry: u64,
    ) -> Result<CreateInvoiceResponse, Self::Err> {
        let mut inner = self.inner.lock().await;
        let response = inner
            .create_payment(Request::new(CreatePaymentRequest {
                amount: amount.into(),
                unit: unit.to_string(),
                description,
                unix_expiry: Some(unix_expiry),
            }))
            .await?;

        let response = response.into_inner();

        Ok(response.try_into().map_err(|_| {
            crate::cdk_lightning::Error::Anyhow(anyhow!("Could not create invoice"))
        })?)
    }

    async fn get_payment_quote(
        &self,
        melt_quote_request: &MeltQuoteBolt11Request,
    ) -> Result<PaymentQuoteResponse, Self::Err> {
        let mut inner = self.inner.lock().await;
        let response = inner
            .get_payment_quote(Request::new(melt_quote_request.into()))
            .await?;

        let response = response.into_inner();

        Ok(response.into())
    }

    async fn pay_invoice(
        &self,
        melt_quote: mint::MeltQuote,
        partial_amount: Option<Amount>,
        max_fee_amount: Option<Amount>,
    ) -> Result<PayInvoiceResponse, Self::Err> {
        let mut inner = self.inner.lock().await;
        let response = inner
            .make_payment(Request::new(MakePaymentRequest {
                melt_quote: Some(melt_quote.into()),
                partial_amount: partial_amount.map(|a| a.into()),
                max_fee_amount: max_fee_amount.map(|a| a.into()),
            }))
            .await?;

        let response = response.into_inner();

        Ok(response.try_into().map_err(|_err| {
            crate::cdk_lightning::Error::Anyhow(anyhow!("could not make payment"))
        })?)
    }

    /// Listen for invoices to be paid to the mint
    async fn wait_any_invoice(
        &self,
    ) -> Result<Pin<Box<dyn Stream<Item = String> + Send>>, Self::Err> {
        todo!()
    }

    /// Is wait invoice active
    fn is_wait_invoice_active(&self) -> bool {
        todo!()
    }

    /// Cancel wait invoice
    fn cancel_wait_invoice(&self) {
        todo!()
    }

    async fn check_incoming_invoice_status(
        &self,
        request_lookup_id: &str,
    ) -> Result<MintQuoteState, Self::Err> {
        let mut inner = self.inner.lock().await;
        let response = inner
            .check_incoming_payment(Request::new(CheckIncomingPaymentRequest {
                request_lookup_id: request_lookup_id.to_string(),
            }))
            .await?;

        let check_incoming = response.into_inner();

        let status = check_incoming.status().as_str_name();

        Ok(MintQuoteState::from_str(status)?)
    }

    async fn check_outgoing_payment(
        &self,
        request_lookup_id: &str,
    ) -> Result<PayInvoiceResponse, Self::Err> {
        let mut inner = self.inner.lock().await;
        let response = inner
            .check_outgoing_payment(Request::new(CheckOutgoingPaymentRequest {
                request_lookup_id: request_lookup_id.to_string(),
            }))
            .await?;

        let check_outgoing = response.into_inner();

        Ok(check_outgoing
            .try_into()
            .map_err(|_| crate::cdk_lightning::Error::UnknownPaymentState)?)
    }
}
