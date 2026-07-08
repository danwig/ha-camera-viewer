use tokio_tungstenite::connect_async;
use futures_util::{SinkExt, StreamExt};

pub async fn run(ha_url: String, ha_token: String) {
    let ws_url = ha_url.trim_end_matches('/')
        .replacen("http", "ws", 1)
        .to_string() + "/api/websocket";

    loop {
        match connect_async(&ws_url).await {
            Ok((mut ws, _)) => {
                eprintln!("[HA WS] Connected");
                loop {
                    match ws.next().await {
                        Some(Ok(msg)) => {
                            if let Ok(text) = msg.to_text() {
                                if let Ok(json) = serde_json::from_str::<serde_json::Value>(text) {
                                    match json["type"].as_str() {
                                        Some("auth_required") => {
                                            let auth = serde_json::json!({
                                                "type": "auth",
                                                "access_token": ha_token
                                            });
                                            if ws.send(tokio_tungstenite::tungstenite::Message::Text(
                                                auth.to_string().into()
                                            )).await.is_err() {
                                                break;
                                            }
                                        }
                                        Some("auth_ok") => {
                                            eprintln!("[HA WS] Auth OK");
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                        _ => break,
                    }
                }
                eprintln!("[HA WS] Disconnected, reconnecting in 15s");
            }
            Err(e) => {
                eprintln!("[HA WS] Connect failed: {}", e);
            }
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(15)).await;
    }
}
