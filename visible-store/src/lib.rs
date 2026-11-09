//! VisibleStore: the only read path from a vault to MCP (Issue #12: visits and image metadata;
//! Issue #8: page metadata, domain counts and paging).
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

use memocampus_native_host::active_vault::{self, Admitted, GateError, VaultMode};

use memocampus_native_host::policy::CompiledPolicy;
use memocampus_native_host::policy_store::{PolicyStore, StoreError};
use std::path::PathBuf;
use std::time::Duration;
use visible_images::VisibleImages;
use visible_set::{public_id, IdKey, Position, VisibleSet};

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
    /// A cursor this store did not issue, or not in the form it issues.
    InvalidCursor,
    // --- Publication gate (Issue #51, B0): checked before anything is read. ---
    /// No active vault is configured.
    VaultNotConfigured,
    /// `config.json` is damaged or of an unknown version.
    ConfigInvalid,
    /// A fixed-path store whose file is not the active vault.
    VaultNotActive,
    /// The active vault is not (or no longer) approved for MCP.
    VaultPublicationPending,
    /// The active vault or its approval changed during the read; nothing of it is returned.
    VaultSwitched,
    // --- Vault mode (B0-4). ---
    /// The file contradicts the mode the config records (plain SQLite in encrypted mode, or no
    /// SQLite header in plain mode). Never taken for damage, never resolved by switching modes.
    VaultModeMismatch,
    /// The active vault is encrypted, which this version cannot read.
    VaultEncryptionUnsupported,
    /// Reserved for encrypted vaults (Slice S2): the key is not available to this process.
    VaultLocked,
    /// Reserved for encrypted vaults (Slice S2): the key was refused.
    VaultDecryptFailed,
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
            Self::InvalidCursor => "invalid_cursor",
            Self::VaultNotConfigured => "vault_not_configured",
            Self::ConfigInvalid => "config_invalid",
            Self::VaultNotActive => "vault_not_active",
            Self::VaultPublicationPending => "vault_publication_pending",
            Self::VaultSwitched => "vault_switched",
            Self::VaultModeMismatch => "vault_mode_mismatch",
            Self::VaultEncryptionUnsupported => "vault_encryption_unsupported",
            Self::VaultLocked => "vault_locked",
            Self::VaultDecryptFailed => "vault_decrypt_failed",
        }
    }
}

impl From<GateError> for VisibleError {
    fn from(error: GateError) -> Self {
        match error {
            GateError::NotConfigured => Self::VaultNotConfigured,
            GateError::ConfigInvalid => Self::ConfigInvalid,
            GateError::Unavailable | GateError::VaultMissing => Self::VaultUnavailable,
            GateError::NotActive => Self::VaultNotActive,
            GateError::PublicationPending => Self::VaultPublicationPending,
            GateError::Switched => Self::VaultSwitched,
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
    /// The latest metadata stored for this URL (Issue #22), not necessarily read at this visit.
    pub page: Option<VisiblePage>,
}

/// Page metadata of a public URL. Text is page-supplied, published as stored, like titles (D7).
/// The two URLs are given only when they are public themselves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisiblePage {
    pub title: Option<String>,
    pub description: Option<String>,
    pub og_title: Option<String>,
    pub og_description: Option<String>,
    pub og_site_name: Option<String>,
    pub og_image_url: Option<String>,
    pub favicon_url: Option<String>,
    pub captured_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisitPage {
    pub items: Vec<VisibleVisit>,
    /// Matching public rows, before the cursor and the limit.
    pub total: u64,
    /// More public rows follow the last item; `VisibleStore::cursor_after` of it continues.
    pub has_more: bool,
}

/// `from` is inclusive and `to` exclusive, both as `Date#toISOString()` text. `query` is looked
/// for in the URL, the title and the metadata text. `cursor` continues after the visit it was
/// issued for.
#[derive(Debug, Clone, Default)]
pub struct SearchVisits<'a> {
    pub query: &'a str,
    pub from: Option<&'a str>,
    pub to: Option<&'a str>,
    pub limit: Option<u32>,
    pub cursor: Option<&'a str>,
}

/// `date` is `YYYY-MM-DD` in the zone given by `utc_offset`, a fixed offset such as `+09:00`.
#[derive(Debug, Clone, Default)]
pub struct VisitsByDay<'a> {
    pub date: &'a str,
    pub utc_offset: &'a str,
    pub limit: Option<u32>,
    pub cursor: Option<&'a str>,
}

/// Public visits per host in a half-open range, as for `SearchVisits`.
#[derive(Debug, Clone, Default)]
pub struct DomainStats<'a> {
    pub from: Option<&'a str>,
    pub to: Option<&'a str>,
    pub limit: Option<u32>,
}

/// `domain` is the URL's host as the policy judges it: lower case, no trailing dot, no port.
/// Subdomains are counted apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainCount {
    pub domain: String,
    pub visit_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainStatsPage {
    pub items: Vec<DomainCount>,
    /// Public visits in the range, of all domains.
    pub total_visits: u64,
    /// Domains with a public visit in the range, before the limit.
    pub total_domains: u64,
    pub has_more: bool,
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
    /// The publication gate admitted a vault; nothing has been opened yet.
    Admitted,
    PolicyLoaded,
    SchemaChecked,
    VaultClosed,
    SetBuilt,
}

/// The vault one read opens: path and mode as the gate admitted them, and the file identity the
/// open file must have (None only for this crate's ungated test stores).
pub(crate) struct Target {
    pub path: PathBuf,
    pub mode: VaultMode,
    pub token: Option<String>,
}

type ReadRows<Row> = fn(&Target, &mut dyn FnMut(Stage)) -> Result<Vec<Row>, VisibleError>;

/// Which vault a store reads.
enum Source {
    /// The active vault `config.json` names now, resolved for every read (the default for MCP).
    Active,
    /// A fixed path, accepted only while it is the active vault file itself.
    Pinned(PathBuf),
    /// This crate's tests only: a vault read without the publication gate.
    #[cfg(test)]
    Ungated(PathBuf, VaultMode),
}

pub struct VisibleStore {
    source: Source,
    /// The global application directory: `config.json` and the policy, independent of the vault.
    app_dir: PathBuf,
    policy: PolicyStore,
    id_key: IdKey,
}

impl VisibleStore {
    /// Reads whatever vault `app_dir/config.json` names as active and publishes (Issue #51 I1, I2):
    /// every read passes the publication gate again, so a switch, a pending vault or a replaced
    /// file is never read. `app_dir` also holds the global `policy.json`.
    pub fn follow_active(app_dir: PathBuf) -> Result<Self, VisibleError> {
        let policy =
            PolicyStore::new(app_dir.clone()).map_err(|_| VisibleError::PolicyUnavailable)?;
        Ok(Self::with_policy(Source::Active, app_dir, policy))
    }

    /// Reads only `vault`, and only while it is the active vault with a confirmed publication
    /// (`vault_not_active` otherwise). For diagnostics and clients started with a path.
    pub fn pinned(vault: PathBuf, app_dir: PathBuf) -> Result<Self, VisibleError> {
        let policy =
            PolicyStore::new(app_dir.clone()).map_err(|_| VisibleError::PolicyUnavailable)?;
        Ok(Self::with_policy(Source::Pinned(vault), app_dir, policy))
    }

    /// The application directory `%LOCALAPPDATA%\Memocampus` (or `MEMOCAMPUS_APP_DIR`): pinned
    /// to `vault` if given, else following the active vault.
    pub fn from_process(vault: Option<PathBuf>) -> Result<Self, VisibleError> {
        let policy = PolicyStore::from_process().map_err(|_| VisibleError::PolicyUnavailable)?;
        let app_dir = policy.app_dir().to_path_buf();
        let source = match vault {
            Some(vault) => Source::Pinned(vault),
            None => Source::Active,
        };
        Ok(Self::with_policy(source, app_dir, policy))
    }

    /// This crate's tests: `vault` without the publication gate, as a plain vault.
    #[cfg(test)]
    pub(crate) fn ungated(vault: PathBuf, app_dir: PathBuf) -> Result<Self, VisibleError> {
        Self::ungated_in(vault, app_dir, VaultMode::Plain)
    }

    #[cfg(test)]
    pub(crate) fn ungated_in(
        vault: PathBuf,
        app_dir: PathBuf,
        mode: VaultMode,
    ) -> Result<Self, VisibleError> {
        let policy =
            PolicyStore::new(app_dir.clone()).map_err(|_| VisibleError::PolicyUnavailable)?;
        Ok(Self::with_policy(
            Source::Ungated(vault, mode),
            app_dir,
            policy,
        ))
    }

    fn with_policy(source: Source, app_dir: PathBuf, policy: PolicyStore) -> Self {
        let mut id_key = [0u8; 32];
        id_key[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        id_key[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        Self {
            source,
            app_dir,
            policy,
            id_key,
        }
    }

    fn pinned_path(&self) -> Option<&std::path::Path> {
        match &self.source {
            Source::Pinned(path) => Some(path),
            _ => None,
        }
    }

    /// The publication gate: the vault to open now, and what admitted it.
    fn admit(&self) -> Result<(Target, Option<Admitted>), VisibleError> {
        #[cfg(test)]
        if let Source::Ungated(path, mode) = &self.source {
            let target = Target {
                path: path.clone(),
                mode: *mode,
                token: None,
            };
            return Ok((target, None));
        }
        let admitted = active_vault::admit(&self.app_dir, self.pinned_path())?;
        let target = Target {
            path: admitted.path.clone(),
            mode: admitted.mode,
            token: Some(admitted.token.clone()),
        };
        Ok((target, Some(admitted)))
    }

    /// After a read: the gate still admits the very same vault, else the read is discarded.
    fn still_admitted(&self, admitted: &Option<Admitted>) -> Result<(), VisibleError> {
        match admitted {
            Some(admitted) => {
                active_vault::still_admitted(&self.app_dir, self.pinned_path(), admitted)
                    .map_err(VisibleError::from)
            }
            None => Ok(()),
        }
    }

    pub fn search_visits(&self, request: &SearchVisits) -> Result<VisitPage, VisibleError> {
        self.search_visits_staged(request, &mut |_| {})
    }

    /// The vault passes every check a read makes (publication, location, mode, schema), and
    /// nothing is read. The policy is not needed for this.
    pub fn check_vault(&self) -> Result<(), VisibleError> {
        self.check_vault_staged(&mut |_| {})
    }

    fn check_vault_staged(&self, stage: &mut dyn FnMut(Stage)) -> Result<(), VisibleError> {
        let (target, admitted) = self.admit()?;
        stage(Stage::Admitted);
        snapshot::check_vault(&target, stage)?;
        self.still_admitted(&admitted)
    }

    pub fn domain_stats(&self, request: &DomainStats) -> Result<DomainStatsPage, VisibleError> {
        let (_, range, limit) = search_terms("", request.from, request.to, request.limit)?;
        self.read_visible(
            &mut |_| {},
            snapshot::read_visits,
            VisibleSet::build,
            |set: &VisibleSet| set.domains(range, limit),
        )
    }

    /// The cursor that continues a page after `visit`, a visit this store returned. It names a
    /// position only: the next read applies the policy of its own time, and a row that became
    /// private is simply not there. It is valid for this store only.
    pub fn cursor_after(&self, visit: &VisibleVisit) -> Result<String, VisibleError> {
        let ms = time::parse_timestamp(&visit.visited_at).ok_or(VisibleError::InvalidArgument)?;
        if !is_visit_id(&visit.id) {
            return Err(VisibleError::InvalidArgument);
        }
        Ok(format!(
            "{ms}.{}.{}",
            visit.id,
            self.cursor_mac(ms, &visit.id)
        ))
    }

    fn cursor_mac(&self, ms: i64, id: &str) -> String {
        let mac = public_id(&self.id_key, "cursor", &format!("{ms}\0{id}"));
        mac["cursor_".len()..].to_owned()
    }

    fn position(&self, cursor: Option<&str>) -> Result<Option<Position>, VisibleError> {
        let Some(cursor) = cursor else {
            return Ok(None);
        };
        let mut parts = cursor.split('.');
        let (Some(ms), Some(id), Some(mac), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(VisibleError::InvalidCursor);
        };
        let valid = !ms.is_empty()
            && ms.len() <= 15
            && ms.bytes().all(|byte| byte.is_ascii_digit())
            && is_visit_id(id);
        // Only the spelling `cursor_after` writes: no sign and no leading zero.
        let ms_text = ms;
        let ms: i64 = match ms_text.parse() {
            Ok(ms) if valid && format!("{ms}") == ms_text => ms,
            _ => return Err(VisibleError::InvalidCursor),
        };
        if self.cursor_mac(ms, id) != mac {
            return Err(VisibleError::InvalidCursor);
        }
        Ok(Some((ms, id.to_owned())))
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
        let after = self.position(request.cursor)?;
        self.read_visible(
            stage,
            snapshot::read_visits,
            VisibleSet::build,
            |set: &VisibleSet| set.query(&pattern, range, after.as_ref(), limit),
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
        let after = self.position(request.cursor)?;
        self.read_visible(
            stage,
            snapshot::read_visits,
            VisibleSet::build,
            |set: &VisibleSet| set.query("%", range, after.as_ref(), limit),
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
            // The publication gate comes first: an unpublished vault is not even opened.
            let (target, admitted) = self.admit()?;
            stage(Stage::Admitted);
            // Compiled once for this attempt, as it was loaded. A policy that does not compile
            // gives no decision at all.
            let policy = self.read_policy(PolicyStore::load)?;
            stage(Stage::PolicyLoaded);
            let rows = read(&target, stage)?;
            stage(Stage::VaultClosed);
            // Rows of a vault that stopped being the published active one during the read are
            // dropped unjudged.
            self.still_admitted(&admitted)?;
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

/// `visit_` and 32 lower-case hexadecimal digits, as `public_id` writes them.
fn is_visit_id(id: &str) -> bool {
    id.strip_prefix("visit_").is_some_and(|hex| {
        hex.len() == 32
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn checked_limit(limit: Option<u32>) -> Result<u32, VisibleError> {
    match limit {
        None => Ok(DEFAULT_LIMIT),
        Some(limit) if (1..=MAX_LIMIT).contains(&limit) => Ok(limit),
        Some(_) => Err(VisibleError::InvalidArgument),
    }
}
