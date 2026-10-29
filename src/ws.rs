//! WebSocket tunnelling (used by the notifications service).
//!
//! Bitwarden clients keep a SignalR WebSocket open against `/notifications/hub`.
//! The proxy performs a handshake with the upstream and relays messages
//! bidirectionally, so live sync keeps working.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::FromRequestParts;
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as UpstreamMessage;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use tracing::{debug, warn};

use crate::proxy::AppState;

type UpstreamSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Does this request ask for a WebSocket upgrade?
pub fn is_websocket_upgrade(headers: &HeaderMap) -> bool {
    fn contains(headers: &HeaderMap, name: header::HeaderName, needle: &str) -> bool {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(|value| {
                value
                    .split(',')
                    .any(|token| token.trim().eq_ignore_ascii_case(needle))
            })
            .unwrap_or(false)
    }

    contains(headers, header::UPGRADE, "websocket")
        && contains(headers, header::CONNECTION, "upgrade")
}

pub async fn proxy(state: Arc<AppState>, req: Request<Body>) -> Response {
    let (mut parts, _body) = req.into_parts();

    let upgrade = match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
        Ok(upgrade) => upgrade,
        Err(rejection) => return rejection.into_response(),
    };

    let path_and_query = parts
        .uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");
    let target = match state.upstream.resolve_websocket(path_and_query) {
        Ok(target) => target,
        Err(error) => {
            warn!(%error, "could not resolve upstream WebSocket URL");
            return (StatusCode::BAD_GATEWAY, "invalid upstream URL\n").into_response();
        }
    };

    // Perform the upstream handshake before answering the client. If the
    // upstream refuses the connection the client gets a proper HTTP error
    // instead of a socket that closes immediately.
    let upstream = match connect(&target, &parts.headers).await {
        Ok(socket) => socket,
        Err(error) => {
            warn!(%target, %error, "upstream WebSocket handshake failed");
            return (StatusCode::BAD_GATEWAY, "upstream WebSocket failed\n").into_response();
        }
    };

    debug!(%target, "tunnelling WebSocket");
    upgrade.on_upgrade(move |client| bridge(client, upstream))
}

async fn connect(
    target: &url::Url,
    headers: &HeaderMap,
) -> Result<UpstreamSocket, Box<dyn std::error::Error + Send + Sync>> {
    let mut request = target.as_str().into_client_request()?;
    {
        let outgoing = request.headers_mut();
        for (name, value) in headers {
            if should_not_forward(name) {
                continue;
            }
            outgoing.insert(name.clone(), value.clone());
        }
        // The handshake headers (Host, Connection, Upgrade, Sec-WebSocket-*)
        // were generated for the upstream URL; keep those.
    }

    let (socket, response) = connect_async(request).await?;
    debug!(status = %response.status(), "upstream WebSocket established");
    Ok(socket)
}

fn should_not_forward(name: &header::HeaderName) -> bool {
    matches!(
        name.as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "proxy-connection"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "host"
            | "content-length"
            | "accept-encoding"
            | "origin"
            | "sec-websocket-key"
            | "sec-websocket-version"
            | "sec-websocket-extensions"
            | "sec-websocket-protocol"
    )
}

async fn bridge(mut client: WebSocket, mut upstream: UpstreamSocket) {
    loop {
        tokio::select! {
            message = client.recv() => {
                match message {
                    Some(Ok(Message::Text(text))) => {
                        if upstream
                            .send(UpstreamMessage::Text(text.as_str().into()))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Some(Ok(Message::Binary(bytes))) => {
                        if upstream.send(UpstreamMessage::Binary(bytes)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        let _ = upstream.close(None).await;
                        break;
                    }
                    // Ping/pong are answered automatically on each side.
                    Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => {}
                    Some(Err(error)) => {
                        debug!(%error, "client WebSocket error");
                        break;
                    }
                }
            }
            message = upstream.next() => {
                match message {
                    Some(Ok(UpstreamMessage::Text(text))) => {
                        if client.send(Message::Text(text.as_str().into())).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(UpstreamMessage::Binary(bytes))) => {
                        if client.send(Message::Binary(bytes)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(UpstreamMessage::Close(_))) | None => {
                        let _ = client.send(Message::Close(None)).await;
                        break;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(error)) => {
                        debug!(%error, "upstream WebSocket error");
                        break;
                    }
                }
            }
        }
    }

    let _ = upstream.close(None).await;
    let _ = client.send(Message::Close(None)).await;
}
