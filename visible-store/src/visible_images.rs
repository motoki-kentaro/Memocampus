//! The visible image set: a private `:memory:` database of public saves. A save is judged alone, and
//! every count, date and source is computed from this database, so nothing recorded about the whole
//! image in the vault, and no private save, can reach an answer.

use crate::snapshot::RawImageSave;
use crate::visible_set::{failed, public_id, IdKey};
use crate::{time, ImagePage, ImageSourcePage, VisibleError, VisibleImage, VisibleImageSource};
use memocampus_native_host::policy::{Decision, Policy};
use memocampus_native_host::schema::is_data_image_marker;
use rusqlite::limits::Limit;
use rusqlite::{params, Connection, Row};

/// The formats the host stores (`ImageFormat`). Any other stored type is not described to anyone.
const MIME_TYPES: [&str; 6] = [
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "image/avif",
    "image/bmp",
];
/// As for a save (`images::MAX_URL_CHARS`, `MAX_IMAGE_EDGE`).
const MAX_URL_CHARS: usize = 8192;
const MAX_EDGE: i64 = 65_535;
const SHA256_HEX_LEN: usize = 64;

const SCHEMA: &str = "CREATE TABLE visible_images (
    public_id TEXT PRIMARY KEY,
    mime_type TEXT NOT NULL,
    width INTEGER NOT NULL,
    height INTEGER NOT NULL,
    public_save_count INTEGER,
    first_saved_ms INTEGER,
    last_saved_ms INTEGER
) WITHOUT ROWID;
CREATE TABLE visible_image_sources (
    public_id TEXT PRIMARY KEY,
    image_id TEXT NOT NULL,
    image_url TEXT NOT NULL,
    page_url TEXT,
    frame_url TEXT,
    saved_ms INTEGER NOT NULL
) WITHOUT ROWID;
CREATE INDEX visible_image_sources_by_image ON visible_image_sources (image_id, saved_ms DESC, public_id);";
const RECOUNT: &str = "UPDATE visible_images SET (public_save_count, first_saved_ms, last_saved_ms) =
    (SELECT count(*), min(saved_ms), max(saved_ms) FROM visible_image_sources WHERE image_id = visible_images.public_id)";
const MATCHES: &str = "EXISTS (SELECT 1 FROM visible_image_sources s WHERE s.image_id = i.public_id
    AND (s.image_url LIKE ?1 ESCAPE '\\' OR s.page_url LIKE ?1 ESCAPE '\\' OR s.frame_url LIKE ?1 ESCAPE '\\')
    AND s.saved_ms >= ?2 AND s.saved_ms < ?3)";

pub(crate) struct VisibleImages {
    pub(crate) db: Connection,
}

/// `Decision::None` is the only public outcome, and an unusable URL is `NeverSave`. Only an absent
/// source and the recorded marker of a `data:` image have nothing to judge, and at least one source
/// must have been judged.
fn sources_are_public(save: &RawImageSave, policy: &Policy) -> bool {
    let image_url = (!is_data_image_marker(&save.image_url)).then_some(save.image_url.as_str());
    let judged: Vec<&str> = [
        image_url,
        save.page_url.as_deref(),
        save.frame_url.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect();
    !judged.is_empty()
        && judged
            .iter()
            .all(|url| policy.evaluate(url) == Decision::None)
}

/// Stored values that cannot be described truthfully are not repaired: the save stays private.
fn metadata_is_usable(save: &RawImageSave) -> bool {
    let urls = [
        Some(save.image_url.as_str()),
        save.page_url.as_deref(),
        save.frame_url.as_deref(),
    ];
    MIME_TYPES.contains(&save.mime_type.as_str())
        && (1..=MAX_EDGE).contains(&save.width)
        && (1..=MAX_EDGE).contains(&save.height)
        && save.sha256.len() == SHA256_HEX_LEN
        && save.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        && !save.save_id.is_empty()
        && time::format_timestamp(save.saved_at).is_some()
        && urls
            .into_iter()
            .flatten()
            .all(|url| url.chars().count() <= MAX_URL_CHARS)
}

/// One `saved_images` row, judged alone. An incognito row is never public, whatever its flag says.
pub(crate) fn save_is_public(save: &RawImageSave, policy: &Policy) -> bool {
    save.mcp_public == 1
        && save.incognito == 0
        && metadata_is_usable(save)
        && sources_are_public(save, policy)
}

fn saved_at(ms: i64) -> rusqlite::Result<String> {
    time::format_timestamp(ms).ok_or(rusqlite::Error::IntegralValueOutOfRange(0, ms))
}

fn image_of(row: &Row) -> rusqlite::Result<VisibleImage> {
    Ok(VisibleImage {
        id: row.get(0)?,
        mime_type: row.get(1)?,
        width: row.get(2)?,
        height: row.get(3)?,
        public_save_count: row.get(4)?,
        first_saved_at: saved_at(row.get(5)?)?,
        last_saved_at: saved_at(row.get(6)?)?,
        latest_image_url: row.get(7)?,
        latest_page_url: row.get(8)?,
    })
}

fn source_of(row: &Row) -> rusqlite::Result<VisibleImageSource> {
    Ok(VisibleImageSource {
        id: row.get(0)?,
        image_url: row.get(1)?,
        page_url: row.get(2)?,
        frame_url: row.get(3)?,
        saved_at: saved_at(row.get(4)?)?,
    })
}

impl VisibleImages {
    /// A set that fails to build is dropped whole; the caller only ever sees a complete one.
    pub(crate) fn build(
        rows: Vec<RawImageSave>,
        policy: &Policy,
        key: &IdKey,
    ) -> Result<Self, VisibleError> {
        let db = Connection::open_in_memory().map_err(failed)?;
        db.set_limit(Limit::SQLITE_LIMIT_ATTACHED, 0);
        db.execute_batch(SCHEMA).map_err(failed)?;
        db.execute_batch("BEGIN").map_err(failed)?;
        {
            let mut image = db
                .prepare(
                    "INSERT OR IGNORE INTO visible_images (public_id, mime_type, width, height)
                     VALUES (?1, ?2, ?3, ?4)",
                )
                .map_err(failed)?;
            let mut source = db
                .prepare("INSERT INTO visible_image_sources VALUES (?1, ?2, ?3, ?4, ?5, ?6)")
                .map_err(failed)?;
            for save in rows.iter().filter(|save| save_is_public(save, policy)) {
                let image_id = public_id(key, "image", &save.sha256);
                image
                    .execute(params![image_id, save.mime_type, save.width, save.height])
                    .map_err(failed)?;
                source
                    .execute(params![
                        public_id(key, "source", &save.save_id),
                        image_id,
                        save.image_url,
                        save.page_url,
                        save.frame_url,
                        save.saved_at
                    ])
                    .map_err(failed)?;
            }
        }
        db.execute_batch(RECOUNT).map_err(failed)?;
        db.execute_batch("COMMIT; PRAGMA query_only = ON;")
            .map_err(failed)?;
        Ok(Self { db })
    }

    /// Images are ordered by their newest public save; `total` is counted before the limit.
    pub(crate) fn search(
        &self,
        pattern: &str,
        range: (i64, i64),
        limit: u32,
    ) -> Result<ImagePage, VisibleError> {
        let total: i64 = self
            .db
            .query_row(
                &format!("SELECT count(*) FROM visible_images i WHERE {MATCHES}"),
                params![pattern, range.0, range.1],
                |row| row.get(0),
            )
            .map_err(failed)?;
        let newest = |column: &str| {
            format!(
                "(SELECT {column} FROM visible_image_sources n WHERE n.image_id = i.public_id
                  ORDER BY n.saved_ms DESC, n.public_id LIMIT 1)"
            )
        };
        let mut statement = self
            .db
            .prepare(&format!(
                "SELECT i.public_id, i.mime_type, i.width, i.height, i.public_save_count,
                        i.first_saved_ms, i.last_saved_ms, {}, {}
                 FROM visible_images i WHERE {MATCHES}
                 ORDER BY i.last_saved_ms DESC, i.public_id LIMIT ?4",
                newest("n.image_url"),
                newest("n.page_url"),
            ))
            .map_err(failed)?;
        let items = statement
            .query_map(params![pattern, range.0, range.1, limit], image_of)
            .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
            .map_err(failed)?;
        let total = u64::try_from(total).map_err(failed)?;
        Ok(ImagePage {
            has_more: total > items.len() as u64,
            items,
            total,
        })
    }

    /// An image without a public save is simply not here, so it cannot differ from an unknown ID.
    pub(crate) fn sources(
        &self,
        image_id: &str,
        limit: u32,
    ) -> Result<ImageSourcePage, VisibleError> {
        let total: i64 = self
            .db
            .query_row(
                "SELECT count(*) FROM visible_image_sources WHERE image_id = ?1",
                [image_id],
                |row| row.get(0),
            )
            .map_err(failed)?;
        if total == 0 {
            return Err(VisibleError::NotFound);
        }
        let mut statement = self
            .db
            .prepare(
                "SELECT public_id, image_url, page_url, frame_url, saved_ms
                 FROM visible_image_sources WHERE image_id = ?1
                 ORDER BY saved_ms DESC, public_id LIMIT ?2",
            )
            .map_err(failed)?;
        let items = statement
            .query_map(params![image_id, limit], source_of)
            .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
            .map_err(failed)?;
        let total = u64::try_from(total).map_err(failed)?;
        Ok(ImageSourcePage {
            has_more: total > items.len() as u64,
            items,
            total,
        })
    }
}
