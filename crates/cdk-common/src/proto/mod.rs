use std::str::FromStr;

use cashu::{Bolt11Invoice, CurrencyUnit, MeltQuoteBolt11Request};
use melt_options::Options;

use crate::lightning::{CreateInvoiceResponse, PayInvoiceResponse, Settings};

tonic::include_proto!("cdk_payment_processor");

impl From<Settings> for SettingsResponse {
    fn from(value: Settings) -> Self {
        Self {
            mpp: value.mpp,
            unit: value.unit.to_string(),
            invoice_description: value.invoice_description,
        }
    }
}

impl TryFrom<MakePaymentResponse> for PayInvoiceResponse {
    type Error = crate::error::Error;
    fn try_from(value: MakePaymentResponse) -> Result<Self, Self::Error> {
        Ok(Self {
            payment_lookup_id: value.payment_lookup_id.clone(),
            payment_preimage: value.payment_proof.clone(),
            status: value.status().as_str_name().parse()?,
            total_spent: value.total_spent.into(),
            unit: value.unit.parse()?,
        })
    }
}

impl From<PayInvoiceResponse> for MakePaymentResponse {
    fn from(value: PayInvoiceResponse) -> Self {
        Self {
            payment_lookup_id: value.payment_lookup_id.clone(),
            payment_proof: value.payment_preimage.clone(),
            status: QuoteState::from(value.status).into(),
            total_spent: value.total_spent.into(),
            unit: value.unit.to_string(),
        }
    }
}

impl From<CreateInvoiceResponse> for CreatePaymentResponse {
    fn from(value: CreateInvoiceResponse) -> Self {
        Self {
            request_lookup_id: value.request_lookup_id,
            request: value.request.to_string(),
            expiry: value.expiry,
        }
    }
}

impl TryFrom<CreatePaymentResponse> for CreateInvoiceResponse {
    type Error = crate::error::Error;

    fn try_from(value: CreatePaymentResponse) -> Result<Self, Self::Error> {
        Ok(Self {
            request_lookup_id: value.request_lookup_id,
            request: value.request.parse()?,
            expiry: value.expiry,
        })
    }
}

impl From<&MeltQuoteBolt11Request> for PaymentQuoteRequest {
    fn from(value: &MeltQuoteBolt11Request) -> Self {
        Self {
            request: value.request.to_string(),
            unit: value.unit.to_string(),
            options: value.options.map(|o| o.into()),
        }
    }
}

impl From<crate::lightning::PaymentQuoteResponse> for PaymentQuoteResponse {
    fn from(value: crate::lightning::PaymentQuoteResponse) -> Self {
        Self {
            request_lookup_id: value.request_lookup_id,
            amount: value.amount.into(),
            fee: value.fee.into(),
            state: QuoteState::from(value.state).into(),
        }
    }
}

impl From<cashu::nut05::MeltOptions> for MeltOptions {
    fn from(value: cashu::nut05::MeltOptions) -> Self {
        Self {
            options: Some(value.into()),
        }
    }
}

impl From<cashu::nut05::MeltOptions> for Options {
    fn from(value: cashu::nut05::MeltOptions) -> Self {
        match value {
            cashu::MeltOptions::Mpp { mpp } => Self::Mpp(Mpp {
                amount: mpp.amount.into(),
            }),
        }
    }
}

impl From<MeltOptions> for cashu::nut05::MeltOptions {
    fn from(value: MeltOptions) -> Self {
        let options = value.options.expect("option defined");
        match options {
            Options::Mpp(mpp) => cashu::MeltOptions::new_mpp(mpp.amount),
        }
    }
}

impl From<PaymentQuoteResponse> for crate::lightning::PaymentQuoteResponse {
    fn from(value: PaymentQuoteResponse) -> Self {
        Self {
            request_lookup_id: value.request_lookup_id.clone(),
            amount: value.amount.into(),
            fee: value.fee.into(),
            state: value.state().into(),
        }
    }
}

impl From<QuoteState> for cashu::nut05::QuoteState {
    fn from(value: QuoteState) -> Self {
        match value {
            QuoteState::Unpaid => Self::Unpaid,
            QuoteState::Paid => Self::Paid,
            QuoteState::Pending => Self::Pending,
            QuoteState::Unknown => Self::Unknown,
            QuoteState::Failed => Self::Failed,
            QuoteState::Issued => Self::Unknown,
        }
    }
}

impl From<cashu::nut05::QuoteState> for QuoteState {
    fn from(value: cashu::nut05::QuoteState) -> Self {
        match value {
            cashu::MeltQuoteState::Unpaid => Self::Unpaid,
            cashu::MeltQuoteState::Paid => Self::Paid,
            cashu::MeltQuoteState::Pending => Self::Pending,
            cashu::MeltQuoteState::Unknown => Self::Unknown,
            cashu::MeltQuoteState::Failed => Self::Failed,
        }
    }
}

impl From<cashu::nut04::QuoteState> for QuoteState {
    fn from(value: cashu::nut04::QuoteState) -> Self {
        match value {
            cashu::MintQuoteState::Unpaid => Self::Unpaid,
            cashu::MintQuoteState::Paid => Self::Paid,
            cashu::MintQuoteState::Pending => Self::Pending,
            cashu::MintQuoteState::Issued => Self::Issued,
        }
    }
}

impl From<cashu::mint::MeltQuote> for MeltQuote {
    fn from(value: cashu::mint::MeltQuote) -> Self {
        Self {
            id: value.id.to_string(),
            unit: value.unit.to_string(),
            amount: value.amount.into(),
            request: value.request,
            fee_reserve: value.fee_reserve.into(),
            state: QuoteState::from(value.state).into(),
            expiry: value.expiry,
            payment_preimage: value.payment_preimage,
            request_lookup_id: value.request_lookup_id,
            msat_to_pay: value.msat_to_pay.map(|a| a.into()),
        }
    }
}

impl TryFrom<MeltQuote> for cashu::mint::MeltQuote {
    type Error = crate::error::Error;

    fn try_from(value: MeltQuote) -> Result<Self, Self::Error> {
        Ok(Self {
            id: value
                .id
                .parse()
                .map_err(|_| crate::error::Error::Internal)?,
            unit: value.unit.parse()?,
            amount: value.amount.into(),
            request: value.request.clone(),
            fee_reserve: value.fee_reserve.into(),
            state: cashu::nut05::QuoteState::from(value.state()),
            expiry: value.expiry,
            payment_preimage: value.payment_preimage,
            request_lookup_id: value.request_lookup_id,
            msat_to_pay: value.msat_to_pay.map(|a| a.into()),
        })
    }
}

impl TryFrom<PaymentQuoteRequest> for MeltQuoteBolt11Request {
    type Error = crate::error::Error;

    fn try_from(value: PaymentQuoteRequest) -> Result<Self, Self::Error> {
        Ok(Self {
            request: Bolt11Invoice::from_str(&value.request)?,
            unit: CurrencyUnit::from_str(&value.unit)?,
            options: value.options.map(|o| o.into()),
        })
    }
}
