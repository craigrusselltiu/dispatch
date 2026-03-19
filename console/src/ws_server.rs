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

// --- Server entry point --------------------------------------------------

/// Start the WebSocket server on `0.0.0.0:{port}` with TLS.
/// Accepts connections only when the `?psk=<key>` query parameter matches.
pub async fn run_server(state: SharedState, port: u16, psk: String, tls: TlsAcceptor) {
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
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
        tokio::spawn(async move {
            let tls_stream = match tls.accept(stream).await {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("ws: TLS handshake failed from {peer_addr}: {e}");
                    return;
                }
            };
            if let Err(e) = handle_connection(tls_stream, peer_addr, state, psk).await {
                eprintln!("ws: connection error from {peer_addr}: {e}");
            }
        });
    }
}

// --- Connection handler --------------------------------------------------

async fn handle_connection<S: AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    peer_addr: SocketAddr,
    state: SharedState,
    psk: String,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut auth_ok = false;

    let result = accept_hdr_async(stream, |req: &Request, resp: Response| {
        let valid = req
            .uri()
            .query()
            .unwrap_or("")
            .split('&')
            .any(|part| part == format!("psk={}", psk).as_str());

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
        Err(e) => {
            if !auth_ok {
                eprintln!("ws: rejected {peer_addr}: invalid PSK");
            } else {
                eprintln!("ws: handshake error from {peer_addr}: {e}");
            }
            return Ok(());
        }
    };

    let (mut tx, mut rx) = ws_stream.split();

    while let Some(msg) = rx.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(_) => break,
        };

        let text = match msg {
            Message::Text(t) => t,
            Message::Close(_) => break,
            _ => continue,
        };

        let raw: RawInbound = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue, // silently ignore malformed JSON
        };

        if let Some(response) = handle_message(raw, &state) {
            let json = serde_json::to_string(&response)?;
            tx.send(Message::Text(json)).await?;
        }
    }

    Ok(())
}
