//! The thumbnail of a public image: the bytes `image_thumbnails` holds, released only for an image
//! the visible set contains, and only if they are what the host stores (Issue #61, Slice 1).
//!
//! The bytes were read in the same committed state as the rows the set was built from
//! (`snapshot::read_image_saves_with_thumbnail`), by a join on the SHA-256 of the image the ID names.
//! Nothing here opens the vault, decodes an image or makes one. The checks restate the host's own
//! (`images::validate_thumbnail`, `thumbnail`): the host's code belongs to its binary and to its
//! writing session, and this crate reads only.

use crate::snapshot::{ImageSavesWithThumbnail, RawThumbnail, MAX_THUMBNAIL_BYTES};
use crate::visible_images::VisibleImages;
use crate::visible_set::IdKey;
use crate::{VisibleError, VisibleThumbnail};
use memocampus_native_host::policy::CompiledPolicy;

/// As `images::MAX_THUMBNAIL_EDGE`.
const MAX_EDGE: i64 = 1024;

pub(crate) struct ThumbnailSet {
    images: VisibleImages,
    wanted_sha256: Option<String>,
    thumbnail: Option<RawThumbnail>,
}

impl ThumbnailSet {
    /// The public images of the snapshot, and the one thumbnail read with it (not yet released).
    pub(crate) fn build(
        snapshot: ImageSavesWithThumbnail,
        policy: &CompiledPolicy,
        key: &IdKey,
    ) -> Result<Self, VisibleError> {
        Ok(Self {
            images: VisibleImages::build(snapshot.saves, policy, key)?,
            wanted_sha256: snapshot.wanted_sha256,
            thumbnail: snapshot.thumbnail,
        })
    }

    /// `NotFound` for an image that has no public save, whatever the reason: unknown, never
    /// stored, private, or no longer public. Only then are the bytes looked at.
    pub(crate) fn release(&self, image_id: &str) -> Result<VisibleThumbnail, VisibleError> {
        if !self.images.contains(image_id)? {
            return Err(VisibleError::NotFound);
        }
        let raw = self
            .thumbnail
            .as_ref()
            .ok_or(VisibleError::ThumbnailMissing)?;
        check(raw, self.wanted_sha256.as_deref()).ok_or(VisibleError::ThumbnailInvalid)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Png,
    Jpeg,
    Webp,
}

impl Format {
    fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Webp => "image/webp",
        }
    }
}

fn u16_le(bytes: &[u8], at: usize) -> Option<i64> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?).into())
}
fn u16_be(bytes: &[u8], at: usize) -> Option<i64> {
    Some(u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?).into())
}
fn u32_le(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}
fn u32_be(bytes: &[u8], at: usize) -> Option<i64> {
    Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?).into())
}
fn u24_le(bytes: &[u8], at: usize) -> Option<i64> {
    let b = bytes.get(at..at + 3)?;
    Some(i64::from(b[0]) | i64::from(b[1]) << 8 | i64::from(b[2]) << 16)
}

/// From the magic bytes only; a thumbnail is PNG, JPEG or WebP and nothing else.
fn sniff(bytes: &[u8]) -> Option<Format> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(Format::Png)
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(Format::Jpeg)
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some(Format::Webp)
    } else {
        None
    }
}

/// The size the file's own header states.
fn header_dimensions(format: Format, bytes: &[u8]) -> Option<(i64, i64)> {
    match format {
        Format::Png if bytes.get(12..16) == Some(b"IHDR") => {
            Some((u32_be(bytes, 16)?, u32_be(bytes, 20)?))
        }
        Format::Png => None,
        Format::Webp => match bytes.get(12..16)? {
            b"VP8 " if bytes.get(23..26) == Some(&[0x9d, 0x01, 0x2a]) => {
                Some((u16_le(bytes, 26)? & 0x3fff, u16_le(bytes, 28)? & 0x3fff))
            }
            b"VP8L" if bytes.get(20) == Some(&0x2f) => {
                let bits = u32_le(bytes, 21)?;
                Some((
                    i64::from(bits & 0x3fff) + 1,
                    i64::from((bits >> 14) & 0x3fff) + 1,
                ))
            }
            b"VP8X" => Some((u24_le(bytes, 24)? + 1, u24_le(bytes, 27)? + 1)),
            _ => None,
        },
        Format::Jpeg => {
            let mut at = 2;
            while at + 9 < bytes.len() {
                if bytes[at] != 0xFF {
                    return None;
                }
                let marker = bytes[at + 1];
                if marker == 0xFF {
                    at += 1;
                    continue;
                }
                if matches!(marker, 0xD0..=0xD9 | 0x01) {
                    at += 2;
                    continue;
                }
                let is_sof = matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
                if is_sof {
                    return Some((u16_be(bytes, at + 7)?, u16_be(bytes, at + 5)?));
                }
                at += 2 + usize::try_from(u16_be(bytes, at + 2)?).ok()?;
            }
            None
        }
    }
}

/// The thumbnail as the host stores it, or None. Each condition is one the host enforces when it
/// writes one; a row that breaks any of them was not written by the host and is not repaired.
fn check(raw: &RawThumbnail, wanted_sha256: Option<&str>) -> Option<VisibleThumbnail> {
    // It belongs to the very image the ID named (the join's own answer, checked again).
    if raw.sha256.as_deref()? != wanted_sha256? || !raw.data_is_blob {
        return None;
    }
    let data = raw.data.as_ref()?;
    if data.is_empty()
        || data.len() > MAX_THUMBNAIL_BYTES
        || usize::try_from(raw.data_len?).ok()? != data.len()
        || raw.generator? <= 0
    {
        return None;
    }
    let format = sniff(data)?;
    let (width, height) = (raw.width?, raw.height?);
    if raw.mime_type.as_deref()? != format.mime()
        || !(1..=MAX_EDGE).contains(&width)
        || !(1..=MAX_EDGE).contains(&height)
        || header_dimensions(format, data)? != (width, height)
    {
        return None;
    }
    Some(VisibleThumbnail {
        mime_type: format.mime().to_owned(),
        width: u32::try_from(width).ok()?,
        height: u32::try_from(height).ok()?,
        data: data.clone(),
    })
}
