//! End-to-end tests: a fake Bitwarden upstream, the proxy, and an HTTP client.

use std::io;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ws::{WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, Request};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use bitwarden_proxy::{router, AppState, Config};
use bytes::Bytes;
use futures_util::stream;
use serde_json::{json, Value};

const PROXY_ORIGIN: &str = "https://bw.example.com";

const CONFIG_JSON: &str = r#"{
  "version": "2026.10.1",
  "gitHash": "3edee6bea2a7dbe0bc8d6deefb46553facee9552",
  "server": null,
  "environment": {
    "cloudRegion": "US",
    "vault": "https://vault.bitwarden.com",
    "api": "https://api.bitwarden.com",
    "identity": "https://identity.bitwarden.com",
    "notifications": "https://notifications.bitwarden.com",
    "sso": "https://sso.bitwarden.com",
    "fillAssistRules": "https://fillassist.bitwarden.com"
  }
}"#;

const PROFILE_JSON: &str = r#"{
  "id": "user-id",
  "email": "user@example.com",
  "premium": false,
  "premiumFromOrganization": false,
  "organizations": [
    {"id": "org-1", "name": "Org", "usersGetPremium": false}
  ],
  "organizationsNew": [
    {"id": "org-2", "name": "New Org", "usersGetPremium": false}
  ]
}"#;

fn sync_json() -> String {
    let ciphers: Vec<Value> = (0..200)
        .map(|i| {
            json!({
                "id": format!("cipher-{i}"),
                "name": format!("Item {i}"),
                "notes": "some notes with \"quotes\" and } braces {",
                "login": {"username": "user", "password": "secret"},
            })
        })
        .collect();
    json!({
        "profile": {
            "id": "user-id",
            "premium": false,
            "premiumFromOrganization": false,
            "email": "user@example.com"
        },
        "folders": [],
        "ciphers": ciphers,
    })
    .to_string()
}

/// Stream a body in small chunks so token boundaries are split across frames.
fn chunked_body(text: String, chunk_size: usize) -> Body {
    let chunks: Vec<Result<Bytes, io::Error>> = text
        .into_bytes()
        .chunks(chunk_size)
        .map(|chunk| Ok(Bytes::copy_from_slice(chunk)))
        .collect();
    Body::from_stream(stream::iter(chunks))
}

async fn upstream(request: Request) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let query = request.uri().query().unwrap_or_default().to_owned();
    let headers = request.headers().clone();

    if path == "/notifications/hub" {
        let (mut parts, _body) = request.into_parts();
        let upgrade = WebSocketUpgrade::from_request_parts(&mut parts, &())
            .await
            .expect("valid websocket handshake");
        return upgrade.on_upgrade(echo_socket);
    }

    let body = axum::body::to_bytes(request.into_body(), usize::MAX)
        .await
        .unwrap();

    match path.as_str() {
        "/api/config" => {
            let mut response = json_response(CONFIG_JSON, 9);
            response.headers_mut().insert(
                "x-seen-accept-encoding",
                headers
                    .get(header::ACCEPT_ENCODING)
                    .cloned()
                    .unwrap_or_else(|| header::HeaderValue::from_static("<none>")),
            );
            response
        }
        "/api/accounts/profile" => json_response(PROFILE_JSON, 13),
        "/api/sync" => json_response(&sync_json(), 64),
        "/api/notfound" => {
            (StatusCode::NOT_FOUND, axum::Json(json!({"premium": false}))).into_response()
        }
        "/api/encoded" => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::CONTENT_ENCODING, "gzip")
            .body(Body::from(r#"{"premium":false}"#))
            .unwrap(),
        "/api/plain" => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Body::from("premium: false"))
            .unwrap(),
        "/api/echo" => axum::Json(json!({
            "method": method.as_str(),
            "path": path,
            "query": query,
            "authorization": headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()),
            "accept_encoding": headers.get(header::ACCEPT_ENCODING).and_then(|v| v.to_str().ok()),
            "body": String::from_utf8_lossy(&body),
        }))
        .into_response(),
        "/api/cookies" => Response::builder()
            .status(StatusCode::OK)
            .header(header::SET_COOKIE, "a=1; Path=/")
            .header(header::SET_COOKIE, "b=2; Path=/")
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Body::from("ok"))
            .unwrap(),
        _ => (StatusCode::NOT_FOUND, "unknown upstream path").into_response(),
    }
}

async fn echo_socket(mut socket: WebSocket) {
    while let Some(Ok(message)) = socket.recv().await {
        if socket.send(message).await.is_err() {
            break;
        }
    }
}

fn json_response(text: &str, chunk_size: usize) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
        .body(chunked_body(text.to_owned(), chunk_size))
        .unwrap()
}

async fn spawn(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{address}")
}

struct Harness {
    proxy: String,
    _upstream: String,
}

async fn harness() -> Harness {
    let upstream = spawn(Router::new().fallback(any(upstream))).await;
    let config = Config::new(&upstream, PROXY_ORIGIN, "127.0.0.1:0").unwrap();
    let state = AppState::new(&config).unwrap();
    let proxy = spawn(router(Arc::clone(&state))).await;
    Harness {
        proxy,
        _upstream: upstream,
    }
}

async fn get_json(url: &str) -> (StatusCode, HeaderMap, Value) {
    let response = reqwest::get(url).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.text().await.unwrap();
    (status, headers, serde_json::from_str(&body).unwrap())
}

#[tokio::test]
async fn rewrites_config() {
    let harness = harness().await;
    let (status, _headers, body) = get_json(&format!("{}/api/config", harness.proxy)).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["environment"]["vault"], PROXY_ORIGIN);
    assert_eq!(body["environment"]["api"], format!("{PROXY_ORIGIN}/api"));
    assert_eq!(
        body["environment"]["identity"],
        format!("{PROXY_ORIGIN}/identity")
    );
    assert_eq!(
        body["environment"]["notifications"],
        format!("{PROXY_ORIGIN}/notifications")
    );
    assert_eq!(body["environment"]["cloudRegion"], "US");
    assert_eq!(body["version"], "2026.10.1");
}

#[tokio::test]
async fn rewrites_profile() {
    let harness = harness().await;
    let (status, _headers, body) =
        get_json(&format!("{}/api/accounts/profile", harness.proxy)).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["premium"], true);
    assert_eq!(body["premiumFromOrganization"], true);
    assert_eq!(body["organizations"][0]["usersGetPremium"], true);
    assert_eq!(body["organizationsNew"][0]["usersGetPremium"], true);
    assert_eq!(body["email"], "user@example.com");
}

#[tokio::test]
async fn rewrites_sync_and_preserves_large_content() {
    let harness = harness().await;
    let (status, _headers, body) = get_json(&format!("{}/api/sync", harness.proxy)).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["profile"]["premium"], true);
    assert_eq!(body["profile"]["premiumFromOrganization"], true);
    assert_eq!(body["ciphers"].as_array().unwrap().len(), 200);
    assert_eq!(body["ciphers"][199]["name"], "Item 199");
    assert_eq!(
        body["ciphers"][42]["notes"],
        "some notes with \"quotes\" and } braces {"
    );
}

#[tokio::test]
async fn forwards_unmodified_responses() {
    let harness = harness().await;

    let response = reqwest::get(format!("{}/api/plain?x=1", harness.proxy))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.text().await.unwrap(), "premium: false");
}

#[tokio::test]
async fn does_not_rewrite_error_responses() {
    let harness = harness().await;
    let (status, _headers, body) = get_json(&format!("{}/api/notfound", harness.proxy)).await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["premium"], false);
}

#[tokio::test]
async fn does_not_rewrite_compressed_responses() {
    let harness = harness().await;
    let response = reqwest::get(format!("{}/api/encoded", harness.proxy))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_ENCODING)
            .and_then(|value| value.to_str().ok()),
        Some("gzip")
    );
    assert_eq!(response.text().await.unwrap(), r#"{"premium":false}"#);
}

#[tokio::test]
async fn forwards_method_headers_body_and_query() {
    let harness = harness().await;
    let response = reqwest::Client::new()
        .post(format!("{}/api/echo?a=b&c=d", harness.proxy))
        .header(header::AUTHORIZATION, "Bearer token")
        .body("hello upstream")
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
    assert_eq!(body["method"], "POST");
    assert_eq!(body["path"], "/api/echo");
    assert_eq!(body["query"], "a=b&c=d");
    assert_eq!(body["authorization"], "Bearer token");
    assert_eq!(body["body"], "hello upstream");
}

#[tokio::test]
async fn tunnels_websockets() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let harness = harness().await;
    let (mut socket, response) = tokio_tungstenite::connect_async(format!(
        "{}/notifications/hub?access_token=abc",
        harness.proxy.replace("http://", "ws://")
    ))
    .await
    .expect("websocket handshake through the proxy");
    assert_eq!(response.status(), 101);

    socket
        .send(Message::Text("hello from client".into()))
        .await
        .unwrap();
    let echoed = socket.next().await.unwrap().unwrap();
    assert_eq!(echoed, Message::Text("hello from client".into()));

    socket.close(None).await.unwrap();
}

#[tokio::test]
async fn requests_identity_encoding_only_for_rewritten_paths() {
    let harness = harness().await;
    let client = reqwest::Client::new();

    let rewritten = client
        .get(format!("{}/api/config", harness.proxy))
        .header(header::ACCEPT_ENCODING, "gzip, br")
        .send()
        .await
        .unwrap();
    assert_eq!(
        rewritten
            .headers()
            .get("x-seen-accept-encoding")
            .and_then(|value| value.to_str().ok()),
        Some("identity")
    );

    let passthrough = client
        .get(format!("{}/api/echo", harness.proxy))
        .header(header::ACCEPT_ENCODING, "gzip, br")
        .send()
        .await
        .unwrap();
    let body: Value = serde_json::from_str(&passthrough.text().await.unwrap()).unwrap();
    assert_eq!(body["accept_encoding"], "gzip, br");
}

#[tokio::test]
async fn preserves_multiple_set_cookie_headers() {
    let harness = harness().await;
    let response = reqwest::get(format!("{}/api/cookies", harness.proxy))
        .await
        .unwrap();

    let cookies: Vec<_> = response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .collect();
    assert_eq!(cookies.len(), 2);
    assert_eq!(cookies[0].to_str().unwrap(), "a=1; Path=/");
    assert_eq!(cookies[1].to_str().unwrap(), "b=2; Path=/");
}
