//! Rules describing which JSON fields the proxy rewrites.

/// One segment of a JSON path.
///
/// A path is a sequence of object keys and array indices, e.g.
/// `organizations[*].usersGetPremium` is represented as
/// `[Key("organizations"), AnyIndex, Key("usersGetPremium")]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    Key(String),
    /// Matches any array index.
    AnyIndex,
}

impl Segment {
    pub fn key(name: &str) -> Self {
        Segment::Key(name.to_owned())
    }
}

/// What to do with the value found at a rule's path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Replace a boolean (or any other literal) with `value`.
    SetBool(bool),
    /// Replace a string value with `value`.
    SetString(String),
}

#[derive(Debug, Clone)]
pub struct Rule {
    pub path: Vec<Segment>,
    pub action: Action,
}

/// A named set of rewrite rules.
#[derive(Debug, Clone)]
pub struct RuleSet {
    name: &'static str,
    rules: Vec<Rule>,
    /// Rule indices grouped by path length so lookups stay cheap even for
    /// multi-megabyte responses.
    by_depth: Vec<Vec<usize>>,
}

impl RuleSet {
    pub fn new(name: &'static str, rules: Vec<Rule>) -> Self {
        let mut by_depth: Vec<Vec<usize>> = Vec::new();
        for (index, rule) in rules.iter().enumerate() {
            if by_depth.len() <= rule.path.len() {
                by_depth.resize_with(rule.path.len() + 1, Vec::new);
            }
            by_depth[rule.path.len()].push(index);
        }
        Self {
            name,
            rules,
            by_depth,
        }
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    pub(crate) fn candidates(&self, depth: usize) -> &[usize] {
        self.by_depth
            .get(depth)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }
}

fn bool_rule(path: &[Segment], value: bool) -> Rule {
    Rule {
        path: path.to_vec(),
        action: Action::SetBool(value),
    }
}

/// Rules for `GET /api/config`: point every advertised service URL back at the
/// proxy, exactly like a self-hosted Bitwarden server would.
pub fn config_rules(proxy_url: &str) -> RuleSet {
    let base = proxy_url.trim_end_matches('/');
    let services = [
        ("vault", ""),
        ("api", "/api"),
        ("identity", "/identity"),
        ("notifications", "/notifications"),
        ("sso", "/sso"),
        ("events", "/events"),
        ("fillAssistRules", "/fillassist"),
    ];

    let rules = services
        .into_iter()
        .map(|(field, suffix)| Rule {
            path: vec![Segment::key("environment"), Segment::key(field)],
            action: Action::SetString(format!("{base}{suffix}")),
        })
        .collect();

    RuleSet::new("config", rules)
}

/// Rules for `GET /api/accounts/profile`.
pub fn profile_rules() -> RuleSet {
    RuleSet::new(
        "profile",
        vec![
            bool_rule(&[Segment::key("premium")], true),
            bool_rule(&[Segment::key("premiumFromOrganization")], true),
            bool_rule(
                &[
                    Segment::key("organizations"),
                    Segment::AnyIndex,
                    Segment::key("usersGetPremium"),
                ],
                true,
            ),
            bool_rule(
                &[
                    Segment::key("organizationsNew"),
                    Segment::AnyIndex,
                    Segment::key("usersGetPremium"),
                ],
                true,
            ),
        ],
    )
}

/// Rules for `GET /api/sync`.
pub fn sync_rules() -> RuleSet {
    RuleSet::new(
        "sync",
        vec![
            bool_rule(&[Segment::key("profile"), Segment::key("premium")], true),
            bool_rule(
                &[
                    Segment::key("profile"),
                    Segment::key("premiumFromOrganization"),
                ],
                true,
            ),
        ],
    )
}
