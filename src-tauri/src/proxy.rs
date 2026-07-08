use axum::{body::Body, extract::State, response::IntoResponse, Router};
use reqwest::Client;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::config::Config;

type SharedConfig = Arc<Mutex<Config>>;

pub async fn start(config: SharedConfig) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("proxy bind");
    let port = listener.local_addr().unwrap().port();

    let app = Router::new()
        .fallback(proxy_handler)
        .with_state(config);

    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    eprintln!("[Proxy] Listening on :{}", port);
    port
}

async fn proxy_handler(
    State(shared_config): State<SharedConfig>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let (ha_url, ha_token) = {
        let cfg = shared_config.lock().await;
        (
            cfg.ha_url.trim_end_matches('/').to_string(),
            cfg.ha_token.clone(),
        )
    };

    let target = format!("{}{}", ha_url, req.uri());

    let client = Client::new();
    let resp = client
        .request(req.method().clone(), &target)
        .header("Authorization", format!("Bearer {}", ha_token))
        .send()
        .await;

    match resp {
        Ok(r) => {
            let status = axum::http::StatusCode::from_u16(r.status().as_u16())
                .unwrap_or(axum::http::StatusCode::OK);
            let mut headers = axum::http::HeaderMap::new();
            for (name, value) in r.headers() {
                if name.as_str() != "transfer-encoding" {
                    if let Ok(n) = axum::http::HeaderName::try_from(name.as_str()) {
                        headers.insert(n, value.clone());
                    }
                }
            }
            let body = Body::from_stream(r.bytes_stream());
            (status, headers, body).into_response()
        }
        Err(_) => (axum::http::StatusCode::BAD_GATEWAY, "proxy error").into_response(),
    }
}
