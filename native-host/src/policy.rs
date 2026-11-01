//! Version 1 URL/rule contract; see docs/issue-010-privacy-policy.md.
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::LazyLock;
use url::Url;

pub const MAX_REVISION: u64 = 9_007_199_254_740_991;
pub const DEFAULT_JSON: &str = include_str!("../../extension/policy-defaults.json");

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
    pub rules: Vec<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidPolicy,
    InvalidUrl,
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
        let policy: Self = serde_json::from_slice(bytes).map_err(|_| Error::InvalidPolicy)?;
        policy.validate()?;
        Ok(policy)
    }
    fn check_header(&self) -> Result<(), Error> {
        if self.schema_version != 1
            || self.engine_version != 1
            || !(1..=MAX_REVISION).contains(&self.revision)
        {
            return Err(Error::InvalidPolicy);
        }
        for layer in [&self.layers.never_save, &self.layers.save_but_no_mcp] {
            if layer.rules.len() > 1000 {
                return Err(Error::InvalidPolicy);
            }
        }
        Ok(())
    }
    pub fn validate(&self) -> Result<(), Error> {
        self.check_header()?;
        for layer in [&self.layers.never_save, &self.layers.save_but_no_mcp] {
            for rule in &layer.rules {
                Rule::parse(rule)?;
            }
        }
        Ok(())
    }
    /// For one URL. To judge several, compile once and use `CompiledPolicy::evaluate`.
    /// Invalid configuration or URL always denies automatic recording and MCP disclosure.
    pub fn evaluate(&self, input: &str) -> Decision {
        CompiledPolicy::compile(self).map_or(Decision::NeverSave, |policy| policy.evaluate(input))
    }
}

/// Rules of one zone by host, so a URL meets only the rules of its own host and parent domains.
/// A `None` path covers the whole host.
#[derive(Debug, Default)]
struct Zone {
    exact: HashMap<String, Vec<Option<String>>>,
    wildcard: HashMap<String, Vec<Option<String>>>,
}
impl Zone {
    fn add(&mut self, rule: &str) -> Result<(), Error> {
        let rule = Rule::parse(rule)?;
        let hosts = if rule.wildcard {
            &mut self.wildcard
        } else {
            &mut self.exact
        };
        hosts.entry(rule.host).or_default().push(rule.path);
        Ok(())
    }
    fn add_all(&mut self, rules: &[String]) -> Result<(), Error> {
        rules.iter().try_for_each(|rule| self.add(rule))
    }
    fn matches(&self, url: &NormalizedUrl) -> bool {
        let covers = |paths: &Vec<Option<String>>| {
            paths.iter().any(|path| {
                path.as_ref()
                    .is_none_or(|p| url.path.starts_with(p.as_str()))
            })
        };
        if self.exact.get(&url.host).is_some_and(covers) {
            return true;
        }
        // The whole host, then what follows each dot: exactly the suffixes on a label boundary.
        let mut suffix = url.host.as_str();
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
    pub fn compile(policy: &Policy) -> Result<Self, Error> {
        policy.check_header()?;
        let a = &policy.layers.never_save;
        let b = &policy.layers.save_but_no_mcp;
        let c = &policy.categories;
        let mut blind = Zone::default();
        blind.add_all(&a.rules)?;
        if c.authentication {
            blind.add_all(&BUILTINS.authentication)?;
        }
        let mut private = Zone::default();
        private.add_all(&b.rules)?;
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
    /// Blind Zone first. An invalid URL is `NeverSave` whatever the layers say.
    pub fn evaluate(&self, input: &str) -> Decision {
        let Ok(url) = normalize_url(input) else {
            return Decision::NeverSave;
        };
        if self.blind.matches(&url)
            || (self.auth_paths
                && BUILTINS
                    .auth_paths
                    .iter()
                    .any(|p| path_matches(&url.path, p)))
            || (self.credentials && has_credentials(&url))
        {
            return Decision::NeverSave;
        }
        if self.private.matches(&url) {
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
pub fn normalize_url(input: &str) -> Result<NormalizedUrl, Error> {
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
    Ok(NormalizedUrl {
        host,
        port: url.port().map(|p| p.to_string()),
        path: path_of(url.path())?,
        userinfo: !url.username().is_empty() || url.password().is_some_and(|p| !p.is_empty()),
        query: url.query().unwrap_or("").into(),
        fragment: url.fragment().unwrap_or("").into(),
    })
}

#[derive(Debug)]
pub struct Rule {
    host: String,
    wildcard: bool,
    path: Option<String>,
}
impl Rule {
    pub fn parse(rule: &str) -> Result<Self, Error> {
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
        let path = if let Some(path) = raw_path {
            if !path.ends_with("/*") || path[..path.len() - 1].contains('*') {
                return Err(Error::InvalidPolicy);
            }
            Some(path_of(&path[..path.len() - 1]).map_err(|_| Error::InvalidPolicy)?)
        } else {
            None
        };
        Ok(Self {
            host: normalized.host,
            wildcard,
            path,
        })
    }
}
fn path_matches(path: &str, prefix: &str) -> bool {
    path == prefix || path.starts_with(&format!("{prefix}/"))
}
/// What follows the last `?` of a parameter name counts, so `?token`, `??token` and `a?token` are
/// `token`: the name of a credential is not hidden by the separator of a query inside a query.
fn parameter_name(key: &str) -> String {
    key.rsplit('?').next().unwrap_or(key).to_ascii_lowercase()
}
fn has_credentials(url: &NormalizedUrl) -> bool {
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
                    .any(|p| path_matches(&url.path, p))))
}

#[cfg(test)]
mod tests;
