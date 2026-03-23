// WebSocket server for the Dispatch console.

use std::net::SocketAddr;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::{
    accept_hdr_async,
    tungstenite::{
        handshake::server::{Request, Response},
        Message,
    },
};

// Re-export core types so main.rs can keep using ws_server::* paths.
pub use dispatch_core::handler::{
    AgentSlot, AgentStatus, ConsoleState, SharedState, WsEvent,
};
use dispatch_core::handler::handle_message;
use dispatch_core::protocol::RawInbound;

/// Broadcast sender for pushing unsolicited messages (chat) to all connected clients.
pub type ChatBroadcast = tokio::sync::broadcast::Sender<String>;

// --- Server entry point --------------------------------------------------

/// Start the WebSocket server on `{bind}:{port}` with TLS.
/// Accepts connections only when the `?psk=<key>` query parameter matches.
pub async fn run_server(state: SharedState, bind: String, port: u16, psk: String, tls: TlsAcceptor, chat_tx: ChatBroadcast) {
    let addr: SocketAddr = format!("{bind}:{port}")
        .parse()
        .expect("invalid bind address");
    let listener = TcpListener::bind(addr)
        .await
        .expect("failed to bind WebSocket server");

    loop {
        let (stream, peer_addr) = match listener.accept().await {
            Ok(v) => v,
            Err(_) => continue,
        };
        let state = Arc::clone(&state);
        let psk = psk.clone();
        let tls = tls.clone();
        let chat_rx = chat_tx.subscribe();
        tokio::spawn(async move {
            let tls_stream = match tls.accept(stream).await {
                Ok(s) => s,
                Err(e) => {
                    let st = state.lock().unwrap();
                    if let Some(tx) = &st.event_tx {
                        let _ = tx.send(WsEvent::TlsError {
                            addr: peer_addr.to_string(),
                            error: e.to_string(),
                        });
                    }
                    return;
                }
            };
            let _ = handle_connection(tls_stream, peer_addr, state, psk, chat_rx).await;
        });
    }
}

// --- Connection handler --------------------------------------------------

async fn handle_connection<S: AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    peer_addr: SocketAddr,
    state: SharedState,
    psk: String,
    mut chat_rx: tokio::sync::broadcast::Receiver<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut auth_ok = false;

    let result = accept_hdr_async(stream, |req: &Request, resp: Response| {
        let expected = format!("psk={}", psk);
        let valid = req
            .uri()
            .query()
            .unwrap_or("")
            .split('&')
            .any(|part| part.eq_ignore_ascii_case(&expected));

        if valid {
            auth_ok = true;
            Ok(resp)
        } else {
            use tokio_tungstenite::tungstenite::http;
            let err = http::Response::builder()
                .status(http::StatusCode::UNAUTHORIZED)
                .body(None)
                .unwrap();
            Err(err)
        }
    })
    .await;

    let ws_stream = match result {
        Ok(ws) => ws,
        Err(_) => {
            if !auth_ok {
                // Route through the TUI event channel instead of eprintln,
                // which would corrupt the alternate-screen rendering.
                let st = state.lock().unwrap();
                if let Some(tx) = &st.event_tx {
                    let _ = tx.send(WsEvent::InvalidPsk { addr: peer_addr.to_string() });
                }
            }
            return Ok(());
        }
    };

    let (mut tx, mut rx) = ws_stream.split();

    loop {
        tokio::select! {
            ws_msg = rx.next() => {
                let msg = match ws_msg {
                    Some(Ok(m)) => m,
                    _ => break,
                };

                if let Message::Close(_) = msg {
                    break;
                }
                let text = match msg {
                    Message::Text(t) => t,
                    _ => { continue; },
                };

                let raw: RawInbound = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(_) => { continue; },
                };

                if let Some(response) = handle_message(raw, &state) {
                    let json = serde_json::to_string(&response)?;
                    tx.send(Message::Text(json)).await?;
                }
            }
            chat_msg = chat_rx.recv() => {
                match chat_msg {
                    Ok(json) => {
                        if tx.send(Message::Text(json)).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        // Slow client missed some messages — continue.
                    }
                    Err(_) => break,
                }
            }
        }
    }

    #[allow(unreachable_code)]
    Ok(())
}
