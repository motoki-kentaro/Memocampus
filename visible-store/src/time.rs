//! Strict timestamps. The host stores `visited_at` as sent, so a value is trusted only in the exact
//! `Date#toISOString()` form, the one form whose text order and day boundaries are meaningful.

const MS_PER_MINUTE: i64 = 60_000;
const MS_PER_DAY: i64 = 86_400_000;
const MIN_OFFSET_MINUTES: i64 = -12 * 60;
const MAX_OFFSET_MINUTES: i64 = 14 * 60;
/// 9999-12-31T23:59:59.999Z
const LAST_FORMATTABLE_MS: i64 = 253_402_300_799_999;

fn digits(text: &[u8]) -> Option<i64> {
    text.iter().try_fold(0i64, |value, byte| {
        byte.is_ascii_digit()
            .then(|| value * 10 + i64::from(byte - b'0'))
    })
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 (Howard Hinnant's days-from-civil).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `YYYY-MM-DD`, as days since the epoch.
fn parse_date(text: &[u8]) -> Option<i64> {
    if text.len() != 10 || text[4] != b'-' || text[7] != b'-' {
        return None;
    }
    let (year, month, day) = (
        digits(&text[..4])?,
        digits(&text[5..7])?,
        digits(&text[8..10])?,
    );
    ((1..=12).contains(&month) && (1..=days_in_month(year, month)).contains(&day))
        .then(|| days_from_civil(year, month, day))
}

/// Milliseconds since the epoch for exactly `YYYY-MM-DDTHH:MM:SS.mmmZ`.
pub(crate) fn parse_timestamp(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() != 24
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'.'
        || bytes[23] != b'Z'
    {
        return None;
    }
    let days = parse_date(&bytes[..10])?;
    let (hour, minute, second, ms) = (
        digits(&bytes[11..13])?,
        digits(&bytes[14..16])?,
        digits(&bytes[17..19])?,
        digits(&bytes[20..23])?,
    );
    (hour < 24 && minute < 60 && second < 60)
        .then(|| days * MS_PER_DAY + (hour * 60 + minute) * MS_PER_MINUTE + second * 1000 + ms)
}

/// The inverse of `days_from_civil`.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let doe = days - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, doy - (153 * mp + 2) / 5 + 1)
}

/// The `Date#toISOString()` text of a time in milliseconds; None outside years 1970 to 9999.
pub(crate) fn format_timestamp(ms: i64) -> Option<String> {
    if !(0..=LAST_FORMATTABLE_MS).contains(&ms) {
        return None;
    }
    let (year, month, day) = civil_from_days(ms / MS_PER_DAY);
    let rest = ms % MS_PER_DAY;
    let (hour, minute) = (rest / 3_600_000, rest / MS_PER_MINUTE % 60);
    let (second, milli) = (rest / 1000 % 60, rest % 1000);
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{milli:03}Z"
    ))
}

/// The half-open UTC range of one local date at a fixed offset such as `+09:00`.
pub(crate) fn day_range(date: &str, utc_offset: &str) -> Option<(i64, i64)> {
    let days = parse_date(date.as_bytes())?;
    let offset = utc_offset.as_bytes();
    if offset.len() != 6 || offset[3] != b':' {
        return None;
    }
    let (hours, minutes) = (digits(&offset[1..3])?, digits(&offset[4..6])?);
    let magnitude = (minutes < 60).then_some(hours * 60 + minutes)?;
    let signed = match offset[0] {
        b'+' => magnitude,
        b'-' => -magnitude,
        _ => return None,
    };
    if !(MIN_OFFSET_MINUTES..=MAX_OFFSET_MINUTES).contains(&signed) {
        return None;
    }
    let start = days * MS_PER_DAY - signed * MS_PER_MINUTE;
    Some((start, start + MS_PER_DAY))
}
