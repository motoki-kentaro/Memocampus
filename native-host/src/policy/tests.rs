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
            path: parsed.path.map(|path| match path {
                PathRule::Prefix(prefix) => prefix,
                other => panic!("version 1 has prefixes only: {other:?}"),
            }),
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
        "///private/a",
        "/private//a",
        "//login",
        "/x//callback",
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

/// How much a decision withholds: more is stricter.
fn strictness(decision: Decision) -> u8 {
    match decision {
        Decision::None => 0,
        Decision::SaveButNoMcp => 1,
        Decision::NeverSave => 2,
    }
}

/// Since Slice 4 a path rule also meets the path with its runs of `/` collapsed.
fn has_a_run_of_slashes(url: &str) -> bool {
    normalize_url(url).is_ok_and(|url| url.path.contains("//"))
}

fn compare(policy: &Policy, urls: &[String]) {
    let compiled = CompiledPolicy::compile(policy).unwrap();
    for url in urls {
        let (old, new) = (legacy::evaluate(policy, url), compiled.evaluate(url));
        if old != new {
            assert!(
                (is_intended_change(url)
                    && new == Decision::NeverSave
                    && policy.categories.credentials)
                    || (has_a_run_of_slashes(url) && strictness(new) > strictness(old)),
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
        "a.test/*.png",
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
        .split("pub fn evaluate(&self, input: &str) -> Decision {\n        let Ok((url, parsed))")
        .nth(1)
        .or_else(|| {
            source
                .split(
                    "pub fn evaluate(&self, input: &str) -> Decision {\r\n        let Ok((url, parsed))",
                )
                .nth(1)
        })
        .unwrap();
    let body = body.split("fn decode").next().unwrap();
    for banned in ["Rule::parse", "validate", "compile", "Regex"] {
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
    assert!(
        parse_json(regex).is_ok(),
        "a regex is evaluated since Slice 5"
    );
    let mut regex_private = with_rules(2, serde_json::json!([]));
    regex_private["layers"]["saveButNoMcp"]["rules"] =
        serde_json::json!([{ "kind": "regex", "pattern": "(?<=a)x" }]);
    assert_eq!(
        parse_json(regex_private).unwrap_err(),
        Error::UnsupportedRule
    );
    let mut regex_off = with_rules(
        2,
        serde_json::json!([{ "kind": "regex", "pattern": "(?=x)" }]),
    );
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
        serde_json::json!([{ "kind": "pattern", "pattern": "a.test/*/x" }]),
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

// --- Slice 4: exact paths, a wildcard in the last segment, collapsed slashes ------------------

/// Version 2 with patterns version 1 could not hold.
fn v2_typed(never: &[&str], private: &[&str]) -> Policy {
    let mut policy = Policy::default();
    policy.layers.never_save.rules = never.iter().map(|&r| r.into()).collect();
    policy.layers.save_but_no_mcp.rules = private.iter().map(|&r| r.into()).collect();
    policy.validate().unwrap();
    policy
}

/// Whether each URL is refused by `rule` as a version 2 pattern of the Blind Zone.
fn refused(rule: &str, cases: &[(&str, bool)]) {
    let compiled = CompiledPolicy::compile(&v2_typed(&[rule], &[])).unwrap();
    for &(url, expected) in cases {
        assert_eq!(
            compiled.evaluate(url) == Decision::NeverSave,
            expected,
            "{rule} {url}"
        );
    }
}

#[test]
fn an_exact_path_means_that_path_only() {
    refused(
        "example.com/private",
        &[
            ("https://example.com/private", true),
            ("https://example.com/private?q=1", true),
            ("https://example.com/private/", false),
            ("https://example.com/private/a", false),
            ("https://example.com/private2", false),
            ("https://example.com/Private", false),
            ("https://www.example.com/private", false),
            ("https://example.com/", false),
        ],
    );
    refused(
        "*.example.com/private",
        &[
            ("https://example.com/private", true),
            ("https://a.b.example.com/private", true),
            ("https://a.b.example.com/private/a", false),
        ],
    );
    refused(
        "example.com/",
        &[
            ("https://example.com", true),
            ("https://example.com/", true),
            ("https://example.com/a", false),
        ],
    );
    refused(
        "example.com/private/",
        &[
            ("https://example.com/private/", true),
            ("https://example.com/private", false),
            ("https://example.com/private/a", false),
        ],
    );
}

#[test]
fn a_wildcard_matches_within_the_last_segment_only() {
    refused(
        "example.com/*.png",
        &[
            ("https://example.com/a.png", true),
            ("https://example.com/long-name.png", true),
            ("https://example.com/.png", true),
            ("https://example.com/images/a.png", false),
            ("https://example.com/a.jpg", false),
            ("https://example.com/a.PNG", false),
            ("https://example.com/a.png/", false),
            ("https://example.com/a.png/b", false),
        ],
    );
    refused(
        "example.com/images/*.png",
        &[
            ("https://example.com/images/a.png", true),
            ("https://example.com/images/long-name.png", true),
            ("https://example.com/images/sub/a.png", false),
            ("https://example.com/a.png", false),
            ("https://example.com/images-a.png", false),
        ],
    );
    refused(
        "example.com/file-*.jpg",
        &[
            ("https://example.com/file-.jpg", true),
            ("https://example.com/file-123.jpg", true),
            ("https://example.com/file-a/b.jpg", false),
            ("https://example.com/file-.jp", false),
            ("https://example.com/file.jpg", false),
            ("https://example.com/other.jpg", false),
        ],
    );
    refused(
        "example.com/images/photo-*",
        &[
            ("https://example.com/images/photo-", true),
            ("https://example.com/images/photo-1.png", true),
            ("https://example.com/images/photo-1/a", false),
            ("https://example.com/images/photo", false),
        ],
    );
    // The text around the wildcard does not overlap: `ab*ba` needs four characters.
    refused(
        "example.com/ab*ba",
        &[
            ("https://example.com/aba", false),
            ("https://example.com/abba", true),
        ],
    );
    // The old trailing `/*` keeps covering everything below the directory.
    refused(
        "example.com/private/*",
        &[
            ("https://example.com/private/a", true),
            ("https://example.com/private/a/b/c", true),
            ("https://example.com/private", false),
        ],
    );
}

#[test]
fn a_wildcard_elsewhere_and_the_new_forms_in_version_1_are_refused() {
    for bad in [
        "example.com/user/*/profile",
        "example.com/*/*.png",
        "example.com/images/**/photo.png",
        "example.com/**",
        "example.com/a*b*c.png",
        "example.com/*/",
        "example.com/a/./b",
        "example.com/a/../b",
        "example.com/a/*.png/..",
        "example.com/%2A.png",
        "example.com/*%2Fa.png",
        "example.com/a%",
        "example.com/a%2",
        "example.com/%FF",
        "example.com/a b",
        "example.com/a\\b",
        "example.com/a?b",
        "example.com/a#b",
        "*.example.com*/a",
        "ex*ample.com/a",
        "*example.com/a",
        "*.*.example.com/a",
        "example.com:443/a",
        "user@example.com/a",
        "https://example.com/a",
        "/a",
        "",
    ] {
        assert!(Rule::parse_in(bad, Syntax::Version2).is_err(), "{bad}");
        assert!(Rule::parse(bad).is_err(), "{bad}");
        let mut value = with_rules(
            2,
            serde_json::json!([{ "kind": "pattern", "pattern": bad }]),
        );
        assert_eq!(
            parse_json(value.clone()).unwrap_err(),
            Error::InvalidPolicy,
            "{bad}"
        );
        value["layers"]["neverSave"]["enabled"] = false.into();
        assert_eq!(
            parse_json(value).unwrap_err(),
            Error::InvalidPolicy,
            "{bad}"
        );
    }
    for new in [
        "example.com/private",
        "example.com/",
        "example.com/*.png",
        "example.com/images/*.png",
        "example.com/file-*.jpg",
        "example.com/images/photo-*",
    ] {
        assert!(Rule::parse_in(new, Syntax::Version2).is_ok(), "{new}");
        assert!(
            Rule::parse(new).is_err(),
            "version 1 keeps its grammar: {new}"
        );
        assert_eq!(
            parse_json(with_rules(1, serde_json::json!([new]))).unwrap_err(),
            Error::InvalidPolicy,
            "{new}"
        );
    }
    // A pattern valid in version 1 is valid in version 2 and means the same.
    for rule in RULES {
        let (v1, v2) = (Rule::parse(rule), Rule::parse_in(rule, Syntax::Version2));
        assert_eq!(v1.is_ok(), v2.is_ok(), "{rule}");
        if let (Ok(v1), Ok(v2)) = (v1, v2) {
            assert_eq!(
                (v1.host, v1.wildcard, v1.path),
                (v2.host, v2.wildcard, v2.path)
            );
        }
    }
}

#[test]
fn a_path_rule_meets_the_path_as_judged_and_with_its_slashes_collapsed() {
    refused(
        "example.com/private/*",
        &[
            ("https://example.com/private/secret", true),
            ("https://example.com//private/secret", true),
            ("https://example.com///private/secret", true),
            ("https://example.com/////////private/secret", true),
            ("https://example.com/private//secret", true),
            ("https://example.com//private//", true),
            ("https://example.com//private", false),
            ("https://example.com//Private/secret", false),
            ("https://example.com//public/secret", false),
        ],
    );
    // Only the judged path matches this rule: collapsing never takes a match away.
    refused(
        "example.com//private/*",
        &[
            ("https://example.com//private/secret", true),
            ("https://example.com/private/secret", false),
        ],
    );
    refused(
        "example.com/private",
        &[
            ("https://example.com//private", true),
            ("https://example.com///private", true),
            ("https://example.com//private/", false),
        ],
    );
    refused(
        "example.com/images/*.png",
        &[
            ("https://example.com/images//a.png", true),
            ("https://example.com//images/a.png", true),
            ("https://example.com/images//sub/a.png", false),
        ],
    );
    // What the judged path already decoded before Slice 4 stays decoded, and nothing more is:
    // `%2F` was a `/` already, and a doubly encoded one is still an invalid URL.
    refused(
        "example.com/private/*",
        &[
            ("https://example.com/private%2Fsecret", true),
            ("https://example.com/%2Fprivate/secret", true),
            ("https://example.com/%2F%2Fprivate/secret", true),
            ("https://example.com/%252Fprivate/secret", true),
        ],
    );
    assert_eq!(collapse_slashes("/private/a"), None);
    assert_eq!(
        collapse_slashes("//private/a").as_deref(),
        Some("/private/a")
    );
    assert_eq!(collapse_slashes("/a///b//").as_deref(), Some("/a/b/"));
    assert_eq!(collapse_slashes("//").as_deref(), Some("/"));
    assert_eq!(
        collapse_slashes("//A/%2F/./b").as_deref(),
        Some("/A/%2F/./b"),
        "nothing but slashes changes"
    );
    let open = CompiledPolicy::compile(&Policy::default()).unwrap();
    assert_eq!(
        open.evaluate("https://public.test//private/a"),
        Decision::None
    );
    assert_eq!(
        open.evaluate("https://public.test//login"),
        Decision::NeverSave
    );
    assert_eq!(
        open.evaluate("https://public.test//callback?code=1"),
        Decision::NeverSave
    );
}

#[test]
fn the_new_forms_work_in_both_zones_and_the_blind_zone_wins() {
    let compiled = CompiledPolicy::compile(&v2_typed(
        &["example.com/secret", "example.com/img/*.png"],
        &[
            "example.com/notes",
            "example.com/img/*",
            "example.com/file-*.jpg",
        ],
    ))
    .unwrap();
    for (url, expected) in [
        ("https://example.com/secret", Decision::NeverSave),
        ("https://example.com//secret", Decision::NeverSave),
        ("https://example.com/secret/", Decision::None),
        ("https://example.com/img/a.png", Decision::NeverSave),
        ("https://example.com/img//a.png", Decision::NeverSave),
        ("https://example.com/img/a.jpg", Decision::SaveButNoMcp),
        ("https://example.com/img/sub/a.png", Decision::SaveButNoMcp),
        ("https://example.com/notes", Decision::SaveButNoMcp),
        ("https://example.com//notes", Decision::SaveButNoMcp),
        ("https://example.com/notes/a", Decision::None),
        ("https://example.com/file-.jpg", Decision::SaveButNoMcp),
        (
            "https://example.com/file-1.jpg?token=x",
            Decision::NeverSave,
        ),
        ("https://example.com/other.jpg", Decision::None),
    ] {
        assert_eq!(compiled.evaluate(url), expected, "{url}");
    }
    let mut off = v2_typed(&["example.com/secret"], &["example.com/*.png"]);
    off.layers.never_save.enabled = false;
    off.layers.save_but_no_mcp.enabled = false;
    let off = CompiledPolicy::compile(&off).unwrap();
    assert_eq!(off.evaluate("https://example.com/secret"), Decision::None);
    assert_eq!(off.evaluate("https://example.com/a.png"), Decision::None);
}

#[test]
fn a_migration_keeps_each_rule_as_written() {
    let original = policy(&["example.com/a/*", "*.b.test"], &["c.test//x/*"]);
    let migrated = original.migrated().unwrap();
    assert_eq!(
        migrated.layers.never_save.rules,
        [
            RuleSpec::from("example.com/a/*"),
            RuleSpec::from("*.b.test")
        ]
    );
    assert_eq!(
        migrated.layers.save_but_no_mcp.rules,
        [RuleSpec::from("c.test//x/*")]
    );
}

// --- Slice 5: regex rules ---------------------------------------------------------------------

fn regex(pattern: &str) -> RuleSpec {
    RuleSpec::Typed(TypedRule::Regex {
        pattern: pattern.into(),
    })
}

fn with_regexes(never: &[&str], private: &[&str]) -> Policy {
    let mut policy = Policy::default();
    policy.layers.never_save.rules = never.iter().map(|p| regex(p)).collect();
    policy.layers.save_but_no_mcp.rules = private.iter().map(|p| regex(p)).collect();
    policy
}

/// Whether each URL is refused by `pattern` as a regex of the Blind Zone.
fn regex_refuses(pattern: &str, cases: &[(&str, bool)]) {
    let compiled = CompiledPolicy::compile(&with_regexes(&[pattern], &[])).unwrap();
    for &(url, expected) in cases {
        assert_eq!(
            compiled.evaluate(url) == Decision::NeverSave,
            expected,
            "{pattern} {url}"
        );
    }
}

#[test]
fn a_regex_meets_the_whole_url_as_the_parser_writes_it() {
    regex_refuses(
        r"^https://www\.google\.com/search(?:\?|$)",
        &[
            ("https://www.google.com/search?q=memocampus", true),
            ("https://www.google.com/search", true),
            ("HTTPS://WWW.GOOGLE.COM:443/search?q=test", true),
            ("https://www.google.com./search?q=test", true),
            ("blob:https://www.google.com/search", true),
            ("https://www.google.com/searchx?q=1", false),
            ("http://www.google.com/search?q=1", false),
            ("https://www.google.co.jp/search?q=1", false),
            ("https://www.google.com/Search?q=1", false),
        ],
    );
    // Found anywhere unless anchored.
    regex_refuses(
        r"example\.com/private",
        &[
            ("https://a.example.com/private/x", true),
            ("https://example.com/privates", true),
            ("https://example.com/public?next=example.com/private", true),
            ("https://example.com/public", false),
        ],
    );
    regex_refuses(
        r"^https://example\.com/private$",
        &[
            ("https://example.com/private", true),
            ("https://example.com/private/", false),
            ("https://example.com/private?q=1", false),
            ("https://example.com/private#top", false),
        ],
    );
    regex_refuses(
        "^http:",
        &[
            ("http://example.com/", true),
            ("https://example.com/", false),
        ],
    );
    regex_refuses(
        r"^https?://[^/]*\.example\.com/",
        &[
            ("https://a.example.com/", true),
            ("http://a.b.example.com/x", true),
            ("https://a.example.com.evil.test/", false),
            ("https://example.com/", false),
        ],
    );
    regex_refuses(
        r"[?&]q=secret(?:&|$)",
        &[
            ("https://x.test/?a=1&q=secret", true),
            ("https://x.test/?q=secret&b=2", true),
            ("https://x.test/?q=secrets", false),
        ],
    );
    regex_refuses(
        "#private$",
        &[
            ("https://x.test/page#private", true),
            ("https://x.test/page#public", false),
        ],
    );
    // Case-sensitive unless the regex says otherwise. The parser writes scheme and host in
    // lower case, so an upper-case host in a regex never matches.
    regex_refuses(
        "/Private/",
        &[
            ("https://x.test/Private/a", true),
            ("https://x.test/private/a", false),
        ],
    );
    regex_refuses(
        "(?i)/private/",
        &[
            ("https://x.test/PRIVATE/a", true),
            ("https://x.test/Private/a", true),
        ],
    );
    regex_refuses(
        r"^https://EXAMPLE\.com/",
        &[
            ("https://EXAMPLE.com/", false),
            ("https://example.com/", false),
        ],
    );
    regex_refuses(
        r"^https://(?:mail|chat)\.example\.com/[0-9]{4}/",
        &[
            ("https://mail.example.com/2026/x", true),
            ("https://chat.example.com/1999/", true),
            ("https://news.example.com/2026/x", false),
            ("https://mail.example.com/26/x", false),
        ],
    );
    // The parser writes a name in punycode and the rest percent-encoded, and keeps an encoding
    // as it came: a regex meets that text, not the decoded one a pattern meets.
    regex_refuses("秘密", &[("https://例え.テスト/秘密", false)]);
    regex_refuses(
        r"^https://xn--r8jz45g\.xn--zckzah/%E7%A7%98%E5%AF%86",
        &[("https://例え.テスト/秘密", true)],
    );
    regex_refuses(
        r"\p{Han}|a%2Fb",
        &[
            ("https://x.test/a%2Fb", true),
            ("https://x.test/a/b", false),
            ("https://x.test/%E7%A7%98", false),
        ],
    );
    regex_refuses(
        r"^https://127\.0\.0\.1:8443/",
        &[("https://127.1:8443/", true), ("https://127.0.0.1/", false)],
    );
    regex_refuses(
        r":443/",
        &[
            ("https://x.test:443/", false),
            ("https://x.test:4430/", false),
        ],
    );
}

#[test]
fn the_url_without_trailing_dots_is_made_only_when_it_differs() {
    let dotless = |url: &str| without_trailing_dots(&Url::parse(url).unwrap());
    assert_eq!(
        dotless("https://example.com./x?q#f").as_deref(),
        Some("https://example.com/x?q#f")
    );
    assert_eq!(
        dotless("https://example.com../").as_deref(),
        Some("https://example.com/")
    );
    assert_eq!(dotless("https://example.com/"), None);
    assert_eq!(dotless("https://[::1]/"), None);
    assert_eq!(dotless("https://127.0.0.1/"), None);
}

#[test]
fn regex_limits_and_errors_are_told_apart_and_refuse_the_policy_even_in_a_disabled_layer() {
    assert_eq!(
        (
            MAX_REGEX_RULES,
            MAX_REGEX_BYTES,
            REGEX_TOTAL_SIZE_LIMIT,
            REGEX_SIZE_LIMIT,
            REGEX_TOTAL_DFA_SIZE_LIMIT,
            REGEX_DFA_SIZE_LIMIT
        ),
        (128, 4096, 8 << 20, 1 << 20, 8 << 20, 256 << 10),
        "the documented limits"
    );
    let long = "x".repeat(MAX_REGEX_BYTES + 1);
    let long_wide = "秘".repeat(MAX_REGEX_BYTES / 3 + 1);
    let nested = format!("{}a{}", "(".repeat(300), ")".repeat(300));
    for (pattern, expected) in [
        (r"(a)\1", Error::UnsupportedRule),
        ("(?<=a)b", Error::UnsupportedRule),
        ("(?<!a)b", Error::UnsupportedRule),
        ("a(?=b)", Error::UnsupportedRule),
        ("a(?!b)", Error::UnsupportedRule),
        ("(a", Error::InvalidPolicy),
        ("a)", Error::InvalidPolicy),
        ("[a", Error::InvalidPolicy),
        ("a{2,1}", Error::InvalidPolicy),
        ("*a", Error::InvalidPolicy),
        (r"\p{NoSuchClass}", Error::InvalidPolicy),
        (r"\", Error::InvalidPolicy),
        ("a{100000}", Error::RegexLimit),
        (r"\w{100}", Error::RegexLimit),
        (r"\w{21}", Error::RegexLimit),
        (long.as_str(), Error::RegexLimit),
        (long_wide.as_str(), Error::RegexLimit),
        (nested.as_str(), Error::RegexLimit),
    ] {
        let name: String = pattern.chars().take(20).collect();
        for (never, private, enabled) in [
            (true, false, true),
            (false, true, true),
            (true, false, false),
            (false, true, false),
        ] {
            let one = [pattern];
            let mut policy = with_regexes(
                if never { &one } else { &[] },
                if private { &one } else { &[] },
            );
            policy.layers.never_save.enabled = enabled;
            policy.layers.save_but_no_mcp.enabled = enabled;
            assert_eq!(policy.validate().unwrap_err(), expected, "{name}");
            assert_eq!(
                CompiledPolicy::compile(&policy).unwrap_err(),
                expected,
                "{name}"
            );
            assert_eq!(policy.evaluate("https://public.test/"), Decision::NeverSave);
        }
    }
    // At the limits a regex is accepted.
    for pattern in [
        "x".repeat(MAX_REGEX_BYTES),
        "秘".repeat(MAX_REGEX_BYTES / 3),
        format!("{}a{}", "(".repeat(200), ")".repeat(200)),
        r"\w{20}".to_owned(),
    ] {
        assert!(
            with_regexes(&[&pattern], &[]).validate().is_ok(),
            "{}",
            pattern.chars().take(10).collect::<String>()
        );
    }
    // The number of regexes counts both layers, enabled or not.
    let half: Vec<String> = (0..MAX_REGEX_RULES / 2).map(|n| format!("r{n}x")).collect();
    let half: Vec<&str> = half.iter().map(String::as_str).collect();
    let mut full = with_regexes(&half, &half);
    full.layers.never_save.rules.push("many.test".into());
    assert!(CompiledPolicy::compile(&full).is_ok());
    full.layers.save_but_no_mcp.enabled = false;
    full.layers.save_but_no_mcp.rules.push(regex("one-more"));
    assert_eq!(full.validate().unwrap_err(), Error::RegexLimit);
    assert_eq!(
        CompiledPolicy::compile(&full).unwrap_err(),
        Error::RegexLimit
    );
    // A regex is no rule of version 1.
    let mut v1 = policy(&[], &[]);
    v1.layers.never_save.rules.push(regex("x"));
    assert_eq!(v1.validate().unwrap_err(), Error::InvalidPolicy);
}

#[test]
fn a_regex_cannot_be_made_to_backtrack() {
    let input = format!("https://example.com/{}!", "a".repeat(30_000));
    for pattern in ["^(a+)+$", "(a+)+$", "(a|aa)+$", "(a|a)*b", "(?:a*)*b"] {
        let compiled = CompiledPolicy::compile(&with_regexes(&[pattern], &[])).unwrap();
        assert_eq!(compiled.evaluate(&input), Decision::None, "{pattern}");
    }
    let compiled = CompiledPolicy::compile(&with_regexes(&["(a+)+!$"], &[])).unwrap();
    assert_eq!(compiled.evaluate(&input), Decision::NeverSave);
}

#[test]
fn regexes_and_patterns_keep_the_order_of_the_zones() {
    let blind_regex = |extra: RuleSpec| {
        let mut policy = with_regexes(&["secret"], &[]);
        policy.layers.save_but_no_mcp.rules = vec![extra, regex("^https://mixed\\.test/")];
        CompiledPolicy::compile(&policy).unwrap()
    };
    for compiled in [
        blind_regex("example.com".into()),
        blind_regex(regex(r"example\.com")),
    ] {
        for (url, expected) in [
            ("https://example.com/secret", Decision::NeverSave),
            ("https://example.com/x", Decision::SaveButNoMcp),
            ("https://mixed.test/secret", Decision::NeverSave),
            ("https://mixed.test/x", Decision::SaveButNoMcp),
            ("https://public.test/x", Decision::None),
            ("https://public.test/secret", Decision::NeverSave),
        ] {
            assert_eq!(compiled.evaluate(url), expected, "{url}");
        }
    }
    // A Blind pattern wins over a Private regex, whichever comes first in its layer.
    for rules in [
        vec!["blind.test".into(), regex("unused")],
        vec![regex("unused"), "blind.test".into()],
    ] {
        let mut policy = with_regexes(&[], &["blind"]);
        policy.layers.never_save.rules = rules;
        let compiled = CompiledPolicy::compile(&policy).unwrap();
        assert_eq!(
            compiled.evaluate("https://blind.test/"),
            Decision::NeverSave
        );
        assert_eq!(
            compiled.evaluate("https://x.test/blind"),
            Decision::SaveButNoMcp
        );
    }
    // A disabled layer's regex decides nothing.
    let mut off = with_regexes(&["secret"], &["private"]);
    off.layers.never_save.enabled = false;
    let compiled = CompiledPolicy::compile(&off).unwrap();
    assert_eq!(compiled.evaluate("https://x.test/secret"), Decision::None);
    assert_eq!(
        compiled.evaluate("https://x.test/secret/private"),
        Decision::SaveButNoMcp
    );
    off.layers.save_but_no_mcp.enabled = false;
    let compiled = CompiledPolicy::compile(&off).unwrap();
    assert_eq!(compiled.evaluate("https://x.test/private"), Decision::None);
    // An invalid URL is refused before any regex, even one that would match nothing.
    let compiled = CompiledPolicy::compile(&with_regexes(&[], &["^$"])).unwrap();
    assert_eq!(compiled.evaluate("not a url"), Decision::NeverSave);
    assert_eq!(compiled.evaluate("https://x.test/%FF"), Decision::NeverSave);
}

#[test]
fn adding_a_regex_never_refuses_less() {
    let urls = urls();
    let some: Vec<String> = urls.iter().step_by(13).cloned().collect();
    let mut bases = vec![
        v2(&RULES[..6], &RULES[6..]),
        v2(&["*.example.com"], &["example.com/private/*"]),
        Policy::default(),
    ];
    let mut off = v2(&RULES[..6], &RULES[6..]);
    off.categories.credentials = false;
    bases.push(off);
    for base in bases {
        let before = CompiledPolicy::compile(&base).unwrap();
        for (never, pattern) in [
            (true, "private"),
            (false, "private"),
            (true, "^https://"),
            (false, "(?i)EXAMPLE"),
            (false, "never-matches-anything"),
        ] {
            let mut added = base.clone();
            let layer = if never {
                &mut added.layers.never_save
            } else {
                &mut added.layers.save_but_no_mcp
            };
            layer.rules.push(regex(pattern));
            let after = CompiledPolicy::compile(&added).unwrap();
            for url in &some {
                let (old, new) = (before.evaluate(url), after.evaluate(url));
                assert!(
                    strictness(new) >= strictness(old),
                    "{pattern} {url}: {old:?} -> {new:?}"
                );
                if pattern == "never-matches-anything" {
                    assert_eq!(old, new, "{url}");
                }
            }
        }
    }
}

#[test]
fn the_regexes_of_a_policy_share_one_budget() {
    // One regex may use the limit of a rule; each of 128 an eighth of a megabyte at most.
    assert!(with_regexes(&[r"\w{20}"], &[]).validate().is_ok());
    let many = |extra: &str| {
        let fillers: Vec<String> = (0..MAX_REGEX_RULES - 1)
            .map(|n| format!("^https://site{n}\\.example/"))
            .collect();
        let mut never: Vec<&str> = fillers.iter().map(String::as_str).collect();
        never.push(extra);
        with_regexes(&never[..64], &never[64..])
    };
    for fits in [
        r"\w+",
        r"(?i)^https://mail\.example\.com/[0-9]+/inbox",
        r"\p{L}+",
    ] {
        assert!(many(fits).validate().is_ok(), "{fits}");
        assert!(CompiledPolicy::compile(&many(fits)).is_ok(), "{fits}");
    }
    for over in [r"\w\w", r"\w{20}"] {
        assert_eq!(
            many(over).validate().unwrap_err(),
            Error::RegexLimit,
            "{over}"
        );
        assert_eq!(
            CompiledPolicy::compile(&many(over)).unwrap_err(),
            Error::RegexLimit,
            "{over}"
        );
    }
    let budget = RegexBudget::of(&many("x"));
    assert_eq!((budget.size, budget.dfa), (64 << 10, 64 << 10));
    let budget = RegexBudget::of(&Policy::default());
    assert_eq!((budget.size, budget.dfa), (1 << 20, 256 << 10));
}

// --- Slice 6: problems of a draft, one rule tested alone ---------------------------------------

#[test]
fn problems_name_each_rule_that_fails_and_nothing_for_a_policy_that_compiles() {
    assert_eq!(Policy::default().problems(), []);
    let mut draft = v2_typed(&["ok.test", "ok2.test"], &["ok3.test"]);
    draft.layers.never_save.rules[1] = regex("(?<=a)b");
    draft.layers.save_but_no_mcp.rules.push("bad/*/x".into());
    draft.layers.save_but_no_mcp.rules.push(regex("(a"));
    draft.layers.save_but_no_mcp.rules.push(regex(r"\w{100}"));
    draft
        .layers
        .save_but_no_mcp
        .rules
        .push(RuleSpec::Text("v1.test".into()));
    draft.layers.save_but_no_mcp.enabled = false;
    let at = |layer, index, code| Problem {
        layer: Some(layer),
        rule_index: Some(index),
        code,
    };
    assert_eq!(
        draft.problems(),
        [
            at("neverSave", 1, "unsupported_regex"),
            at("saveButNoMcp", 1, "invalid_pattern"),
            at("saveButNoMcp", 2, "invalid_regex"),
            at("saveButNoMcp", 3, "regex_limit"),
            at("saveButNoMcp", 4, "invalid_rule_form"),
        ]
    );
    // What compiling says and what the problems say agree.
    assert!(draft.validate().is_err());
    let header = Policy {
        schema_version: 9,
        revision: 0,
        ..Policy::default()
    };
    assert_eq!(
        header.problems(),
        [
            Problem::of("invalid_version"),
            Problem::of("invalid_revision")
        ]
    );
    let mut too_many = Policy::default();
    too_many.layers.never_save.rules = vec!["a.test".into(); 1001];
    assert_eq!(
        too_many.problems(),
        [Problem {
            layer: Some("neverSave"),
            ..Problem::of("too_many_rules")
        }]
    );
    // The budget of each regex depends on how many a policy has, as when compiling.
    let mut crowded = with_regexes(&[], &[]);
    crowded.layers.never_save.rules = (0..127).map(|n| regex(&format!("r{n}"))).collect();
    crowded.layers.never_save.rules.push(regex(r"\w{20}"));
    assert_eq!(
        crowded.problems(),
        [Problem {
            layer: Some("neverSave"),
            rule_index: Some(127),
            code: "regex_limit"
        }]
    );
    assert!(with_regexes(&[r"\w{20}"], &[]).problems().is_empty());
}

#[test]
fn a_rule_that_matches_alone_refuses_in_any_policy_that_holds_it() {
    let urls = urls();
    let some: Vec<String> = urls.iter().step_by(17).cloned().collect();
    let rules: Vec<RuleSpec> = RULES
        .iter()
        .map(|&rule| rule.into())
        .chain(
            [
                "example.com/private",
                "example.com/*.png",
                "*.example.com/login",
            ]
            .map(RuleSpec::from),
        )
        .chain(["private", r"(?i)^https://[^/]*example\.com/", "token="].map(regex))
        .filter(|rule| rule_matches(rule, "https://example.com/").is_ok())
        .collect();
    assert!(rules.len() > 15);
    for rule in &rules {
        let mut holding = v2(&RULES[..3], &RULES[3..6]);
        holding.layers.never_save.rules.push(rule.clone());
        let compiled = CompiledPolicy::compile(&holding).unwrap();
        for url in &some {
            match rule_matches(rule, url) {
                Ok(true) => assert_eq!(
                    compiled.evaluate(url),
                    Decision::NeverSave,
                    "{rule:?} {url}"
                ),
                Ok(false) => {}
                Err(error) => {
                    assert_eq!(error, Error::InvalidUrl);
                    assert_eq!(compiled.evaluate(url), Decision::NeverSave, "{url}");
                }
            }
        }
    }
    // A string is a version 1 rule, which this test does not take.
    assert_eq!(
        rule_matches(&RuleSpec::Text("a.test".into()), "https://a.test/"),
        Err(Error::InvalidPolicy)
    );
}

// --- Domain wildcard (docs/issue-025-policy-engine.md, Domain Wildcard) ---

fn domain(pattern: &str) -> RuleSpec {
    RuleSpec::Typed(TypedRule::DomainWildcard {
        pattern: pattern.into(),
    })
}

fn with_domains(never: &[&str], private: &[&str]) -> Policy {
    let mut policy = Policy::default();
    policy.layers.never_save.rules = never.iter().map(|p| domain(p)).collect();
    policy.layers.save_but_no_mcp.rules = private.iter().map(|p| domain(p)).collect();
    policy
}

/// Whether the policy's own compiler takes the rule; the domain function must agree.
fn accepted(pattern: &str) -> bool {
    let compiled = CompiledPolicy::compile(&with_domains(&[pattern], &[])).is_ok();
    assert_eq!(
        compiled,
        crate::public_suffix::wildcard_domain(pattern).is_some(),
        "{pattern:?}"
    );
    compiled
}

#[test]
fn the_bundled_public_suffix_list_is_the_pinned_one_and_is_read() {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(crate::public_suffix::LIST_TEXT.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(
        hex, "d3e6243b51695327984abb3e2a5aa7077096867f75121e031e6a5a3840ca8bcd",
        "updating the list is a reviewed change: replace the file and this digest together"
    );
    assert!(crate::public_suffix::loaded());
}

#[test]
fn a_domain_wildcard_is_star_dot_and_a_registrable_domain() {
    for valid in [
        "*.amazon.co.jp",
        "*.example.com",
        "*.example.co.jp",
        "*.foo.github.io",
        "*.city.kawasaki.jp",
        "*.www.ck",
        "*.mybucket.s3.amazonaws.com",
        "*.Amazon.CO.jp",
        "*.xn--r8jz45g.jp",
        "*.例え.jp",
    ] {
        assert!(accepted(valid), "{valid}");
    }
    for invalid in [
        // The examples of the order.
        "*.amazon.co.jp/*",
        "https://*.amazon.co.jp",
        "*.amazon.co.jp:443",
        "*.amazon.co.jp?q=test",
        "*.amazon.co.jp#fragment",
        "www.*.amazon.co.jp",
        "*.*.example.com",
        "*amazon.co.jp",
        "www?.example.com",
        "?.example.com",
        "example.*",
        "*.example.*",
        "*.com",
        "*.co.jp",
        "*.github.io",
        "amazon.co.jp",
        "*",
        // Spaces are not trimmed, and nothing else is written for the person.
        " *.example.com",
        "*.example.com ",
        "*. example.com",
        "*.exa mple.com",
        "*.example.com\t",
        "*.example.com\u{3000}",
        "*.example.com.",
        "*..example.com",
        "*.example..com",
        "*.",
        "",
        "**.example.com",
        "*.*",
        ".example.com",
        "*example.com",
        // Below a registrable domain, or a domain with public suffixes below it.
        "*.www.example.com",
        "*.mail.amazon.co.jp",
        "*.kawasaki.jp",
        "*.foo.kawasaki.jp",
        "*.amazonaws.com",
        "*.s3.amazonaws.com",
        "*.blogspot.com",
        "*.foo.ck",
        "*.jp",
        // A suffix the list does not know.
        "*.example.test",
        "*.corp.internal",
        "*.localhost",
        // Not a DNS name.
        "*.1.2.3.4",
        "*.192.168.0.1",
        "*.[::1]",
        "*.example_x.com",
        "*.-example.com",
        "*.example-.com",
        "*.ex%61mple.com",
        "*.user@example.com",
        "*.example.com/",
        "*.example.com\\x",
    ] {
        assert!(!accepted(invalid), "{invalid:?}");
    }
    let label = "a".repeat(63);
    assert!(accepted(&format!("*.{label}.com")));
    assert!(!accepted(&format!("*.a{label}.com")), "a label of 64");
    assert!(!accepted(&format!("*.{}.com", "a".repeat(5000))));
}

#[test]
fn every_public_suffix_of_the_list_is_refused() {
    let mut checked = 0;
    for line in crate::public_suffix::LIST_TEXT.lines() {
        let rule = line.split_whitespace().next().unwrap_or("");
        if rule.is_empty() || rule.starts_with("//") || rule.starts_with('!') {
            continue;
        }
        let suffix = rule.trim_start_matches("*.");
        assert!(!accepted(&format!("*.{suffix}")), "{rule}");
        if rule.starts_with("*.") {
            assert!(!accepted(&format!("*.x.{suffix}")), "{rule}");
        }
        checked += 1;
    }
    assert!(checked > 9000, "{checked}");
}

#[test]
fn a_domain_wildcard_matches_the_domain_and_its_subdomains_on_label_boundaries() {
    let rule = domain("*.amazon.co.jp");
    for (url, expected) in [
        ("https://amazon.co.jp/", true),
        ("https://www.amazon.co.jp/", true),
        ("https://seller.amazon.co.jp/", true),
        ("https://a.b.amazon.co.jp/", true),
        ("https://fake-amazon.co.jp/", false),
        ("https://amazon.co.jp.evil.com/", false),
        ("https://notamazon.co.jp/", false),
        ("https://co.jp/", false),
        ("https://amazon.co/", false),
        // Case, trailing dots, ports, userinfo, path, query and fragment.
        ("https://WWW.Amazon.CO.JP/", true),
        ("https://www.amazon.co.jp./", true),
        ("https://www.amazon.co.jp../", true),
        ("https://www.amazon.co.jp:8443/", true),
        ("http://amazon.co.jp:80/", true),
        ("https://user:pw@www.amazon.co.jp/", true),
        ("https://amazon.co.jp@evil.com/", false),
        ("https://evil.com/amazon.co.jp", false),
        ("https://evil.com/?u=https://www.amazon.co.jp/", false),
        ("https://evil.com/#amazon.co.jp", false),
        ("https://www.amazon.co.jp/any/path?q=1#f", true),
        ("blob:https://www.amazon.co.jp/uuid", true),
        // Not a host of it.
        ("https://1.2.3.4/", false),
        ("https://[::1]/", false),
    ] {
        assert_eq!(rule_matches(&rule, url), Ok(expected), "{url}");
    }
    let deep = format!("https://{}amazon.co.jp/", "a.".repeat(60));
    assert_eq!(rule_matches(&rule, &deep), Ok(true));
    let unrelated = format!("https://{}.example.com/", "a".repeat(60));
    assert_eq!(rule_matches(&rule, &unrelated), Ok(false));
    for invalid in [
        "not a url",
        "amazon.co.jp",
        "ftp://amazon.co.jp/",
        "https://www.amazon.co.jp/%zz",
        "https://www.amazon.co.jp/%FF",
        "https://www.amazon.co.jp/?q=%",
        "https:// amazon.co.jp/",
    ] {
        assert_eq!(
            rule_matches(&rule, invalid),
            Err(Error::InvalidUrl),
            "{invalid}"
        );
    }
    let huge = format!("https://www.amazon.co.jp/{}", "x".repeat(40_000));
    assert_eq!(rule_matches(&rule, &huge), Err(Error::InvalidUrl));
    // A refused rule matches nothing: a restricted domain without an approval, or bad syntax.
    assert_eq!(
        rule_matches(&domain("*.co.jp"), "https://amazon.co.jp/"),
        Err(Error::UnapprovedDomain)
    );
    assert_eq!(
        rule_matches(&domain("*.co.jp/*"), "https://amazon.co.jp/"),
        Err(Error::InvalidPolicy)
    );
}

#[test]
fn unicode_and_punycode_name_the_same_domain_as_a_url_host_does() {
    let unicode = domain("*.例え.jp");
    let ascii = domain("*.xn--r8jz45g.jp");
    for url in [
        "https://例え.jp/",
        "https://www.例え.jp/",
        "https://xn--r8jz45g.jp/",
        "https://a.xn--r8jz45g.jp/x",
        "https://WWW.XN--R8JZ45G.JP/",
    ] {
        assert_eq!(rule_matches(&unicode, url), Ok(true), "{url}");
        assert_eq!(rule_matches(&ascii, url), Ok(true), "{url}");
    }
    assert_eq!(rule_matches(&unicode, "https://例え.com/"), Ok(false));
    // An IDN public suffix (個人.香港) is refused in either form.
    assert!(!accepted("*.個人.香港"));
    assert!(!accepted("*.xn--55qx5d.xn--j6w193g"));
    assert!(accepted("*.foo.個人.香港"));
    // Labels the URL parser refuses are refused as well.
    for invalid in ["*.xn--zz-.com", "*.xn--.com"] {
        assert!(!accepted(invalid), "{invalid:?}");
    }
}

#[test]
fn a_domain_wildcard_decides_as_the_pattern_of_the_same_domain() {
    let urls = urls();
    for name in ["example.com", "amazon.co.jp"] {
        let pattern = format!("*.{name}");
        for never in [true, false] {
            let place = |rule: RuleSpec| {
                let mut policy = Policy::default();
                if never {
                    policy.layers.never_save.rules = vec![rule];
                } else {
                    policy.layers.save_but_no_mcp.rules = vec![rule];
                }
                CompiledPolicy::compile(&policy).unwrap()
            };
            let by_domain = place(domain(&pattern));
            let by_pattern = place(RuleSpec::from(pattern.as_str()));
            for url in &urls {
                assert_eq!(by_domain.evaluate(url), by_pattern.evaluate(url), "{url}");
            }
        }
    }
}

#[test]
fn a_domain_wildcard_is_no_regex() {
    let rule = domain("*.example.com");
    for url in [
        "https://xexample.com/",
        "https://wwwxexample.com/",
        "https://example.com.evil.com/",
        "https://evil.com/.example.com",
    ] {
        assert_eq!(rule_matches(&rule, url), Ok(false), "{url}");
    }
    // Not counted as a regex: 128 regexes and domain wildcards compile together.
    let mut policy = with_regexes(&vec!["a"; MAX_REGEX_RULES], &[]);
    policy
        .layers
        .save_but_no_mcp
        .rules
        .extend(["*.example.com", "*.example.org", "*.amazon.co.jp"].map(domain));
    assert!(CompiledPolicy::compile(&policy).is_ok());
}

#[test]
fn a_domain_wildcard_is_kind_and_pattern_in_version_2_only() {
    let json = serde_json::json!([{ "kind": "domainWildcard", "pattern": "*.amazon.co.jp" }]);
    let policy = parse_json(with_rules(2, json.clone())).unwrap();
    assert_eq!(policy.layers.never_save.rules, [domain("*.amazon.co.jp")]);
    assert_eq!(
        serde_json::to_value(&policy).unwrap()["layers"]["neverSave"]["rules"],
        json
    );
    assert_eq!(
        parse_json(with_rules(1, json)).unwrap_err(),
        Error::InvalidPolicy,
        "version 1 has strings only"
    );
    for rules in [
        serde_json::json!([{ "kind": "DomainWildcard", "pattern": "*.amazon.co.jp" }]),
        serde_json::json!([{ "kind": "domain_wildcard", "pattern": "*.amazon.co.jp" }]),
        serde_json::json!([{ "kind": "domainwildcard", "pattern": "*.amazon.co.jp" }]),
        serde_json::json!([{ "kind": "domain", "pattern": "*.amazon.co.jp" }]),
        serde_json::json!([{ "kind": "domainWildcard" }]),
        serde_json::json!([{ "kind": "domainWildcard", "pattern": 1 }]),
        serde_json::json!([{ "kind": "domainWildcard", "pattern": "*.amazon.co.jp", "x": 1 }]),
        serde_json::json!([{ "kind": "domainWildcard", "pattern": "*.co.jp" }]),
    ] {
        assert!(parse_json(with_rules(2, rules.clone())).is_err(), "{rules}");
    }
}

#[test]
fn an_invalid_domain_wildcard_refuses_the_policy_even_in_a_disabled_layer_and_is_named() {
    let mut policy = with_domains(&["*.example.com"], &["*.co.jp/x", "*.amazon.co.jp"]);
    policy.layers.save_but_no_mcp.enabled = false;
    assert_eq!(
        CompiledPolicy::compile(&policy).unwrap_err(),
        Error::InvalidPolicy
    );
    assert_eq!(
        policy.evaluate("https://public.test/"),
        Decision::NeverSave,
        "an unusable policy refuses every URL"
    );
    assert_eq!(
        policy.problems(),
        [Problem {
            layer: Some("saveButNoMcp"),
            rule_index: Some(0),
            code: "invalid_domain_wildcard",
        }]
    );
    let rule = domain("*.co.jp");
    assert_eq!(
        rule.error_code(Error::InvalidPolicy),
        "invalid_domain_wildcard"
    );
    assert_eq!(rule.error_code(Error::InvalidUrl), "invalid_url");
    assert_eq!(regex("a").error_code(Error::RegexLimit), "regex_limit");
    assert_eq!(
        regex("a").error_code(Error::UnsupportedRule),
        "unsupported_regex"
    );
    assert_eq!(regex("a").error_code(Error::InvalidPolicy), "invalid_regex");
    assert_eq!(
        RuleSpec::from("a").error_code(Error::InvalidPolicy),
        "invalid_pattern"
    );
}

#[test]
fn domain_wildcards_keep_the_order_of_the_zones() {
    let mut policy = with_domains(&["*.example.com"], &["*.example.com", "*.example.org"]);
    let compiled = CompiledPolicy::compile(&policy).unwrap();
    assert_eq!(
        compiled.evaluate("https://a.example.com/"),
        Decision::NeverSave
    );
    assert_eq!(
        compiled.evaluate("https://a.example.org/"),
        Decision::SaveButNoMcp
    );
    assert_eq!(compiled.evaluate("https://example.net/"), Decision::None);
    policy.layers.never_save.enabled = false;
    let compiled = CompiledPolicy::compile(&policy).unwrap();
    assert_eq!(
        compiled.evaluate("https://a.example.com/"),
        Decision::SaveButNoMcp
    );
    policy.layers.save_but_no_mcp.enabled = false;
    let compiled = CompiledPolicy::compile(&policy).unwrap();
    assert_eq!(compiled.evaluate("https://a.example.com/"), Decision::None);
    // The builtins still apply on top of a domain wildcard.
    let compiled = CompiledPolicy::compile(&with_domains(&[], &["*.example.com"])).unwrap();
    assert_eq!(
        compiled.evaluate("https://a.example.com/?token=x"),
        Decision::NeverSave
    );
}

// --- PSL override (docs/issue-025-policy-engine.md, PSL override) ---

#[test]
fn the_list_snapshot_is_named_as_bundled() {
    use crate::public_suffix::{SNAPSHOT_DATE, SOURCE, SOURCE_COMMIT};
    // Bound to the digest checked in `the_bundled_public_suffix_list_is_the_pinned_one_and_is_read`.
    assert_eq!(SOURCE, "publicsuffix/list");
    assert_eq!(SOURCE_COMMIT, "8437e5262f13305f94087abe699b4fe2be1d1de8");
    assert_eq!(SNAPSHOT_DATE, "2026-10-22");
    let doc = include_str!("../../../docs/issue-025-policy-engine.md");
    assert!(doc.contains(SOURCE_COMMIT) && doc.contains(SNAPSHOT_DATE));
}

#[test]
fn syntax_and_the_list_are_judged_apart() {
    use crate::public_suffix::{diagnose, Diagnosis, Restriction::*};
    let restricted = |domain: &str, restriction| Diagnosis::Restricted {
        domain: domain.into(),
        restriction,
    };
    for (rule, expected) in [
        ("*.amazon.co.jp", Diagnosis::Valid("amazon.co.jp".into())),
        ("*.Amazon.CO.jp", Diagnosis::Valid("amazon.co.jp".into())),
        ("*.co.jp", restricted("co.jp", PublicSuffix)),
        ("*.CO.JP", restricted("co.jp", PublicSuffix)),
        ("*.com", restricted("com", PublicSuffix)),
        ("*.github.io", restricted("github.io", PublicSuffix)),
        (
            "*.foo.kawasaki.jp",
            restricted("foo.kawasaki.jp", PublicSuffix),
        ),
        (
            "*.個人.香港",
            restricted("xn--gmqw5a.xn--j6w193g", PublicSuffix),
        ),
        (
            "*.xn--55qx5d.xn--j6w193g",
            restricted("xn--55qx5d.xn--j6w193g", PublicSuffix),
        ),
        (
            "*.example.newtld",
            restricted("example.newtld", UnknownSuffix),
        ),
        (
            "*.corp.internal",
            restricted("corp.internal", UnknownSuffix),
        ),
        ("*.newtld", restricted("newtld", UnknownSuffix)),
        (
            "*.kawasaki.jp",
            restricted("kawasaki.jp", DelegatedBoundary),
        ),
        (
            "*.amazonaws.com",
            restricted("amazonaws.com", DelegatedBoundary),
        ),
        (
            "*.www.example.com",
            restricted("www.example.com", NotRegistrable),
        ),
        (
            "*.mail.amazon.co.jp",
            restricted("mail.amazon.co.jp", NotRegistrable),
        ),
    ] {
        assert_eq!(diagnose(rule), expected, "{rule}");
    }
    for hard in [
        "*.amazon.co.jp/*",
        "https://*.amazon.co.jp",
        "*.amazon.co.jp:443",
        "www.*.amazon.co.jp",
        "*.*.example.com",
        "www?.example.com",
        "?.example.com",
        "example.*",
        "*.example.*",
        "*",
        "*.",
        " *.co.jp",
        "*.co.jp.",
        "*..co.jp",
        "*.-a.com",
        "*.a_b.com",
        "*.1.2.3.4",
        "*.xn--.com",
        "amazon.co.jp",
    ] {
        assert_eq!(diagnose(hard), Diagnosis::InvalidSyntax, "{hard:?}");
    }
    assert_eq!(
        diagnose(&format!("*.{}.com", "a".repeat(64))),
        Diagnosis::InvalidSyntax
    );
    assert_eq!(
        diagnose(&format!("*.{}.com", "a".repeat(5000))),
        Diagnosis::InvalidSyntax
    );
}

fn approved(domains: &[&str]) -> DomainApprovals {
    DomainApprovals::of(domains.iter().map(|d| d.to_string()))
}

#[test]
fn an_approval_makes_exactly_its_domain_usable_and_nothing_else() {
    let co = approved(&["co.jp"]);
    let rule = domain("*.co.jp");
    for (url, expected) in [
        ("https://co.jp/", true),
        ("https://amazon.co.jp/", true),
        ("https://a.b.example.co.jp/", true),
        ("https://amazon.ne.jp/", false),
        ("https://co.jp.evil.com/", false),
    ] {
        assert_eq!(rule_matches_with(&rule, url, &co), Ok(expected), "{url}");
    }
    assert_eq!(
        rule_matches_with(&domain("*.CO.JP"), "https://x.co.jp/", &co),
        Ok(true),
        "the canonical domain is the identity"
    );
    for other in [
        "*.ne.jp",
        "*.x.co.jp.newtld",
        "*.github.io",
        "*.kawasaki.jp",
    ] {
        assert_eq!(
            rule_matches_with(&domain(other), "https://a.ne.jp/", &co),
            Err(Error::UnapprovedDomain),
            "{other}"
        );
    }
    // No approval makes bad syntax usable, whatever is approved.
    let everything = approved(&["co.jp", "amazon.co.jp", "com", "example.com"]);
    for hard in [
        "*.co.jp/*",
        "https://*.co.jp",
        "*.co.jp:443",
        "*.*.co.jp",
        "co.jp",
    ] {
        assert_eq!(
            rule_matches_with(&domain(hard), "https://a.co.jp/", &everything),
            Err(Error::InvalidPolicy),
            "{hard}"
        );
    }
    // An approval is no match: a URL elsewhere is not refused by it.
    let mut policy = with_domains(&[], &["*.co.jp"]);
    let compiled = CompiledPolicy::compile_with(&policy, &co).unwrap();
    assert_eq!(compiled.evaluate("https://public.test/"), Decision::None);
    assert_eq!(
        compiled.evaluate("https://shop.co.jp/"),
        Decision::SaveButNoMcp
    );
    // A valid domain needs none, and gains nothing from one.
    policy.layers.save_but_no_mcp.rules = vec![domain("*.example.com")];
    let plain = CompiledPolicy::compile(&policy).unwrap();
    let with = CompiledPolicy::compile_with(&policy, &everything).unwrap();
    for url in urls() {
        assert_eq!(plain.evaluate(&url), with.evaluate(&url), "{url}");
    }
}

#[test]
fn an_unapproved_domain_refuses_the_whole_policy_and_is_named() {
    let mut policy = with_domains(&["*.example.com"], &["*.github.io", "*.co.jp/x"]);
    policy.layers.save_but_no_mcp.enabled = false;
    assert_eq!(
        CompiledPolicy::compile_with(&policy, &approved(&["co.jp"])).unwrap_err(),
        Error::UnapprovedDomain
    );
    assert_eq!(
        policy.problems_with(&approved(&["co.jp"])),
        [
            Problem {
                layer: Some("saveButNoMcp"),
                rule_index: Some(0),
                code: "domain_wildcard_not_approved",
            },
            Problem {
                layer: Some("saveButNoMcp"),
                rule_index: Some(1),
                code: "invalid_domain_wildcard",
            }
        ]
    );
    policy.layers.save_but_no_mcp.rules.pop();
    assert!(policy.problems_with(&approved(&["github.io"])).is_empty());
    assert!(policy.validate_with(&approved(&["github.io"])).is_ok());
    assert_eq!(policy.validate().unwrap_err(), Error::UnapprovedDomain);
    assert_eq!(
        domain("*.a").error_code(Error::UnapprovedDomain),
        "domain_wildcard_not_approved"
    );
    assert_eq!(
        domain("*.a").error_code(Error::SuffixListUnavailable),
        "psl_unavailable"
    );
}

#[test]
fn a_policy_depends_on_the_approvals_of_its_restricted_domains_only() {
    let mut policy = with_domains(&["*.co.jp", "*.example.com"], &["*.CO.JP", "*.newtld"]);
    policy.layers.never_save.enabled = false;
    policy
        .layers
        .save_but_no_mcp
        .rules
        .push(domain("*.co.jp/*"));
    policy.layers.save_but_no_mcp.rules.push("*.com".into());
    assert_eq!(
        policy.approval_dependencies(),
        HashSet::from(["co.jp".to_owned(), "newtld".to_owned()]),
        "patterns, valid domains and bad syntax need none"
    );
}

#[test]
fn the_engine_reads_no_store() {
    let engine = include_str!("../policy.rs");
    let list = include_str!("../public_suffix.rs");
    for source in [engine, list] {
        for forbidden in ["rusqlite", "domain_approvals", "std::fs", "File::"] {
            assert!(!source.contains(forbidden), "{forbidden}");
        }
    }
}

// --- Exact rules (Issue #43, Slice 2) -------------------------------------------------------------

fn exact_rule(url: &str) -> RuleSpec {
    RuleSpec::Typed(TypedRule::Exact {
        pattern: url.into(),
    })
}

fn typed_policy(never: &[RuleSpec], private: &[RuleSpec]) -> Policy {
    let mut policy = Policy::default();
    policy.layers.never_save.rules = never.to_vec();
    policy.layers.save_but_no_mcp.rules = private.to_vec();
    policy
}

fn blind(rules: &[RuleSpec]) -> CompiledPolicy {
    CompiledPolicy::compile(&typed_policy(rules, &[])).unwrap()
}

#[test]
fn an_exact_rule_is_one_whole_url_as_the_parser_writes_it() {
    let rule = "https://example.com/thread/123?page=3#c1";
    let compiled = blind(&[exact_rule(rule)]);
    // The same URL: as the parser writes it, or in a form it writes the same.
    for same in [
        rule,
        "HTTPS://EXAMPLE.COM/thread/123?page=3#c1",
        "https://example.com:443/thread/123?page=3#c1",
        "https://example.com./thread/123?page=3#c1",
        "https://example.com/thread/x/../123?page=3#c1",
        "blob:https://example.com/thread/123?page=3#c1",
    ] {
        assert_eq!(compiled.evaluate(same), Decision::NeverSave, "{same}");
    }
    // Any other URL: no prefix, no wildcard, no case folding of path or query, no decoding.
    for other in [
        "https://example.com/thread/123",
        "https://example.com/thread/123?page=3",
        "https://example.com/thread/123?page=3#c2",
        "https://example.com/thread/123?page=30#c1",
        "https://example.com/thread/123?Page=3#c1",
        "https://example.com/thread/123/?page=3#c1",
        "https://example.com/Thread/123?page=3#c1",
        "https://example.com/thread/12%33?page=3#c1",
        "http://example.com/thread/123?page=3#c1",
        "https://example.com:8443/thread/123?page=3#c1",
        "https://www.example.com/thread/123?page=3#c1",
        "https://example.com/thread/123?page=3&x=1#c1",
        "https://example.com/thread/123?#c1",
    ] {
        assert_eq!(compiled.evaluate(other), Decision::None, "{other}");
    }
    // A trailing slash is another URL, as the parser keeps it.
    let slash = blind(&[exact_rule("https://example.com/a/")]);
    assert_eq!(
        slash.evaluate("https://example.com/a/"),
        Decision::NeverSave
    );
    assert_eq!(slash.evaluate("https://example.com/a"), Decision::None);
    // The parser writes an empty path as "/": one URL.
    let bare = blind(&[exact_rule("https://example.com")]);
    assert_eq!(bare.evaluate("https://example.com/"), Decision::NeverSave);
    // Percent-encoding as the parser writes it: a raw character it encodes is the same URL, an
    // escape in other hex case is another one.
    let encoded = blind(&[exact_rule("https://example.com/%E6%BC%AB")]);
    assert_eq!(
        encoded.evaluate("https://example.com/漫"),
        Decision::NeverSave
    );
    assert_eq!(
        encoded.evaluate("https://example.com/%e6%bc%ab"),
        Decision::None
    );
}

#[test]
fn exact_url_key_is_the_compared_form() {
    assert_eq!(
        exact_url_key("HTTPS://Example.COM.:443/a/./b?q=1#f").unwrap(),
        "https://example.com/a/b?q=1#f"
    );
    assert_eq!(
        exact_url_key("https://bücher.test/").unwrap(),
        "https://xn--bcher-kva.test/"
    );
    for bad in [
        "",
        "example.com/a",
        "/a",
        "ftp://example.com/",
        "chrome://settings",
        "blob:https://example.com/x",
        "https://example.com/a b",
        "https://example.com/%zz",
        "https://example.com/\\a",
        "https://",
    ] {
        assert_eq!(exact_url_key(bad), Err(Error::InvalidPolicy), "{bad:?}");
    }
    assert!(exact_url_key(&format!("https://e.com/{}", "a".repeat(32_768))).is_err());
}

#[test]
fn a_bad_exact_rule_refuses_the_whole_policy_even_in_a_disabled_layer() {
    for bad in [
        "",
        "not a url",
        "ftp://example.com/",
        "https://example.com/%zz",
    ] {
        let mut policy = typed_policy(&[], &[exact_rule(bad)]);
        policy.layers.save_but_no_mcp.enabled = false;
        assert!(CompiledPolicy::compile(&policy).is_err(), "{bad:?}");
        assert_eq!(
            policy.evaluate("https://example.com/"),
            Decision::NeverSave,
            "fail closed"
        );
        assert_eq!(
            policy.problems(),
            [Problem {
                layer: Some("saveButNoMcp"),
                rule_index: Some(0),
                code: "invalid_exact_url"
            }]
        );
    }
    // Version 1 has no exact rule.
    let mut old = policy(&[], &[]);
    old.layers
        .never_save
        .rules
        .push(exact_rule("https://example.com/"));
    assert!(old.validate().is_err());
    assert!(old.migrated().is_err());
}

#[test]
fn exact_rules_keep_the_order_of_layers_and_the_builtins() {
    let url = "https://example.com/a?b=1";
    let both = typed_policy(&[exact_rule(url)], &[exact_rule(url)]);
    assert_eq!(both.evaluate(url), Decision::NeverSave, "Never Save wins");
    let private = typed_policy(&[], &[exact_rule(url)]);
    assert_eq!(private.evaluate(url), Decision::SaveButNoMcp);
    let mut off = private.clone();
    off.layers.save_but_no_mcp.enabled = false;
    assert_eq!(
        off.evaluate(url),
        Decision::None,
        "a disabled layer decides nothing"
    );
    // Save but no MCP on a URL a builtin refuses: still refused.
    for refused in [
        "https://accounts.google.com/signin",
        "https://example.com/a?token=x",
        "https://u:p@example.com/",
    ] {
        let policy = typed_policy(&[], &[exact_rule(refused)]);
        assert_eq!(policy.evaluate(refused), Decision::NeverSave, "{refused}");
    }
    // With patterns and regexes in the same layers.
    let mixed = typed_policy(
        &[RuleSpec::from("other.test"), exact_rule(url)],
        &[
            RuleSpec::Typed(TypedRule::Regex {
                pattern: "secret".into(),
            }),
            exact_rule("https://example.com/c"),
        ],
    );
    assert_eq!(mixed.evaluate(url), Decision::NeverSave);
    assert_eq!(mixed.evaluate("https://other.test/x"), Decision::NeverSave);
    assert_eq!(
        mixed.evaluate("https://example.com/secret"),
        Decision::SaveButNoMcp
    );
    assert_eq!(
        mixed.evaluate("https://example.com/c"),
        Decision::SaveButNoMcp
    );
    assert_eq!(mixed.evaluate("https://example.com/d"), Decision::None);
}

#[test]
fn adding_an_exact_rule_never_loosens_a_decision() {
    let corpus = urls();
    let sample: Vec<&String> = corpus.iter().step_by(corpus.len() / 40 + 1).collect();
    let bases = [
        typed_policy(&[], &[]),
        typed_policy(&[RuleSpec::from("example.com")], &[]),
        typed_policy(&[], &[RuleSpec::from("*.example.com")]),
    ];
    let mut added = 0;
    for base in &bases {
        let before = CompiledPolicy::compile(base).unwrap();
        for url in &sample {
            let Ok(key) = exact_url_key(url) else {
                continue;
            };
            for layer in 0..2 {
                let mut more = base.clone();
                let rules = if layer == 0 {
                    &mut more.layers.never_save.rules
                } else {
                    &mut more.layers.save_but_no_mcp.rules
                };
                rules.push(exact_rule(&key));
                let after = CompiledPolicy::compile(&more).unwrap();
                assert_ne!(after.evaluate(url), Decision::None, "{url}");
                for other in &sample {
                    assert!(
                        strictness(after.evaluate(other)) >= strictness(before.evaluate(other)),
                        "{url} loosened {other}"
                    );
                }
                added += 1;
            }
        }
    }
    assert!(added > 30, "{added}");
}

#[test]
fn exact_rules_compile_no_regex_and_use_no_regex_budget() {
    // 128 regexes and 1000 - 128 exact rules in one layer, 1000 exact rules in the other.
    let mut never: Vec<RuleSpec> = (0..MAX_REGEX_RULES)
        .map(|n| {
            RuleSpec::Typed(TypedRule::Regex {
                pattern: format!("^https://r{n}\\.test/"),
            })
        })
        .collect();
    never.extend((0..1000 - MAX_REGEX_RULES).map(|n| exact_rule(&format!("https://e.test/{n}"))));
    let private: Vec<RuleSpec> = (0..1000)
        .map(|n| exact_rule(&format!("https://p.test/{n}?x")))
        .collect();
    let policy = typed_policy(&never, &private);
    assert_eq!(policy.regex_count(), MAX_REGEX_RULES);
    let compiled = CompiledPolicy::compile(&policy).unwrap();
    assert_eq!(
        compiled.blind.regexes.len(),
        MAX_REGEX_RULES,
        "only regexes are compiled"
    );
    assert_eq!(
        (compiled.blind.urls.len(), compiled.private.urls.len()),
        (1000 - MAX_REGEX_RULES, 1000)
    );
    assert!(compiled.private.regexes.is_empty());
    assert_eq!(compiled.evaluate("https://e.test/5"), Decision::NeverSave);
    assert_eq!(
        compiled.evaluate("https://p.test/999?x"),
        Decision::SaveButNoMcp
    );
    assert_eq!(compiled.evaluate("https://p.test/999"), Decision::None);
    // Each regex gets the budget of 128 regexes, as without the exact rules.
    let alone = typed_policy(&never[..MAX_REGEX_RULES], &[]);
    let (with, without) = (RegexBudget::of(&policy), RegexBudget::of(&alone));
    assert_eq!((with.size, with.dfa), (without.size, without.dfa));
    // The rule limit of a layer still holds.
    let mut over = policy.clone();
    over.layers
        .save_but_no_mcp
        .rules
        .push(exact_rule("https://p.test/x"));
    assert_eq!(
        CompiledPolicy::compile(&over).err(),
        Some(Error::InvalidPolicy)
    );
    // A 129th regex is still refused.
    let mut regexes = policy.clone();
    regexes.layers.save_but_no_mcp.rules[0] = RuleSpec::Typed(TypedRule::Regex {
        pattern: "x".into(),
    });
    assert_eq!(
        CompiledPolicy::compile(&regexes).err(),
        Some(Error::RegexLimit)
    );
}

#[test]
fn an_exact_rule_round_trips_as_json() {
    let json = r#"{"kind":"exact","pattern":"https://example.com/thread/123?page=3"}"#;
    let rule: RuleSpec = serde_json::from_str(json).unwrap();
    assert_eq!(rule, exact_rule("https://example.com/thread/123?page=3"));
    assert_eq!(serde_json::to_string(&rule).unwrap(), json);
    for bad in [
        r#"{"kind":"exact"}"#,
        r#"{"kind":"exact","pattern":1}"#,
        r#"{"kind":"exact","pattern":"x","extra":1}"#,
    ] {
        assert!(serde_json::from_str::<TypedRule>(bad).is_err(), "{bad}");
    }
    assert!(rule_matches(&rule, "https://example.com/thread/123?page=3").unwrap());
    assert!(!rule_matches(&rule, "https://example.com/thread/123").unwrap());
}
