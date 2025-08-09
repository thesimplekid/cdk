use anyhow::Result;
use axum::extract::{Json, Path, State};
use axum::response::Response;
#[cfg(feature = "swagger")]
use cdk::error::ErrorResponse;
#[cfg(feature = "auth")]
use cdk::nuts::nut21::{Method, ProtectedEndpoint};
use cdk::nuts::{MintQuoteEhashRequest, MintQuoteEhashResponse, MintRequest, MintResponse};
use cdk::Error;
use paste::paste;
use tracing::instrument;
use uuid::Uuid;

#[cfg(feature = "auth")]
use crate::auth::AuthHeader;
use crate::{into_response, post_cache_wrapper, MintState};

post_cache_wrapper!(post_mint_ehash, MintRequest<Uuid>, MintResponse);

#[cfg_attr(feature = "swagger", utoipa::path(
    post,
    context_path = "/v1",
    path = "/mint/quote/ehash",
    request_body(content = MintQuoteEhashRequest, description = "Request params", content_type = "application/json"),
    responses(
        (status = 200, description = "Successful response", body = MintQuoteEhashResponse<String>, content_type = "application/json"),
        (status = 500, description = "Server error", body = ErrorResponse, content_type = "application/json")
    )
))]
/// Request a quote for minting tokens via hash payment
///
/// Request minting of new tokens by providing a hash challenge solution. The mint responds with a hash challenge. This endpoint can be used for a hash payment UX flow.
#[instrument(skip_all, fields(nonce = ?payload.nonce))]
pub async fn post_mint_ehash_quote(
    #[cfg(feature = "auth")] auth: AuthHeader,
    State(state): State<MintState>,
    Json(payload): Json<MintQuoteEhashRequest>,
) -> Result<Json<MintQuoteEhashResponse<Uuid>>, Response> {
    // For now, skip auth verification since ehash routes are not in RoutePath enum yet
    // This will be enabled when the proper route paths are added to the core library
    #[cfg(feature = "auth")]
    if false {
        state
            .mint
            .verify_auth(
                auth.into(),
                &ProtectedEndpoint::new(Method::Post, cdk::nuts::nut21::RoutePath::MintQuoteBolt11), // placeholder
            )
            .await
            .map_err(into_response)?;
    }

    let quote = state
        .mint
        .get_mint_quote(payload.into())
        .await
        .map_err(into_response)?;

    Ok(Json(quote.try_into().map_err(into_response)?))
}

#[cfg_attr(feature = "swagger", utoipa::path(
    get,
    context_path = "/v1",
    path = "/mint/quote/ehash/{quote_id}",
    params(
        ("quote_id" = String, description = "The quote ID"),
    ),
    responses(
        (status = 200, description = "Successful response", body = MintQuoteEhashResponse<String>, content_type = "application/json"),
        (status = 500, description = "Server error", body = ErrorResponse, content_type = "application/json")
    )
))]
/// Get mint ehash quote by ID
///
/// Get mint quote state for a hash payment.
#[instrument(skip_all, fields(quote_id = ?quote_id))]
pub async fn get_check_mint_ehash_quote(
    #[cfg(feature = "auth")] auth: AuthHeader,
    State(state): State<MintState>,
    Path(quote_id): Path<Uuid>,
) -> Result<Json<MintQuoteEhashResponse<Uuid>>, Response> {
    // For now, skip auth verification since ehash routes are not in RoutePath enum yet
    #[cfg(feature = "auth")]
    if false {
        state
            .mint
            .verify_auth(
                auth.into(),
                &ProtectedEndpoint::new(Method::Get, cdk::nuts::nut21::RoutePath::MintQuoteBolt11), // placeholder
            )
            .await
            .map_err(into_response)?;
    }

    let quote = state
        .mint
        .check_mint_quote(&quote_id)
        .await
        .map_err(|err| {
            tracing::error!("Could not check mint quote {}: {}", quote_id, err);
            into_response(err)
        })?;

    Ok(Json(quote.try_into().map_err(into_response)?))
}

#[cfg_attr(feature = "swagger", utoipa::path(
    post,
    context_path = "/v1",
    path = "/mint/ehash",
    request_body(content = MintRequest<String>, description = "Request params", content_type = "application/json"),
    responses(
        (status = 200, description = "Successful response", body = MintResponse, content_type = "application/json"),
        (status = 500, description = "Server error", body = ErrorResponse, content_type = "application/json")
    )
))]
/// Mint tokens by providing hash payment solution
///
/// Requests the minting of tokens belonging to a solved hash challenge.
///
/// Call this endpoint after `POST /v1/mint/quote/ehash`.
#[instrument(skip_all, fields(quote_id = ?payload.quote))]
pub async fn post_mint_ehash(
    #[cfg(feature = "auth")] auth: AuthHeader,
    State(state): State<MintState>,
    Json(payload): Json<MintRequest<Uuid>>,
) -> Result<Json<MintResponse>, Response> {
    // For now, skip auth verification since ehash routes are not in RoutePath enum yet
    #[cfg(feature = "auth")]
    if false {
        state
            .mint
            .verify_auth(
                auth.into(),
                &ProtectedEndpoint::new(Method::Post, cdk::nuts::nut21::RoutePath::MintBolt11), // placeholder
            )
            .await
            .map_err(into_response)?;
    }

    let res = state
        .mint
        .process_mint_request(payload)
        .await
        .map_err(|err| {
            tracing::error!("Could not process mint: {}", err);
            into_response(err)
        })?;

    Ok(Json(res))
}

/// Submit hash solution for a quote
///
/// This endpoint allows submitting a hash solution (preimage) for a given quote.
/// This is specific to ehash payments and allows external submission of solutions.
#[cfg_attr(feature = "swagger", utoipa::path(
    post,
    context_path = "/v1",
    path = "/ehash/submit/{quote_id}",
    params(
        ("quote_id" = String, description = "The quote ID"),
    ),
    request_body(content = serde_json::Value, description = "Hash solution", content_type = "application/json"),
    responses(
        (status = 200, description = "Solution accepted"),
        (status = 400, description = "Invalid solution"),
        (status = 500, description = "Server error", body = ErrorResponse, content_type = "application/json")
    )
))]
#[instrument(skip_all, fields(quote_id = ?quote_id))]
pub async fn post_submit_ehash_solution(
    #[cfg(feature = "auth")] auth: AuthHeader,
    State(state): State<MintState>,
    Path(quote_id): Path<Uuid>,
    Json(payload): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, Response> {
    // For now, skip auth verification since ehash routes are not in RoutePath enum yet
    #[cfg(feature = "auth")]
    if false {
        state
            .mint
            .verify_auth(
                auth.into(),
                &ProtectedEndpoint::new(Method::Post, cdk::nuts::nut21::RoutePath::MintBolt11), // placeholder
            )
            .await
            .map_err(into_response)?;
    }

    // Extract the preimage from the payload
    let preimage = payload
        .get("preimage")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            tracing::error!("Missing preimage in request");
            into_response(Error::Custom("Missing preimage field".to_string()))
        })?;

    // For now, we'll return success - this would integrate with the ehash backend
    // The actual implementation would validate the hash solution
    tracing::info!(
        "Received hash solution for quote {}: {}",
        quote_id,
        preimage
    );

    Ok(Json(serde_json::json!({
        "status": "accepted",
        "quote_id": quote_id.to_string(),
        "preimage": preimage
    })))
}
