//! Feature flag overrides for the `/api/config` `featureStates` object.
//!
//! Overrides can be supplied with repeatable `--feature KEY=true|false` flags or
//! with `BITWARDEN_PROXY_FEATURE_<NAME>` environment variables. Environment
//! variable names are lowercased and underscores become hyphens, so
//! `BITWARDEN_PROXY_FEATURE_ENABLE_BASIC_AUTH_RESPONSE=true` sets
//! `enable-basic-auth-response`. Flags override environment variables for the
//! same key.

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};

pub const ENV_PREFIX: &str = "BITWARDEN_PROXY_FEATURE_";

/// Feature flag names mapped to the value the client should see.
pub type FeatureMap = BTreeMap<String, bool>;

/// Parse a `NAME=true` / `NAME=false` assignment from `--feature`.
pub fn parse_assignment(raw: &str) -> Result<(String, bool)> {
    let (name, value) = raw
        .split_once('=')
        .with_context(|| format!("expected KEY=true or KEY=false, got {raw:?}"))?;
    let name = name.trim();
    if name.is_empty() {
        bail!("feature name must not be empty");
    }
    let value = parse_bool(value)
        .with_context(|| format!("invalid value for feature {name:?}: {value:?}"))?;
    Ok((name.to_owned(), value))
}

/// Convert the suffix of a `BITWARDEN_PROXY_FEATURE_*` variable to a feature name.
pub fn env_name(suffix: &str) -> Result<String> {
    if suffix.is_empty() {
        bail!("{ENV_PREFIX} must be followed by a feature name");
    }
    Ok(suffix.to_ascii_lowercase().replace('_', "-"))
}

/// Collect overrides from environment variables and `--feature` flags.
///
/// The iterator is passed in (instead of reading the process environment
/// directly) so the merging logic is easy to test.
pub fn collect<I>(vars: I, flags: &[String]) -> Result<FeatureMap>
where
    I: IntoIterator<Item = (String, String)>,
{
    let mut features = FeatureMap::new();

    for (name, value) in vars {
        let Some(suffix) = name.strip_prefix(ENV_PREFIX) else {
            continue;
        };
        let key = env_name(suffix)?;
        let value = parse_bool(&value)
            .with_context(|| format!("invalid value for environment variable {name}"))?;
        features.insert(key, value);
    }

    // Applied after the environment so flags take precedence.
    for raw in flags {
        let (key, value) = parse_assignment(raw)?;
        features.insert(key, value);
    }

    Ok(features)
}

fn parse_bool(raw: &str) -> Result<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        other => bail!("expected true or false, got {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect()
    }

    #[test]
    fn parses_assignments() {
        assert_eq!(
            parse_assignment("enable-basic-auth-response=true").unwrap(),
            ("enable-basic-auth-response".to_owned(), true)
        );
        assert_eq!(
            parse_assignment(" something = FALSE ").unwrap(),
            ("something".to_owned(), false)
        );
    }

    #[test]
    fn rejects_invalid_assignments() {
        assert!(parse_assignment("missing-equals").is_err());
        assert!(parse_assignment("=true").is_err());
        assert!(parse_assignment("key=maybe").is_err());
        assert!(parse_assignment("key=").is_err());
    }

    #[test]
    fn converts_environment_names() {
        assert_eq!(
            env_name("ENABLE_BASIC_AUTH_RESPONSE").unwrap(),
            "enable-basic-auth-response"
        );
        assert_eq!(
            env_name("PM_35803_BROWSER_AUTO_CONFIRM_LOG_IN").unwrap(),
            "pm-35803-browser-auto-confirm-log-in"
        );
        assert!(env_name("").is_err());
    }

    #[test]
    fn collects_environment_and_flags_with_flag_precedence() {
        let features = collect(
            vars(&[
                ("BITWARDEN_PROXY_FEATURE_ENABLE_BASIC_AUTH_RESPONSE", "true"),
                ("BITWARDEN_PROXY_FEATURE_PM_34145_POLICIES", "false"),
                ("BITWARDEN_SERVER", "bitwarden.com"),
                ("UNRELATED", "value"),
            ]),
            &["enable-basic-auth-response=false".to_owned()],
        )
        .unwrap();

        assert_eq!(features.len(), 2);
        assert!(!features["enable-basic-auth-response"]);
        assert!(!features["pm-34145-policies"]);
    }

    #[test]
    fn accepts_common_boolean_spellings() {
        let features = collect(
            vars(&[
                ("BITWARDEN_PROXY_FEATURE_A", "1"),
                ("BITWARDEN_PROXY_FEATURE_B", "0"),
                ("BITWARDEN_PROXY_FEATURE_C", "yes"),
                ("BITWARDEN_PROXY_FEATURE_D", "off"),
            ]),
            &[],
        )
        .unwrap();

        assert!(features["a"]);
        assert!(!features["b"]);
        assert!(features["c"]);
        assert!(!features["d"]);
    }

    #[test]
    fn rejects_invalid_environment_values() {
        assert!(collect(vars(&[("BITWARDEN_PROXY_FEATURE_BROKEN", "maybe")]), &[]).is_err());
        assert!(collect(vars(&[("BITWARDEN_PROXY_FEATURE_", "true")]), &[]).is_err());
    }
}
