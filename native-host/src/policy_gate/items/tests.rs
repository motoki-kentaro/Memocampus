use super::*;
use memocampus_native_host::policy_store::PolicyStore;

const URL: &str = "https://news.test/story?id=1#top";
const SAME_PAGE: &str = "https://news.test/story?id=2";
const OTHER_PAGE: &str = "https://news.test/other";
const SUBDOMAIN: &str = "https://www.news.test/story?id=1#top";

fn policy_with(never: &[RuleSpec], private: &[RuleSpec]) -> Policy {
    let mut policy = Policy::default();
    policy.layers.never_save.rules = never.to_vec();
    policy.layers.save_but_no_mcp.rules = private.to_vec();
    policy
}

/// `.test` is no public suffix: a domain wildcard under it needs an approval, as on the settings page.
fn approvals() -> DomainApprovals {
    DomainApprovals::of(["news.test".to_owned()])
}

fn run(policy: &Policy, url: &str, action: Action, scope: Scope) -> Plan {
    let compiled = CompiledPolicy::compile_with(policy, &approvals()).unwrap();
    plan(policy, &compiled, &approvals(), url, action, scope).unwrap()
}

fn decide(policy: &Policy, url: &str) -> Decision {
    CompiledPolicy::compile_with(policy, &approvals())
        .unwrap()
        .evaluate(url)
}

fn pattern(text: &str) -> RuleSpec {
    RuleSpec::Typed(TypedRule::Pattern {
        pattern: text.into(),
    })
}

fn regex(text: &str) -> RuleSpec {
    RuleSpec::Typed(TypedRule::Regex {
        pattern: text.into(),
    })
}

fn exact(url: &str) -> RuleSpec {
    url_rule(url).unwrap()
}

#[test]
fn a_url_rule_names_exactly_that_url() {
    let policy = Policy::default();
    let plan = run(&policy, URL, Action::NeverSave, Scope::Url);
    assert_eq!(
        (plan.current, plan.expected),
        (Decision::None, Decision::NeverSave)
    );
    assert_eq!(plan.refusal, None);
    assert_eq!(
        plan.changes,
        [Change {
            remove: false,
            layer: LayerName::NeverSave,
            index: None,
            rule: RuleSpec::Typed(TypedRule::Exact {
                pattern: URL.into()
            }),
        }]
    );
    let after = plan.candidate.unwrap();
    assert_eq!(
        after.revision, policy.revision,
        "the store writes the next revision"
    );
    assert_eq!(decide(&after, URL), Decision::NeverSave);
    // The same URL written otherwise is the same URL to the engine; any other URL is untouched.
    assert_eq!(
        decide(&after, "HTTPS://NEWS.TEST:443/story?id=1#top"),
        Decision::NeverSave
    );
    for other in [
        SAME_PAGE,
        "https://news.test/story?id=1",
        "https://news.test/story?id=10#top",
        "http://news.test/story?id=1#top",
        "https://news.test:8443/story?id=1#top",
        "https://news.test/story?id=1#top2",
        SUBDOMAIN,
        OTHER_PAGE,
    ] {
        assert_eq!(decide(&after, other), Decision::None, "{other}");
    }
}

#[test]
fn regex_characters_of_a_url_are_plain_text() {
    let url = "https://a.test/p(1)+x.*[a]|b^$?q={2}&r=)";
    let Ok(()) = check_url(url) else {
        // Not every such URL is one the engine can judge; this one must be.
        panic!("test URL must be valid");
    };
    let plan = run(&Policy::default(), url, Action::NeverSave, Scope::Url);
    let after = plan.candidate.expect("added");
    assert_eq!(decide(&after, url), Decision::NeverSave);
    for other in [
        "https://a.test/p1+x.*[a]|b^$?q={2}&r=)",
        "https://a.test/p(1)xx.*[a]|b^$?q={2}&r=)",
        "https://a.test/p(1)+xA*[a]|b^$?q={2}&r=)",
        "https://a.test/p(1)+x.*[a]|b^$?q={2}&r=))",
        "https://a.test/p(1)+x.*[a]|b^$?q={22}&r=)",
    ] {
        assert_eq!(decide(&after, other), Decision::None, "{other}");
    }
}

#[test]
fn a_host_rule_covers_the_host_only_when_asked_for() {
    let plan = run(&Policy::default(), URL, Action::SaveButNoMcp, Scope::Host);
    assert_eq!(
        plan.changes,
        [Change {
            remove: false,
            layer: LayerName::SaveButNoMcp,
            index: None,
            rule: pattern("news.test"),
        }]
    );
    let after = plan.candidate.unwrap();
    for url in [URL, SAME_PAGE, OTHER_PAGE, "http://news.test:8080/x"] {
        assert_eq!(decide(&after, url), Decision::SaveButNoMcp, "{url}");
    }
    assert_eq!(
        decide(&after, SUBDOMAIN),
        Decision::None,
        "not the subdomains"
    );
    // The host of a URL as the engine reads it: lower case, no trailing dot, punycode.
    assert_eq!(
        host_rule("https://WWW.Example.TEST./a").unwrap(),
        pattern("www.example.test")
    );
    assert_eq!(
        host_rule("https://bücher.test/").unwrap(),
        pattern("xn--bcher-kva.test")
    );
    assert_eq!(host_rule("https://[::1]/a"), Err(UNSUPPORTED_URL));
    let refused = run(
        &Policy::default(),
        "https://[::1]/a",
        Action::NeverSave,
        Scope::Host,
    );
    assert_eq!(
        (refused.refusal, refused.candidate.is_none()),
        (Some(UNSUPPORTED_URL), true)
    );
}

#[test]
fn adding_is_idempotent_and_never_loosens() {
    let rule = exact(URL);
    let policy = policy_with(std::slice::from_ref(&rule), &[]);
    let again = run(&policy, URL, Action::NeverSave, Scope::Url);
    assert_eq!(
        (
            again.refusal,
            again.changes.len(),
            again.candidate.is_none()
        ),
        (None, 0, true)
    );
    assert_eq!(again.expected, Decision::NeverSave);
    // Save but no MCP on a Never Save URL: added (it outlives the Never Save rule), no looser.
    let private = run(&policy, URL, Action::SaveButNoMcp, Scope::Url);
    assert_eq!(
        (private.current, private.expected),
        (Decision::NeverSave, Decision::NeverSave)
    );
    assert_eq!(private.candidate.unwrap().layers.never_save.rules, [rule]);
}

#[test]
fn a_disabled_layer_takes_no_rule() {
    let mut policy = Policy::default();
    policy.layers.save_but_no_mcp.enabled = false;
    let plan = run(&policy, URL, Action::SaveButNoMcp, Scope::Url);
    assert_eq!(plan.refusal, Some(LAYER_DISABLED));
    assert!(plan.candidate.is_none());
    assert_eq!(plan.expected, Decision::None);
}

#[test]
fn publishing_removes_the_url_rules_of_this_url_only() {
    let other = exact(SAME_PAGE);
    let policy = policy_with(
        &[other.clone(), exact(URL)],
        &[exact(URL), pattern("elsewhere.test")],
    );
    let plan = run(&policy, URL, Action::Publish, Scope::Url);
    assert_eq!(
        (plan.current, plan.expected, plan.refusal),
        (Decision::NeverSave, Decision::None, None)
    );
    assert_eq!(
        plan.changes
            .iter()
            .map(|change| (change.remove, change.layer, change.index))
            .collect::<Vec<_>>(),
        [
            (true, LayerName::NeverSave, Some(1)),
            (true, LayerName::SaveButNoMcp, Some(0))
        ]
    );
    let after = plan.candidate.unwrap();
    assert_eq!(after.layers.never_save.rules, [other]);
    assert_eq!(
        after.layers.save_but_no_mcp.rules,
        [pattern("elsewhere.test")]
    );
    assert_eq!(
        decide(&after, SAME_PAGE),
        Decision::NeverSave,
        "another URL keeps its rule"
    );
}

#[test]
fn publishing_an_allowed_url_changes_nothing() {
    let policy = policy_with(&[exact(SAME_PAGE)], &[]);
    let plan = run(&policy, URL, Action::Publish, Scope::Url);
    assert_eq!(
        (plan.current, plan.expected, plan.refusal),
        (Decision::None, Decision::None, None)
    );
    assert!(plan.changes.is_empty() && plan.candidate.is_none());
}

#[test]
fn a_broader_rule_blocks_publishing_and_nothing_is_removed() {
    for (never, private, blockers) in [
        // Only a host rule.
        (
            vec![pattern("news.test")],
            vec![],
            vec![(LayerName::NeverSave, 0, "pattern")],
        ),
        // The URL rule and a wildcard: removing one would not publish it.
        (
            vec![exact(URL), pattern("*.news.test")],
            vec![],
            vec![(LayerName::NeverSave, 1, "pattern")],
        ),
        // Never Save by its URL rule, Save but no MCP by a regex: never relaxed to the other layer.
        (
            vec![exact(URL)],
            vec![regex("story")],
            vec![(LayerName::SaveButNoMcp, 0, "regex")],
        ),
        // A path rule and a domain wildcard, both matching.
        (
            vec![pattern("news.test/story")],
            vec![RuleSpec::Typed(TypedRule::DomainWildcard {
                pattern: "*.news.test".into(),
            })],
            vec![
                (LayerName::NeverSave, 0, "pattern"),
                (LayerName::SaveButNoMcp, 0, "domainWildcard"),
            ],
        ),
    ] {
        let policy = policy_with(&never, &private);
        let current = decide(&policy, URL);
        let plan = run(&policy, URL, Action::Publish, Scope::Url);
        assert_eq!(plan.refusal, Some(PUBLISH_BLOCKED), "{never:?} {private:?}");
        assert!(plan.candidate.is_none());
        assert_eq!((plan.current, plan.expected), (current, current));
        assert_eq!(
            plan.blockers,
            blockers
                .into_iter()
                .map(|(layer, index, kind)| Blocker {
                    layer,
                    rule: Some((index, kind))
                })
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn a_builtin_category_blocks_publishing() {
    let url = "https://accounts.google.com/signin?x=1";
    let policy = policy_with(&[exact(url)], &[]);
    let plan = run(&policy, url, Action::Publish, Scope::Url);
    assert_eq!(plan.refusal, Some(PUBLISH_BLOCKED));
    assert_eq!(
        plan.blockers,
        [Blocker {
            layer: LayerName::NeverSave,
            rule: None
        }]
    );
    // A credential in the URL is a builtin too.
    let url = "https://news.test/a?token=x";
    let plan = run(&policy_with(&[], &[]), url, Action::Publish, Scope::Url);
    assert_eq!(
        plan.blockers,
        [Blocker {
            layer: LayerName::NeverSave,
            rule: None
        }]
    );
    // A disabled layer blocks nothing: its rule is no blocker.
    let mut policy = policy_with(&[pattern("news.test")], &[regex("news")]);
    policy.layers.never_save.enabled = false;
    let plan = run(&policy, URL, Action::Publish, Scope::Url);
    assert_eq!(
        plan.blockers,
        [Blocker {
            layer: LayerName::SaveButNoMcp,
            rule: Some((0, "regex"))
        }]
    );
}

#[test]
fn a_version_1_policy_needs_its_migration_first() {
    let mut policy: Policy = serde_json::from_value(json!({
        "schemaVersion": 1, "engineVersion": 1, "revision": 3,
        "categories": { "authentication": true, "credentials": true, "government": true, "webmail": true, "aiChats": true },
        "layers": { "neverSave": { "enabled": true, "rules": ["news.test/story/*"] }, "saveButNoMcp": { "enabled": true, "rules": [] } }
    }))
    .unwrap();
    for action in [Action::NeverSave, Action::SaveButNoMcp] {
        assert_eq!(
            run(&policy, URL, action, Scope::Url).refusal,
            Some(MIGRATION_REQUIRED)
        );
    }
    assert_eq!(
        run(
            &policy,
            "https://news.test/story/a",
            Action::Publish,
            Scope::Url
        )
        .refusal,
        Some(MIGRATION_REQUIRED)
    );
    policy.layers.never_save.rules.clear();
    assert_eq!(
        run(&policy, URL, Action::Publish, Scope::Url).refusal,
        None,
        "allowed: a no-op"
    );
}

#[test]
fn exact_rules_use_no_regex_budget_but_the_rule_limit_holds() {
    // 128 regexes, the limit: an exact rule is still possible (it is no regex), a host rule too.
    let regexes: Vec<RuleSpec> = (0..policy::MAX_REGEX_RULES)
        .map(|n| regex(&format!("^x{n}$")))
        .collect();
    for scope in [Scope::Url, Scope::Host] {
        let plan = run(&policy_with(&[], &regexes), URL, Action::NeverSave, scope);
        assert_eq!(plan.refusal, None, "{scope:?}");
    }
    // 1000 rules in a layer, the limit of a layer: refused, whatever the kind.
    let full: Vec<RuleSpec> = (0..1000)
        .map(|n| exact(&format!("https://news.test/{n}")))
        .collect();
    let plan = run(&policy_with(&full, &[]), URL, Action::NeverSave, Scope::Url);
    assert_eq!(
        (plan.refusal, plan.candidate.is_none()),
        (Some(POLICY_LIMIT), true)
    );
    // A long URL, beyond what a regex rule could hold: fine as an exact rule.
    let long = format!("https://news.test/{}", "a".repeat(10_000));
    let plan = run(&Policy::default(), &long, Action::NeverSave, Scope::Url);
    assert_eq!(decide(&plan.candidate.unwrap(), &long), Decision::NeverSave);
}

#[test]
fn an_exact_rule_written_otherwise_is_the_same_rule() {
    let written = RuleSpec::Typed(TypedRule::Exact {
        pattern: "HTTPS://NEWS.TEST.:443/story?id=1#top".into(),
    });
    let policy = policy_with(
        std::slice::from_ref(&written),
        std::slice::from_ref(&written),
    );
    assert_eq!(decide(&policy, URL), Decision::NeverSave);
    let again = run(&policy, URL, Action::NeverSave, Scope::Url);
    assert_eq!(
        (again.refusal, again.changes.len()),
        (None, 0),
        "idempotent"
    );
    let plan = run(&policy, URL, Action::Publish, Scope::Url);
    assert_eq!(plan.changes.len(), 2, "both spellings removed");
    assert_eq!(decide(&plan.candidate.unwrap(), URL), Decision::None);
    // A Slice 1 regex for the URL is a regex like any other: it blocks, and is not removed.
    let old = regex(r"^https://news\.test/story\?id=1\#top$");
    let plan = run(&policy_with(&[old], &[]), URL, Action::Publish, Scope::Url);
    assert_eq!(plan.refusal, Some(PUBLISH_BLOCKED));
    assert_eq!(
        plan.blockers,
        [Blocker {
            layer: LayerName::NeverSave,
            rule: Some((0, "regex"))
        }]
    );
}

#[test]
fn only_a_web_url_the_engine_can_judge_is_taken() {
    let policy = Policy::default();
    let compiled = CompiledPolicy::compile(&policy).unwrap();
    for url in [
        "",
        "news.test/a",
        "chrome://settings",
        "file:///C:/a",
        "blob:https://news.test/x",
        "https://news.test/%zz",
        "https://news.test/a b",
    ] {
        assert_eq!(
            plan(
                &policy,
                &compiled,
                &DomainApprovals::none(),
                url,
                Action::NeverSave,
                Scope::Url
            )
            .err(),
            Some(INVALID_URL),
            "{url}"
        );
    }
}

// --- Through the gate: the stored policy, the ETag and the store -------------------------------

struct Dir(std::path::PathBuf);
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn gate() -> (Dir, PolicyGate, PolicyStore) {
    let dir = Dir(std::env::temp_dir().join(format!("memocampus-items-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir_all(&dir.0).unwrap();
    PolicyStore::new(dir.0.clone())
        .unwrap()
        .initialize()
        .unwrap();
    let gate = PolicyGate::with_store(PolicyStore::new(dir.0.clone()).unwrap());
    let store = PolicyStore::new(dir.0.clone()).unwrap();
    (dir, gate, store)
}

fn preview(gate: &PolicyGate, action: &str, scope: Option<&str>) -> Value {
    gate.item_preview(&json!({ "type": "policy_item_preview", "requestId": "p", "url": URL, "action": action, "scope": scope }))
}

fn apply(gate: &PolicyGate, action: &str, scope: Option<&str>, etag: &Value) -> Value {
    gate.item_apply(&json!({ "type": "policy_item_apply", "requestId": "a", "url": URL, "action": action, "scope": scope, "expectedEtag": etag }))
}

#[test]
fn preview_then_apply_stores_the_change_and_the_engine_judges_it() {
    let (_dir, gate, store) = gate();
    let shown = preview(&gate, "never_save", None);
    assert_eq!(shown["ok"], true, "{shown}");
    assert_eq!(
        shown,
        json!({
            "ok": true, "type": "policy_item_preview_result", "requestId": "p",
            "etag": store.load().unwrap().etag, "revision": 1, "url": URL,
            "action": "never_save", "scope": "url", "current": "allow", "expected": "never_save",
            "changes": [{ "op": "add", "layer": "neverSave", "rule": { "kind": "exact", "pattern": URL } }],
            "affectsOtherUrls": false, "executable": true,
        })
    );
    assert_eq!(
        gate.decide(URL),
        Ok(Decision::None),
        "a preview changes nothing"
    );
    let done = apply(&gate, "never_save", None, &shown["etag"]);
    assert_eq!(done["ok"], true, "{done}");
    assert_eq!(
        (
            done["revision"].clone(),
            done["previous"].clone(),
            done["decision"].clone(),
            done["changed"].clone()
        ),
        (json!(2), json!("allow"), json!("never_save"), json!(true))
    );
    assert_eq!(done["etag"], store.load().unwrap().etag);
    assert_eq!(gate.decide(URL), Ok(Decision::NeverSave));
    assert_eq!(gate.decide(SAME_PAGE), Ok(Decision::None));
    // The same again: idempotent, nothing written.
    let again = apply(&gate, "never_save", None, &done["etag"]);
    assert_eq!(
        (again["changed"].clone(), again["revision"].clone()),
        (json!(false), json!(2))
    );
    // And published back.
    let shown = preview(&gate, "publish", None);
    assert_eq!(
        (shown["expected"].clone(), shown["executable"].clone()),
        (json!("allow"), json!(true))
    );
    let done = apply(&gate, "publish", None, &shown["etag"]);
    assert_eq!(
        (done["decision"].clone(), done["revision"].clone()),
        (json!("allow"), json!(3))
    );
    assert_eq!(store.load().unwrap().policy.layers.never_save.rules, []);
}

#[test]
fn a_preview_of_an_older_policy_is_not_applied() {
    let (_dir, gate, store) = gate();
    let shown = preview(&gate, "publish", None);
    // Meanwhile (the settings page, another window) the policy changes.
    let current = store.load().unwrap();
    let mut changed = current.policy.clone();
    changed.layers.never_save.rules.push(url_rule(URL).unwrap());
    store.update(&current, changed).unwrap();
    let refused = apply(&gate, "publish", None, &shown["etag"]);
    assert_eq!(
        (refused["ok"].clone(), refused["error"].clone()),
        (json!(false), json!("policy_conflict"))
    );
    assert_eq!(gate.decide(URL), Ok(Decision::NeverSave), "nothing written");
    let refused = apply(&gate, "never_save", Some("host"), &json!(""));
    assert_eq!(refused["error"], "invalid_etag");
}

#[test]
fn a_refused_plan_writes_nothing_and_says_why() {
    let (_dir, gate, store) = gate();
    let current = store.load().unwrap();
    let mut policy = current.policy.clone();
    policy.layers.never_save.rules = vec![url_rule(URL).unwrap(), pattern("news.test")];
    let stored = store.update(&current, policy).unwrap();
    let shown = preview(&gate, "publish", None);
    assert_eq!(
        (
            shown["executable"].clone(),
            shown["reason"].clone(),
            shown["blockers"].clone(),
            shown["current"].clone(),
            shown["expected"].clone()
        ),
        (
            json!(false),
            json!("publish_blocked"),
            json!([{ "layer": "neverSave", "ruleIndex": 1, "kind": "pattern" }]),
            json!("never_save"),
            json!("never_save")
        )
    );
    assert!(
        !shown.to_string().contains("\"news.test\""),
        "a blocking rule's text is not sent"
    );
    let refused = apply(&gate, "publish", None, &shown["etag"]);
    assert_eq!(refused["error"], "publish_blocked");
    assert_eq!(store.load().unwrap().etag, stored.etag);
    assert_eq!(
        preview(&gate, "never_save", Some("host"))["affectsOtherUrls"],
        true
    );
}

#[test]
fn bad_requests_are_refused_before_the_policy_is_read() {
    let (_dir, gate, _store) = gate();
    for (message, code) in [
        (
            json!({ "type": "policy_item_preview", "url": URL, "action": "publish" }),
            "invalid_request_id",
        ),
        (
            json!({ "type": "policy_item_preview", "requestId": "r", "action": "publish" }),
            "invalid_url",
        ),
        (
            json!({ "type": "policy_item_preview", "requestId": "r", "url": "chrome://x", "action": "publish" }),
            "invalid_url",
        ),
        (
            json!({ "type": "policy_item_preview", "requestId": "r", "url": URL }),
            "invalid_action",
        ),
        (
            json!({ "type": "policy_item_preview", "requestId": "r", "url": URL, "action": "allow" }),
            "invalid_action",
        ),
        (
            json!({ "type": "policy_item_preview", "requestId": "r", "url": URL, "action": "never_save", "scope": "domain" }),
            "invalid_scope",
        ),
        (
            json!({ "type": "policy_item_preview", "requestId": "r", "url": URL, "action": "never_save", "scope": true }),
            "invalid_scope",
        ),
        (
            json!({ "type": "policy_item_preview", "requestId": "r", "url": URL, "action": "publish", "scope": "host" }),
            "invalid_scope",
        ),
        (
            json!({ "type": "policy_item_apply", "requestId": "r", "url": URL, "action": "publish" }),
            "invalid_etag",
        ),
    ] {
        let reply = if message["type"] == "policy_item_apply" {
            gate.item_apply(&message)
        } else {
            gate.item_preview(&message)
        };
        assert_eq!(
            (reply["ok"].clone(), reply["error"].clone()),
            (json!(false), json!(code)),
            "{message}"
        );
    }
}
