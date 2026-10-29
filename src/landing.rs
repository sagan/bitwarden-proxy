//! Plain-text landing page served at `/`.

use crate::config::Config;

/// Render the instructions / configuration summary shown at the proxy root.
pub fn render(config: &Config) -> String {
    format!(
        "\
bitwarden-proxy {version}
=========================

This is a Bitwarden reverse proxy, not the Bitwarden web vault.

To use it, open your Bitwarden client, choose \"Self-hosted\" (or
\"Custom environment\") on the login screen and enter:

    {proxy}

Then log in. Requests are forwarded to the upstream Bitwarden server and the
premium flags in the account profile are rewritten, which unlocks client-side
premium features such as the TOTP generator.

Configuration
-------------
  upstream server : {upstream}
  public origin   : {proxy}
  listen address  : {bind}
  root page       : enabled

Notes
-----
  * Log out and back in after changing the server URL.
  * Only client-side premium checks are bypassed; features enforced by the
    Bitwarden server itself (for example creating attachments) still require a
    real premium subscription.
  * This proxy handles the credentials and vault traffic it forwards. Only
    share its URL with people you trust.
  * Disable this page with BITWARDEN_PROXY_ROOT_PAGE=false or --root-page false.
",
        version = env!("CARGO_PKG_VERSION"),
        proxy = config.proxy_url,
        upstream = config.server.description(),
        bind = config.bind,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_configuration_details() {
        let config = Config::new(
            "bitwarden.eu",
            "https://bw.example.com",
            "0.0.0.0:6680",
            true,
        )
        .unwrap();
        let page = render(&config);

        assert!(page.contains("bitwarden-proxy"));
        assert!(page.contains("https://bw.example.com"));
        assert!(page.contains("official (bitwarden.eu)"));
        assert!(page.contains("0.0.0.0:6680"));
    }
}
