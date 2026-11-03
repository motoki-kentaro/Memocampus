//! VisibleStore: the only read path from a vault to MCP (Issue #12: visits and image metadata).
//!
//! Every call builds its own visible set from a fresh policy and a fresh snapshot, answers from it
//! and drops it. Nothing is cached, so no failure can fall back on an older set. See
//! `docs/issue-012-visible-store-design.md`.
//!
//! The API offers typed reads:
//!
//! ```
//! use memocampus_visible_store::{SearchVisits, VisibleStore};
//! fn f(store: &VisibleStore) {
//!     let _ = store.search_visits(&SearchVisits::default());
//! }
//! ```
//!
//! and has no way to obtain a connection, run SQL, or reach the set:
//!
//! ```compile_fail
//! use memocampus_visible_store::VisibleSet;
//! ```
//! ```compile_fail
//! use memocampus_visible_store::snapshot::read_visits;
//! ```
//! ```compile_fail
//! use memocampus_visible_store::snapshot::read_image_saves;
//! ```
//! ```compile_fail
//! fn f(store: &memocampus_visible_store::VisibleStore) { let _ = store.db; }
//! ```
//! ```compile_fail
//! fn f(store: &memocampus_visible_store::VisibleStore) { let _ = store.policy; }
//! ```
//! ```compile_fail
//! fn f(store: &memocampus_visible_store::VisibleStore) { store.execute("SELECT 1"); }
//! ```

mod location;
mod snapshot;
#[cfg(test)]
mod tests;
mod time;
mod visible_images;
mod visible_set;

use memocampus_native_host::policy::CompiledPolicy;
use memocampus_native_host::policy_store::{PolicyStore, StoreError};
use std::path::{Path, PathBuf};
use std::time::Duration;
use visible_images::VisibleImages;
use visible_set::{IdKey, VisibleSet};

pub const DEFAULT_LIMIT: u32 = 50;
pub const MAX_LIMIT: u32 = 100;
pub const MAX_QUERY_CHARS: usize = 200;

/// A writer holds the policy lock only for one small file replacement.
const POLICY_BUSY_RETRIES: u32 = 5;
const POLICY_BUSY_WAIT: Duration = Duration::from_millis(20);
/// How often a set may be rebuilt because the policy changed while it was being built.
const POLICY_CHANGE_ATTEMPTS: u32 = 3;
/// Beyond any timestamp `time::parse_timestamp` accepts.
const START_OF_TIME: i64 = i64::MIN;
const END_OF_TIME: i64 = i64::MAX;

/// A failure carries a fixed code only, never a URL, title, path or row count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisibleError {
    InvalidArgument,
    PolicyUnavailable,
    VaultUnavailable,
    VaultSchemaMismatch,
    VaultUnsupported,
    VaultCorrupt,
    VisibleSetFailed,
    /// The same for an image that never existed and one with no public save.
    NotFound,
}

impl VisibleError {
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidArgument => "invalid_argument",
            Self::PolicyUnavailable => "policy_unavailable",
            Self::VaultUnavailable => "vault_unavailable",
            Self::VaultSchemaMismatch => "vault_schema_mismatch",
            Self::VaultUnsupported => "vault_unsupported",
            Self::VaultCorrupt => "vault_corrupt",
            Self::VisibleSetFailed => "visible_set_failed",
            Self::NotFound => "not_found",
        }
    }
}

/// `id` is opaque and stable only for the lifetime of the `VisibleStore` that returned it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleVisit {
    pub id: String,
    pub url: String,
    pub title: String,
    pub visited_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisitPage {
    pub items: Vec<VisibleVisit>,
    /// Matching public rows, before the limit.
    pub total: u64,
    pub has_more: bool,
}

/// `from` is inclusive and `to` exclusive, both as `Date#toISOString()` text.
#[derive(Debug, Clone, Default)]
pub struct SearchVisits<'a> {
    pub query: &'a str,
    pub from: Option<&'a str>,
    pub to: Option<&'a str>,
    pub limit: Option<u32>,
}

/// `date` is `YYYY-MM-DD` in the zone given by `utc_offset`, a fixed offset such as `+09:00`.
#[derive(Debug, Clone)]
pub struct VisitsByDay<'a> {
    pub date: &'a str,
    pub utc_offset: &'a str,
    pub limit: Option<u32>,
}

/// One image, described from its public saves only. `id` is opaque and stable only for the lifetime
/// of the `VisibleStore` that returned it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleImage {
    pub id: String,
    pub mime_type: String,
    pub width: u32,
    pub height: u32,
    pub public_save_count: u64,
    pub first_saved_at: String,
    pub last_saved_at: String,
    /// `image_url` of the newest public save: a URL or `data:image/<subtype>`.
    pub latest_image_url: String,
    pub latest_page_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImagePage {
    pub items: Vec<VisibleImage>,
    /// Matching images with a public save, before the limit.
    pub total: u64,
    pub has_more: bool,
}

/// One public save. `id` is opaque like `VisibleImage::id`, and of a different kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleImageSource {
    pub id: String,
    pub image_url: String,
    pub page_url: Option<String>,
    pub frame_url: Option<String>,
    pub saved_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageSourcePage {
    pub items: Vec<VisibleImageSource>,
    /// Public saves of the image, before the limit.
    pub total: u64,
    pub has_more: bool,
}

/// An image matches if one of its public saves has the text in a source URL and lies within the
/// range; what is returned for it still describes all its public saves. `from` is inclusive and
/// `to` exclusive, as for visits.
#[derive(Debug, Clone, Default)]
pub struct SearchImages<'a> {
    pub query: &'a str,
    pub from: Option<&'a str>,
    pub to: Option<&'a str>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct GetImageSources<'a> {
    pub image_id: &'a str,
    pub limit: Option<u32>,
}

/// Points in one read at which tests interleave other activity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stage {
    PolicyLoaded,
    SchemaChecked,
    VaultClosed,
    SetBuilt,
}

type ReadRows<Row> = fn(&Path, &mut dyn FnMut(Stage)) -> Result<Vec<Row>, VisibleError>;

pub struct VisibleStore {
    vault: PathBuf,
    policy: PolicyStore,
    id_key: IdKey,
}

impl VisibleStore {
    /// `app_dir` is the global application directory holding `policy.json`, independent of the vault.
    pub fn new(vault: PathBuf, app_dir: PathBuf) -> Result<Self, VisibleError> {
        let policy = PolicyStore::new(app_dir).map_err(|_| VisibleError::PolicyUnavailable)?;
        Ok(Self::with_policy(vault, policy))
    }

    /// The policy of `%LOCALAPPDATA%\Memocampus` (or `MEMOCAMPUS_APP_DIR`).
    pub fn from_process(vault: PathBuf) -> Result<Self, VisibleError> {
        let policy = PolicyStore::from_process().map_err(|_| VisibleError::PolicyUnavailable)?;
        Ok(Self::with_policy(vault, policy))
    }

    fn with_policy(vault: PathBuf, policy: PolicyStore) -> Self {
        let mut id_key = [0u8; 32];
        id_key[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        id_key[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        Self {
            vault,
            policy,
            id_key,
        }
    }

    pub fn search_visits(&self, request: &SearchVisits) -> Result<VisitPage, VisibleError> {
        self.search_visits_staged(request, &mut |_| {})
    }

    pub fn get_visits_by_day(&self, request: &VisitsByDay) -> Result<VisitPage, VisibleError> {
        self.get_visits_by_day_staged(request, &mut |_| {})
    }

    pub fn search_images(&self, request: &SearchImages) -> Result<ImagePage, VisibleError> {
        self.search_images_staged(request, &mut |_| {})
    }

    pub fn get_image_sources(
        &self,
        request: &GetImageSources,
    ) -> Result<ImageSourcePage, VisibleError> {
        let limit = checked_limit(request.limit)?;
        self.read_visible(
            &mut |_| {},
            snapshot::read_image_saves,
            VisibleImages::build,
            |set| set.sources(request.image_id, limit),
        )
    }

    fn search_visits_staged(
        &self,
        request: &SearchVisits,
        stage: &mut dyn FnMut(Stage),
    ) -> Result<VisitPage, VisibleError> {
        let (pattern, range, limit) =
            search_terms(request.query, request.from, request.to, request.limit)?;
        self.read_visible(
            stage,
            snapshot::read_visits,
            VisibleSet::build,
            |set: &VisibleSet| set.query(&pattern, range, limit),
        )
    }

    fn search_images_staged(
        &self,
        request: &SearchImages,
        stage: &mut dyn FnMut(Stage),
    ) -> Result<ImagePage, VisibleError> {
        let (pattern, range, limit) =
            search_terms(request.query, request.from, request.to, request.limit)?;
        self.read_visible(
            stage,
            snapshot::read_image_saves,
            VisibleImages::build,
            |set| set.search(&pattern, range, limit),
        )
    }

    fn get_visits_by_day_staged(
        &self,
        request: &VisitsByDay,
        stage: &mut dyn FnMut(Stage),
    ) -> Result<VisitPage, VisibleError> {
        let range = time::day_range(request.date, request.utc_offset)
            .ok_or(VisibleError::InvalidArgument)?;
        let limit = checked_limit(request.limit)?;
        self.read_visible(
            stage,
            snapshot::read_visits,
            VisibleSet::build,
            |set: &VisibleSet| set.query("%", range, limit),
        )
    }

    /// Answers from a set whose policy was still current after the set was complete. The read takes
    /// effect at that check: a policy update finished before this call began is always applied.
    fn read_visible<Row, Set, T>(
        &self,
        stage: &mut dyn FnMut(Stage),
        read: ReadRows<Row>,
        build: fn(Vec<Row>, &CompiledPolicy, &IdKey) -> Result<Set, VisibleError>,
        query: impl Fn(&Set) -> Result<T, VisibleError>,
    ) -> Result<T, VisibleError> {
        for _ in 0..POLICY_CHANGE_ATTEMPTS {
            // Compiled once for this attempt, as it was loaded. A policy that does not compile
            // gives no decision at all.
            let policy = self.read_policy(PolicyStore::load)?;
            stage(Stage::PolicyLoaded);
            let rows = read(&self.vault, stage)?;
            stage(Stage::VaultClosed);
            let set = build(rows, &policy.compiled, &self.id_key)?;
            stage(Stage::SetBuilt);
            // The same bytes as loaded, so the same valid policy: no need to compile it again.
            if self.read_policy(PolicyStore::current_etag)? == policy.etag {
                return query(&set);
            }
        }
        Err(VisibleError::PolicyUnavailable)
    }

    /// Uninitialized, damaged, unsupported and locked policies all mean that no decision exists.
    fn read_policy<T>(
        &self,
        read: fn(&PolicyStore) -> Result<T, StoreError>,
    ) -> Result<T, VisibleError> {
        let mut attempt = 0;
        loop {
            match read(&self.policy) {
                Ok(snapshot) => return Ok(snapshot),
                Err(StoreError::Busy) if attempt < POLICY_BUSY_RETRIES => {
                    attempt += 1;
                    std::thread::sleep(POLICY_BUSY_WAIT);
                }
                Err(_) => return Err(VisibleError::PolicyUnavailable),
            }
        }
    }
}

/// The checked LIKE pattern, half-open range and limit of a search.
fn search_terms(
    query: &str,
    from: Option<&str>,
    to: Option<&str>,
    limit: Option<u32>,
) -> Result<(String, (i64, i64), u32), VisibleError> {
    let query = query.trim();
    if query.chars().count() > MAX_QUERY_CHARS {
        return Err(VisibleError::InvalidArgument);
    }
    let bound = |text: Option<&str>, default: i64| match text {
        None => Ok(default),
        Some(text) => time::parse_timestamp(text).ok_or(VisibleError::InvalidArgument),
    };
    let range = (bound(from, START_OF_TIME)?, bound(to, END_OF_TIME)?);
    if range.0 > range.1 {
        return Err(VisibleError::InvalidArgument);
    }
    let limit = checked_limit(limit)?;
    Ok((visible_set::like_pattern(query), range, limit))
}

fn checked_limit(limit: Option<u32>) -> Result<u32, VisibleError> {
    match limit {
        None => Ok(DEFAULT_LIMIT),
        Some(limit) if (1..=MAX_LIMIT).contains(&limit) => Ok(limit),
        Some(_) => Err(VisibleError::InvalidArgument),
    }
}
