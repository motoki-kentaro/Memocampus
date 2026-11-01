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
            && (matches_any(&a.rules, &url)
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
            && (matches_any(&b.rules, &url)
                || (c.government && matches_any(&BUILTINS.government, &url))
                || (c.webmail && matches_any(&BUILTINS.webmail, &url))
                || (c.ai_chats && matches_any(&BUILTINS.ai_chats, &url)))
        {
            return Decision::SaveButNoMcp;
        }
        Decision::None
    }
}

fn policy(never: &[&str], private: &[&str]) -> Policy {
    let mut policy = Policy::default();
    policy.layers.never_save.rules = never.iter().map(|r| r.to_string()).collect();
    policy.layers.save_but_no_mcp.rules = private.iter().map(|r| r.to_string()).collect();
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
            engine_version: 2,
            ..Policy::default()
        },
        Policy {
            schema_version: 2,
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
    many.layers.never_save.rules = hosts;
    many.layers.save_but_no_mcp.rules = paths;
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
fn the_stored_form_of_a_policy_is_unchanged() {
    let stored = serde_json::to_value(Policy::default()).unwrap();
    assert_eq!(stored["schemaVersion"], 1);
    assert_eq!(stored["engineVersion"], 1);
    assert_eq!(
        stored,
        serde_json::from_str::<serde_json::Value>(DEFAULT_JSON).unwrap()
    );
}
