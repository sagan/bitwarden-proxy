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
        default_value = "0.0.0.0:8080"
    )]
    pub bind: String,
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
        assert_eq!(cli.bind, "0.0.0.0:8080");
    }

    #[test]
    fn requires_server_and_proxy() {
        // Both are required unless the corresponding environment variables are
        // set, which is not the case in the test environment.
        assert!(Cli::try_parse_from(["bitwarden-proxy", "--server", "bitwarden.com"]).is_err());
        assert!(Cli::try_parse_from(["bitwarden-proxy", "--proxy", "https://x"]).is_err());
    }
}
