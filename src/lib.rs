//! A streaming reverse proxy for Bitwarden servers.
//!
//! The proxy is meant to be used as a custom Bitwarden server URL. It forwards
//! every request to an official (or self-hosted) Bitwarden server and rewrites
//! the few JSON fields that decide whether the client believes it has a
//! premium subscription.

pub mod cli;
pub mod config;
pub mod proxy;
pub mod rewrite;
pub mod routes;
pub mod ws;

pub use cli::Cli;
pub use config::Config;
pub use proxy::{router, AppState};
