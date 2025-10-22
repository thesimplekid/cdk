use std::sync::Arc;

use axum::extract::State;
use axum::response::Response;
use axum::Json;
use portal::nostr::key::Keys;
use portal::protocol::LocalKeypair;

use crate::MintState;

pub(crate) async fn get_portal_auth(
    State(state): State<MintState>,
) -> Result<Json<String>, Response> {
    tracing::info!("Got portal auth");

    let p = state.portal;

    let (handshake, mut stream) = p.new_key_handshake_url(None, None).await.unwrap();

    let p_key = state.portal_key;

    let p_clone = Arc::clone(&p);

    tokio::spawn(async move {
        loop {
            if let Some(t) = stream.next().await {
                let t = t.unwrap();

                let t = t.main_key;

                let mut key = p_key.lock().await;
                *key = Some(t);

                p_clone.authenticate_key(t, vec![]).await.unwrap();
            }
        }
    });

    Ok(Json(handshake.to_string()))
}
