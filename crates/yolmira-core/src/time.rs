//! RFC 3339 UTC timestamps without a date-time dependency.

use std::time::{SystemTime, UNIX_EPOCH};

const MS_PER_DAY: i64 = 86_400_000;

/// `YYYY-MM-DDTHH:MM:SS.mmmZ` for the given instant (UTC).
pub fn rfc3339_utc(t: SystemTime) -> String {
    let ms = match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_millis() as i64,
        Err(e) => -(e.duration().as_millis() as i64),
    };
    format_unix_ms(ms)
}

pub fn now_rfc3339() -> String {
    rfc3339_utc(SystemTime::now())
}

fn format_unix_ms(ms: i64) -> String {
    let days = ms.div_euclid(MS_PER_DAY);
    let rem = ms.rem_euclid(MS_PER_DAY);
    let (y, m, d) = civil_from_days(days);
    let (h, min, s, milli) = (
        rem / 3_600_000,
        rem / 60_000 % 60,
        rem / 1000 % 60,
        rem % 1000,
    );
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{min:02}:{s:02}.{milli:03}Z")
}

/// Days since 1970-01-01 → (year, month, day). Howard Hinnant's `civil_from_days`.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_known_instants() {
        assert_eq!(format_unix_ms(0), "1970-01-01T00:00:00.000Z");
        // 2000-02-29 (leap day) 12:34:56.789 UTC
        assert_eq!(format_unix_ms(951_827_696_789), "2000-02-29T12:34:56.789Z");
        // 2026-09-25 00:00:00 UTC
        assert_eq!(
            format_unix_ms(1_790_294_400_000),
            "2026-09-25T00:00:00.000Z"
        );
        assert_eq!(format_unix_ms(-1), "1969-12-31T23:59:59.999Z");
    }
}
