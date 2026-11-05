//! URL/rule contract; see docs/issue-010-privacy-policy.md. Version 1 stores each rule as a string,
//! version 2 as a typed rule (docs/issue-025-policy-engine.md). A pattern means the same in both;
//! a regex, version 2 only, meets the whole URL; a domain wildcard, version 2 only, meets the host.
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;
use url::Url;

pub const MAX_REVISION: u64 = 9_007_199_254_740_991;
pub const DEFAULT_JSON: &str = include_str!("../../extension/policy-defaults.json");
/// Limits of regex rules, enabled or not (docs/issue-025-policy-engine.md, Slice 5).
pub const MAX_REGEX_RULES: usize = 128;
pub const MAX_REGEX_BYTES: usize = 4096;
/// The compiled size of all regexes of a policy, shared equally, and at most this for one.
pub const REGEX_TOTAL_SIZE_LIMIT: usize = 8 * 1024 * 1024;
pub const REGEX_SIZE_LIMIT: usize = 1024 * 1024;
/// The same for the lazy DFA cache a regex grows while matching.
pub const REGEX_TOTAL_DFA_SIZE_LIMIT: usize = 8 * 1024 * 1024;
pub const REGEX_DFA_SIZE_LIMIT: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Policy {
    pub schema_version: u32,
    pub engine_version: u32,
    pub revision: u64,
    pub categories: Categories,
    pub layers: Layers,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Categories {
    pub authentication: bool,
    pub credentials: bool,
    pub government: bool,
    pub webmail: bool,
    pub ai_chats: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Layers {
    pub never_save: Layer,
    pub save_but_no_mcp: Layer,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layer {
    pub enabled: bool,
    pub rules: Vec<RuleSpec>,
}
/// One stored rule: a string in version 1, a typed rule in version 2. A policy holds one form only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RuleSpec {
    Text(String),
    Typed(TypedRule),
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum TypedRule {
    Pattern {
        pattern: String,
    },
    /// A regular expression of the `regex` crate, found anywhere in the URL (`compile_regex`).
    Regex {
        pattern: String,
    },
    /// `*.` and a registrable domain (`public_suffix::wildcard_domain`): that domain and every
    /// subdomain, by host alone.
    DomainWildcard {
        pattern: String,
    },
}
/// A pattern in the current form (version 2). A version 1 rule is written `RuleSpec::Text`.
impl From<&str> for RuleSpec {
    fn from(rule: &str) -> Self {
        rule.to_owned().into()
    }
}
impl From<String> for RuleSpec {
    fn from(pattern: String) -> Self {
        Self::Typed(TypedRule::Pattern { pattern })
    }
}
/// What a stored rule is: a pattern with the grammar of its version, a regex, or a domain wildcard.
enum Form<'a> {
    Pattern(&'a str, Syntax),
    Regex(&'a str),
    Domain(&'a str),
}
impl RuleSpec {
    fn form(&self) -> Form<'_> {
        match self {
            Self::Text(pattern) => Form::Pattern(pattern, Syntax::Version1),
            Self::Typed(TypedRule::Pattern { pattern }) => Form::Pattern(pattern, Syntax::Version2),
            Self::Typed(TypedRule::Regex { pattern }) => Form::Regex(pattern),
            Self::Typed(TypedRule::DomainWildcard { pattern }) => Form::Domain(pattern),
        }
    }
    /// The code of `error` for this rule: what `problems` and the rule test say of it.
    pub fn error_code(&self, error: Error) -> &'static str {
        match (self.form(), error) {
            (_, Error::InvalidUrl) => "invalid_url",
            (Form::Pattern(..), _) => "invalid_pattern",
            (Form::Domain(_), Error::UnapprovedDomain) => "domain_wildcard_not_approved",
            (Form::Domain(_), Error::SuffixListUnavailable) => "psl_unavailable",
            (Form::Domain(_), _) => "invalid_domain_wildcard",
            (Form::Regex(_), Error::UnsupportedRule) => "unsupported_regex",
            (Form::Regex(_), Error::RegexLimit) => "regex_limit",
            (Form::Regex(_), _) => "invalid_regex",
        }
    }
}
/// The grammar of a pattern. Version 2 adds an exact path and a wildcard in the last path segment;
/// a version 1 string keeps the grammar it had, and so do the builtins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Syntax {
    Version1,
    Version2,
}
/// One reason a draft policy cannot be used: a code, and the rule it belongs to when it belongs to
/// one. It never holds the text of a rule or a URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Problem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layer: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule_index: Option<usize>,
    pub code: &'static str,
}
impl Problem {
    pub fn of(code: &'static str) -> Self {
        Self {
            layer: None,
            rule_index: None,
            code,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidPolicy,
    InvalidUrl,
    /// A rule this version knows but cannot evaluate, such as a regex with look-around or a
    /// backreference. The policy cannot be used.
    UnsupportedRule,
    /// Regex rules over a limit: too many, too long, or compiled too large. The policy cannot be
    /// used.
    RegexLimit,
    /// Only a valid version 1 policy can be migrated.
    NotMigratable,
    /// A domain wildcard the Public Suffix List restricts, for a domain no one approved.
    UnapprovedDomain,
    /// The bundled Public Suffix List could not be read: no domain wildcard can be judged.
    SuffixListUnavailable,
}

/// The domains a person approved for domain wildcards the Public Suffix List restricts, canonical
/// (`public_suffix::Diagnosis`). The host reads them from its own store and hands them over; the
/// engine reads no file or database. An approval makes a restricted rule usable and nothing else:
/// it matches no URL by itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DomainApprovals {
    domains: HashSet<String>,
}
impl DomainApprovals {
    /// No approval: what a failure to read them means.
    pub fn none() -> Self {
        Self::default()
    }
    pub fn of(domains: impl IntoIterator<Item = String>) -> Self {
        Self {
            domains: domains.into_iter().collect(),
        }
    }
    pub fn contains(&self, domain: &str) -> bool {
        self.domains.contains(domain)
    }
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Builtins {
    authentication: Vec<String>,
    government: Vec<String>,
    webmail: Vec<String>,
    ai_chats: Vec<String>,
    credential_keys: Vec<String>,
    auth_paths: Vec<String>,
    callback_paths: Vec<String>,
}
static BUILTINS: LazyLock<Builtins> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../extension/policy-builtins.json"))
        .expect("bundled builtins validated by tests")
});

impl Policy {
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        Self::parse_compiled(bytes).map(|(policy, _)| policy)
    }
    /// The policy and its compiled form, compiling each rule once: compiling is the validation.
    pub fn parse_compiled(bytes: &[u8]) -> Result<(Self, CompiledPolicy), Error> {
        Self::parse_compiled_with(bytes, &DomainApprovals::none())
    }
    /// `parse_compiled` with the domains a person approved.
    pub fn parse_compiled_with(
        bytes: &[u8],
        approvals: &DomainApprovals,
    ) -> Result<(Self, CompiledPolicy), Error> {
        let policy: Self = serde_json::from_slice(bytes).map_err(|_| Error::InvalidPolicy)?;
        let compiled = CompiledPolicy::compile_with(&policy, approvals)?;
        Ok((policy, compiled))
    }
    /// The canonical domains of the domain wildcards that need an approval, in every layer: what a
    /// stored policy depends on in the approvals.
    pub fn approval_dependencies(&self) -> HashSet<String> {
        use crate::public_suffix::{diagnose, Diagnosis};
        [&self.layers.never_save, &self.layers.save_but_no_mcp]
            .iter()
            .flat_map(|layer| &layer.rules)
            .filter_map(|rule| match rule.form() {
                Form::Domain(pattern) => match diagnose(pattern) {
                    Diagnosis::Restricted { domain, .. } => Some(domain),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }
    /// Versions 1 and 2 only, and each with its own form of rule throughout.
    fn check_header(&self) -> Result<(), Error> {
        let typed = match (self.schema_version, self.engine_version) {
            (1, 1) => false,
            (2, 2) => true,
            _ => return Err(Error::InvalidPolicy),
        };
        if !(1..=MAX_REVISION).contains(&self.revision) {
            return Err(Error::InvalidPolicy);
        }
        for layer in [&self.layers.never_save, &self.layers.save_but_no_mcp] {
            if layer.rules.len() > 1000
                || layer
                    .rules
                    .iter()
                    .any(|rule| matches!(rule, RuleSpec::Typed(_)) != typed)
            {
                return Err(Error::InvalidPolicy);
            }
        }
        if self.regex_count() > MAX_REGEX_RULES {
            return Err(Error::RegexLimit);
        }
        Ok(())
    }
    fn regex_count(&self) -> usize {
        [&self.layers.never_save, &self.layers.save_but_no_mcp]
            .iter()
            .flat_map(|layer| &layer.rules)
            .filter(|rule| matches!(rule.form(), Form::Regex(_)))
            .count()
    }
    /// Every rule, also of a disabled layer. Valid means exactly that the policy compiles.
    pub fn validate(&self) -> Result<(), Error> {
        self.validate_with(&DomainApprovals::none())
    }
    pub fn validate_with(&self, approvals: &DomainApprovals) -> Result<(), Error> {
        CompiledPolicy::compile_with(self, approvals).map(drop)
    }
    /// Why a policy does not compile, rule by rule where the rule is known; empty if it compiles.
    /// A cause that belongs to no rule has no place, and none is guessed.
    pub fn problems(&self) -> Vec<Problem> {
        self.problems_with(&DomainApprovals::none())
    }
    /// `problems` with the domains a person approved.
    pub fn problems_with(&self, approvals: &DomainApprovals) -> Vec<Problem> {
        if CompiledPolicy::compile_with(self, approvals).is_ok() {
            return Vec::new();
        }
        let mut found = Vec::new();
        let typed = match (self.schema_version, self.engine_version) {
            (1, 1) => Some(false),
            (2, 2) => Some(true),
            _ => {
                found.push(Problem::of("invalid_version"));
                None
            }
        };
        if !(1..=MAX_REVISION).contains(&self.revision) {
            found.push(Problem::of("invalid_revision"));
        }
        if self.regex_count() > MAX_REGEX_RULES {
            found.push(Problem::of("too_many_regex_rules"));
        }
        let budget = RegexBudget::of(self);
        for (name, layer) in [
            ("neverSave", &self.layers.never_save),
            ("saveButNoMcp", &self.layers.save_but_no_mcp),
        ] {
            if layer.rules.len() > 1000 {
                found.push(Problem {
                    layer: Some(name),
                    ..Problem::of("too_many_rules")
                });
            }
            for (index, rule) in layer.rules.iter().enumerate() {
                let at = |code| Problem {
                    layer: Some(name),
                    rule_index: Some(index),
                    code,
                };
                if typed.is_some_and(|typed| matches!(rule, RuleSpec::Typed(_)) != typed) {
                    found.push(at("invalid_rule_form"));
                    continue;
                }
                let error = match rule.form() {
                    Form::Pattern(pattern, syntax) => Rule::parse_in(pattern, syntax).err(),
                    Form::Regex(pattern) => compile_regex(pattern, budget).err(),
                    Form::Domain(pattern) => domain_of(pattern, approvals).err(),
                };
                found.extend(error.map(|error| at(rule.error_code(error))));
            }
        }
        if found.is_empty() {
            found.push(Problem::of("invalid_policy"));
        }
        found
    }
    /// The version 2 form of a valid version 1 policy: every rule becomes a pattern rule with the
    /// same text, in the same layer and order, and nothing else changes. The caller stores it.
    pub fn migrated(&self) -> Result<Self, Error> {
        if (self.schema_version, self.engine_version) != (1, 1) {
            return Err(Error::NotMigratable);
        }
        self.validate()?;
        let typed = |layer: &Layer| -> Result<Layer, Error> {
            let rules = layer
                .rules
                .iter()
                .map(|rule| match rule {
                    RuleSpec::Text(pattern) => Ok(RuleSpec::Typed(TypedRule::Pattern {
                        pattern: pattern.clone(),
                    })),
                    RuleSpec::Typed(_) => Err(Error::NotMigratable),
                })
                .collect::<Result<_, _>>()?;
            Ok(Layer {
                enabled: layer.enabled,
                rules,
            })
        };
        let candidate = Self {
            schema_version: 2,
            engine_version: 2,
            revision: self.revision,
            categories: self.categories.clone(),
            layers: Layers {
                never_save: typed(&self.layers.never_save)?,
                save_but_no_mcp: typed(&self.layers.save_but_no_mcp)?,
            },
        };
        candidate.validate()?;
        Ok(candidate)
    }
    /// For one URL. To judge several, compile once and use `CompiledPolicy::evaluate`.
    /// Invalid configuration or URL always denies automatic recording and MCP disclosure.
    pub fn evaluate(&self, input: &str) -> Decision {
        CompiledPolicy::compile(self).map_or(Decision::NeverSave, |policy| policy.evaluate(input))
    }
}

/// Patterns of one zone by host, so a URL meets only the patterns of its own host and parent
/// domains. A `None` path covers the whole host. Regexes have no host and meet every URL. A domain
/// wildcard is a wildcard host with no path: it is judged exactly as the pattern `*.domain` is.
#[derive(Debug, Default)]
struct Zone {
    exact: HashMap<String, Vec<Option<PathRule>>>,
    wildcard: HashMap<String, Vec<Option<PathRule>>>,
    regexes: Vec<Regex>,
}
impl Zone {
    fn add(&mut self, rule: &str, syntax: Syntax) -> Result<(), Error> {
        let rule = Rule::parse_in(rule, syntax)?;
        let hosts = if rule.wildcard {
            &mut self.wildcard
        } else {
            &mut self.exact
        };
        hosts.entry(rule.host).or_default().push(rule.path);
        Ok(())
    }
    fn add_all(&mut self, rules: &[String]) -> Result<(), Error> {
        rules
            .iter()
            .try_for_each(|rule| self.add(rule, Syntax::Version1))
    }
    fn add_specs(
        &mut self,
        rules: &[RuleSpec],
        budget: RegexBudget,
        approvals: &DomainApprovals,
    ) -> Result<(), Error> {
        rules.iter().try_for_each(|rule| match rule.form() {
            Form::Pattern(pattern, syntax) => self.add(pattern, syntax),
            Form::Regex(pattern) => {
                self.regexes.push(compile_regex(pattern, budget)?);
                Ok(())
            }
            Form::Domain(pattern) => {
                self.wildcard
                    .entry(domain_of(pattern, approvals)?)
                    .or_default()
                    .push(None);
                Ok(())
            }
        })
    }
    /// A pattern meets any of the views of the path, a regex any of the views of the URL. Where a
    /// rule of the zone matches decides nothing else: the zone matches.
    fn matches(&self, host: &str, paths: &[&str], urls: &[&str]) -> bool {
        self.matches_patterns(host, paths)
            || self
                .regexes
                .iter()
                .any(|regex| urls.iter().any(|url| regex.is_match(url)))
    }
    fn matches_patterns(&self, host: &str, paths: &[&str]) -> bool {
        let covers = |rules: &Vec<Option<PathRule>>| {
            rules.iter().any(|rule| {
                rule.as_ref()
                    .is_none_or(|rule| paths.iter().any(|path| rule.matches(path)))
            })
        };
        if self.exact.get(host).is_some_and(covers) {
            return true;
        }
        // The whole host, then what follows each dot: exactly the suffixes on a label boundary.
        let mut suffix = host;
        loop {
            if self.wildcard.get(suffix).is_some_and(covers) {
                return true;
            }
            match suffix.split_once('.') {
                Some((_, rest)) => suffix = rest,
                None => return false,
            }
        }
    }
}

/// A policy prepared for judging many URLs. It exists only if every rule of the policy is valid,
/// also those of a disabled layer, and is immutable: a change of policy means compiling again.
#[derive(Debug)]
pub struct CompiledPolicy {
    blind: Zone,
    auth_paths: bool,
    credentials: bool,
    private: Zone,
}
impl CompiledPolicy {
    /// With no approved domain: what tests and pure checks use. The host compiles with the
    /// approvals it read (`compile_with`).
    pub fn compile(policy: &Policy) -> Result<Self, Error> {
        Self::compile_with(policy, &DomainApprovals::none())
    }
    pub fn compile_with(policy: &Policy, approvals: &DomainApprovals) -> Result<Self, Error> {
        policy.check_header()?;
        let a = &policy.layers.never_save;
        let b = &policy.layers.save_but_no_mcp;
        let c = &policy.categories;
        let budget = RegexBudget::of(policy);
        let mut blind = Zone::default();
        blind.add_specs(&a.rules, budget, approvals)?;
        if c.authentication {
            blind.add_all(&BUILTINS.authentication)?;
        }
        let mut private = Zone::default();
        private.add_specs(&b.rules, budget, approvals)?;
        for (enabled, rules) in [
            (c.government, &BUILTINS.government),
            (c.webmail, &BUILTINS.webmail),
            (c.ai_chats, &BUILTINS.ai_chats),
        ] {
            if enabled {
                private.add_all(rules)?;
            }
        }
        Ok(Self {
            blind: if a.enabled { blind } else { Zone::default() },
            auth_paths: a.enabled && c.authentication,
            credentials: a.enabled && c.credentials,
            private: if b.enabled { private } else { Zone::default() },
        })
    }
    /// Blind Zone first. An invalid URL is `NeverSave` whatever the layers say. A path rule meets
    /// the judged path and the same with its runs of `/` collapsed: either match counts.
    /// A regex meets the URL as the parser writes it, and without the trailing dots of its host.
    pub fn evaluate(&self, input: &str) -> Decision {
        let Ok((url, parsed)) = parse_url(input) else {
            return Decision::NeverSave;
        };
        let dotless = if self.blind.regexes.is_empty() && self.private.regexes.is_empty() {
            None
        } else {
            without_trailing_dots(&parsed)
        };
        let whole = [parsed.as_str(), dotless.as_deref().unwrap_or_default()];
        let urls = if dotless.is_some() {
            &whole[..]
        } else {
            &whole[..1]
        };
        let collapsed = collapse_slashes(&url.path);
        let both = [url.path.as_str(), collapsed.as_deref().unwrap_or_default()];
        let paths = if collapsed.is_some() {
            &both[..]
        } else {
            &both[..1]
        };
        if self.blind.matches(&url.host, paths, urls)
            || (self.auth_paths
                && BUILTINS
                    .auth_paths
                    .iter()
                    .any(|p| paths.iter().any(|path| path_matches(path, p))))
            || (self.credentials && has_credentials(&url, paths))
        {
            return Decision::NeverSave;
        }
        if self.private.matches(&url.host, paths, urls) {
            return Decision::SaveButNoMcp;
        }
        Decision::None
    }
}
impl Default for Policy {
    fn default() -> Self {
        Self::parse(DEFAULT_JSON.as_bytes()).expect("bundled default")
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Decision {
    NeverSave,
    SaveButNoMcp,
    None,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NormalizedUrl {
    pub host: String,
    pub port: Option<String>,
    pub path: String,
    pub userinfo: bool,
    pub query: String,
    pub fragment: String,
}
fn decode(input: &str) -> Result<String, Error> {
    let bytes = input.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = |b: u8| (b as char).to_digit(16).map(|v| v as u8);
            if i + 2 >= bytes.len() {
                return Err(Error::InvalidUrl);
            }
            let a = hex(bytes[i + 1]).ok_or(Error::InvalidUrl)?;
            let b = hex(bytes[i + 2]).ok_or(Error::InvalidUrl)?;
            out.push(a * 16 + b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| Error::InvalidUrl)
}
fn path_of(path: &str) -> Result<String, Error> {
    let decoded = decode(path)?;
    if decoded
        .chars()
        .any(|c| c <= ' ' || c == '\u{7f}' || c == '%' || c == '\\')
    {
        return Err(Error::InvalidUrl);
    }
    let mut parts = Vec::new();
    for part in decoded.split('/').skip(1) {
        match part {
            "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    Ok(format!("/{}", parts.join("/")))
}
/// The judged path with each run of `/` made one, if that changes it. Nothing else is rewritten:
/// no decoding beyond what `path_of` did, no case, dot segment or query change.
fn collapse_slashes(path: &str) -> Option<String> {
    if !path.contains("//") {
        return None;
    }
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        if !(c == '/' && out.ends_with('/')) {
            out.push(c);
        }
    }
    Some(out)
}
pub fn normalize_url(input: &str) -> Result<NormalizedUrl, Error> {
    parse_url(input).map(|(url, _)| url)
}
/// The judged form, and the parsed HTTP(S) URL it was taken from (the inner one of a `blob:`).
fn parse_url(input: &str) -> Result<(NormalizedUrl, Url), Error> {
    if input.encode_utf16().count() > 32768
        || input
            .chars()
            .any(|c| c <= ' ' || c == '\u{7f}' || c == '\\')
    {
        return Err(Error::InvalidUrl);
    }
    let mut url = Url::parse(input).map_err(|_| Error::InvalidUrl)?;
    if url.scheme() == "blob" {
        url = Url::parse(&input[5..]).map_err(|_| Error::InvalidUrl)?;
    }
    if !matches!(url.scheme(), "http" | "https") {
        return Err(Error::InvalidUrl);
    }
    let host = url
        .host_str()
        .ok_or(Error::InvalidUrl)?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if host.is_empty() {
        return Err(Error::InvalidUrl);
    }
    for text in [url.query().unwrap_or(""), url.fragment().unwrap_or("")] {
        if decode(text)?
            .chars()
            .any(|c| c < ' ' || c == '\u{7f}' || c == '%')
        {
            return Err(Error::InvalidUrl);
        }
    }
    let normalized = NormalizedUrl {
        host,
        port: url.port().map(|p| p.to_string()),
        path: path_of(url.path())?,
        userinfo: !url.username().is_empty() || url.password().is_some_and(|p| !p.is_empty()),
        query: url.query().unwrap_or("").into(),
        fragment: url.fragment().unwrap_or("").into(),
    };
    Ok((normalized, url))
}
/// The URL with the trailing dots of its host removed, if it has any: `example.com.` is the
/// host `example.com` to a pattern, and must not slip past a regex written for that.
fn without_trailing_dots(url: &Url) -> Option<String> {
    let host = url.host_str()?;
    let trimmed = host.trim_end_matches('.');
    if trimmed.len() == host.len() {
        return None;
    }
    let mut dotless = url.clone();
    dotless.set_host(Some(trimmed)).ok()?;
    Some(dotless.into())
}

#[derive(Debug)]
pub struct Rule {
    host: String,
    wildcard: bool,
    path: Option<PathRule>,
}
/// The path part of a rule, compared with a judged path.
#[derive(Debug, Clone, PartialEq)]
enum PathRule {
    /// `/dir/*`, the only form of version 1: the directory and everything below it.
    Prefix(String),
    /// Version 2: this path and no other, not even with a trailing `/`.
    Exact(String),
    /// Version 2: one `*` in the last segment, matching any text, also none, without a `/`.
    Name {
        dir: String,
        before: String,
        after: String,
    },
}
impl PathRule {
    fn parse(path: &str, syntax: Syntax) -> Result<Self, Error> {
        let stars = path.matches('*').count();
        if stars == 1 && path.ends_with("/*") {
            return Ok(Self::Prefix(path_of(&path[..path.len() - 1])?));
        }
        if syntax == Syntax::Version1 || stars > 1 {
            return Err(Error::InvalidPolicy);
        }
        // Read like a judged path. A dot segment, or an encoded `*` or `/` that would move the
        // wildcard, makes the rule mean something else than it says, so it is refused.
        let judged = path_of(path)?;
        if judged != decode(path)? || judged.matches('*').count() != stars {
            return Err(Error::InvalidPolicy);
        }
        let Some((before, after)) = judged.split_once('*') else {
            return Ok(Self::Exact(judged));
        };
        if after.contains('/') {
            return Err(Error::InvalidPolicy);
        }
        let (dir, before) = before.split_at(before.rfind('/').map_or(0, |i| i + 1));
        Ok(Self::Name {
            dir: dir.into(),
            before: before.into(),
            after: after.into(),
        })
    }
    fn matches(&self, path: &str) -> bool {
        match self {
            Self::Prefix(prefix) => path.starts_with(prefix.as_str()),
            Self::Exact(exact) => path == exact,
            Self::Name { dir, before, after } => {
                path.strip_prefix(dir.as_str()).is_some_and(|name| {
                    !name.contains('/')
                        && name.len() >= before.len() + after.len()
                        && name.starts_with(before.as_str())
                        && name.ends_with(after.as_str())
                })
            }
        }
    }
}
impl Rule {
    /// A version 1 pattern.
    pub fn parse(rule: &str) -> Result<Self, Error> {
        Self::parse_in(rule, Syntax::Version1)
    }
    /// A pattern read with the grammar of `syntax`.
    pub fn parse_in(rule: &str, syntax: Syntax) -> Result<Self, Error> {
        if rule.encode_utf16().count() > 2048
            || rule
                .chars()
                .any(|c| c <= ' ' || c == '\u{7f}' || "\\?#@:".contains(c))
        {
            return Err(Error::InvalidPolicy);
        }
        let wildcard = rule.starts_with("*.");
        let body = rule.strip_prefix("*.").unwrap_or(rule);
        let (raw_host, raw_path) = body
            .find('/')
            .map_or((body, None), |i| (&body[..i], Some(&body[i..])));
        if raw_host.is_empty() || raw_host.contains('*') {
            return Err(Error::InvalidPolicy);
        }
        let normalized =
            normalize_url(&format!("https://{raw_host}/")).map_err(|_| Error::InvalidPolicy)?;
        if normalized.userinfo || normalized.port.is_some() || normalized.path != "/" {
            return Err(Error::InvalidPolicy);
        }
        let path = raw_path
            .map(|path| PathRule::parse(path, syntax).map_err(|_| Error::InvalidPolicy))
            .transpose()?;
        Ok(Self {
            host: normalized.host,
            wildcard,
            path,
        })
    }
}
/// The domain a domain wildcard covers; see `public_suffix::diagnose`. A restricted domain only if
/// it was approved, exactly that canonical domain; a syntax error or an unreadable list never.
fn domain_of(pattern: &str, approvals: &DomainApprovals) -> Result<String, Error> {
    use crate::public_suffix::{diagnose, Diagnosis};
    if pattern.encode_utf16().count() > 2048 {
        return Err(Error::InvalidPolicy);
    }
    match diagnose(pattern) {
        Diagnosis::Valid(domain) => Ok(domain),
        Diagnosis::Restricted { domain, .. } if approvals.contains(&domain) => Ok(domain),
        Diagnosis::Restricted { .. } => Err(Error::UnapprovedDomain),
        Diagnosis::InvalidSyntax => Err(Error::InvalidPolicy),
        Diagnosis::ListUnavailable => Err(Error::SuffixListUnavailable),
    }
}
/// What one regex of a policy may use: an equal share of the policy's budget, at most the limit
/// of one rule. So all regexes of a policy together stay within the totals.
#[derive(Debug, Clone, Copy)]
struct RegexBudget {
    size: usize,
    dfa: usize,
}
impl RegexBudget {
    fn of(policy: &Policy) -> Self {
        let count = policy.regex_count().max(1);
        Self {
            size: (REGEX_TOTAL_SIZE_LIMIT / count).min(REGEX_SIZE_LIMIT),
            dfa: (REGEX_TOTAL_DFA_SIZE_LIMIT / count).min(REGEX_DFA_SIZE_LIMIT),
        }
    }
}
/// A regex rule ready to judge. A syntax error makes the policy invalid; look-around and
/// backreferences, which the `regex` crate does not have, unsupported; a limit, `RegexLimit`.
fn compile_regex(pattern: &str, budget: RegexBudget) -> Result<Regex, Error> {
    if pattern.len() > MAX_REGEX_BYTES {
        return Err(Error::RegexLimit);
    }
    RegexBuilder::new(pattern)
        .size_limit(budget.size)
        .dfa_size_limit(budget.dfa)
        .build()
        .map_err(|error| match error {
            regex::Error::CompiledTooBig(_) => Error::RegexLimit,
            _ => regex_syntax_error(pattern),
        })
}
fn regex_syntax_error(pattern: &str) -> Error {
    use regex_syntax::ast::ErrorKind;
    match regex_syntax::ast::parse::Parser::new().parse(pattern) {
        Err(error) => match error.kind() {
            ErrorKind::UnsupportedLookAround | ErrorKind::UnsupportedBackreference => {
                Error::UnsupportedRule
            }
            ErrorKind::NestLimitExceeded(_) => Error::RegexLimit,
            _ => Error::InvalidPolicy,
        },
        Ok(_) => Error::InvalidPolicy,
    }
}
/// Whether one version 2 rule alone matches `url`, judged by the compiled form a policy uses: the
/// rule alone in an enabled Blind Zone, with no builtin. This is no decision: a URL it does not
/// match may still be refused by another rule or a builtin. An invalid URL is `InvalidUrl`, which
/// a policy would refuse. The rule gets the regex budget of a policy holding only it.
pub fn rule_matches(rule: &RuleSpec, url: &str) -> Result<bool, Error> {
    rule_matches_with(rule, url, &DomainApprovals::none())
}
/// `rule_matches` with the domains a person approved, as the host judges.
pub fn rule_matches_with(
    rule: &RuleSpec,
    url: &str,
    approvals: &DomainApprovals,
) -> Result<bool, Error> {
    if matches!(rule, RuleSpec::Text(_)) {
        return Err(Error::InvalidPolicy);
    }
    normalize_url(url)?;
    let alone = Policy {
        categories: Categories {
            authentication: false,
            credentials: false,
            government: false,
            webmail: false,
            ai_chats: false,
        },
        layers: Layers {
            never_save: Layer {
                enabled: true,
                rules: vec![rule.clone()],
            },
            save_but_no_mcp: Layer {
                enabled: false,
                rules: Vec::new(),
            },
        },
        ..Policy::default()
    };
    Ok(CompiledPolicy::compile_with(&alone, approvals)?.evaluate(url) == Decision::NeverSave)
}
fn path_matches(path: &str, prefix: &str) -> bool {
    path == prefix || path.starts_with(&format!("{prefix}/"))
}
/// What follows the last `?` of a parameter name counts, so `?token`, `??token` and `a?token` are
/// `token`: the name of a credential is not hidden by the separator of a query inside a query.
fn parameter_name(key: &str) -> String {
    key.rsplit('?').next().unwrap_or(key).to_ascii_lowercase()
}
fn has_credentials(url: &NormalizedUrl, paths: &[&str]) -> bool {
    let fragment = url
        .fragment
        .split_once('?')
        .map_or(url.fragment.as_str(), |(_, q)| q);
    let names: Vec<String> = [&url.query, &url.fragment, fragment]
        .iter()
        .flat_map(|text| {
            url::form_urlencoded::parse(text.as_bytes()).map(|(k, _)| parameter_name(&k))
        })
        .collect();
    url.userinfo
        || names.iter().any(|k| BUILTINS.credential_keys.contains(k))
        || (names.iter().any(|k| k == "code")
            && (names.iter().any(|k| k == "state")
                || BUILTINS
                    .callback_paths
                    .iter()
                    .any(|p| paths.iter().any(|path| path_matches(path, p)))))
}

#[cfg(test)]
mod tests;
