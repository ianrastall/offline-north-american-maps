//! UTC timestamps without a date-time dependency.
use std::time::{SystemTime, UNIX_EPOCH};

/// Current UTC time as RFC 3339 (seconds precision).
pub fn utc_now() -> String {
    format_unix(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()) as i64,
    )
}

/// Seconds since 1970 as RFC 3339 UTC.
pub fn format_unix(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Parses an HTTP IMF-fixdate (`Sun, 08 Nov 2026 04:12:34 GMT`) into a
/// tuple that orders chronologically: (year, month, day, hour, minute, second).
pub fn parse_http_date(text: &str) -> Option<(u16, u8, u8, u8, u8, u8)> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (_, rest) = text.trim().split_once(", ")?;
    let parts: Vec<&str> = rest.split(' ').collect();
    let [day, month, year, time, "GMT"] = parts[..] else {
        return None;
    };
    let month = MONTHS.iter().position(|m| *m == month)? as u8 + 1;
    let [h, m, s] = time
        .split(':')
        .map(|p| p.parse::<u8>().ok())
        .collect::<Option<Vec<_>>>()?[..]
    else {
        return None;
    };
    let (day, year) = (day.parse::<u8>().ok()?, year.parse::<u16>().ok()?);
    ((1..=31).contains(&day) && h < 24 && m < 60 && s < 61).then_some((year, month, day, h, m, s))
}

#[cfg(test)]
mod tests {
    use super::parse_http_date;

    #[test]
    fn parses_http_dates_in_order() {
        let nov8 = parse_http_date("Sun, 08 Nov 2026 04:12:34 GMT").unwrap();
        let nov1 = parse_http_date("Sun, 01 Nov 2026 04:09:47 GMT").unwrap();
        let dec31 = parse_http_date("Thu, 31 Dec 2026 23:59:59 GMT").unwrap();
        assert_eq!(nov8, (2026, 11, 8, 4, 12, 34));
        assert!(nov1 < nov8 && nov8 < dec31);
        for bad in [
            "",
            "08 Nov 2026",
            "Sun, 08 Foo 2026 04:12:34 GMT",
            "Sun, 08 Nov 2026 25:00:00 GMT",
            "Sun, 08 Nov 2026 04:12:34 PST",
        ] {
            assert!(parse_http_date(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn formats_rfc3339() {
        let now = super::utc_now();
        assert_eq!(now.len(), 20, "{now}");
        assert!(now.ends_with('Z') && now.as_bytes()[10] == b'T');
        assert!(now.as_str() > "2026-01-01T00:00:00Z");
    }
}
