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
