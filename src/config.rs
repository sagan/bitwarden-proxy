//! Environment configuration.

use std::net::SocketAddr;

use anyhow::{bail, Context, Result};
use url::Url;

use crate::routes::Upstream;

/// Runtime configuration, read from the environment.
#[derive(Debug, Clone)]
pub struct Config {
    /// The Bitwarden server requests are forwarded to.
    pub server: Upstream,
    /// Public origin of this proxy, e.g. `https://bitwarden-proxy.example.com`.
    pub proxy_url: String,
    /// Address the HTTP server listens on.
    pub bind: SocketAddr,
}

impl Config {
    pub fn new(server: &str, proxy: &str, bind: &str) -> Result<Self> {
        Ok(Self {
            server: Upstream::parse(server)?,
            proxy_url: normalize_proxy_url(proxy)?,
            bind: bind.parse().with_context(|| {
                format!("BITWARDEN_PROXY_BIND is not a valid socket address: {bind:?}")
            })?,
        })
    }
}

/// Validate and normalize `BITWARDEN_PROXY` into a scheme + authority origin.
fn normalize_proxy_url(raw: &str) -> Result<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        bail!("BITWARDEN_PROXY must not be empty");
    }
    let url =
        Url::parse(raw).with_context(|| format!("BITWARDEN_PROXY is not a valid URL: {raw:?}"))?;

    if !matches!(url.scheme(), "http" | "https") {
        bail!(
            "BITWARDEN_PROXY must use http or https, got {:?}",
            url.scheme()
        );
    }
    if url.host_str().is_none() {
        bail!("BITWARDEN_PROXY has no host");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("BITWARDEN_PROXY must not contain credentials");
    }
    if !url.path().is_empty() && url.path() != "/" {
        bail!(
            "BITWARDEN_PROXY must be a bare origin without a path, got {:?}",
            url.path()
        );
    }
    if url.query().is_some() || url.fragment().is_some() {
        bail!("BITWARDEN_PROXY must not contain a query or fragment");
    }

    Ok(url.origin().ascii_serialization())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_proxy_origin() {
        assert_eq!(
            normalize_proxy_url("https://bw.example.com/").unwrap(),
            "https://bw.example.com"
        );
        assert_eq!(
            normalize_proxy_url("http://127.0.0.1:8080").unwrap(),
            "http://127.0.0.1:8080"
        );
        assert_eq!(
            normalize_proxy_url("https://bw.example.com:443").unwrap(),
            "https://bw.example.com"
        );
    }

    #[test]
    fn rejects_proxy_with_path_or_credentials() {
        assert!(normalize_proxy_url("https://bw.example.com/sub").is_err());
        assert!(normalize_proxy_url("https://user:pass@bw.example.com").is_err());
        assert!(normalize_proxy_url("ftp://bw.example.com").is_err());
        assert!(normalize_proxy_url("").is_err());
    }

    #[test]
    fn parses_full_config() {
        let config =
            Config::new("bitwarden.eu", "https://bw.example.com", "127.0.0.1:1234").unwrap();
        assert!(config.server.is_official());
        assert_eq!(config.proxy_url, "https://bw.example.com");
        assert_eq!(config.bind.port(), 1234);
    }
}
