//! Synthetic URLs only. `legacy` is the evaluator as it was before compilation, kept to prove that
//! compiling changed no decision.

use super::*;

mod legacy {
    use super::*;

    struct LegacyRule {
        host: String,
        wildcard: bool,
        path: Option<String>,
    }

    fn parse(rule: &str) -> Option<LegacyRule> {
        let parsed = Rule::parse(rule).ok()?;
        Some(LegacyRule {
            host: parsed.host,
            wildcard: parsed.wildcard,
            path: parsed.path,
        })
    }

    fn matches_any(rules: &[String], url: &NormalizedUrl) -> bool {
        rules.iter().any(|rule| {
            parse(rule).is_some_and(|r| {
                (url.host == r.host || (r.wildcard && url.host.ends_with(&format!(".{}", r.host))))
                    && r.path.as_ref().is_none_or(|p| url.path.starts_with(p))
            })
        })
    }

    /// As `has_credentials` was: the name only lower-cased.
    fn old_credentials(url: &NormalizedUrl) -> bool {
        let fragment = url
            .fragment
            .split_once('?')
            .map_or(url.fragment.as_str(), |(_, q)| q);
        let names: Vec<String> = [&url.query, &url.fragment, fragment]
            .iter()
            .flat_map(|text| {
                url::form_urlencoded::parse(text.as_bytes()).map(|(k, _)| k.to_ascii_lowercase())
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

    /// The former evaluator knew string rules only.
    fn texts(rules: &[RuleSpec]) -> Vec<String> {
        rules
            .iter()
            .map(|rule| match rule {
                RuleSpec::Text(text) => text.clone(),
                RuleSpec::Typed(_) => panic!("version 1 only"),
            })
            .collect()
    }

    pub fn evaluate(policy: &Policy, input: &str) -> Decision {
        if policy.validate().is_err() {
            return Decision::NeverSave;
        }
        let Ok(url) = normalize_url(input) else {
            return Decision::NeverSave;
        };
        let a = &policy.layers.never_save;
        let b = &policy.layers.save_but_no_mcp;
        let c = &policy.categories;
        if a.enabled
            && (matches_any(&texts(&a.rules), &url)
                || (c.authentication
                    && (matches_any(&BUILTINS.authentication, &url)
                        || BUILTINS
                            .auth_paths
                            .iter()
                            .any(|p| path_matches(&url.path, p))))
                || (c.credentials && old_credentials(&url)))
        {
            return Decision::NeverSave;
        }
        if b.enabled
            && (matches_any(&texts(&b.rules), &url)
                || (c.government && matches_any(&BUILTINS.government, &url))
                || (c.webmail && matches_any(&BUILTINS.webmail, &url))
                || (c.ai_chats && matches_any(&BUILTINS.ai_chats, &url)))
        {
            return Decision::SaveButNoMcp;
        }
        Decision::None
    }
}

/// Version 1, as the former evaluator knew it.
fn policy(never: &[&str], private: &[&str]) -> Policy {
    let text = |rules: &[&str]| -> Vec<RuleSpec> {
        rules.iter().map(|&r| RuleSpec::Text(r.into())).collect()
    };
    let mut policy = Policy {
        schema_version: 1,
        engine_version: 1,
        ..Policy::default()
    };
    policy.layers.never_save.rules = text(never);
    policy.layers.save_but_no_mcp.rules = text(private);
    policy
}

const RULES: [&str; 18] = [
    "example.com",
    "*.example.com",
    "example.com/*",
    "*.example.com/*",
    "example.com/private/*",
    "*.example.com/private/*",
    "evil-example.com",
    "EXAMPLE.COM.",
    "example.com/../*",
    "例え.テスト",
    "例え.テスト/秘密/*",
    "xn--r8jz45g.xn--zckzah",
    "127.0.0.1",
    "127.1",
    ".example.com",
    "*..example.com",
    "a..b",
    "*.0.0.1",
];

fn urls() -> Vec<String> {
    let schemes = ["https://", "http://", "blob:https://", "ftp://", "HTTP://"];
    let hosts = [
        "example.com",
        "EXAMPLE.com.",
        "www.example.com",
        "a.b.example.com",
        "evil-example.com",
        "example.com.evil.test",
        "notexample.com",
        ".example.com",
        "a..example.com",
        "a..b",
        "b",
        "例え.テスト",
        "x.例え.テスト",
        "xn--r8jz45g.xn--zckzah",
        "xn--a.example.com",
        "127.0.0.1",
        "127.1",
        "0.0.1",
        "[::1]",
        "example.com:8443",
        "user:pw@example.com",
        "accounts.google.com",
        "mail.google.com",
        "sub.myna.go.jp",
        "",
    ];
    let paths = [
        "",
        "/",
        "/private",
        "/private/",
        "/private/a",
        "/Private/a",
        "/private-x",
        "/%70rivate/a",
        "/private%2Fa",
        "/x/../private/a",
        "//private/a",
        "/秘密/a",
        "/login",
        "/login/x",
        "/login-guide",
        "/callback",
        "/%FF",
        "/a b",
    ];
    let tails = [
        "",
        "?q=rust",
        "?token=x",
        "?TOKEN=",
        "?%74oken=x",
        "?code=1",
        "?code=1&state=2",
        "#token=x",
        "#/r?id_token=x",
        "?q=%FF",
        "??token=x",
        "#??token=x",
        "?a?token=x",
    ];
    let mut all = Vec::new();
    for scheme in schemes {
        for host in hosts {
            for path in paths {
                for tail in tails {
                    all.push(format!("{scheme}{host}{path}{tail}"));
                }
            }
        }
    }
    all
}

/// Only names hidden behind a `?`, which the old evaluator read as other names.
fn is_intended_change(url: &str) -> bool {
    url.ends_with("??token=x") || url.ends_with("?a?token=x")
}

fn compare(policy: &Policy, urls: &[String]) {
    let compiled = CompiledPolicy::compile(policy).unwrap();
    for url in urls {
        let (old, new) = (legacy::evaluate(policy, url), compiled.evaluate(url));
        if old != new {
            assert!(
                is_intended_change(url)
                    && new == Decision::NeverSave
                    && policy.categories.credentials,
                "{url}: {old:?} -> {new:?}"
            );
        }
    }
    // The one-URL entry point is the same evaluator.
    for url in urls.iter().step_by(101) {
        assert_eq!(policy.evaluate(url), compiled.evaluate(url), "{url}");
    }
}

#[test]
fn compiling_changes_no_decision_for_each_rule_and_setting() {
    let urls = urls();
    let some: Vec<String> = urls.iter().step_by(37).cloned().collect();
    for rule in RULES {
        compare(&policy(&[rule], &[]), &some);
        compare(&policy(&[], &[rule]), &some);
    }
    let many: Vec<String> = urls.iter().step_by(3).cloned().collect();
    compare(&policy(&RULES, &[]), &many);
    compare(&policy(&[], &RULES), &many);
    compare(
        &policy(&["*.example.com"], &["example.com/private/*"]),
        &many,
    );
    let toggles: [fn(&mut Policy); 7] = [
        |p| p.categories.authentication = false,
        |p| p.categories.credentials = false,
        |p| p.categories.government = false,
        |p| p.categories.webmail = false,
        |p| p.categories.ai_chats = false,
        |p| p.layers.never_save.enabled = false,
        |p| p.layers.save_but_no_mcp.enabled = false,
    ];
    for toggle in toggles {
        let mut changed = policy(&RULES[..6], &RULES[6..]);
        toggle(&mut changed);
        compare(&changed, &some);
    }
}

#[test]
fn the_intended_change_only_refuses_more() {
    let open = Policy::default();
    let compiled = CompiledPolicy::compile(&open).unwrap();
    for url in [
        "https://example.com/??token=x",
        "https://example.com/#??token=x",
        "https://example.com/?a?token=x",
    ] {
        assert_eq!(legacy::evaluate(&open, url), Decision::None, "{url}");
        assert_eq!(compiled.evaluate(url), Decision::NeverSave, "{url}");
    }
}

#[test]
fn invalid_rules_refuse_the_whole_policy_even_in_a_disabled_layer() {
    for bad in [
        "*",
        "https://a.test/*",
        "a.test/x",
        "a.test:1",
        "xn--a.test",
        "",
    ] {
        let mut broken = policy(&["good.test"], &[bad]);
        assert!(CompiledPolicy::compile(&broken).is_err(), "{bad}");
        assert_eq!(broken.evaluate("https://public.test/"), Decision::NeverSave);
        broken.layers.save_but_no_mcp.enabled = false;
        assert!(CompiledPolicy::compile(&broken).is_err(), "{bad}");
        let broken = policy(&[bad], &["good.test"]);
        assert!(CompiledPolicy::compile(&broken).is_err(), "{bad}");
    }
    for unusable in [
        Policy {
            engine_version: 3,
            ..Policy::default()
        },
        Policy {
            schema_version: 3,
            ..Policy::default()
        },
        Policy {
            schema_version: 1,
            ..Policy::default()
        },
        Policy {
            revision: 0,
            ..Policy::default()
        },
    ] {
        assert!(CompiledPolicy::compile(&unusable).is_err());
    }
    let too_many = vec!["a.test"; 1001];
    assert!(CompiledPolicy::compile(&policy(&too_many, &[])).is_err());
    assert!(CompiledPolicy::compile(&policy(&[], &too_many)).is_err());
}

#[test]
fn blind_wins_over_private_and_invalid_urls_are_refused_with_both_layers_off() {
    let both =
        CompiledPolicy::compile(&policy(&["both.test"], &["both.test", "only.test"])).unwrap();
    assert_eq!(both.evaluate("https://both.test/"), Decision::NeverSave);
    assert_eq!(both.evaluate("https://only.test/"), Decision::SaveButNoMcp);
    assert_eq!(
        both.evaluate("https://only.test/?token=x"),
        Decision::NeverSave
    );
    assert_eq!(
        both.evaluate("https://only.test/login"),
        Decision::NeverSave
    );
    let mut off = Policy::default();
    off.layers.never_save.enabled = false;
    off.layers.save_but_no_mcp.enabled = false;
    let off = CompiledPolicy::compile(&off).unwrap();
    assert_eq!(off.evaluate("https://example.com/?token=x"), Decision::None);
    assert_eq!(off.evaluate("https://mail.google.com/"), Decision::None);
    assert_eq!(off.evaluate("not a url"), Decision::NeverSave);
}

#[test]
fn host_boundaries_hold_in_the_index() {
    let compiled = CompiledPolicy::compile(&policy(&["*.bank.test", "exact.test"], &[])).unwrap();
    for (url, expected) in [
        ("https://bank.test/", Decision::NeverSave),
        ("https://a.bank.test/", Decision::NeverSave),
        ("https://a.b.bank.test/", Decision::NeverSave),
        ("https://evilbank.test/", Decision::None),
        ("https://bank.test.evil.test/", Decision::None),
        ("https://exact.test/", Decision::NeverSave),
        ("https://a.exact.test/", Decision::None),
        ("https://test/", Decision::None),
    ] {
        assert_eq!(compiled.evaluate(url), expected, "{url}");
    }
}

#[test]
fn a_thousand_rules_compile_and_every_one_of_them_works() {
    let hosts: Vec<String> = (0..1000).map(|n| format!("*.host{n}.example")).collect();
    let paths: Vec<String> = (0..1000)
        .map(|n| format!("same.example/dir{n}/*"))
        .collect();
    let mut many = Policy::default();
    many.layers.never_save.rules = hosts.into_iter().map(Into::into).collect();
    many.layers.save_but_no_mcp.rules = paths.into_iter().map(Into::into).collect();
    let compiled = CompiledPolicy::compile(&many).unwrap();
    for n in [0, 1, 499, 998, 999] {
        assert_eq!(
            compiled.evaluate(&format!("https://a.host{n}.example/")),
            Decision::NeverSave,
            "{n}"
        );
        assert_eq!(
            compiled.evaluate(&format!("https://same.example/dir{n}/x")),
            Decision::SaveButNoMcp,
            "{n}"
        );
    }
    assert_eq!(
        compiled.evaluate("https://host1000.example/"),
        Decision::None
    );
    assert_eq!(
        compiled.evaluate("https://same.example/dir1000/x"),
        Decision::None
    );
}

#[test]
fn judging_url_after_url_parses_no_rule() {
    let source = include_str!("../policy.rs");
    let body = source
        .split("pub fn evaluate(&self, input: &str) -> Decision {\n        let Ok(url)")
        .nth(1)
        .or_else(|| {
            source
                .split("pub fn evaluate(&self, input: &str) -> Decision {\r\n        let Ok(url)")
                .nth(1)
        })
        .unwrap();
    let body = body.split("fn decode").next().unwrap();
    for banned in ["Rule::parse", "validate", "compile"] {
        assert!(!body.contains(banned), "{banned}");
    }
}

#[test]
fn a_new_policy_is_version_2_and_written_as_bundled() {
    let stored = serde_json::to_value(Policy::default()).unwrap();
    assert_eq!(stored["schemaVersion"], 2);
    assert_eq!(stored["engineVersion"], 2);
    assert_eq!(
        stored,
        serde_json::from_str::<serde_json::Value>(DEFAULT_JSON).unwrap()
    );
}

// --- Version 2: typed rules ------------------------------------------------------------------

fn v2(never: &[&str], private: &[&str]) -> Policy {
    policy(never, private).migrated().unwrap()
}

fn parse_json(value: serde_json::Value) -> Result<Policy, Error> {
    Policy::parse(&serde_json::to_vec(&value).unwrap())
}

fn with_rules(version: u32, never: serde_json::Value) -> serde_json::Value {
    let mut value = serde_json::to_value(Policy::default()).unwrap();
    value["schemaVersion"] = version.into();
    value["engineVersion"] = version.into();
    value["layers"]["neverSave"]["rules"] = never;
    value
}

#[test]
fn a_typed_rule_reads_and_writes_as_kind_and_pattern() {
    let value = with_rules(
        2,
        serde_json::json!([{ "kind": "pattern", "pattern": "*.example.com" }]),
    );
    let parsed = parse_json(value.clone()).unwrap();
    assert_eq!(
        parsed.layers.never_save.rules,
        [RuleSpec::Typed(TypedRule::Pattern {
            pattern: "*.example.com".into()
        })]
    );
    assert_eq!(serde_json::to_value(&parsed).unwrap(), value);
    let v1 = parse_json(with_rules(1, serde_json::json!(["*.example.com"]))).unwrap();
    assert_eq!(
        v1.layers.never_save.rules,
        [RuleSpec::Text("*.example.com".into())]
    );
}

#[test]
fn regex_unknown_kinds_malformed_rules_and_other_versions_are_refused() {
    let regex = with_rules(
        2,
        serde_json::json!([{ "kind": "regex", "pattern": "^https://a\\.test/" }]),
    );
    assert_eq!(parse_json(regex).unwrap_err(), Error::UnsupportedRule);
    let mut regex_private = with_rules(2, serde_json::json!([]));
    regex_private["layers"]["saveButNoMcp"]["rules"] =
        serde_json::json!([{ "kind": "regex", "pattern": "x" }]);
    assert_eq!(
        parse_json(regex_private).unwrap_err(),
        Error::UnsupportedRule
    );
    let mut regex_off = with_rules(2, serde_json::json!([{ "kind": "regex", "pattern": "x" }]));
    regex_off["layers"]["neverSave"]["enabled"] = false.into();
    assert_eq!(
        parse_json(regex_off).unwrap_err(),
        Error::UnsupportedRule,
        "even in a disabled layer"
    );
    for rules in [
        serde_json::json!([{ "kind": "glob", "pattern": "a.test" }]),
        serde_json::json!([{ "kind": "Pattern", "pattern": "a.test" }]),
        serde_json::json!([{ "kind": "pattern" }]),
        serde_json::json!([{ "pattern": "a.test" }]),
        serde_json::json!([{ "kind": "pattern", "pattern": "a.test", "isRegex": false }]),
        serde_json::json!([{ "kind": "pattern", "pattern": 1 }]),
        serde_json::json!([{ "kind": "pattern", "pattern": "https://a.test/*" }]),
        serde_json::json!([{ "kind": "pattern", "pattern": "a.test/x" }]),
        serde_json::json!([1]),
        serde_json::json!([null]),
        serde_json::json!(["a.test"]),
        serde_json::json!([{ "kind": "pattern", "pattern": "a.test" }, "b.test"]),
    ] {
        assert_eq!(
            parse_json(with_rules(2, rules.clone())).unwrap_err(),
            Error::InvalidPolicy,
            "{rules}"
        );
    }
    assert_eq!(
        parse_json(with_rules(
            1,
            serde_json::json!([{ "kind": "pattern", "pattern": "a.test" }])
        ))
        .unwrap_err(),
        Error::InvalidPolicy,
        "version 1 holds strings only"
    );
    for (schema, engine) in [(2, 1), (1, 2), (3, 3), (0, 0), (3, 2)] {
        let mut value = with_rules(2, serde_json::json!([]));
        value["schemaVersion"] = schema.into();
        value["engineVersion"] = engine.into();
        assert_eq!(
            parse_json(value).unwrap_err(),
            Error::InvalidPolicy,
            "{schema}/{engine}"
        );
    }
}

const SAMPLE: [&str; 7] = [
    "https://example.com/",
    "https://www.example.com/",
    "https://a.b.example.com/",
    "https://example.com/private",
    "https://example.com/private/",
    "https://example.com/private/a",
    "https://example.com/private/a/b",
];

#[test]
fn a_migrated_policy_decides_exactly_as_the_original() {
    let urls = urls();
    let some: Vec<String> = urls
        .iter()
        .step_by(11)
        .cloned()
        .chain(SAMPLE.map(str::to_owned))
        .collect();
    let mut cases = vec![
        policy(&RULES, &[]),
        policy(&[], &RULES),
        policy(&["*.example.com"], &["example.com/private/*"]),
        policy(&["example.com"], &["*.example.com"]),
        policy(&["example.com/private/*"], &[]),
    ];
    for rule in RULES {
        cases.push(policy(&[rule], &[rule]));
    }
    let mut switched = policy(&RULES[..6], &RULES[6..]);
    switched.categories.credentials = false;
    switched.layers.save_but_no_mcp.enabled = false;
    cases.push(switched);
    for original in cases {
        let migrated = original.migrated().unwrap();
        let (old, new) = (
            CompiledPolicy::compile(&original).unwrap(),
            CompiledPolicy::compile(&migrated).unwrap(),
        );
        for url in &some {
            assert_eq!(old.evaluate(url), new.evaluate(url), "{url}");
        }
    }
    // The cases found between the two runtimes.
    let open = policy(&[], &[]).migrated().unwrap();
    assert_eq!(
        serde_json::to_value(&open).unwrap(),
        serde_json::to_value(Policy::default()).unwrap(),
        "a new install starts where a migrated empty policy arrives"
    );
    for url in [
        "https://example.com/??token=x",
        "https://example.com/???token=x",
        "https://example.com/?a?token=x",
        "https://example.com/?%3Ftoken=x",
        "https://xn--a.example.com/",
    ] {
        assert_eq!(open.evaluate(url), Decision::NeverSave, "{url}");
    }
}

#[test]
fn the_stated_meaning_of_patterns_holds_in_both_versions() {
    for (rule, expected) in [
        ("example.com", [true, false, false, true, true, true, true]),
        ("*.example.com", [true, true, true, true, true, true, true]),
        (
            "example.com/private/*",
            [false, false, false, false, true, true, true],
        ),
    ] {
        for policy in [policy(&[rule], &[]), v2(&[rule], &[])] {
            let compiled = CompiledPolicy::compile(&policy).unwrap();
            let got = SAMPLE.map(|url| compiled.evaluate(url) == Decision::NeverSave);
            assert_eq!(got, expected, "{rule} v{}", policy.schema_version);
        }
    }
}

#[test]
fn migration_converts_every_rule_in_order_and_changes_nothing_else() {
    let mut original = policy(&["c.test", "a.test", "*.b.test/x/*"], &["z.test", "y.test"]);
    original.revision = 7;
    original.categories.webmail = false;
    original.layers.save_but_no_mcp.enabled = false;
    let migrated = original.migrated().unwrap();
    assert_eq!((migrated.schema_version, migrated.engine_version), (2, 2));
    assert_eq!(migrated.revision, 7, "the store sets the next revision");
    assert_eq!(migrated.categories, original.categories);
    let patterns = |layer: &Layer| -> Vec<String> {
        layer
            .rules
            .iter()
            .map(|rule| match rule {
                RuleSpec::Typed(TypedRule::Pattern { pattern }) => pattern.clone(),
                other => panic!("{other:?}"),
            })
            .collect()
    };
    assert_eq!(
        patterns(&migrated.layers.never_save),
        ["c.test", "a.test", "*.b.test/x/*"]
    );
    assert_eq!(
        patterns(&migrated.layers.save_but_no_mcp),
        ["z.test", "y.test"]
    );
    assert!(migrated.layers.never_save.enabled);
    assert!(!migrated.layers.save_but_no_mcp.enabled);
}

#[test]
fn only_a_valid_version_1_policy_can_be_migrated() {
    assert_eq!(
        v2(&["a.test"], &[]).migrated().unwrap_err(),
        Error::NotMigratable
    );
    assert_eq!(
        policy(&["*"], &[]).migrated().unwrap_err(),
        Error::InvalidPolicy
    );
    let future = Policy {
        engine_version: 3,
        ..Policy::default()
    };
    assert_eq!(future.migrated().unwrap_err(), Error::NotMigratable);
}
