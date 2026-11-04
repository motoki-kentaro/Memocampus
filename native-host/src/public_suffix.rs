//! The domain of a `domainWildcard` rule (docs/issue-025-policy-engine.md, Domain Wildcard): a
//! registrable domain by the Public Suffix List bundled with the host, both its ICANN and its
//! PRIVATE section. The list is read from the binary, never from the network; if it cannot be read,
//! no domain is registrable and every such rule is refused.
use publicsuffix::{List, Psl};
use std::collections::HashSet;
use std::sync::LazyLock;
use url::{Host, Url};

/// The list at publicsuffix/list commit 8437e5262f13305f94087abe699b4fe2be1d1de8 (2026-10-22), as
/// published, under the Mozilla Public License 2.0. Updating it is replacing the file and its
/// SHA-256 in the tests.
pub const LIST_TEXT: &str = include_str!("../data/public_suffix_list.dat");
/// The longest host DNS allows, and the longest label.
const MAX_HOST_BYTES: usize = 253;
const MAX_LABEL_BYTES: usize = 63;

struct Suffixes {
    list: List,
    /// Every domain with a public suffix below it, such as `amazonaws.com` (`s3.amazonaws.com`) or
    /// `kawasaki.jp` (`*.kawasaki.jp`): a wildcard on one would span many registrants.
    covering: HashSet<String>,
}

static SUFFIXES: LazyLock<Option<Suffixes>> = LazyLock::new(|| load(LIST_TEXT));

fn load(text: &str) -> Option<Suffixes> {
    if !text.contains("// ===BEGIN ICANN DOMAINS===")
        || !text.contains("// ===BEGIN PRIVATE DOMAINS===")
        || !text.contains("// ===END PRIVATE DOMAINS===")
    {
        return None;
    }
    let list: List = text.parse().ok()?;
    let mut covering = HashSet::new();
    for line in text.lines() {
        let rule = line.split_whitespace().next().unwrap_or("");
        // An exception (`!city.kawasaki.jp`) belongs to a wildcard rule that is marked already.
        if rule.is_empty() || rule.starts_with("//") || rule.starts_with('!') {
            continue;
        }
        let (wildcard, name) = match rule.strip_prefix("*.") {
            Some(name) => (true, name),
            None => (false, rule),
        };
        let ascii = rule_host(name)?;
        if wildcard {
            covering.insert(ascii.clone());
        }
        let mut rest = ascii.as_str();
        while let Some((_, parent)) = rest.split_once('.') {
            covering.insert(parent.to_owned());
            rest = parent;
        }
    }
    Some(Suffixes { list, covering })
}

/// A name of the list as a URL host is written: the same parser, so the same ASCII form. Most names
/// are lower-case ASCII letters, digits, `-` and `.` already, which the parser writes unchanged (a
/// test checks every one), so only the others are parsed.
fn rule_host(name: &str) -> Option<String> {
    let plain = name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.');
    if plain {
        Some(name.to_owned())
    } else {
        ascii_host(name)
    }
}

/// The host of `https://{name}/` if it is a domain name, written by the URL parser: lower case,
/// IDNA to ASCII. None for an IP address or anything the parser refuses.
fn ascii_host(name: &str) -> Option<String> {
    let url = Url::parse(&format!("https://{name}/")).ok()?;
    match url.host()? {
        Host::Domain(domain) => Some(domain.to_owned()),
        Host::Ipv4(_) | Host::Ipv6(_) => None,
    }
}

/// The host a `domainWildcard` rule covers, with all its subdomains, or None if the rule is not
/// `*.` and a registrable domain. Nothing is trimmed: the text is taken as it is written.
pub fn wildcard_domain(rule: &str) -> Option<String> {
    let name = rule.strip_prefix("*.")?;
    if name.is_empty()
        || name.len() > MAX_HOST_BYTES * 4
        || name
            .chars()
            .any(|c| c <= ' ' || c == '\u{7f}' || "*?/\\#@:%[]".contains(c) || c.is_whitespace())
    {
        return None;
    }
    // A leading, trailing or doubled dot leaves an empty label, which the label check refuses.
    let host = ascii_host(name)?;
    let labels_valid = host.len() <= MAX_HOST_BYTES
        && host.split('.').all(|label| {
            (1..=MAX_LABEL_BYTES).contains(&label.len())
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        });
    if !labels_valid {
        return None;
    }
    let suffixes = SUFFIXES.as_ref()?;
    let known = suffixes
        .list
        .suffix(host.as_bytes())
        .is_some_and(|suffix| suffix.is_known() && suffix.typ().is_some());
    let registrable = suffixes
        .list
        .domain(host.as_bytes())
        .is_some_and(|domain| domain.as_bytes() == host.as_bytes());
    (known && registrable && !suffixes.covering.contains(&host)).then_some(host)
}

/// Whether the bundled list was read. A test checks it; nothing else needs to.
pub fn loaded() -> bool {
    SUFFIXES.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_list_that_is_not_the_list_registers_nothing() {
        assert!(load("").is_none());
        assert!(load("com\nco.jp\n").is_none(), "no sections: not the list");
        let cut = LIST_TEXT
            .split("// ===BEGIN PRIVATE DOMAINS===")
            .next()
            .unwrap();
        assert!(load(cut).is_none(), "the private section is missing");
    }

    #[test]
    fn every_name_of_the_list_is_read_as_the_url_parser_writes_it() {
        let mut plain = 0;
        for line in LIST_TEXT.lines() {
            let rule = line.split_whitespace().next().unwrap_or("");
            if rule.is_empty() || rule.starts_with("//") {
                continue;
            }
            let name = rule.trim_start_matches('!').trim_start_matches("*.");
            let parsed = ascii_host(name);
            assert!(parsed.is_some(), "{rule}");
            assert_eq!(rule_host(name), parsed, "{rule}");
            plain += usize::from(name.is_ascii());
        }
        assert!(plain > 9000, "{plain}");
    }

    #[test]
    fn the_covering_set_holds_parents_and_wildcard_bases() {
        let suffixes = SUFFIXES.as_ref().expect("bundled list");
        for covering in ["jp", "com", "kawasaki.jp", "amazonaws.com", "io"] {
            assert!(suffixes.covering.contains(covering), "{covering}");
        }
        for free in ["amazon.co.jp", "example.com", "city.kawasaki.jp"] {
            assert!(!suffixes.covering.contains(free), "{free}");
        }
    }
}
