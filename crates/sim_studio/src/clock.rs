//! Wall-clock helpers for the Launcher's "2h ago" and the heartbeat's staleness (never used by a
//! run: the kernel has no wall clock).

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since 1970-01-01 UTC.
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Days since 1970-01-01 of a civil date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The civil date of days since 1970-01-01.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

/// `secs` as RFC 3339 UTC, seconds precision (`2026-11-03T10:01:00Z`).
pub fn utc(secs: u64) -> String {
    let (y, m, d) = civil_from_days((secs / 86_400) as i64);
    let rem = secs % 86_400;
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Parse the RFC 3339 UTC times SABER writes (`YYYY-MM-DDTHH:MM:SS`, optional fraction, `Z`).
pub fn parse_utc(s: &str) -> Option<u64> {
    let s = s.strip_suffix('Z').or_else(|| s.strip_suffix("+00:00"))?;
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-').map(|p| p.parse::<i64>().ok());
    let (y, mo, da) = (d.next()??, d.next()??, d.next()??);
    let time = time.split('.').next()?;
    let mut t = time.split(':').map(|p| p.parse::<i64>().ok());
    let (h, mi, se) = (t.next()??, t.next()??, t.next()??);
    if d.next().is_some()
        || t.next().is_some()
        || !(1..=12).contains(&mo)
        || !(1..=31).contains(&da)
        || !(0..24).contains(&h)
        || !(0..60).contains(&mi)
        || !(0..=60).contains(&se)
    {
        return None;
    }
    let secs = days_from_civil(y, mo, da) * 86_400 + h * 3600 + mi * 60 + se;
    u64::try_from(secs).ok()
}

/// How long ago `then` was, as the Launcher lists it: "just now", "5 min ago", "2h ago",
/// "yesterday", "3 days ago", then the date.
pub fn ago(now: u64, then: u64) -> String {
    let dt = now.saturating_sub(then);
    match dt {
        0..60 => "just now".to_owned(),
        60..3600 => format!("{} min ago", dt / 60),
        3600..86_400 => format!("{}h ago", dt / 3600),
        86_400..172_800 => "yesterday".to_owned(),
        172_800..2_592_000 => format!("{} days ago", dt / 86_400),
        _ => utc(then)[..10].to_owned(),
    }
}
