use memocampus_native_host::policy::{normalize_url, Decision, Policy, Rule, RuleSpec, TypedRule};

fn as_version_1(policy: &Policy) -> Policy {
    let mut old = policy.clone();
    (old.schema_version, old.engine_version) = (1, 1);
    for layer in [&mut old.layers.never_save, &mut old.layers.save_but_no_mcp] {
        for rule in &mut layer.rules {
            if let RuleSpec::Typed(TypedRule::Pattern { pattern }) = rule {
                *rule = RuleSpec::Text(pattern.clone());
            }
        }
    }
    old
}
use serde_json::Value;

#[test]
fn shared_conformance_vectors() {
    let vectors: Value =
        serde_json::from_str(include_str!("../../tests/privacy/vectors.json")).unwrap();
    for v in vectors["decisions"].as_array().unwrap() {
        let policy = v.get("policy").map_or_else(Policy::default, |p| {
            Policy::parse(&serde_json::to_vec(p).unwrap()).unwrap()
        });
        let expected: Decision = serde_json::from_value(v["expected"].clone()).unwrap();
        assert_eq!(
            policy.evaluate(v["url"].as_str().unwrap()),
            expected,
            "{}",
            v["name"]
        );
        // The same policy in the other version decides the same.
        let other = if policy.schema_version == 1 {
            policy.migrated().unwrap()
        } else {
            as_version_1(&policy)
        };
        assert_eq!(
            other.evaluate(v["url"].as_str().unwrap()),
            expected,
            "other version {}",
            v["name"]
        );
    }
    for v in vectors["normalization"].as_array().unwrap() {
        let actual = normalize_url(v["url"].as_str().unwrap());
        if v["expected"].is_null() {
            assert!(actual.is_err(), "{}", v["url"]);
        } else {
            assert_eq!(
                serde_json::to_value(actual.unwrap()).unwrap(),
                v["expected"],
                "{}",
                v["url"]
            );
        }
    }
    for v in vectors["rules"].as_array().unwrap() {
        assert_eq!(
            Rule::parse(v["rule"].as_str().unwrap()).is_ok(),
            v["valid"].as_bool().unwrap(),
            "{}",
            v["rule"]
        );
    }
    for v in vectors["invalidPolicies"].as_array().unwrap() {
        assert!(
            Policy::parse(&serde_json::to_vec(&v["policy"]).unwrap()).is_err(),
            "{}",
            v["name"]
        );
    }
}
