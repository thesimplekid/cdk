use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use cdk_common::lightning::MintLightning;
use cdk_common::{CurrencyUnit, MeltQuoteBolt11Request};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tonic::transport::{Certificate, Identity, Server, ServerTlsConfig};
use tonic::{async_trait, Request, Response, Status};

use super::cdk_payment_processor_server::{CdkPaymentProcessor, CdkPaymentProcessorServer};
use crate::proto::*;

/// Payment Processor
#[derive(Clone)]
pub struct PaymentProcessorServer {
    inner: Arc<dyn MintLightning<Err = cdk_common::lightning::Error> + Send + Sync>,
    socket_addr: SocketAddr,
    shutdown: Arc<Notify>,
    handle: Option<Arc<JoinHandle<anyhow::Result<()>>>>,
}

impl PaymentProcessorServer {
    /// Start fake wallet grpc server
    pub async fn start(&mut self, tls_dir: Option<PathBuf>) -> anyhow::Result<()> {
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
    pub async fn stop(&self) -> anyhow::Result<()> {
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

impl Drop for PaymentProcessorServer {
    fn drop(&mut self) {
        tracing::debug!("Dropping fake wallet rpc server");
        self.shutdown.notify_one();
    }
}

#[async_trait]
impl CdkPaymentProcessor for PaymentProcessorServer {
    async fn get_settings(
        &self,
        _request: Request<SettingsRequest>,
    ) -> Result<Response<SettingsResponse>, Status> {
        let settings = self
            .inner
            .get_settings()
            .await
            .map_err(|_| Status::internal("Could not get settings"))?;
        Ok(Response::new(settings.into()))
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

        let unit =
            CurrencyUnit::from_str(&unit).map_err(|_| Status::invalid_argument("Invalid unit"))?;
        let invoice_response = self
            .inner
            .create_invoice(amount.into(), &unit, description, unix_expiry.unwrap())
            .await
            .map_err(|_| Status::internal("Could not create invoice"))?;

        Ok(Response::new(invoice_response.into()))
    }

    async fn get_payment_quote(
        &self,
        request: Request<PaymentQuoteRequest>,
    ) -> Result<Response<PaymentQuoteResponse>, Status> {
        let request = request.into_inner();
        let bolt11_melt_quote: MeltQuoteBolt11Request = request
            .try_into()
            .map_err(|_| Status::invalid_argument("Invalid request"))?;

        let payment_quote = self
            .inner
            .get_payment_quote(&bolt11_melt_quote)
            .await
            .map_err(|err| {
                tracing::error!("Could not get bolt11 melt quote: {}", err);
                Status::internal("Could not get melt quote")
            })?;

        Ok(Response::new(payment_quote.into()))
    }

    async fn make_payment(
        &self,
        request: Request<MakePaymentRequest>,
    ) -> Result<Response<MakePaymentResponse>, Status> {
        let request = request.into_inner();

        let pay_invoice = self
            .inner
            .pay_invoice(
                request
                    .melt_quote
                    .ok_or(Status::invalid_argument("Meltquote is required"))?
                    .try_into()
                    .map_err(|_err| Status::invalid_argument("Invalid melt quote"))?,
                request.partial_amount.map(|a| a.into()),
                request.max_fee_amount.map(|a| a.into()),
            )
            .await
            .map_err(|_| Status::internal("Could not pay invoice"))?;

        Ok(Response::new(pay_invoice.into()))
    }

    async fn check_incoming_payment(
        &self,
        request: Request<CheckIncomingPaymentRequest>,
    ) -> Result<Response<CheckIncomingPaymentResponse>, Status> {
        let request = request.into_inner();

        let check_response = self
            .inner
            .check_incoming_invoice_status(&request.request_lookup_id)
            .await
            .map_err(|_| Status::internal("Could not check incoming payment status"))?;

        Ok(Response::new(CheckIncomingPaymentResponse {
            status: QuoteState::from(check_response).into(),
        }))
    }

    async fn check_outgoing_payment(
        &self,
        request: Request<CheckOutgoingPaymentRequest>,
    ) -> Result<Response<MakePaymentResponse>, Status> {
        let request = request.into_inner();

        let check_response = self
            .inner
            .check_outgoing_payment(&request.request_lookup_id)
            .await
            .map_err(|_| Status::internal("Could not check incoming payment status"))?;

        Ok(Response::new(check_response.into()))
    }
}
