//! Dates as a person gives them: `YYYY-MM-DD` in the zone of the computer the server runs on, or an
//! exact UTC instant. VisibleStore takes only exact `Date#toISOString()` text and fixed offsets, so
//! this module turns the first into those.

const MS_PER_MINUTE: i64 = 60_000;
const MS_PER_DAY: i64 = 86_400_000;
const MIN_OFFSET_MINUTES: i64 = -12 * 60;
const MAX_OFFSET_MINUTES: i64 = 14 * 60;

/// A calendar date from 1970 to 9999.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Date {
    pub year: i64,
    pub month: i64,
    pub day: i64,
}

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

impl Date {
    /// Exactly `YYYY-MM-DD`.
    pub fn parse(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
            return None;
        }
        let (year, month, day) = (
            digits(&bytes[..4])?,
            digits(&bytes[5..7])?,
            digits(&bytes[8..10])?,
        );
        ((1970..=9999).contains(&year)
            && (1..=12).contains(&month)
            && (1..=days_in_month(year, month)).contains(&day))
        .then_some(Self { year, month, day })
    }

    /// Days since 1970-01-01 (Howard Hinnant's days-from-civil).
    fn days(self) -> i64 {
        let year = if self.month <= 2 {
            self.year - 1
        } else {
            self.year
        };
        let era = year.div_euclid(400);
        let yoe = year - era * 400;
        let doy = (153 * (self.month + if self.month > 2 { -3 } else { 9 }) + 2) / 5 + self.day - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    fn from_days(days: i64) -> Self {
        let days = days + 719_468;
        let era = days.div_euclid(146_097);
        let doe = days - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        Self {
            year: yoe + era * 400 + i64::from(month <= 2),
            month,
            day: doy - (153 * mp + 2) / 5 + 1,
        }
    }

    pub fn next(self) -> Self {
        Self::from_days(self.days() + 1)
    }

    pub fn text(self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    /// The UTC instant at which this date begins at `offset_minutes` east of UTC.
    pub fn start_ms(self, offset_minutes: i64) -> i64 {
        self.days() * MS_PER_DAY - offset_minutes * MS_PER_MINUTE
    }
}

/// `+09:00` or `-05:00`, from `-12:00` to `+14:00`, in minutes east of UTC.
pub fn parse_offset(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() != 6 || bytes[3] != b':' {
        return None;
    }
    let (hours, minutes) = (digits(&bytes[1..3])?, digits(&bytes[4..6])?);
    if minutes >= 60 {
        return None;
    }
    let signed = match bytes[0] {
        b'+' => hours * 60 + minutes,
        b'-' => -(hours * 60 + minutes),
        _ => return None,
    };
    (MIN_OFFSET_MINUTES..=MAX_OFFSET_MINUTES)
        .contains(&signed)
        .then_some(signed)
}

pub fn format_offset(minutes: i64) -> String {
    let sign = if minutes < 0 { '-' } else { '+' };
    let minutes = minutes.abs();
    format!("{sign}{:02}:{:02}", minutes / 60, minutes % 60)
}

/// `Date#toISOString()` text of an instant from 1970 to 9999, or None.
pub fn format_ms(ms: i64) -> Option<String> {
    if !(0..=253_402_300_799_999).contains(&ms) {
        return None;
    }
    let date = Date::from_days(ms / MS_PER_DAY);
    let rest = ms % MS_PER_DAY;
    Some(format!(
        "{}T{:02}:{:02}:{:02}.{:03}Z",
        date.text(),
        rest / 3_600_000,
        rest / MS_PER_MINUTE % 60,
        rest / 1000 % 60,
        rest % 1000
    ))
}

/// The local offset in effect at the start of `date` on this computer, from its time zone
/// settings (daylight saving time included). On a day when the offset changes, the end of the day
/// is computed with the offset at its start.
#[cfg(windows)]
pub fn local_offset_minutes(date: Date) -> Option<i64> {
    use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows_sys::Win32::System::Time::{SystemTimeToFileTime, TzSpecificLocalTimeToSystemTime};
    let local = SYSTEMTIME {
        wYear: u16::try_from(date.year).ok()?,
        wMonth: u16::try_from(date.month).ok()?,
        wDayOfWeek: 0,
        wDay: u16::try_from(date.day).ok()?,
        wHour: 0,
        wMinute: 0,
        wSecond: 0,
        wMilliseconds: 0,
    };
    let mut utc = local;
    // SAFETY: both pointers refer to live, properly aligned SYSTEMTIME values; a null zone means
    // the current time zone of this computer.
    if unsafe { TzSpecificLocalTimeToSystemTime(std::ptr::null(), &local, &mut utc) } == 0 {
        return None;
    }
    let ticks = |time: &SYSTEMTIME| -> Option<i64> {
        let mut file = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        // SAFETY: as above, for one SYSTEMTIME and one FILETIME.
        if unsafe { SystemTimeToFileTime(time, &mut file) } == 0 {
            return None;
        }
        i64::try_from((u64::from(file.dwHighDateTime) << 32) | u64::from(file.dwLowDateTime)).ok()
    };
    // FILETIME counts 100 ns intervals.
    let minutes = (ticks(&local)? - ticks(&utc)?) / 600_000_000;
    (MIN_OFFSET_MINUTES..=MAX_OFFSET_MINUTES)
        .contains(&minutes)
        .then_some(minutes)
}

/// Memocampus runs on Windows; elsewhere the day is a UTC day.
#[cfg(not(windows))]
pub fn local_offset_minutes(_: Date) -> Option<i64> {
    Some(0)
}

/// One bound of a range: an exact UTC instant as given, or a local date. A date as a lower bound
/// is the start of that day; as an upper bound, the start of the next day, so the date is included.
pub fn bound(text: &str, upper: bool) -> Option<String> {
    if text.len() == 24 {
        // VisibleStore checks the exact form.
        return Some(text.to_owned());
    }
    let date = Date::parse(text)?;
    let date = if upper { date.next() } else { date };
    format_ms(date.start_ms(local_offset_minutes(date)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_parse_exactly_and_step_over_months_years_and_leap_days() {
        let d = |text| Date::parse(text).unwrap();
        assert_eq!(d("2026-12-31").next(), d("2027-01-01"));
        assert_eq!(d("2028-02-28").next(), d("2028-02-29"));
        assert_eq!(d("2026-02-28").next(), d("2026-03-01"));
        assert_eq!(d("2026-11-05").text(), "2026-11-05");
        for bad in [
            "2026-02-29",
            "2026-13-01",
            "2026-00-10",
            "2026-1-05",
            "2026/11/05",
            "1969-12-31",
            "２０２６-11-05",
            "",
            "2026-11-05T",
            "+2026-11-0",
        ] {
            assert_eq!(Date::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_day_at_an_offset_is_the_half_open_utc_range_of_that_local_day() {
        let date = Date::parse("2026-11-05").unwrap();
        assert_eq!(
            format_ms(date.start_ms(9 * 60)).unwrap(),
            "2026-11-04T15:00:00.000Z"
        );
        assert_eq!(
            format_ms(date.next().start_ms(9 * 60)).unwrap(),
            "2026-11-05T15:00:00.000Z"
        );
        assert_eq!(
            format_ms(date.start_ms(-5 * 60)).unwrap(),
            "2026-11-05T05:00:00.000Z"
        );
        assert_eq!(format_ms(0).unwrap(), "1970-01-01T00:00:00.000Z");
        assert_eq!(format_ms(-1), None);
    }

    #[test]
    fn offsets_parse_within_the_range_of_real_zones() {
        assert_eq!(parse_offset("+09:00"), Some(540));
        assert_eq!(parse_offset("-05:30"), Some(-330));
        assert_eq!(parse_offset("+14:00"), Some(840));
        assert_eq!(parse_offset("-12:00"), Some(-720));
        for bad in [
            "+14:01",
            "-12:01",
            "09:00",
            "+9:00",
            "+09:60",
            "Asia/Tokyo",
            "",
            "+09:00 ",
        ] {
            assert_eq!(parse_offset(bad), None, "{bad}");
        }
        assert_eq!(format_offset(540), "+09:00");
        assert_eq!(format_offset(-330), "-05:30");
        assert_eq!(format_offset(0), "+00:00");
    }

    #[test]
    fn the_local_offset_is_the_one_this_computer_uses() {
        // Nothing is assumed about this computer's zone: the start of each day computed here is
        // converted back with the opposite Windows function, and must be local midnight.
        let date = Date::parse("2026-11-05").unwrap();
        let offset = local_offset_minutes(date).unwrap();
        assert!((-720..=840).contains(&offset));
        #[cfg(windows)]
        for text in [
            "2026-01-15",
            "2026-03-29",
            "2026-07-15",
            "2026-10-25",
            "2026-11-05",
        ] {
            use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
            use windows_sys::Win32::System::Time::{
                FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime,
            };
            let date = Date::parse(text).unwrap();
            let start = date.start_ms(local_offset_minutes(date).unwrap());
            // 100 ns intervals since 1601-01-01.
            let ticks = (start + 11_644_473_600_000) as u64 * 10_000;
            let file = FILETIME {
                dwLowDateTime: ticks as u32,
                dwHighDateTime: (ticks >> 32) as u32,
            };
            let mut utc: SYSTEMTIME = unsafe { std::mem::zeroed() };
            let mut local: SYSTEMTIME = unsafe { std::mem::zeroed() };
            assert_ne!(unsafe { FileTimeToSystemTime(&file, &mut utc) }, 0);
            assert_ne!(
                unsafe { SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) },
                0
            );
            assert_eq!(
                (
                    i64::from(local.wYear),
                    i64::from(local.wMonth),
                    i64::from(local.wDay),
                    local.wHour,
                    local.wMinute
                ),
                (date.year, date.month, date.day, 0, 0),
                "{text}"
            );
        }
        assert_eq!(
            offset % 15,
            0,
            "every zone in use is a multiple of 15 minutes"
        );
    }

    #[test]
    fn bounds_take_an_exact_instant_as_is_and_a_date_as_its_local_day() {
        assert_eq!(
            bound("2026-11-05T01:02:03.004Z", false).unwrap(),
            "2026-11-05T01:02:03.004Z"
        );
        let date = Date::parse("2026-11-05").unwrap();
        let start = date.start_ms(local_offset_minutes(date).unwrap());
        let end = date
            .next()
            .start_ms(local_offset_minutes(date.next()).unwrap());
        assert_eq!(bound("2026-11-05", false), format_ms(start));
        assert_eq!(bound("2026-11-05", true), format_ms(end));
        assert_eq!(bound("2026-02-30", false), None);
        assert_eq!(bound("yesterday", false), None);
    }
}
