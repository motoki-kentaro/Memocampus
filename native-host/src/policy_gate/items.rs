//! Policy changes for one URL of a search result (Issue #43, Slice 1): add it to Never Save, add it
//! to Save but no MCP, or "publish" it. Nothing new is judged here: a change is a draft of the
//! stored policy, compiled and evaluated by the policy engine, and stored through `PolicyStore` in
//! place of the policy of a given ETag, exactly as `policy_apply` does.
//!
//! ```text
//! → policy_item_preview { requestId, url, action, scope? }
//! ← { ok: true, etag, revision, url, action, scope, current, expected, changes, affectsOtherUrls,
//!     executable, reason?, blockers? }
//! → policy_item_apply   { requestId, url, action, scope?, expectedEtag }
//! ← { ok: true, etag, revision, url, action, scope, previous, decision, changed, changes }
//! ```
//!
//! - `action`: `never_save`, `save_but_no_mcp` or `publish`. `scope`: `url` (the default) or `host`;
//!   `publish` takes `url` only.
//! - Scope `url` is the URL exactly: an exact rule `{ kind: "exact", pattern: <the URL as the parser
//!   writes it> }`, compared as `policy::exact_url_key` (Slice 2; Slice 1 wrote an escaped regex). The
//!   pattern grammar cannot name a query, a fragment, a scheme or a port, so a pattern for the URL
//!   would cover other URLs too. Scope `host` is a pattern rule of the URL's host alone: every path, query,
//!   scheme and port of that host, not its subdomains. Only an explicit `scope: "host"` gives one.
//! - `publish` is no permission and adds no rule. It removes the URL-exact rules of this URL (the
//!   exact rules naming this URL, however written), in both layers, and only if the policy then allows the URL. A URL kept
//!   private by any other rule (a host, a wildcard, a domain wildcard, another regex) or by a
//!   builtin category is refused with `publish_blocked`, and nothing is removed: such a rule also
//!   covers other URLs, so it is changed on the settings page or not at all. A URL the policy allows
//!   already is a no-op.
//! - `current` and `expected` are decisions (`allow`, `save_but_no_mcp`, `never_save`) of the policy
//!   engine for the stored policy and for the policy after the change. `changes` lists the rules
//!   added or removed: `{ op: "add" | "remove", layer: "neverSave" | "saveButNoMcp", rule, ruleIndex? }`.
//! - `executable: false` gives `reason`: `policy_migration_required` (a version 1 policy has no
//!   exact rule; `policy_migrate` first), `layer_disabled` (the rule would change nothing
//!   while its layer is off), `publish_blocked` (with `blockers`: `{ layer, ruleIndex, kind }` of
//!   each other rule that matches, and `{ layer, builtin: true }` for a builtin category; never the
//!   text of a rule), `policy_limit` (the policy would hold too many rules, or not fit the file),
//!   `unsupported_url` (no pattern can name this host, such as an IPv6 address).
//! - Apply recomputes the plan from the stored policy and refuses with `policy_conflict` unless it
//!   is still the policy of `expectedEtag`, the one the preview showed; the store checks the same
//!   again under its write lock. An unexecutable plan is refused with its `reason`, and a plan that
//!   changes nothing writes nothing (`changed: false`). `decision` is judged on the stored result.
//!
//! The policy is global: these messages need no vault and touch none. MCP reads the policy again
//! for every request (`VisibleStore`), so a stored change applies to its next answer.

use super::{
    answer, decision_code, etag_of, store_code, url_of, PolicyGate, Refusal, CONFLICT, UNAVAILABLE,
};
use memocampus_native_host::policy::{
    self, CompiledPolicy, Decision, DomainApprovals, Layer, Policy, RuleSpec, Syntax, TypedRule,
};
use memocampus_native_host::policy_store as store;
use serde_json::{json, Value};

pub const MIGRATION_REQUIRED: &str = "policy_migration_required";
pub const LAYER_DISABLED: &str = "layer_disabled";
pub const PUBLISH_BLOCKED: &str = "publish_blocked";
pub const POLICY_LIMIT: &str = "policy_limit";
pub const UNSUPPORTED_URL: &str = "unsupported_url";
const INVALID_ACTION: &str = "invalid_action";
const INVALID_SCOPE: &str = "invalid_scope";
const INVALID_URL: &str = "invalid_url";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    NeverSave,
    SaveButNoMcp,
    Publish,
}

impl Action {
    fn code(self) -> &'static str {
        match self {
            Self::NeverSave => "never_save",
            Self::SaveButNoMcp => "save_but_no_mcp",
            Self::Publish => "publish",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Url,
    Host,
}

impl Scope {
    fn code(self) -> &'static str {
        match self {
            Self::Url => "url",
            Self::Host => "host",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerName {
    NeverSave,
    SaveButNoMcp,
}

impl LayerName {
    const BOTH: [Self; 2] = [Self::NeverSave, Self::SaveButNoMcp];

    fn code(self) -> &'static str {
        match self {
            Self::NeverSave => "neverSave",
            Self::SaveButNoMcp => "saveButNoMcp",
        }
    }

    fn of(self, policy: &Policy) -> &Layer {
        match self {
            Self::NeverSave => &policy.layers.never_save,
            Self::SaveButNoMcp => &policy.layers.save_but_no_mcp,
        }
    }

    fn of_mut(self, policy: &mut Policy) -> &mut Layer {
        match self {
            Self::NeverSave => &mut policy.layers.never_save,
            Self::SaveButNoMcp => &mut policy.layers.save_but_no_mcp,
        }
    }
}

/// One rule added to or removed from a layer. `index` is its place in the stored policy, for a
/// removal only.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub remove: bool,
    pub layer: LayerName,
    pub index: Option<usize>,
    pub rule: RuleSpec,
}

/// What keeps a URL private after its URL-exact rules are gone: a rule of an enabled layer that
/// matches it (by place and kind), or a builtin category (`rule: None`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blocker {
    pub layer: LayerName,
    pub rule: Option<(usize, &'static str)>,
}

#[derive(Debug)]
pub struct Plan {
    pub current: Decision,
    pub expected: Decision,
    pub changes: Vec<Change>,
    /// The policy to store, only when something changes and the plan can be carried out.
    pub candidate: Option<Policy>,
    pub refusal: Option<&'static str>,
    pub blockers: Vec<Blocker>,
}

impl Plan {
    fn unchanged(current: Decision) -> Self {
        Self {
            current,
            expected: current,
            changes: Vec::new(),
            candidate: None,
            refusal: None,
            blockers: Vec::new(),
        }
    }

    fn refused(current: Decision, code: &'static str) -> Self {
        Self {
            refusal: Some(code),
            ..Self::unchanged(current)
        }
    }
}

/// The exact rule of `url`: the URL as the parser writes it, compared by the engine as
/// `policy::exact_url_key`.
pub fn url_rule(url: &str) -> Result<RuleSpec, &'static str> {
    let parsed = url::Url::parse(url).map_err(|_| INVALID_URL)?;
    Ok(RuleSpec::Typed(TypedRule::Exact {
        pattern: parsed.as_str().to_owned(),
    }))
}

/// Whether `rule` is an exact rule of the URL whose key is `key`: written in any form the engine
/// reads as this one URL, and of no other.
fn names_exactly(rule: &RuleSpec, key: &str) -> bool {
    matches!(rule, RuleSpec::Typed(TypedRule::Exact { pattern })
        if policy::exact_url_key(pattern).is_ok_and(|other| other == key))
}

/// The pattern rule of the URL's host alone, if the pattern grammar can name it.
pub fn host_rule(url: &str) -> Result<RuleSpec, &'static str> {
    let host = policy::normalize_url(url).map_err(|_| INVALID_URL)?.host;
    policy::Rule::parse_in(&host, Syntax::Version2).map_err(|_| UNSUPPORTED_URL)?;
    Ok(RuleSpec::Typed(TypedRule::Pattern { pattern: host }))
}

fn kind_of(rule: &RuleSpec) -> &'static str {
    match rule {
        RuleSpec::Text(_) | RuleSpec::Typed(TypedRule::Pattern { .. }) => "pattern",
        RuleSpec::Typed(TypedRule::Regex { .. }) => "regex",
        RuleSpec::Typed(TypedRule::DomainWildcard { .. }) => "domainWildcard",
        RuleSpec::Typed(TypedRule::Exact { .. }) => "exact",
    }
}

/// A URL a request may name: one the policy engine can judge, of a web page (no `blob:`).
pub fn check_url(url: &str) -> Result<(), &'static str> {
    let web = url::Url::parse(url).is_ok_and(|parsed| matches!(parsed.scheme(), "http" | "https"));
    if !web || policy::normalize_url(url).is_err() {
        return Err(INVALID_URL);
    }
    Ok(())
}

/// The policy after the change, if the engine can use it and the store can write it, and its
/// compiled form.
fn compile(
    candidate: &Policy,
    approvals: &DomainApprovals,
) -> Result<CompiledPolicy, &'static str> {
    let compiled = CompiledPolicy::compile_with(candidate, approvals).map_err(|_| POLICY_LIMIT)?;
    if !store::fits(candidate) {
        return Err(POLICY_LIMIT);
    }
    Ok(compiled)
}

/// What `action` would do to `policy` (compiled as `compiled`) for `url`. `Err` only for a URL no
/// request may name; every state of the policy that prevents the change is a refusal in the plan.
pub fn plan(
    policy: &Policy,
    compiled: &CompiledPolicy,
    approvals: &DomainApprovals,
    url: &str,
    action: Action,
    scope: Scope,
) -> Result<Plan, &'static str> {
    check_url(url)?;
    let current = compiled.evaluate(url);
    let typed = (policy.schema_version, policy.engine_version) == (2, 2);
    let layer = match action {
        Action::NeverSave => LayerName::NeverSave,
        Action::SaveButNoMcp => LayerName::SaveButNoMcp,
        Action::Publish => return Ok(publish(policy, approvals, url, current, typed)),
    };
    if !typed {
        return Ok(Plan::refused(current, MIGRATION_REQUIRED));
    }
    let rule = match scope {
        Scope::Url => url_rule(url),
        Scope::Host => host_rule(url),
    };
    let rule = match rule {
        Ok(rule) => rule,
        Err(code) => return Ok(Plan::refused(current, code)),
    };
    // The rule must mean this URL to the engine, or it is not added.
    if policy::rule_matches_with(&rule, url, approvals) != Ok(true) {
        return Ok(Plan::refused(current, UNSUPPORTED_URL));
    }
    if !layer.of(policy).enabled {
        return Ok(Plan::refused(current, LAYER_DISABLED));
    }
    let key = policy::exact_url_key(url).map_err(|_| INVALID_URL)?;
    let present = layer.of(policy).rules.iter().any(|other| match scope {
        Scope::Url => names_exactly(other, &key),
        Scope::Host => other == &rule,
    });
    if present {
        return Ok(Plan::unchanged(current));
    }
    let mut candidate = policy.clone();
    layer.of_mut(&mut candidate).rules.push(rule.clone());
    let changes = vec![Change {
        remove: false,
        layer,
        index: None,
        rule,
    }];
    match compile(&candidate, approvals) {
        Ok(after) => Ok(Plan {
            current,
            expected: after.evaluate(url),
            changes,
            candidate: Some(candidate),
            refusal: None,
            blockers: Vec::new(),
        }),
        Err(code) => Ok(Plan {
            changes,
            ..Plan::refused(current, code)
        }),
    }
}

fn publish(
    policy: &Policy,
    approvals: &DomainApprovals,
    url: &str,
    current: Decision,
    typed: bool,
) -> Plan {
    if current == Decision::None {
        return Plan::unchanged(current);
    }
    if !typed {
        return Plan::refused(current, MIGRATION_REQUIRED);
    }
    let Ok(key) = policy::exact_url_key(url) else {
        return Plan::refused(current, INVALID_URL);
    };
    let mut candidate = policy.clone();
    let mut changes = Vec::new();
    for layer in LayerName::BOTH {
        for (index, rule) in layer.of(policy).rules.iter().enumerate() {
            if names_exactly(rule, &key) {
                changes.push(Change {
                    remove: true,
                    layer,
                    index: Some(index),
                    rule: rule.clone(),
                });
            }
        }
        layer
            .of_mut(&mut candidate)
            .rules
            .retain(|rule| !names_exactly(rule, &key));
    }
    let after = match compile(&candidate, approvals) {
        Ok(after) => after.evaluate(url),
        Err(code) => {
            return Plan {
                changes,
                ..Plan::refused(current, code)
            }
        }
    };
    if after != Decision::None {
        return Plan {
            expected: current,
            changes,
            blockers: blockers(policy, approvals, url, &key),
            ..Plan::refused(current, PUBLISH_BLOCKED)
        };
    }
    Plan {
        current,
        expected: after,
        changes,
        candidate: Some(candidate),
        refusal: None,
        blockers: Vec::new(),
    }
}

/// Every other rule of an enabled layer that matches `url`, and the builtin categories that keep it
/// private: what the policy says with every rule of both layers removed.
fn blockers(policy: &Policy, approvals: &DomainApprovals, url: &str, key: &str) -> Vec<Blocker> {
    let mut found = Vec::new();
    for layer in LayerName::BOTH {
        let rules = layer.of(policy);
        if !rules.enabled {
            continue;
        }
        for (index, rule) in rules.rules.iter().enumerate() {
            if !names_exactly(rule, key)
                && policy::rule_matches_with(rule, url, approvals) == Ok(true)
            {
                found.push(Blocker {
                    layer,
                    rule: Some((index, kind_of(rule))),
                });
            }
        }
    }
    let mut bare = policy.clone();
    for layer in LayerName::BOTH {
        layer.of_mut(&mut bare).rules.clear();
    }
    let builtin = CompiledPolicy::compile_with(&bare, approvals)
        .map_or(Decision::NeverSave, |compiled| compiled.evaluate(url));
    let builtin_layer = match builtin {
        Decision::NeverSave => Some(LayerName::NeverSave),
        Decision::SaveButNoMcp => Some(LayerName::SaveButNoMcp),
        Decision::None => None,
    };
    found.extend(builtin_layer.map(|layer| Blocker { layer, rule: None }));
    found
}

fn rule_json(rule: &RuleSpec) -> Value {
    match rule {
        RuleSpec::Text(pattern) => json!({ "kind": "pattern", "pattern": pattern }),
        RuleSpec::Typed(typed) => json!(typed),
    }
}

fn changes_json(changes: &[Change]) -> Value {
    changes
        .iter()
        .map(|change| {
            let mut item = json!({
                "op": if change.remove { "remove" } else { "add" },
                "layer": change.layer.code(),
                "rule": rule_json(&change.rule),
            });
            if let Some(index) = change.index {
                item["ruleIndex"] = index.into();
            }
            item
        })
        .collect()
}

fn blockers_json(blockers: &[Blocker]) -> Value {
    blockers
        .iter()
        .map(|blocker| match blocker.rule {
            Some((index, kind)) => {
                json!({ "layer": blocker.layer.code(), "ruleIndex": index, "kind": kind })
            }
            None => json!({ "layer": blocker.layer.code(), "builtin": true }),
        })
        .collect()
}

/// The URL, action and scope of a request.
fn request_of(message: &Value) -> Result<(&str, Action, Scope), Refusal> {
    let url = url_of(message)?;
    let action = match message.get("action").and_then(Value::as_str) {
        Some("never_save") => Action::NeverSave,
        Some("save_but_no_mcp") => Action::SaveButNoMcp,
        Some("publish") => Action::Publish,
        _ => return Err(Refusal::Code(INVALID_ACTION)),
    };
    let scope = match message.get("scope") {
        None | Some(Value::Null) => Scope::Url,
        Some(value) => match value.as_str() {
            Some("url") => Scope::Url,
            Some("host") if action != Action::Publish => Scope::Host,
            _ => return Err(Refusal::Code(INVALID_SCOPE)),
        },
    };
    check_url(url).map_err(Refusal::Code)?;
    Ok((url, action, scope))
}

impl PolicyGate {
    /// What an action would change, judged by the engine. Changes nothing.
    pub fn item_preview(&self, message: &Value) -> Value {
        answer(message, "policy_item_preview_result", || {
            let (url, action, scope) = request_of(message)?;
            let snapshot = self.load().map_err(Refusal::Code)?;
            let plan = plan(
                &snapshot.policy,
                &snapshot.compiled,
                &self.approvals(),
                url,
                action,
                scope,
            )
            .map_err(Refusal::Code)?;
            let mut reply = json!({
                "etag": snapshot.etag,
                "revision": snapshot.policy.revision,
                "url": url,
                "action": action.code(),
                "scope": scope.code(),
                "current": decision_code(plan.current),
                "expected": decision_code(plan.expected),
                "changes": changes_json(&plan.changes),
                "affectsOtherUrls": scope == Scope::Host && action != Action::Publish,
                "executable": plan.refusal.is_none(),
            });
            if let Some(code) = plan.refusal {
                reply["reason"] = code.into();
            }
            if !plan.blockers.is_empty() {
                reply["blockers"] = blockers_json(&plan.blockers);
            }
            Ok(reply)
        })
    }

    /// The previewed change, only against the policy of `expectedEtag`.
    pub fn item_apply(&self, message: &Value) -> Value {
        answer(message, "policy_item_apply_result", || {
            let (url, action, scope) = request_of(message)?;
            let expected = etag_of(message, "expectedEtag")?;
            let current = self.load().map_err(Refusal::Code)?;
            if current.etag != expected {
                return Err(Refusal::Code(CONFLICT));
            }
            let plan = plan(
                &current.policy,
                &current.compiled,
                &self.approvals(),
                url,
                action,
                scope,
            )
            .map_err(Refusal::Code)?;
            if let Some(code) = plan.refusal {
                return Err(Refusal::Code(code));
            }
            let stored = match plan.candidate {
                None => current,
                Some(candidate) => {
                    let store = self
                        .store
                        .as_ref()
                        .map_err(|_| Refusal::Code(UNAVAILABLE))?;
                    store
                        .update(&current, candidate)
                        .map_err(|error| Refusal::Code(store_code(error)))?
                }
            };
            Ok(json!({
                "etag": stored.etag,
                "revision": stored.policy.revision,
                "url": url,
                "action": action.code(),
                "scope": scope.code(),
                "previous": decision_code(plan.current),
                "decision": decision_code(stored.compiled.evaluate(url)),
                "changed": !plan.changes.is_empty(),
                "changes": changes_json(&plan.changes),
            }))
        })
    }
}

#[cfg(test)]
mod tests;
