//! HTTP proxying and response rewriting.

use std::io;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Router;
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use tracing::{debug, warn};

use crate::config::Config;
use crate::landing;
use crate::rewrite::{config_rules, profile_rules, sync_rules, JsonRewriter, RuleSet};
use crate::routes::Upstream;
use crate::ws;

/// Shared state for every request.
pub struct AppState {
    client: reqwest::Client,
    pub(crate) upstream: Upstream,
    config_rules: Arc<RuleSet>,
    profile_rules: Arc<RuleSet>,
    sync_rules: Arc<RuleSet>,
    /// Pre-rendered landing page, or `None` when it is disabled.
    landing: Option<String>,
}

impl AppState {
    pub fn new(config: &Config) -> anyhow::Result<Arc<Self>> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_nodelay(true)
            // Redirects are passed through to the client: following them here
            // would hide the final URL and could bypass response rewriting.
            .redirect(reqwest::redirect::Policy::none())
            .build()?;

        Ok(Arc::new(Self {
            client,
            upstream: config.server.clone(),
            config_rules: Arc::new(config_rules(&config.proxy_url, &config.features)),
            profile_rules: Arc::new(profile_rules()),
            sync_rules: Arc::new(sync_rules()),
            landing: config.root_page.then(|| landing::render(config)),
        }))
    }
}

/// Build the axum router. Every path is proxied, so a fallback is enough.
pub fn router(state: Arc<AppState>) -> Router {
    Router::new().fallback(handle).with_state(state)
}

async fn handle(State(state): State<Arc<AppState>>, req: Request<Body>) -> Response {
    if req.uri().path() == "/" {
        return root_response(&state, req.method());
    }
    if ws::is_websocket_upgrade(req.headers()) {
        return ws::proxy(state, req).await;
    }
    proxy_http(state, req).await
}

/// `/` is never proxied: it either serves the instructions page or a 404,
/// depending on `BITWARDEN_PROXY_ROOT_PAGE` / `--root-page`.
fn root_response(state: &AppState, method: &Method) -> Response {
    let Some(page) = &state.landing else {
        return plain_error(StatusCode::NOT_FOUND, "not found");
    };

    if method != Method::GET && method != Method::HEAD {
        let mut response = plain_error(StatusCode::METHOD_NOT_ALLOWED, "method not allowed");
        response
            .headers_mut()
            .insert(header::ALLOW, header::HeaderValue::from_static("GET, HEAD"));
        return response;
    }

    let mut response = Response::new(Body::from(page.clone()));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

async fn proxy_http(state: Arc<AppState>, req: Request<Body>) -> Response {
    let path = req.uri().path().to_owned();
    let path_and_query = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or(&path)
        .to_owned();

    let target = match state.upstream.resolve(&path_and_query) {
        Ok(target) => target,
        Err(error) => {
            warn!(%path, %error, "could not resolve upstream URL");
            return plain_error(StatusCode::BAD_GATEWAY, "invalid upstream URL");
        }
    };

    // Response rewriting is selected from the public path, i.e. the path the
    // Bitwarden client uses against this proxy.
    let rules = rules_for(&state, &path);

    let method = req.method().clone();
    let mut request_headers = HeaderMap::new();
    for (name, value) in req.headers() {
        if is_hop_by_hop(name) || name == header::HOST || name == header::CONTENT_LENGTH {
            continue;
        }
        request_headers.append(name.clone(), value.clone());
    }
    if rules.is_some() {
        // Ask for an uncompressed body so it can be parsed and rewritten while
        // streaming. Non-rewritten responses keep the client's encoding
        // preferences and are forwarded untouched.
        request_headers.insert(
            header::ACCEPT_ENCODING,
            header::HeaderValue::from_static("identity"),
        );
    }

    let has_body = req
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim() != "0")
        .unwrap_or(false)
        || req.headers().contains_key(header::TRANSFER_ENCODING);

    let mut builder = state.client.request(method.clone(), target.clone());
    builder = builder.headers(request_headers);
    if has_body {
        builder = builder.body(reqwest::Body::wrap_stream(
            req.into_body().into_data_stream(),
        ));
    }

    let upstream = match builder.send().await {
        Ok(response) => response,
        Err(error) => {
            warn!(%method, %target, %error, "upstream request failed");
            return plain_error(
                StatusCode::BAD_GATEWAY,
                format!("upstream request failed: {error}"),
            );
        }
    };

    let status = upstream.status();
    let upstream_headers = upstream.headers().clone();

    let transform = status.is_success()
        && is_json(upstream_headers.get(header::CONTENT_TYPE))
        && !is_encoded(upstream_headers.get(header::CONTENT_ENCODING));

    debug!(%method, %target, %status, rewritten = transform, "proxied");

    let body: Body = match rules {
        Some(rules) if transform => Body::from_stream(rewrite_body(upstream, rules)),
        _ => Body::from_stream(
            upstream
                .bytes_stream()
                .map(|result| result.map_err(io::Error::other)),
        ),
    };

    let mut response = Response::new(body);
    *response.status_mut() = status;
    let response_headers = response.headers_mut();
    for (name, value) in upstream_headers.iter() {
        if is_hop_by_hop(name)
            || name == header::CONTENT_LENGTH
            || name == header::ALT_SVC
            || (transform && name == header::CONTENT_ENCODING)
        {
            continue;
        }
        response_headers.append(name.clone(), value.clone());
    }

    response
}

fn rules_for(state: &AppState, path: &str) -> Option<Arc<RuleSet>> {
    match path.trim_end_matches('/') {
        "/api/config" => Some(state.config_rules.clone()),
        "/api/accounts/profile" => Some(state.profile_rules.clone()),
        "/api/sync" => Some(state.sync_rules.clone()),
        _ => None,
    }
}

/// Wrap an upstream body in a streaming JSON rewriter.
fn rewrite_body(
    upstream: reqwest::Response,
    rules: Arc<RuleSet>,
) -> impl Stream<Item = Result<Bytes, io::Error>> {
    let (sender, receiver) = tokio::sync::mpsc::channel::<Result<Bytes, io::Error>>(2);

    tokio::spawn(async move {
        let mut upstream = upstream.bytes_stream();
        let mut rewriter = JsonRewriter::new(rules);
        let mut buffer = Vec::with_capacity(8 * 1024);

        while let Some(chunk) = upstream.next().await {
            match chunk {
                Ok(chunk) => {
                    buffer.clear();
                    rewriter.push(&chunk, &mut buffer);
                    if !buffer.is_empty()
                        && sender
                            .send(Ok(Bytes::copy_from_slice(&buffer)))
                            .await
                            .is_err()
                    {
                        return;
                    }
                }
                Err(error) => {
                    debug!(%error, "upstream response body failed");
                    let _ = sender.send(Err(io::Error::other(error))).await;
                    return;
                }
            }
        }

        buffer.clear();
        rewriter.finish(&mut buffer);
        if !buffer.is_empty() {
            let _ = sender.send(Ok(Bytes::copy_from_slice(&buffer))).await;
        }
    });

    futures_util::stream::unfold(receiver, |mut receiver| async move {
        receiver.recv().await.map(|item| (item, receiver))
    })
}

fn is_hop_by_hop(name: &header::HeaderName) -> bool {
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
    )
}

fn is_json(content_type: Option<&header::HeaderValue>) -> bool {
    let Some(content_type) = content_type.and_then(|value| value.to_str().ok()) else {
        return false;
    };
    let media_type = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    media_type == "application/json" || media_type.ends_with("+json")
}

fn is_encoded(content_encoding: Option<&header::HeaderValue>) -> bool {
    content_encoding
        .and_then(|value| value.to_str().ok())
        .map(|value| !value.trim().eq_ignore_ascii_case("identity"))
        .unwrap_or(false)
}

fn plain_error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, format!("{}\n", message.into())).into_response()
}
