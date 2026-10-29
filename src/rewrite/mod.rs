//! Streaming JSON response rewriting.

mod rules;
mod stream;

pub use rules::{config_rules, profile_rules, sync_rules, Action, Rule, RuleSet, Segment};
pub use stream::JsonRewriter;

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::Value;

    use super::*;

    /// Run the rewriter, splitting the input into chunks of `chunk_size` bytes.
    fn run(rules: &RuleSet, input: &str, chunk_size: usize) -> String {
        let mut rewriter = JsonRewriter::new(Arc::new(rules.clone()));
        let mut output = Vec::new();
        for chunk in input.as_bytes().chunks(chunk_size) {
            rewriter.push(chunk, &mut output);
        }
        rewriter.finish(&mut output);
        String::from_utf8(output).expect("rewriter produced invalid UTF-8")
    }

    /// Run with every possible chunk size and make sure the result is always
    /// identical. This catches token-boundary bugs.
    fn run_all_chunkings(rules: &RuleSet, input: &str) -> String {
        let expected = run(rules, input, input.len().max(1));
        for chunk_size in 1..=input.len().max(1) {
            assert_eq!(
                run(rules, input, chunk_size),
                expected,
                "chunk size {chunk_size} produced a different result"
            );
        }
        expected
    }

    fn json(value: &str) -> Value {
        serde_json::from_str(value).expect("output is valid JSON")
    }

    const PROFILE: &str = r#"{
  "premium": false,
  "premiumFromOrganization": false,
  "email": "user@example.com",
  "organizations": [
    { "id": "a", "usersGetPremium": false, "name": "First" },
    { "id": "b", "usersGetPremium": false, "name": "Second" }
  ],
  "organizationsNew": [{ "id": "c", "usersGetPremium": false }]
}"#;

    #[test]
    fn rewrites_profile_flags() {
        let output = run_all_chunkings(&profile_rules(), PROFILE);
        let value = json(&output);

        assert_eq!(value["premium"], Value::Bool(true));
        assert_eq!(value["premiumFromOrganization"], Value::Bool(true));
        assert_eq!(
            value["organizations"][0]["usersGetPremium"],
            Value::Bool(true)
        );
        assert_eq!(
            value["organizations"][1]["usersGetPremium"],
            Value::Bool(true)
        );
        assert_eq!(
            value["organizationsNew"][0]["usersGetPremium"],
            Value::Bool(true)
        );
        // Untouched fields stay untouched.
        assert_eq!(value["email"], "user@example.com");
        assert_eq!(value["organizations"][0]["id"], "a");
    }

    #[test]
    fn rewrites_sync_profile() {
        let input = r#"{"profile":{"premium":false,"premiumFromOrganization":true,"name":"x"},"ciphers":[]}"#;
        let output = run_all_chunkings(&sync_rules(), input);
        let value = json(&output);

        assert_eq!(value["profile"]["premium"], Value::Bool(true));
        assert_eq!(
            value["profile"]["premiumFromOrganization"],
            Value::Bool(true)
        );
        assert_eq!(value["profile"]["name"], "x");
    }

    #[test]
    fn rewrites_config_environment() {
        let input = r#"{
  "version": "2026.10.1",
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
        let rules = config_rules("https://bw.example.com");
        let output = run_all_chunkings(&rules, input);
        let value = json(&output);

        assert_eq!(value["environment"]["vault"], "https://bw.example.com");
        assert_eq!(value["environment"]["api"], "https://bw.example.com/api");
        assert_eq!(
            value["environment"]["identity"],
            "https://bw.example.com/identity"
        );
        assert_eq!(
            value["environment"]["notifications"],
            "https://bw.example.com/notifications"
        );
        assert_eq!(value["environment"]["sso"], "https://bw.example.com/sso");
        assert_eq!(
            value["environment"]["fillAssistRules"],
            "https://bw.example.com/fillassist"
        );
        assert_eq!(value["environment"]["cloudRegion"], "US");
        assert_eq!(value["version"], "2026.10.1");
    }

    #[test]
    fn rewrites_strings_and_leaves_structure_byte_for_byte() {
        let input = "{\n  \"environment\": { \"api\": \"https://api.bitwarden.com\" }\n}";
        let expected = "{\n  \"environment\": { \"api\": \"https://bw.example.com/api\" }\n}";
        assert_eq!(
            run_all_chunkings(&config_rules("https://bw.example.com"), input),
            expected
        );
    }

    #[test]
    fn leaves_unrelated_documents_untouched() {
        let input = r#"{"a":1,"b":[true,false,null,"x\"y"],"c":{"d":"e"}}"#;
        assert_eq!(run_all_chunkings(&profile_rules(), input), input);
    }

    #[test]
    fn handles_escaped_keys_and_strings() {
        let input = r#"{"pre\u006dium": false, "other": "a\"b}c]d\\e"}"#;
        let output = run_all_chunkings(&profile_rules(), input);
        let value = json(&output);

        assert_eq!(value["premium"], Value::Bool(true));
        assert_eq!(value["other"], "a\"b}c]d\\e");
    }

    #[test]
    fn array_indices_are_path_segments() {
        // Rules are absolute JSON paths, so array elements shift the path:
        // the premium flag of an array element is `[*].premium`, not `premium`.
        let input = r#"[{"premium":false},{"premium":true}]"#;
        let output = run_all_chunkings(&profile_rules(), input);
        let value = json(&output);
        assert_eq!(value[0]["premium"], Value::Bool(false));
        assert_eq!(value[1]["premium"], Value::Bool(true));
    }

    #[test]
    fn does_not_match_wrong_paths() {
        let input = r#"{"profile":{"premium":false},"premium":false,"data":{"premium":false}}"#;
        let output = run_all_chunkings(&sync_rules(), input);
        let value = json(&output);

        assert_eq!(value["profile"]["premium"], Value::Bool(true));
        assert_eq!(value["premium"], Value::Bool(false));
        assert_eq!(value["data"]["premium"], Value::Bool(false));
    }

    #[test]
    fn handles_numbers_and_nested_whitespace() {
        let input = "{\n\t\"premium\" :\tfalse ,\n\t\"n\" : -1.5e+10\n}";
        let output = run_all_chunkings(&profile_rules(), input);
        let value = json(&output);

        assert_eq!(value["premium"], Value::Bool(true));
        assert_eq!(value["n"], -1.5e10);
    }

    #[test]
    fn malformed_input_does_not_panic() {
        let mut rewriter = JsonRewriter::new(Arc::new(profile_rules()));
        let mut output = Vec::new();
        rewriter.push(br#"{"premium": fals"#, &mut output);
        rewriter.push(b"e} garbage", &mut output);
        rewriter.finish(&mut output);
        assert!(!output.is_empty());
    }

    #[test]
    fn type_mismatches_are_left_alone() {
        // A boolean rule does not touch a string and vice versa.
        let input = r#"{"premium":"false","environment":{"api":false}}"#;
        let mut rules = profile_rules();
        rules = RuleSet::new(
            "mixed",
            [
                rules.rules().to_vec(),
                config_rules("https://bw.example.com").rules().to_vec(),
            ]
            .concat(),
        );
        let output = run_all_chunkings(&rules, input);
        assert_eq!(json(&output)["premium"], "false");
        assert_eq!(json(&output)["environment"]["api"], false);
    }

    #[test]
    fn empty_ruleset_is_a_passthrough() {
        let rules = RuleSet::new("empty", Vec::new());
        let input = r#"{"premium":false}"#;
        assert_eq!(run_all_chunkings(&rules, input), input);
    }
}
