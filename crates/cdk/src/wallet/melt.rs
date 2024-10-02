use std::str::FromStr;

use lightning::offers::offer::Offer;
use lightning_invoice::Bolt11Invoice;
use tracing::instrument;

use crate::{
    amount::amount_for_offer,
    dhke::construct_proofs,
    nuts::{
        CurrencyUnit, MeltQuoteBolt11Response, PaymentMethod, PreMintSecrets, Proofs, PublicKey,
        State,
    },
    types::{Melted, ProofInfo},
    util::unix_time,
    Amount, Error, Wallet,
};

use super::MeltQuote;

impl Wallet {
    /// Melt Quote
    /// # Synopsis
    /// ```rust
    ///  use std::sync::Arc;
    ///
    ///  use cdk::cdk_database::WalletMemoryDatabase;
    ///  use cdk::nuts::CurrencyUnit;
    ///  use cdk::wallet::Wallet;
    ///  use rand::Rng;
    ///
    /// #[tokio::main]
    /// async fn main() -> anyhow::Result<()> {
    ///     let seed = rand::thread_rng().gen::<[u8; 32]>();
    ///     let mint_url = "https://testnut.cashu.space";
    ///     let unit = CurrencyUnit::Sat;
    ///
    ///     let localstore = WalletMemoryDatabase::default();
    ///     let wallet = Wallet::new(mint_url, unit, Arc::new(localstore), &seed, None).unwrap();
    ///     let bolt11 = "lnbc100n1pnvpufspp5djn8hrq49r8cghwye9kqw752qjncwyfnrprhprpqk43mwcy4yfsqdq5g9kxy7fqd9h8vmmfvdjscqzzsxqyz5vqsp5uhpjt36rj75pl7jq2sshaukzfkt7uulj456s4mh7uy7l6vx7lvxs9qxpqysgqedwz08acmqwtk8g4vkwm2w78suwt2qyzz6jkkwcgrjm3r3hs6fskyhvud4fan3keru7emjm8ygqpcrwtlmhfjfmer3afs5hhwamgr4cqtactdq".to_string();
    ///     let quote = wallet.melt_quote(bolt11, None).await?;
    ///
    ///     Ok(())
    /// }
    /// ```
    #[instrument(skip(self, request))]
    pub async fn melt_quote(
        &self,
        request: String,
        mpp: Option<Amount>,
    ) -> Result<MeltQuote, Error> {
        let invoice = Bolt11Invoice::from_str(&request)?;

        let request_amount = invoice
            .amount_milli_satoshis()
            .ok_or(Error::InvoiceAmountUndefined)?;

        let amount = match self.unit {
            CurrencyUnit::Sat => Amount::from(request_amount / 1000),
            CurrencyUnit::Msat => Amount::from(request_amount),
            _ => return Err(Error::UnitUnsupported),
        };

        let quote_res = self
            .client
            .post_melt_quote(self.mint_url.clone().try_into()?, self.unit, invoice, mpp)
            .await?;

        if quote_res.amount != amount {
            return Err(Error::IncorrectQuoteAmount);
        }

        let quote = MeltQuote {
            id: quote_res.quote,
            amount,
            request,
            payment_method: PaymentMethod::Bolt11,
            unit: self.unit,
            fee_reserve: quote_res.fee_reserve,
            state: quote_res.state,
            expiry: quote_res.expiry,
            payment_preimage: quote_res.payment_preimage,
        };

        self.localstore.add_melt_quote(quote.clone()).await?;

        Ok(quote)
    }

    /// Melt Quote bolt12
    #[instrument(skip(self, request))]
    pub async fn melt_bolt12_quote(
        &self,
        request: String,
        amount: Option<Amount>,
    ) -> Result<MeltQuote, Error> {
        let offer = Offer::from_str(&request)?;

        let amount = match amount {
            Some(amount) => amount,
            None => amount_for_offer(&offer, &self.unit).unwrap(),
        };

        let quote_res = self
            .client
            .post_melt_bolt12_quote(
                self.mint_url.clone().try_into()?,
                self.unit,
                request.to_string(),
                Some(amount),
            )
            .await
            .unwrap();

        if quote_res.amount != amount {
            return Err(Error::IncorrectQuoteAmount);
        }

        let quote = MeltQuote {
            id: quote_res.quote,
            amount,
            request,
            payment_method: PaymentMethod::Bolt12,
            unit: self.unit,
            fee_reserve: quote_res.fee_reserve,
            state: quote_res.state,
            expiry: quote_res.expiry,
            payment_preimage: quote_res.payment_preimage,
        };

        self.localstore.add_melt_quote(quote.clone()).await?;

        Ok(quote)
    }

    /// Melt quote status
    #[instrument(skip(self, quote_id))]
    pub async fn melt_quote_status(
        &self,
        quote_id: &str,
    ) -> Result<MeltQuoteBolt11Response, Error> {
        let response = self
            .client
            .get_melt_quote_status(self.mint_url.clone().try_into()?, quote_id)
            .await?;

        match self.localstore.get_melt_quote(quote_id).await? {
            Some(quote) => {
                let mut quote = quote;

                quote.state = response.state;
                self.localstore.add_melt_quote(quote).await?;
            }
            None => {
                tracing::info!("Quote melt {} unknown", quote_id);
            }
        }

        Ok(response)
    }

    /// Melt specific proofs
    #[instrument(skip(self, proofs))]
    pub async fn melt_proofs(&self, quote_id: &str, proofs: Proofs) -> Result<Melted, Error> {
        let quote_info = self.localstore.get_melt_quote(quote_id).await?;
        let quote_info = if let Some(quote) = quote_info {
            if quote.expiry.le(&unix_time()) {
                return Err(Error::ExpiredQuote(quote.expiry, unix_time()));
            }

            quote.clone()
        } else {
            return Err(Error::UnknownQuote);
        };

        let proofs_total = Amount::try_sum(proofs.iter().map(|p| p.amount))?;
        if proofs_total < quote_info.amount + quote_info.fee_reserve {
            return Err(Error::InsufficientFunds);
        }

        let ys = proofs
            .iter()
            .map(|p| p.y())
            .collect::<Result<Vec<PublicKey>, _>>()?;
        self.localstore.set_pending_proofs(ys).await?;

        let active_keyset_id = self.get_active_mint_keyset().await?.id;

        let count = self
            .localstore
            .get_keyset_counter(&active_keyset_id)
            .await?;

        let count = count.map_or(0, |c| c + 1);

        let premint_secrets = PreMintSecrets::from_xpriv_blank(
            active_keyset_id,
            count,
            self.xpriv,
            proofs_total - quote_info.amount,
        )?;

        let melt_response = match quote_info.payment_method {
            PaymentMethod::Bolt11 => {
                self.client
                    .post_melt(
                        self.mint_url.clone().try_into()?,
                        quote_id.to_string(),
                        proofs.clone(),
                        Some(premint_secrets.blinded_messages()),
                    )
                    .await
            }
            PaymentMethod::Bolt12 => {
                self.client
                    .post_melt_bolt12(
                        self.mint_url.clone().try_into()?,
                        quote_id.to_string(),
                        proofs.clone(),
                        Some(premint_secrets.blinded_messages()),
                    )
                    .await
            }
        };

        let melt_response = match melt_response {
            Ok(melt_response) => melt_response,
            Err(err) => {
                tracing::error!("Could not melt: {}", err);
                tracing::info!("Checking status of input proofs.");

                self.reclaim_unspent(proofs).await?;

                return Err(err);
            }
        };

        let active_keys = self
            .localstore
            .get_keys(&active_keyset_id)
            .await?
            .ok_or(Error::NoActiveKeyset)?;

        let change_proofs = match melt_response.change {
            Some(change) => {
                let num_change_proof = change.len();

                let num_change_proof = match (
                    premint_secrets.len() < num_change_proof,
                    premint_secrets.secrets().len() < num_change_proof,
                ) {
                    (true, _) | (_, true) => {
                        tracing::error!("Mismatch in change promises to change");
                        premint_secrets.len()
                    }
                    _ => num_change_proof,
                };

                Some(construct_proofs(
                    change,
                    premint_secrets.rs()[..num_change_proof].to_vec(),
                    premint_secrets.secrets()[..num_change_proof].to_vec(),
                    &active_keys,
                )?)
            }
            None => None,
        };

        let melted = Melted::from_proofs(
            melt_response.state,
            melt_response.payment_preimage,
            quote_info.amount,
            proofs.clone(),
            change_proofs.clone(),
        )?;

        let change_proof_infos = match change_proofs {
            Some(change_proofs) => {
                tracing::debug!(
                    "Change amount returned from melt: {}",
                    Amount::try_sum(change_proofs.iter().map(|p| p.amount))?
                );

                // Update counter for keyset
                self.localstore
                    .increment_keyset_counter(&active_keyset_id, change_proofs.len() as u32)
                    .await?;

                change_proofs
                    .into_iter()
                    .map(|proof| {
                        ProofInfo::new(
                            proof,
                            self.mint_url.clone(),
                            State::Unspent,
                            quote_info.unit,
                        )
                    })
                    .collect::<Result<Vec<ProofInfo>, _>>()?
            }
            None => Vec::new(),
        };

        self.localstore.remove_melt_quote(&quote_info.id).await?;

        let deleted_ys = proofs
            .iter()
            .map(|p| p.y())
            .collect::<Result<Vec<PublicKey>, _>>()?;
        self.localstore
            .update_proofs(change_proof_infos, deleted_ys)
            .await?;

        Ok(melted)
    }

    /// Melt
    /// # Synopsis
    /// ```rust, no_run
    ///  use std::sync::Arc;
    ///
    ///  use cdk::cdk_database::WalletMemoryDatabase;
    ///  use cdk::nuts::CurrencyUnit;
    ///  use cdk::wallet::Wallet;
    ///  use rand::Rng;
    ///
    /// #[tokio::main]
    /// async fn main() -> anyhow::Result<()> {
    ///  let seed = rand::thread_rng().gen::<[u8; 32]>();
    ///  let mint_url = "https://testnut.cashu.space";
    ///  let unit = CurrencyUnit::Sat;
    ///
    ///  let localstore = WalletMemoryDatabase::default();
    ///  let wallet = Wallet::new(mint_url, unit, Arc::new(localstore), &seed, None).unwrap();
    ///  let bolt11 = "lnbc100n1pnvpufspp5djn8hrq49r8cghwye9kqw752qjncwyfnrprhprpqk43mwcy4yfsqdq5g9kxy7fqd9h8vmmfvdjscqzzsxqyz5vqsp5uhpjt36rj75pl7jq2sshaukzfkt7uulj456s4mh7uy7l6vx7lvxs9qxpqysgqedwz08acmqwtk8g4vkwm2w78suwt2qyzz6jkkwcgrjm3r3hs6fskyhvud4fan3keru7emjm8ygqpcrwtlmhfjfmer3afs5hhwamgr4cqtactdq".to_string();
    ///  let quote = wallet.melt_quote(bolt11, None).await?;
    ///  let quote_id = quote.id;
    ///
    ///  let _ = wallet.melt(&quote_id).await?;
    ///
    ///  Ok(())
    /// }
    #[instrument(skip(self))]
    pub async fn melt(&self, quote_id: &str) -> Result<Melted, Error> {
        let quote_info = self.localstore.get_melt_quote(quote_id).await?;

        let quote_info = if let Some(quote) = quote_info {
            if quote.expiry.le(&unix_time()) {
                return Err(Error::ExpiredQuote(quote.expiry, unix_time()));
            }

            quote.clone()
        } else {
            return Err(Error::UnknownQuote);
        };

        let inputs_needed_amount = quote_info.amount + quote_info.fee_reserve;

        let available_proofs = self.get_proofs().await?;

        let input_proofs = self
            .select_proofs_to_swap(inputs_needed_amount, available_proofs)
            .await?;

        self.melt_proofs(quote_id, input_proofs).await
    }
}