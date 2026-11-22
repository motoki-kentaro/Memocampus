//! Page metadata, collected from tokens (no tree). Every value is untrusted text written by the
//! website.
//!
//! - `title`: the text of the first `<title>` in HTML context (a `<title>` inside `<svg>`/`<math>`
//!   is a foreign element, not the document title), whitespace collapsed. Its text never goes into
//!   the body text.
//! - `<meta>` (HTML context only): the keys are read from BOTH the `name` and the `property`
//!   attribute (trimmed, ASCII lower-cased), so `<meta name="description" property="og:description"
//!   content=...>` fills both fields. Target keys: `description`, `keywords`, `og:title`,
//!   `og:description`, `article:tag`. A `<meta>` with no target key is ignored before its `content`
//!   is looked at: its size cannot refuse the page.
//! - Single-valued fields take the first non-empty occurrence; later ones are reported
//!   (`duplicate_meta_*`). `article:tag` keeps every value in document order, exact duplicates once.
//! - `keywords`: split on `,` (U+002C), `、` (U+3001), `，` (U+FF0C), `;`; each item trimmed; empty
//!   items dropped; order kept, exact duplicates once.
//! - Missing means `None` (JSON `null`); a tag whose value is empty after trimming is also `None`
//!   and is reported (`empty_meta_*`, `empty_title`). Values are never filled with empty strings.
//! - Each value (and the joined keyword list, and all article tags together) is limited to
//!   `max_metadata_bytes` after cleaning; above it the page is refused (`metadata_too_large`),
//!   never silently truncated. The title is cleaned while it is read, so a huge `<title>` never
//!   grows a buffer beyond the limit.

use crate::limits::ErrorCode;
use serde::Serialize;

#[derive(Debug, Default, PartialEq, Eq, Serialize)]
pub struct Metadata {
    pub title: Option<String>,
    pub description: Option<String>,
    pub keywords: Option<Vec<String>>,
    pub og_title: Option<String>,
    pub og_description: Option<String>,
    pub article_tags: Vec<String>,
}

/// Whitespace collapsing and control-character removal, fed piece by piece, with a size limit on
/// the cleaned result.
pub struct Cleaner {
    out: String,
    space: bool,
    max: usize,
}

impl Cleaner {
    pub fn new(max: usize) -> Self {
        Self {
            out: String::new(),
            space: false,
            max,
        }
    }
    pub fn push(&mut self, raw: &str) -> Result<(), ErrorCode> {
        for c in raw.chars() {
            if c.is_whitespace() {
                self.space = true;
            } else if c.is_control() {
                continue;
            } else {
                if self.space && !self.out.is_empty() {
                    self.out.push(' ');
                }
                self.space = false;
                self.out.push(c);
                if self.out.len() > self.max {
                    return Err(ErrorCode::MetadataTooLarge);
                }
            }
        }
        Ok(())
    }
    pub fn finish(self) -> String {
        self.out
    }
}

/// Collapses whitespace, removes control characters, trims.
pub fn clean(raw: &str) -> String {
    let mut c = Cleaner::new(usize::MAX);
    let _ = c.push(raw);
    c.finish()
}

fn clean_bounded(raw: &str, max: usize) -> Result<String, ErrorCode> {
    let mut c = Cleaner::new(max);
    c.push(raw)?;
    Ok(c.finish())
}

pub fn split_keywords(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in raw.split([',', '、', '，', ';']) {
        let item = clean(item);
        if !item.is_empty() && !out.contains(&item) {
            out.push(item);
        }
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Key {
    Description,
    Keywords,
    OgTitle,
    OgDescription,
    ArticleTag,
}

fn key_of(raw: &str) -> Option<Key> {
    let k = raw.trim();
    if k.eq_ignore_ascii_case("description") {
        Some(Key::Description)
    } else if k.eq_ignore_ascii_case("keywords") {
        Some(Key::Keywords)
    } else if k.eq_ignore_ascii_case("og:title") {
        Some(Key::OgTitle)
    } else if k.eq_ignore_ascii_case("og:description") {
        Some(Key::OgDescription)
    } else if k.eq_ignore_ascii_case("article:tag") {
        Some(Key::ArticleTag)
    } else {
        None
    }
}

/// Receives title text and `<meta>` attributes from the tokenizer sink.
pub struct Collector {
    pub meta: Metadata,
    max: usize,
    title: Option<Cleaner>,
    seen_title: bool,
    tags_bytes: usize,
}

pub fn warn(warnings: &mut Vec<&'static str>, code: &'static str) {
    if !warnings.contains(&code) {
        warnings.push(code);
    }
}

impl Collector {
    pub fn new(max: usize) -> Self {
        Self {
            meta: Metadata::default(),
            max,
            title: None,
            seen_title: false,
            tags_bytes: 0,
        }
    }

    /// A `<title>` start tag in HTML context. Only the first one is the document title.
    pub fn title_start(&mut self) -> bool {
        if self.seen_title {
            return false;
        }
        self.seen_title = true;
        self.title = Some(Cleaner::new(self.max));
        true
    }

    pub fn title_text(&mut self, text: &str) -> Result<(), ErrorCode> {
        match &mut self.title {
            Some(c) => c.push(text),
            None => Ok(()),
        }
    }

    pub fn title_end(&mut self, warnings: &mut Vec<&'static str>) {
        if let Some(c) = self.title.take() {
            let t = c.finish();
            if t.is_empty() {
                warn(warnings, "empty_title");
            } else {
                self.meta.title = Some(t);
            }
        }
    }

    /// A `<meta>` tag in HTML context: `name`, `property` and `content` attribute values.
    pub fn meta_tag(
        &mut self,
        name: Option<&str>,
        property: Option<&str>,
        content: Option<&str>,
        warnings: &mut Vec<&'static str>,
    ) -> Result<(), ErrorCode> {
        let mut keys: Vec<Key> = Vec::with_capacity(2);
        for k in [name, property].into_iter().flatten().filter_map(key_of) {
            if !keys.contains(&k) {
                keys.push(k);
            }
        }
        // Not a target: ignored before the content is cleaned or measured (MEDIUM-1).
        if keys.is_empty() {
            return Ok(());
        }
        let Some(content) = content else {
            return Ok(());
        };
        let value = clean_bounded(content, self.max)?;
        for key in keys {
            let (slot, dup, empty) = match key {
                Key::Description => (
                    &mut self.meta.description,
                    "duplicate_meta_description",
                    "empty_meta_description",
                ),
                Key::OgTitle => (
                    &mut self.meta.og_title,
                    "duplicate_meta_og_title",
                    "empty_meta_og_title",
                ),
                Key::OgDescription => (
                    &mut self.meta.og_description,
                    "duplicate_meta_og_description",
                    "empty_meta_og_description",
                ),
                Key::Keywords => {
                    if value.is_empty() {
                        warn(warnings, "empty_meta_keywords");
                    } else if self.meta.keywords.is_some() {
                        warn(warnings, "duplicate_meta_keywords");
                    } else {
                        self.meta.keywords = Some(split_keywords(content));
                    }
                    continue;
                }
                Key::ArticleTag => {
                    if value.is_empty() {
                        warn(warnings, "empty_meta_article_tag");
                    } else if !self.meta.article_tags.contains(&value) {
                        self.tags_bytes += value.len();
                        if self.tags_bytes > self.max {
                            return Err(ErrorCode::MetadataTooLarge);
                        }
                        self.meta.article_tags.push(value.clone());
                    }
                    continue;
                }
            };
            if value.is_empty() {
                warn(warnings, empty);
            } else if slot.is_some() {
                warn(warnings, dup);
            } else {
                *slot = Some(value.clone());
            }
        }
        Ok(())
    }
}
