//! Command line interface.
//!
//! Every option can be supplied as a flag or through an environment variable
//! (flags take precedence).

use clap::Parser;

/// Streaming reverse proxy for Bitwarden servers.
///
/// Point a Bitwarden client at this proxy to have the premium flags in its
/// profile rewritten, while all traffic is forwarded to the real server.
#[derive(Debug, Parser)]
#[command(
    name = "bitwarden-proxy",
    version,
    about,
    after_help = "Environment variables:\n  \
        BITWARDEN_SERVER       Upstream server (same as --server)\n  \
        BITWARDEN_PROXY        Public origin of this proxy (same as --proxy)\n  \
        BITWARDEN_PROXY_BIND   Listen address (same as --bind)\n  \
        BITWARDEN_PROXY_ROOT_PAGE  Serve the root landing page (same as --root-page)\n  \
        BITWARDEN_PROXY_FEATURE_<NAME>  Override a featureStates flag (same as --feature);\n  \
        \x20                               underscores become hyphens, e.g.\n  \
        \x20                               BITWARDEN_PROXY_FEATURE_ENABLE_BASIC_AUTH_RESPONSE=true\n  \
        RUST_LOG               Log filter, e.g. `info` or `bitwarden_proxy=debug`\n\n\
        Flags take precedence over environment variables."
)]
pub struct Cli {
    /// Upstream server: bitwarden.com, bitwarden.eu or an origin such as
    /// https://bw.example.com
    #[arg(short = 's', long, env = "BITWARDEN_SERVER", value_name = "SERVER")]
    pub server: String,

    /// Public origin of this proxy, used when rewriting /api/config URLs
    #[arg(short = 'p', long, env = "BITWARDEN_PROXY", value_name = "ORIGIN")]
    pub proxy: String,

    /// Address the plain-HTTP server listens on
    #[arg(
        short = 'b',
        long,
        env = "BITWARDEN_PROXY_BIND",
        value_name = "ADDR",
        default_value = "0.0.0.0:6680"
    )]
    pub bind: String,

    /// Serve a plain-text instructions page at `/` instead of forwarding it
    #[arg(
        long,
        env = "BITWARDEN_PROXY_ROOT_PAGE",
        value_name = "BOOL",
        default_value_t = true,
        action = clap::ArgAction::Set,
        value_parser = clap::builder::BoolishValueParser::new()
    )]
    pub root_page: bool,

    /// Override a featureStates flag in /api/config, e.g.
    /// `--feature enable-basic-auth-response=true` (repeatable)
    #[arg(long = "feature", value_name = "KEY=BOOL")]
    pub feature: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_long_flags() {
        let cli = Cli::try_parse_from([
            "bitwarden-proxy",
            "--server",
            "bitwarden.eu",
            "--proxy",
            "https://bw.example.com",
            "--bind",
            "127.0.0.1:9000",
        ])
        .unwrap();

        assert_eq!(cli.server, "bitwarden.eu");
        assert_eq!(cli.proxy, "https://bw.example.com");
        assert_eq!(cli.bind, "127.0.0.1:9000");
        assert!(cli.root_page);
    }

    #[test]
    fn parses_short_flags_and_defaults_bind() {
        let cli = Cli::try_parse_from([
            "bitwarden-proxy",
            "-s",
            "bitwarden.com",
            "-p",
            "https://bw.example.com",
        ])
        .unwrap();

        assert_eq!(cli.server, "bitwarden.com");
        assert_eq!(cli.bind, "0.0.0.0:6680");
        assert!(cli.root_page);
    }

    #[test]
    fn can_disable_root_page() {
        let cli = Cli::try_parse_from([
            "bitwarden-proxy",
            "--server",
            "bitwarden.com",
            "--proxy",
            "https://bw.example.com",
            "--root-page",
            "false",
        ])
        .unwrap();

        assert!(!cli.root_page);
    }

    #[test]
    fn collects_repeated_feature_flags() {
        let cli = Cli::try_parse_from([
            "bitwarden-proxy",
            "--server",
            "bitwarden.com",
            "--proxy",
            "https://bw.example.com",
            "--feature",
            "enable-basic-auth-response=true",
            "--feature",
            "pm-34145-policies-in-accepted-state=false",
        ])
        .unwrap();

        assert_eq!(
            cli.feature,
            [
                "enable-basic-auth-response=true",
                "pm-34145-policies-in-accepted-state=false",
            ]
        );
    }

    #[test]
    fn requires_server_and_proxy() {
        // Both are required unless the corresponding environment variables are
        // set, which is not the case in the test environment.
        assert!(Cli::try_parse_from(["bitwarden-proxy", "--server", "bitwarden.com"]).is_err());
        assert!(Cli::try_parse_from(["bitwarden-proxy", "--proxy", "https://x"]).is_err());
    }
}
