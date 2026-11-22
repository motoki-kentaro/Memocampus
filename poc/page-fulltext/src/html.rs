//! Search text and metadata from HTML with the html5ever **tokenizer** only: no DOM tree, no CSS,
//! no script. Text tokens are collected in document order; tags only switch a few states.
//!
//! What is kept (bounded):
//! - an open-element stack of names and flags (at most `max_open_elements`; deeper elements are
//!   not tracked and reported), needed to know where an excluded or preformatted region ends when
//!   end tags are missing or misnested;
//! - the raw-text state of the tokenizer (which element's content it is in);
//! - the output text (at most `max_text_bytes`) and the metadata (at most `max_metadata_bytes` per
//!   value).
//!
//! Content model switching (what the HTML tree builder would tell the tokenizer, done here):
//! in HTML context, `<script>` → script data; `<style>`, `<xmp>`, `<iframe>`, `<noembed>`,
//! `<noframes>`, `<noscript>` → RAWTEXT; `<title>`, `<textarea>` → RCDATA; `<plaintext>` →
//! PLAINTEXT. In those states the tokenizer emits only characters until the matching end tag, so a
//! `<` or a tag-like string inside a script or style can never become a tag or body text. Inside
//! SVG/MathML (foreign content) no switching happens (as in browsers), and `<![CDATA[` is
//! recognised there.
//!
//! Excluded from the body text: script, style, noscript, template, svg (whole subtree), canvas,
//! iframe, noembed, noframes, frameset, the head (and title), comments, doctype. Elements with the
//! `hidden` attribute (except `hidden="until-found"`, whose content the browser's find-in-page
//! reveals) or an inline `style` with `display: none`. `aria-hidden` is NOT a reason to exclude
//! text (it hides from assistive technology, not from the screen). Inline `visibility: hidden` is
//! not either (a descendant can be `visibility: visible`). MathML text is kept.
//!
//! Tree-construction approximations (documented, not a full tree builder):
//! - void elements are never pushed; an end tag pops up to the nearest open element of that name
//!   within its scope, otherwise it is ignored (as browsers ignore stray end tags);
//! - start tags that implicitly close an open `<p>`, `<li>`, `<dt>`/`<dd>`, `<option>`, table rows
//!   and cells pop those elements first;
//! - the head ends at `</head>`, `<body>`, any start tag that cannot be in the head, or non-space
//!   text;
//! - in foreign content, the HTML "breakout" start tags (`<p>`, `<div>`, `<b>` ...) and the end
//!   tags `</p>`, `</br>` close the foreign elements down to the nearest HTML element or
//!   integration point, as browsers do;
//! - only the SVG/MathML integration points (`foreignObject`, `desc`, `title`; `mi`, `mo`, `mn`,
//!   `ms`, `mtext`, `annotation-xml`) are scope boundaries for HTML end tags, as in the HTML
//!   Standard; other SVG/MathML elements are closed together with the HTML element they are in;
//! - `</template>` closes the nearest open template whatever is open inside it (no scope check,
//!   as in the HTML Standard); no other end tag closes an element outside a template from inside
//!   it (template is a scope boundary);
//! - an end tag naming an open SVG/MathML element closes it even when HTML elements are still
//!   open inside its integration point (`<svg><foreignObject><p>x</foreignObject></svg>`), unless
//!   a template is open above it. **Deliberate difference from browsers**, which ignore such an
//!   end tag and keep the rest of the page inside the SVG: here the rest would then be excluded
//!   and lost, so the explicit end tag wins (more text, never less);
//! - `</h1>`..`</h6>` close the nearest open heading of any level;
//! - if the stack is full, further elements are not tracked: an exclusion may then end early
//!   (more text, never less).
//!
//! Separation: block elements, `<br>`, `<hr>` and custom elements (name with `-`) end a line;
//! table cells are separated by a space; inline elements add nothing, so `<span>東</span><span>京都
//! </span>` gives `東京都` while `<p>東京</p><p>都内</p>` gives two lines.

use crate::limits::{ErrorCode, Limits};
use crate::metadata::{warn, Collector, Metadata};
use html5ever::tendril::StrTendril;
use html5ever::tokenizer::states::RawKind;
use html5ever::tokenizer::{
    BufferQueue, Tag, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
};
use html5ever::Attribute;
use std::cell::RefCell;
use std::collections::HashMap;

const BLOCKS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "body",
    "caption",
    "center",
    "dd",
    "details",
    "dialog",
    "dir",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hgroup",
    "hr",
    "li",
    "legend",
    "main",
    "menu",
    "nav",
    "ol",
    "p",
    "pre",
    "search",
    "section",
    "summary",
    "table",
    "tbody",
    "tfoot",
    "thead",
    "tr",
    "ul",
    "option",
    "optgroup",
    "select",
    "textarea",
    "listing",
    "xmp",
    "plaintext",
    "html",
];

const CELLS: &[&str] = &["td", "th"];

const VOID: &[&str] = &[
    "area", "base", "basefont", "bgsound", "br", "col", "embed", "frame", "hr", "img", "input",
    "keygen", "link", "meta", "param", "source", "track", "wbr", "image",
];

/// Start tags that close an open `<p>` (HTML Standard, "in body").
const CLOSES_P: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "center",
    "details",
    "dialog",
    "dir",
    "div",
    "dl",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "header",
    "hgroup",
    "main",
    "menu",
    "nav",
    "ol",
    "p",
    "search",
    "section",
    "summary",
    "ul",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "pre",
    "listing",
    "form",
    "table",
    "hr",
    "xmp",
    "plaintext",
    "li",
    "dd",
    "dt",
];

/// Elements that may be in the head; any other start tag ends it.
const HEAD_CONTENT: &[&str] = &[
    "base", "basefont", "bgsound", "link", "meta", "title", "noscript", "noframes", "style",
    "script", "template", "head", "html",
];

/// Scope boundaries for end tags and implicit closing (HTML Standard "has an element in scope",
/// simplified). Every SVG/MathML element is a boundary too.
const SCOPE: &[&str] = &[
    "applet", "caption", "html", "table", "td", "th", "marquee", "object", "template",
];
/// "Table scope": used for table, row group, row and cell elements.
const TABLE_SCOPE: &[&str] = &["html", "table", "template"];
const TABLE_PARTS: &[&str] = &[
    "table", "tbody", "thead", "tfoot", "tr", "td", "th", "caption",
];

/// Start tags that break out of SVG/MathML (HTML Standard, "in foreign content").
const BREAKOUT: &[&str] = &[
    "b",
    "big",
    "blockquote",
    "body",
    "br",
    "center",
    "code",
    "dd",
    "div",
    "dl",
    "dt",
    "em",
    "embed",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "head",
    "hr",
    "i",
    "img",
    "li",
    "listing",
    "menu",
    "meta",
    "nobr",
    "ol",
    "p",
    "pre",
    "ruby",
    "s",
    "small",
    "span",
    "strong",
    "strike",
    "sub",
    "sup",
    "table",
    "tt",
    "u",
    "ul",
    "var",
];

const HEADINGS: &[&str] = &["h1", "h2", "h3", "h4", "h5", "h6"];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ns {
    Html,
    Svg,
    Math,
}

#[derive(Debug)]
struct Open {
    name: StrTendril,
    ns: Ns,
    /// Its content is not body text (template, canvas, svg, frameset, hidden ...).
    excluded: bool,
    /// Its text keeps line breaks (pre, listing, textarea ...).
    pre: bool,
    /// An SVG/MathML integration point: HTML rules apply inside it, and it is a scope boundary.
    ip: bool,
}

/// HTML integration points (SVG) and MathML text integration points (plus `annotation-xml`).
fn integration_point(ns: Ns, name: &str) -> bool {
    match ns {
        Ns::Html => false,
        Ns::Svg => matches!(name, "foreignobject" | "desc" | "title"),
        Ns::Math => matches!(name, "mi" | "mo" | "mn" | "ms" | "mtext" | "annotation-xml"),
    }
}

/// What the tokenizer is inside of, when it is in a raw-text state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Raw {
    /// Dropped content: script, style, noscript, iframe, noembed, noframes.
    Dropped,
    /// The document title (first one) or a later title (dropped).
    Title,
    /// Visible preformatted text: textarea, xmp, plaintext.
    Text,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Sep {
    None,
    Space,
    Line,
}

/// Collects text with whitespace collapsing and boundaries, refusing to grow beyond `max_bytes`.
pub struct TextBuilder {
    out: String,
    pending: Sep,
    max_bytes: usize,
    pub dropped_controls: usize,
}

impl TextBuilder {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            out: String::new(),
            pending: Sep::None,
            max_bytes,
            dropped_controls: 0,
        }
    }
    fn boundary(&mut self, sep: Sep) {
        self.pending = self.pending.max(sep);
    }
    pub fn push_text(&mut self, text: &str, preformatted: bool) -> Result<(), ErrorCode> {
        for c in text.chars() {
            if c == '\n' || c == '\r' {
                self.boundary(if preformatted { Sep::Line } else { Sep::Space });
            } else if c.is_whitespace() {
                self.boundary(Sep::Space);
            } else if c.is_control() {
                // NUL, other C0/C1 controls: never part of the text.
                self.dropped_controls += 1;
            } else {
                if !self.out.is_empty() {
                    match self.pending {
                        Sep::Line => self.out.push('\n'),
                        Sep::Space => self.out.push(' '),
                        Sep::None => {}
                    }
                }
                self.pending = Sep::None;
                self.out.push(c);
                if self.out.len() > self.max_bytes {
                    return Err(ErrorCode::TextTooLarge);
                }
            }
        }
        Ok(())
    }
    pub fn finish(self) -> String {
        self.out
    }
}

/// Counters reported in the diagnostics (codes and numbers only, never text).
#[derive(Debug, Default, Clone, Copy, serde::Serialize)]
pub struct TokenStats {
    pub start_tags: usize,
    pub end_tags: usize,
    pub text_tokens: usize,
    pub comments: usize,
    pub parse_errors: usize,
    pub ignored_end_tags: usize,
    pub implicitly_closed: usize,
    pub max_open_depth: usize,
    pub untracked_elements: usize,
    pub excluded_chars: usize,
}

/// Stack positions per element name, so that every scope search costs a few map lookups instead
/// of a walk down the stack (stray end tags against a deep stack cannot make the work quadratic).
/// Holds only names that are currently open (at most `max_open_elements` positions in total).
#[derive(Default)]
struct Positions {
    by_name: HashMap<String, Vec<usize>>,
    all: Vec<usize>,
}

impl Positions {
    fn add(&mut self, name: &str, i: usize) {
        match self.by_name.get_mut(name) {
            Some(v) => v.push(i),
            None => {
                self.by_name.insert(name.to_owned(), vec![i]);
            }
        }
        self.all.push(i);
    }
    fn remove(&mut self, name: &str) {
        if let Some(v) = self.by_name.get_mut(name) {
            v.pop();
            if v.is_empty() {
                self.by_name.remove(name);
            }
        }
        self.all.pop();
    }
    fn last(&self, name: &str) -> Option<usize> {
        if self.all.is_empty() {
            return None;
        }
        self.by_name.get(name).and_then(|v| v.last().copied())
    }
    fn last_any(&self) -> Option<usize> {
        self.all.last().copied()
    }
}

struct State {
    text: TextBuilder,
    meta: Collector,
    warnings: Vec<&'static str>,
    stack: Vec<Open>,
    html_pos: Positions,
    foreign_pos: Positions,
    /// Stack positions of the open integration points.
    ip_pos: Vec<usize>,
    max_open: usize,
    excluded_open: usize,
    pre_open: usize,
    raw: Option<Raw>,
    in_head: bool,
    has_body: bool,
    excluded_subtrees: usize,
    hidden_subtrees: usize,
    stats: TokenStats,
    error: Option<ErrorCode>,
}

fn attr<'a>(attrs: &'a [Attribute], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|a| &*a.name.local == name)
        .map(|a| &*a.value)
}

/// `hidden` (except `until-found`) or inline `display: none`. Nothing else is decidable without CSS.
pub fn is_hidden(attrs: &[Attribute]) -> bool {
    if let Some(v) = attr(attrs, "hidden") {
        if !v.trim().eq_ignore_ascii_case("until-found") {
            return true;
        }
    }
    if let Some(style) = attr(attrs, "style") {
        for decl in style.split(';') {
            let Some((prop, value)) = decl.split_once(':') else {
                continue;
            };
            let value = value.trim().to_ascii_lowercase();
            let value = value.trim_end_matches("!important").trim();
            if prop.trim().eq_ignore_ascii_case("display") && value == "none" {
                return true;
            }
        }
    }
    false
}

impl State {
    fn sep(&mut self, sep: Sep) {
        self.text.boundary(sep);
    }

    fn top(&self) -> Option<&Open> {
        self.stack.last()
    }

    /// Foreign context: the current node is an SVG/MathML element that is not an HTML integration
    /// point. Raw-text switching and HTML rules do not apply there.
    fn in_foreign(&self) -> bool {
        self.top().is_some_and(|o| o.ns != Ns::Html && !o.ip)
    }

    /// Closes the SVG/MathML elements above the nearest HTML element or integration point.
    fn close_foreign_to_html(&mut self) {
        let keep = self
            .html_pos
            .last_any()
            .max(self.ip_pos.last().copied())
            .map_or(0, |i| i + 1);
        if self.stack.len() > keep {
            self.stats.implicitly_closed += self.stack.len() - keep;
            self.truncate(keep);
        }
    }

    /// Pops the element at `i` and everything above it (an end tag that matched).
    fn close_at(&mut self, i: usize) {
        self.stats.implicitly_closed += self.stack.len() - i - 1;
        self.truncate(i);
    }

    fn push(&mut self, name: &str, ns: Ns, excluded: bool, pre: bool) {
        if self.stack.len() >= self.max_open {
            self.stats.untracked_elements += 1;
            warn(&mut self.warnings, "open_element_limit_reached");
            return;
        }
        let i = self.stack.len();
        if ns == Ns::Html {
            self.html_pos.add(name, i);
        } else {
            self.foreign_pos.add(name, i);
        }
        let ip = integration_point(ns, name);
        if ip {
            self.ip_pos.push(i);
        }
        self.stack.push(Open {
            name: StrTendril::from_slice(name),
            ns,
            excluded,
            pre,
            ip,
        });
        if excluded {
            self.excluded_open += 1;
        }
        if pre {
            self.pre_open += 1;
        }
        self.stats.max_open_depth = self.stats.max_open_depth.max(self.stack.len());
    }

    fn truncate(&mut self, len: usize) {
        while self.stack.len() > len {
            let o = self.stack.pop().expect("len checked");
            if o.ns == Ns::Html {
                self.html_pos.remove(&o.name);
            } else {
                self.foreign_pos.remove(&o.name);
            }
            if o.ip {
                self.ip_pos.pop();
            }
            if o.excluded {
                self.excluded_open -= 1;
            }
            if o.pre {
                self.pre_open -= 1;
            }
            if o.ns == Ns::Html && (BLOCKS.contains(&&*o.name) || o.name.contains('-')) {
                self.text.boundary(Sep::Line);
            } else if CELLS.contains(&&*o.name) {
                self.text.boundary(Sep::Space);
            }
        }
    }

    /// Index of the nearest open HTML element named `name`, provided no scope boundary is open
    /// above it. Table parts use the table scope (html, table, template only); every other name
    /// uses the scope, whose boundaries include the SVG/MathML integration points.
    fn find_in_scope(&self, name: &str, extra_boundaries: &[&str]) -> Option<usize> {
        let idx = self.html_pos.last(name)?;
        let table = TABLE_PARTS.contains(&name);
        let scope = if table { TABLE_SCOPE } else { SCOPE };
        let ip = if table {
            None
        } else {
            self.ip_pos.last().copied()
        };
        let boundary = scope
            .iter()
            .chain(extra_boundaries)
            .filter_map(|b| self.html_pos.last(b))
            .chain(ip)
            .max();
        match boundary {
            Some(b) if b > idx => None,
            _ => Some(idx),
        }
    }

    fn close_implicitly(&mut self, name: &str, extra: &[&str]) {
        if let Some(i) = self.find_in_scope(name, extra) {
            self.stats.implicitly_closed += self.stack.len() - i;
            self.truncate(i);
        }
    }

    fn start_tag(&mut self, tag: &Tag) -> TokenSinkResult<()> {
        self.stats.start_tags += 1;
        let name: &str = &tag.name;

        if self.in_foreign() {
            let breakout = BREAKOUT.contains(&name)
                || (name == "font"
                    && ["color", "face", "size"]
                        .iter()
                        .any(|a| attr(&tag.attrs, a).is_some()));
            if !breakout {
                let ns = self.top().map_or(Ns::Svg, |o| o.ns);
                if !tag.self_closing {
                    let lower = name.to_ascii_lowercase();
                    self.push(&lower, ns, false, false);
                }
                return TokenSinkResult::Continue;
            }
            // Breakout: close the foreign elements (down to the nearest HTML element or
            // integration point), then handle the tag as HTML.
            self.close_foreign_to_html();
        }

        if self.in_head && !HEAD_CONTENT.contains(&name) {
            self.in_head = false;
        }
        match name {
            "head" => {
                if !self.has_body {
                    self.in_head = true;
                }
                return TokenSinkResult::Continue;
            }
            "body" => {
                self.in_head = false;
                self.has_body = true;
                self.sep(Sep::Line);
                return TokenSinkResult::Continue;
            }
            "html" => return TokenSinkResult::Continue,
            _ => {}
        }

        // Implicit end tags (approximation of the tree builder's "in body" rules).
        if CLOSES_P.contains(&name) {
            self.close_implicitly("p", &["button"]);
        }
        match name {
            "li" => self.close_implicitly("li", &["ol", "ul"]),
            "dd" | "dt" => {
                self.close_implicitly("dd", &["dl"]);
                self.close_implicitly("dt", &["dl"]);
            }
            "option" | "optgroup" => {
                if self.top().is_some_and(|o| &*o.name == "option") {
                    self.truncate(self.stack.len() - 1);
                    self.stats.implicitly_closed += 1;
                }
            }
            "tr" => {
                self.close_implicitly("td", &["tr", "tbody", "thead", "tfoot"]);
                self.close_implicitly("th", &["tr", "tbody", "thead", "tfoot"]);
                self.close_implicitly("tr", &["tbody", "thead", "tfoot"]);
            }
            "td" | "th" => {
                self.close_implicitly("td", &["tr"]);
                self.close_implicitly("th", &["tr"]);
            }
            "tbody" | "thead" | "tfoot" => {
                for n in ["td", "th", "tr", "tbody", "thead", "tfoot"] {
                    self.close_implicitly(n, &[]);
                }
            }
            _ => {}
        }

        let hidden = is_hidden(&tag.attrs);
        if name == "meta" {
            let r = self.meta.meta_tag(
                attr(&tag.attrs, "name"),
                attr(&tag.attrs, "property"),
                attr(&tag.attrs, "content"),
                &mut self.warnings,
            );
            if let Err(e) = r {
                self.error = Some(e);
            }
            return TokenSinkResult::Continue;
        }
        if name == "br" || name == "hr" {
            if self.excluded_open == 0 && !hidden {
                self.sep(Sep::Line);
            }
            return TokenSinkResult::Continue;
        }
        if VOID.contains(&name) {
            return TokenSinkResult::Continue;
        }

        // Raw-text elements: the tokenizer must switch state (even for `<script/>`: in HTML the
        // self-closing flag does not end a script).
        let raw = match name {
            "script" => Some((Raw::Dropped, RawKind::ScriptData)),
            "style" | "iframe" | "noembed" | "noframes" | "noscript" => {
                Some((Raw::Dropped, RawKind::Rawtext))
            }
            "xmp" => Some((Raw::Text, RawKind::Rawtext)),
            "title" => Some((Raw::Title, RawKind::Rcdata)),
            "textarea" => Some((Raw::Text, RawKind::Rcdata)),
            _ => None,
        };
        if let Some((kind, raw_kind)) = raw {
            let kind = match kind {
                Raw::Title if self.meta.title_start() => Raw::Title,
                Raw::Title => Raw::Dropped,
                Raw::Text if self.excluded_open > 0 || hidden => Raw::Dropped,
                k => k,
            };
            if kind == Raw::Dropped && name != "title" {
                self.excluded_subtrees += 1;
            }
            if kind == Raw::Text {
                self.sep(Sep::Line);
            }
            self.raw = Some(kind);
            return TokenSinkResult::RawData(raw_kind);
        }
        if name == "plaintext" {
            self.sep(Sep::Line);
            self.raw = Some(if self.excluded_open > 0 || hidden {
                Raw::Dropped
            } else {
                Raw::Text
            });
            return TokenSinkResult::Plaintext;
        }

        let (ns, excluded) = match name {
            "svg" => (Ns::Svg, true),
            "math" => (Ns::Math, false),
            "template" | "canvas" | "frameset" => (Ns::Html, true),
            _ => (Ns::Html, false),
        };
        if excluded {
            self.excluded_subtrees += 1;
        } else if hidden {
            self.hidden_subtrees += 1;
        }
        let excluded = excluded || hidden;
        if ns != Ns::Html && tag.self_closing {
            return TokenSinkResult::Continue;
        }
        if ns == Ns::Html && (BLOCKS.contains(&name) || name.contains('-')) {
            self.sep(Sep::Line);
        } else if CELLS.contains(&name) {
            self.sep(Sep::Space);
        }
        let pre = matches!(name, "pre" | "listing");
        self.push(name, ns, excluded, pre);
        TokenSinkResult::Continue
    }

    fn end_tag(&mut self, tag: &Tag) {
        self.stats.end_tags += 1;
        let name: &str = &tag.name;
        if let Some(raw) = self.raw.take() {
            // In a raw-text state the only tag the tokenizer emits is the matching end tag.
            match raw {
                Raw::Title => self.meta.title_end(&mut self.warnings),
                Raw::Text => self.sep(Sep::Line),
                Raw::Dropped => {}
            }
            return;
        }
        // `</template>` closes the nearest template, whatever is still open inside it (tables,
        // cells, SVG ...): the HTML Standard checks no scope for it.
        if name == "template" {
            match self.html_pos.last("template") {
                Some(i) => self.close_at(i),
                None => self.stats.ignored_end_tags += 1,
            }
            return;
        }
        // An end tag naming an open SVG/MathML element (ASCII case-insensitive) that is nearer
        // than any open HTML element of that name closes it, together with whatever HTML is still
        // open inside its integration points, but never across an open template (its content
        // stays separate). See the module documentation: browsers ignore it when HTML elements
        // are open, which would leave the rest of the page inside the excluded SVG.
        // (The tokenizer has already lowercased the tag name.)
        if let Some(i) = self.foreign_pos.last(name) {
            let html_same = self.html_pos.last(name);
            let template_above = self.html_pos.last("template").is_some_and(|t| t > i);
            if html_same.is_none_or(|h| h < i) && !template_above {
                self.close_at(i);
                return;
            }
        }
        // `</p>` and `</br>` in foreign content leave it first (HTML Standard).
        if matches!(name, "p" | "br") && self.in_foreign() {
            self.close_foreign_to_html();
        }
        if HEADINGS.contains(&name) {
            // Any heading end tag closes the nearest open heading.
            match HEADINGS
                .iter()
                .filter_map(|h| self.find_in_scope(h, &[]))
                .max()
            {
                Some(i) => self.close_at(i),
                None => {
                    self.stats.ignored_end_tags += 1;
                    self.sep(Sep::Line);
                }
            }
            return;
        }
        match name {
            "head" => {
                self.in_head = false;
                return;
            }
            "body" | "html" => return,
            "br" => {
                // `</br>` is treated as `<br>` by browsers.
                self.sep(Sep::Line);
                return;
            }
            "p" if self.find_in_scope("p", &["button"]).is_none() => {
                // `</p>` without an open `<p>`: browsers insert an empty paragraph.
                self.sep(Sep::Line);
                return;
            }
            _ => {}
        }
        // `</li>` uses the list item scope; every other end tag the scope (or table scope).
        let extra: &[&str] = if name == "li" { &["ol", "ul"] } else { &[] };
        match self.find_in_scope(name, extra) {
            Some(i) => self.close_at(i),
            None => {
                self.stats.ignored_end_tags += 1;
                if BLOCKS.contains(&name) {
                    self.sep(Sep::Line);
                }
            }
        }
    }

    fn text(&mut self, s: &str) -> Result<(), ErrorCode> {
        self.stats.text_tokens += 1;
        match self.raw {
            Some(Raw::Title) => return self.meta.title_text(s),
            Some(Raw::Text) => return self.text.push_text(s, true),
            Some(Raw::Dropped) => {
                self.stats.excluded_chars += s.chars().count();
                return Ok(());
            }
            None => {}
        }
        if self.excluded_open > 0 {
            self.stats.excluded_chars += s.chars().count();
            return Ok(());
        }
        if self.in_head {
            if s.chars().all(char::is_whitespace) {
                return Ok(());
            }
            // Non-space text ends the head; the text belongs to the body.
            self.in_head = false;
        }
        if !s.chars().all(char::is_whitespace) {
            self.has_body = true;
        }
        self.text.push_text(s, self.pre_open > 0)
    }
}

struct Sink {
    state: RefCell<State>,
}

impl TokenSink for Sink {
    type Handle = ();

    fn process_token(&self, token: Token, _line: u64) -> TokenSinkResult<()> {
        let mut st = self.state.borrow_mut();
        if st.error.is_some() {
            return TokenSinkResult::Continue;
        }
        match token {
            Token::TagToken(tag) => match tag.kind {
                TagKind::StartTag => return st.start_tag(&tag),
                TagKind::EndTag => st.end_tag(&tag),
            },
            Token::CharacterTokens(s) => {
                if let Err(e) = st.text(&s) {
                    st.error = Some(e);
                }
            }
            Token::NullCharacterToken => st.text.dropped_controls += 1,
            Token::CommentToken(_) => st.stats.comments += 1,
            Token::ParseError(_) => st.stats.parse_errors += 1,
            Token::DoctypeToken(_) | Token::EOFToken => {}
        }
        TokenSinkResult::Continue
    }

    fn end(&self) {
        let mut st = self.state.borrow_mut();
        // A title without its end tag (EOF in RCDATA) is still the title.
        if st.raw == Some(Raw::Title) {
            let State { meta, warnings, .. } = &mut *st;
            meta.title_end(warnings);
        }
        st.raw = None;
    }

    fn adjusted_current_node_present_but_not_in_html_namespace(&self) -> bool {
        self.state.borrow().in_foreign()
    }
}

pub struct Extracted {
    pub text: String,
    pub metadata: Metadata,
    pub has_body: bool,
    pub excluded_subtrees: usize,
    pub hidden_subtrees: usize,
    pub dropped_controls: usize,
    pub malformed: bool,
    pub warnings: Vec<&'static str>,
    pub stats: TokenStats,
}

/// Converts `body` (already transfer-decoded, BOM removed) from `encoding` and tokenizes it piece
/// by piece. The decoded document is never held whole: each converted piece (at most
/// `tokenizer_chunk_bytes` source bytes) is handed to the tokenizer and dropped.
pub fn extract(
    encoding: &'static encoding_rs::Encoding,
    body: &[u8],
    limits: &Limits,
) -> Result<Extracted, ErrorCode> {
    let sink = Sink {
        state: RefCell::new(State {
            text: TextBuilder::new(limits.max_text_bytes),
            meta: Collector::new(limits.max_metadata_bytes),
            warnings: Vec::new(),
            stack: Vec::new(),
            html_pos: Positions::default(),
            foreign_pos: Positions::default(),
            ip_pos: Vec::new(),
            max_open: limits.max_open_elements,
            excluded_open: 0,
            pre_open: 0,
            raw: None,
            in_head: false,
            has_body: false,
            excluded_subtrees: 0,
            hidden_subtrees: 0,
            stats: TokenStats::default(),
            error: None,
        }),
    };
    let tokenizer = Tokenizer::new(
        sink,
        TokenizerOpts {
            discard_bom: false,
            ..TokenizerOpts::default()
        },
    );
    let queue = BufferQueue::default();
    let malformed = crate::charset::decode_streaming(
        encoding,
        body,
        limits.tokenizer_chunk_bytes,
        limits.max_decoded_html_bytes,
        |piece| {
            queue.push_back(StrTendril::from_slice(piece));
            let _ = tokenizer.feed(&queue);
            match tokenizer.sink.state.borrow().error {
                Some(e) => Err(e),
                None => Ok(()),
            }
        },
    )?;
    tokenizer.end();
    let st = tokenizer.sink.state.into_inner();
    if let Some(e) = st.error {
        return Err(e);
    }
    let mut warnings = st.warnings;
    if malformed {
        warn(&mut warnings, "malformed_bytes_replaced_with_u_fffd");
    }
    let dropped_controls = st.text.dropped_controls;
    Ok(Extracted {
        text: st.text.finish(),
        metadata: st.meta.meta,
        has_body: st.has_body,
        excluded_subtrees: st.excluded_subtrees,
        hidden_subtrees: st.hidden_subtrees,
        dropped_controls,
        malformed,
        warnings,
        stats: st.stats,
    })
}
