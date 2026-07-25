//! Minimal timestamp parsing for the catalog's UTC columns.
//!
//! The catalog writes two shapes — `2021-04-06 16:02:01+00:00` and
//! `2021-04-06 18:05:14.808880+00:00` — and occasionally an empty cell. That
//! is a small enough surface that a dependency-free parser is preferable to
//! pulling in a date library, and it keeps the manifest hash a pure function
//! of this crate.

/// Days from 1970-01-01 to `y-m-d`, by Howard Hinnant's `days_from_civil`.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Inverse of [`days_from_civil`], for formatting.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Parse a catalog UTC timestamp to epoch milliseconds.
///
/// Accepts `YYYY-MM-DD[ T]HH:MM:SS[.frac][Z|±HH:MM]`. Returns `None` for an
/// empty or unparseable cell rather than guessing — a wrong recording time
/// propagates into every derived timestamp.
pub fn parse_utc_ms(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let bytes = s.as_bytes();
    if bytes.len() < 19 {
        return None;
    }
    let num = |a: usize, b: usize| -> Option<i64> { s.get(a..b)?.parse::<i64>().ok() };

    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let (h, mi, sec) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if h > 23 || mi > 59 || sec > 60 {
        return None;
    }

    // fractional seconds, truncated to milliseconds
    let mut rest = &s[19..];
    let mut millis = 0i64;
    if let Some(frac) = rest.strip_prefix('.') {
        let digits: String = frac.chars().take_while(|c| c.is_ascii_digit()).collect();
        rest = &frac[digits.len()..];
        let mut ms = String::from("0");
        ms.push_str(&digits.chars().take(3).collect::<String>());
        // pad "5" -> "500", "50" -> "500"
        let pad = 3usize.saturating_sub(digits.len().min(3));
        for _ in 0..pad {
            ms.push('0');
        }
        millis = ms.parse().unwrap_or(0);
    }

    // offset: Z, empty (assume UTC), or ±HH:MM / ±HHMM
    let offset_min = match rest.as_bytes().first() {
        None | Some(b'Z') | Some(b'z') => 0,
        Some(c @ (b'+' | b'-')) => {
            let sign = if *c == b'-' { -1 } else { 1 };
            let body = &rest[1..];
            let (oh, om) = if let Some((a, b)) = body.split_once(':') {
                (a.parse::<i64>().ok()?, b.parse::<i64>().ok()?)
            } else if body.len() >= 4 {
                (body[..2].parse().ok()?, body[2..4].parse().ok()?)
            } else {
                (body.parse::<i64>().ok()?, 0)
            };
            sign * (oh * 60 + om)
        }
        _ => return None,
    };

    let days = days_from_civil(y, mo, d);
    Some(((days * 86_400 + h * 3_600 + mi * 60 + sec) - offset_min * 60) * 1000 + millis)
}

/// Format epoch milliseconds as `YYYY-MM-DDTHH:MM:SSZ`, for record stamps.
pub fn format_utc_ms(ms: i64) -> String {
    let (days, rem) = (ms.div_euclid(86_400_000), ms.rem_euclid(86_400_000));
    let (y, mo, d) = civil_from_days(days);
    let (h, mi, s) = (rem / 3_600_000, rem % 3_600_000 / 60_000, rem % 60_000 / 1000);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// ISO week number, for BirdNET's week-of-year prior.
///
/// BirdNET conditions its species list on location and week; the archive is
/// April 2021, so this materially narrows the candidate set.
pub fn iso_week(ms: i64) -> u32 {
    let days = ms.div_euclid(86_400_000);
    let (y, _, _) = civil_from_days(days);
    let jan1 = days_from_civil(y, 1, 1);
    // 1970-01-01 was a Thursday (weekday 4 with Monday = 1)
    let weekday = |d: i64| ((d % 7 + 7) % 7 + 3) % 7 + 1;
    let doy = days - jan1 + 1;
    let week = (doy + 7 - weekday(days) as i64 + 3) / 7;
    week.clamp(1, 53) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_both_catalog_timestamp_shapes() {
        // the catalog's two observed formats must agree to the second
        let plain = parse_utc_ms("2021-04-06 16:02:01+00:00").unwrap();
        assert_eq!(format_utc_ms(plain), "2021-04-06T16:02:01Z");

        let micros = parse_utc_ms("2021-04-06 18:05:14.808880+00:00").unwrap();
        assert_eq!(format_utc_ms(micros), "2021-04-06T18:05:14Z");
        // microseconds truncate to milliseconds, they do not round
        assert_eq!(micros % 1000, 808);
    }

    #[test]
    fn epoch_and_offsets() {
        assert_eq!(parse_utc_ms("1970-01-01 00:00:00+00:00"), Some(0));
        assert_eq!(parse_utc_ms("1970-01-01T00:00:00Z"), Some(0));
        // a positive offset means the UTC instant is earlier
        assert_eq!(parse_utc_ms("1970-01-01 02:00:00+02:00"), Some(0));
        assert_eq!(
            parse_utc_ms("1970-01-01 00:00:00-05:00"),
            Some(5 * 3_600_000)
        );
    }

    #[test]
    fn empty_and_malformed_cells_yield_none() {
        // guessing here would poison every derived timestamp
        for bad in ["", "   ", "not a date", "2021-04-06", "2021-13-01 00:00:00Z"] {
            assert_eq!(parse_utc_ms(bad), None, "{bad:?} should not parse");
        }
    }

    #[test]
    fn fractional_padding_is_positional() {
        assert_eq!(parse_utc_ms("2021-04-06 00:00:00.5Z").unwrap() % 1000, 500);
        assert_eq!(parse_utc_ms("2021-04-06 00:00:00.05Z").unwrap() % 1000, 50);
        assert_eq!(parse_utc_ms("2021-04-06 00:00:00.005Z").unwrap() % 1000, 5);
    }

    #[test]
    fn round_trips_across_a_leap_year_boundary() {
        for s in [
            "2020-02-29T12:00:00Z",
            "2021-01-01T00:00:00Z",
            "2021-12-31T23:59:59Z",
            "2024-02-29T00:00:00Z",
        ] {
            let ms = parse_utc_ms(s).unwrap();
            assert_eq!(format_utc_ms(ms), s, "round trip failed for {s}");
        }
    }

    #[test]
    fn april_2021_lands_in_the_expected_birdnet_week() {
        // the corpus is April 2021; weeks 13-17 are the migration window
        // BirdNET's prior should be narrowing to
        let w = iso_week(parse_utc_ms("2021-04-06 16:02:01+00:00").unwrap());
        assert_eq!(w, 14);
        let w2 = iso_week(parse_utc_ms("2021-04-30 01:21:44Z").unwrap());
        assert_eq!(w2, 17);
    }
}
