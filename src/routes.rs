//! Resolution of incoming proxy paths to upstream URLs.

use anyhow::{bail, Context, Result};
use url::Url;

/// Path prefixes that map onto dedicated subdomains of the official Bitwarden
/// servers, e.g. `/api/...` -> `api.bitwarden.com/...`.
const SERVICES: &[&str] = &[
    "api",
    "identity",
    "notifications",
    "events",
    "sso",
    "fillassist",
];

#[derive(Debug, Clone)]
enum Kind {
    /// Official Bitwarden cloud (`bitwarden.com` / `bitwarden.eu`).
    Official { base_domain: String },
    /// Any other Bitwarden-compatible server. Paths are forwarded unchanged.
    Custom { origin: String, base_path: String },
}

/// The upstream Bitwarden server requests are forwarded to.
#[derive(Debug, Clone)]
pub struct Upstream {
    kind: Kind,
}

impl Upstream {
    /// Parse `BITWARDEN_SERVER`.
    ///
    /// Accepts a bare domain (`bitwarden.com`), an origin
    /// (`https://bw.example.com`) and optionally a base path
    /// (`https://example.com/bitwarden`).
    pub fn parse(raw: &str) -> Result<Self> {
        let raw = raw.trim();
        if raw.is_empty() {
            bail!("BITWARDEN_SERVER must not be empty");
        }

        let normalized = if raw.contains("://") {
            raw.to_owned()
        } else {
            format!("https://{raw}")
        };
        let url = Url::parse(&normalized)
            .with_context(|| format!("BITWARDEN_SERVER is not a valid URL: {raw:?}"))?;

        if !matches!(url.scheme(), "http" | "https") {
            bail!(
                "BITWARDEN_SERVER must use http or https, got {:?}",
                url.scheme()
            );
        }
        if url.query().is_some() || url.fragment().is_some() {
            bail!("BITWARDEN_SERVER must not contain a query or fragment");
        }
        let host = url
            .host_str()
            .context("BITWARDEN_SERVER has no host")?
            .to_ascii_lowercase();

        // The official cloud servers get special treatment: each service lives
        // on its own subdomain, so `/api/...` has to become `api.bitwarden.com/...`.
        let official_domain = if host == "bitwarden.com" || host.ends_with(".bitwarden.com") {
            Some("bitwarden.com")
        } else if host == "bitwarden.eu" || host.ends_with(".bitwarden.eu") {
            Some("bitwarden.eu")
        } else {
            None
        };
        if let Some(base_domain) = official_domain {
            return Ok(Self {
                kind: Kind::Official {
                    base_domain: base_domain.to_owned(),
                },
            });
        }

        let origin = format!("{}://{}", url.scheme(), url.authority());
        let base_path = url.path().trim_end_matches('/').to_owned();
        Ok(Self {
            kind: Kind::Custom { origin, base_path },
        })
    }

    /// Whether this is one of the official Bitwarden cloud servers.
    pub fn is_official(&self) -> bool {
        matches!(self.kind, Kind::Official { .. })
    }

    /// Short description for log output.
    pub fn description(&self) -> String {
        match &self.kind {
            Kind::Official { base_domain } => format!("official ({base_domain})"),
            Kind::Custom { origin, base_path } => format!("{origin}{base_path}"),
        }
    }

    /// Resolve an origin-form path (with optional query) to an absolute
    /// upstream URL.
    pub fn resolve(&self, path_and_query: &str) -> Result<Url> {
        let (path, query) = split_path_and_query(path_and_query);
        let (scheme, authority, upstream_path) = match &self.kind {
            Kind::Official { base_domain } => {
                let (host, path) = official_target(base_domain, path);
                ("https".to_owned(), host, path)
            }
            Kind::Custom { origin, base_path } => {
                let url = Url::parse(origin)?;
                let scheme = url.scheme().to_owned();
                let authority = url.authority().to_owned();
                (scheme, authority, format!("{base_path}{path}"))
            }
        };
        self.build(&scheme, &authority, &upstream_path, query)
    }

    /// Resolve an origin-form path to a WebSocket URL.
    pub fn resolve_websocket(&self, path_and_query: &str) -> Result<Url> {
        let http = self.resolve(path_and_query)?;
        let scheme = match http.scheme() {
            "https" => "wss",
            _ => "ws",
        };
        self.build(scheme, http.authority(), http.path(), http.query())
    }

    fn build(&self, scheme: &str, authority: &str, path: &str, query: Option<&str>) -> Result<Url> {
        let path = if path.is_empty() { "/" } else { path };
        let mut raw = format!("{scheme}://{authority}{path}");
        if let Some(query) = query {
            raw.push('?');
            raw.push_str(query);
        }
        Url::parse(&raw).with_context(|| format!("failed to build upstream URL from {raw:?}"))
    }
}

/// Split `"/api/sync?x=1"` into `("/api/sync", Some("x=1"))`.
fn split_path_and_query(path_and_query: &str) -> (&str, Option<&str>) {
    match path_and_query.split_once('?') {
        Some((path, query)) => (if path.is_empty() { "/" } else { path }, Some(query)),
        None => (
            if path_and_query.is_empty() {
                "/"
            } else {
                path_and_query
            },
            None,
        ),
    }
}

/// Map an incoming path onto `(host, path)` for the official cloud servers.
fn official_target(base_domain: &str, path: &str) -> (String, String) {
    let trimmed = path.trim_start_matches('/');

    let (first, rest) = match trimmed.split_once('/') {
        Some((first, rest)) => (first, Some(rest)),
        None => (trimmed, None),
    };

    if SERVICES.contains(&first) {
        let path = match rest {
            Some(rest) if !rest.is_empty() => format!("/{rest}"),
            _ => "/".to_owned(),
        };
        (format!("{first}.{base_domain}"), path)
    } else if first == "icons" {
        // Icons are served from a different top-level domain.
        let path = match rest {
            Some(rest) if !rest.is_empty() => format!("/{rest}"),
            _ => "/".to_owned(),
        };
        ("icons.bitwarden.net".to_owned(), path)
    } else {
        (base_domain.to_owned(), path.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn official(server: &str, path: &str) -> String {
        Upstream::parse(server)
            .unwrap()
            .resolve(path)
            .unwrap()
            .to_string()
    }

    fn custom(server: &str, path: &str) -> String {
        Upstream::parse(server)
            .unwrap()
            .resolve(path)
            .unwrap()
            .to_string()
    }

    #[test]
    fn official_service_paths_map_to_subdomains() {
        assert_eq!(
            official("bitwarden.com", "/api/accounts/profile"),
            "https://api.bitwarden.com/accounts/profile"
        );
        assert_eq!(
            official("https://bitwarden.com", "/identity/connect/token"),
            "https://identity.bitwarden.com/connect/token"
        );
        assert_eq!(
            official("bitwarden.eu", "/notifications/hub?access_token=x"),
            "https://notifications.bitwarden.eu/hub?access_token=x"
        );
        assert_eq!(
            official("bitwarden.com", "/api"),
            "https://api.bitwarden.com/"
        );
        assert_eq!(
            official("bitwarden.com", "/icons/example.com/icon.png"),
            "https://icons.bitwarden.net/example.com/icon.png"
        );
    }

    #[test]
    fn official_other_paths_stay_on_the_main_domain() {
        assert_eq!(official("bitwarden.com", "/"), "https://bitwarden.com/");
        assert_eq!(
            official("bitwarden.com", "/app/main.js"),
            "https://bitwarden.com/app/main.js"
        );
        // A path that merely starts with a service name is not a service path.
        assert_eq!(
            official("bitwarden.com", "/apixyz/foo"),
            "https://bitwarden.com/apixyz/foo"
        );
    }

    #[test]
    fn official_subdomains_are_recognized() {
        assert_eq!(
            official("https://api.bitwarden.com", "/api/sync"),
            "https://api.bitwarden.com/sync"
        );
        assert_eq!(
            official("https://vault.bitwarden.eu", "/identity/x"),
            "https://identity.bitwarden.eu/x"
        );
    }

    #[test]
    fn custom_servers_keep_the_path() {
        assert_eq!(
            custom("https://bw.example.com", "/api/sync?x=1"),
            "https://bw.example.com/api/sync?x=1"
        );
        assert_eq!(
            custom("bw.example.com:8443", "/api/config"),
            "https://bw.example.com:8443/api/config"
        );
        assert_eq!(
            custom("http://127.0.0.1:8081", "/api/config"),
            "http://127.0.0.1:8081/api/config"
        );
        assert_eq!(
            custom("https://example.com/bitwarden/", "/api/config"),
            "https://example.com/bitwarden/api/config"
        );
    }

    #[test]
    fn websocket_urls_are_built() {
        let upstream = Upstream::parse("bitwarden.com").unwrap();
        assert_eq!(
            upstream
                .resolve_websocket("/notifications/hub?access_token=x")
                .unwrap()
                .as_str(),
            "wss://notifications.bitwarden.com/hub?access_token=x"
        );

        let upstream = Upstream::parse("http://127.0.0.1:8081").unwrap();
        assert_eq!(
            upstream
                .resolve_websocket("/notifications/hub")
                .unwrap()
                .as_str(),
            "ws://127.0.0.1:8081/notifications/hub"
        );
    }

    #[test]
    fn rejects_invalid_config() {
        assert!(Upstream::parse("").is_err());
        assert!(Upstream::parse("ftp://example.com").is_err());
        assert!(Upstream::parse("https://example.com/?x=1").is_err());
    }
}
