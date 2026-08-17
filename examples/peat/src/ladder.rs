//! The temporal ladder: bounded reading over an unbounded ledger.
//!
//! Rung 0 (per-day `DayStats`) is the only materialized aggregate; every
//! higher rung is a *read-time regrouping* of those rows into calendar
//! windows that widen geometrically with distance from now — 2 days,
//! 2 weeks, 2 months, 2 quarters, then years, then one deep-past band.
//! Nothing is stored, so nothing can go stale: changing the budget
//! re-slices the same rows and recomputes nothing, and `asof` gets a
//! correct ladder for free by calling with its cutoff as `now`.
//!
//! Windows are calendar units, not dyadic pairs, because their names are
//! the descent handles (`w33`, `2026-07`, `q2`) — every band line ends in
//! the exact command that opens it. `now` enters only at the read
//! boundary; same day rows + same `now` + same budget → same bands.
//!
//! Day buckets are UTC (`ts_ms / DAY_MS`), consistent with the digest.

use std::collections::BTreeMap;

use crate::pipeline::DayStats;

// ---------------------------------------------------------------- civil days

/// A civil date, convertible to/from the day-bucket index. Algorithms are
/// Howard Hinnant's; `date_label` in transcript.rs uses the same math.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Civil {
    pub y: i64,
    pub m: u32,
    pub d: u32,
}

impl Civil {
    pub fn from_bucket(days: u64) -> Civil {
        let days = days as i64;
        let era = (days + 719_468).div_euclid(146_097);
        let doe = days + 719_468 - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        Civil { y: yoe + era * 400 + i64::from(m <= 2), m, d }
    }

    pub fn bucket(self) -> u64 {
        let y = self.y - i64::from(self.m <= 2);
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let mp = if self.m > 2 { self.m - 3 } else { self.m + 9 } as i64;
        let doy = (153 * mp + 2) / 5 + self.d as i64 - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        (era * 146_097 + doe - 719_468) as u64
    }

    /// 0 = Monday .. 6 = Sunday.
    pub fn weekday(self) -> u32 {
        ((self.bucket() + 3) % 7) as u32
    }

    /// ISO week number and its year.
    pub fn iso_week(self) -> (i64, u32) {
        let thursday = self.bucket() + 3 - self.weekday() as u64;
        let c = Civil::from_bucket(thursday);
        let jan1 = Civil { y: c.y, m: 1, d: 1 }.bucket();
        (c.y, ((thursday - jan1) / 7 + 1) as u32)
    }

    fn month_start(self) -> Civil {
        Civil { d: 1, ..self }
    }

    fn quarter_start(self) -> Civil {
        Civil { m: (self.m - 1) / 3 * 3 + 1, d: 1, ..self }
    }
}

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

// ---------------------------------------------------------------- bands

/// One rung of the ladder: a window's extractive digest plus its handle.
#[derive(serde::Serialize, Clone)]
pub struct Band {
    pub label: String,
    /// human span, e.g. "aug 10–15"; empty when the label carries it
    pub span: String,
    /// the exact command that descends into this window
    pub handle: String,
    pub start: u64,
    pub end: u64,
    pub tools: i64,
    pub fails: i64,
    pub commits: i64,
    pub sessions: i64,
    pub obs: i64,
    /// a file names the band only when it dominates it (≥25% of touches)
    pub files: Vec<String>,
}

/// Digest one explicit window (zoom's header and children reuse this).
pub fn digest(
    day_rows: &BTreeMap<u64, DayStats>,
    obs_per_day: &BTreeMap<u64, i64>,
    start: u64,
    end: u64,
    label: String,
    span: String,
    handle: String,
) -> Band {
    let mut b = Band {
        label,
        span,
        handle,
        start,
        end,
        tools: 0,
        fails: 0,
        commits: 0,
        sessions: 0,
        obs: 0,
        files: Vec::new(),
    };
    let mut files: BTreeMap<&str, i64> = BTreeMap::new();
    let mut total_touches = 0i64;
    for (_, s) in day_rows.range(start..=end) {
        b.tools += s.tools;
        b.fails += s.fails;
        b.commits += s.commits;
        b.sessions += s.sessions;
        for (f, n) in &s.files {
            *files.entry(f).or_default() += n;
            total_touches += n;
        }
    }
    b.obs = obs_per_day.range(start..=end).map(|(_, n)| n).sum();
    let mut fs: Vec<(&str, i64)> = files.into_iter().collect();
    fs.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    b.files = fs
        .iter()
        .take(2)
        .filter(|(_, n)| *n * 4 >= total_touches) // dominance, not mere presence
        .map(|(f, _)| crate::ui::short_path(f))
        .collect();
    b
}

/// The ladder walk: starting the day before `frontier`, emit 2 windows per
/// rung (week → month → quarter), then years, then one deep-past band —
/// stopping early if the budget runs out or the ledger does. Bands tile
/// `[oldest ..= frontier-1]` exactly: no gaps, no overlaps.
pub fn bands(
    day_rows: &BTreeMap<u64, DayStats>,
    obs_per_day: &BTreeMap<u64, i64>,
    frontier: u64,
    budget: usize,
) -> Vec<Band> {
    let Some(oldest) = day_rows.keys().next().copied() else {
        return Vec::new();
    };
    if frontier == 0 || oldest >= frontier {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut end = frontier - 1; // inclusive upper edge of the next band
    let mut rung = 0usize; // 0,1 → weeks; 2,3 → months; 4,5 → quarters; 6+ → years
    while end >= oldest {
        if out.len() + 1 >= budget {
            // budget spent: one terminal band swallows the rest of the past
            let s = Civil::from_bucket(oldest);
            let e = Civil::from_bucket(end);
            out.push(digest(
                day_rows,
                obs_per_day,
                oldest,
                end,
                "earlier".into(),
                format!("{} {} {} – {} {} {}", MONTHS[s.m as usize - 1], s.d, s.y, MONTHS[e.m as usize - 1], e.d, e.y),
                format!(
                    "peat {:04}-{:02}-{:02}..{:04}-{:02}-{:02}",
                    s.y, s.m, s.d, e.y, e.m, e.d
                ),
            ));
            break;
        }
        let c = Civil::from_bucket(end);
        let (start, label, span, handle) = match rung {
            0 | 1 => {
                let start = end - c.weekday() as u64;
                let (wy, wn) = c.iso_week();
                let s = Civil::from_bucket(start.max(oldest));
                let e = Civil::from_bucket(end);
                (
                    start,
                    format!("w{wn}"),
                    format!("{} {}–{}", MONTHS[s.m as usize - 1], s.d, e.d),
                    format!("peat {wy}-w{wn}"),
                )
            }
            2 | 3 => {
                let start = c.month_start().bucket();
                (
                    start,
                    MONTHS[c.m as usize - 1].to_string(),
                    String::new(),
                    format!("peat {:04}-{:02}", c.y, c.m),
                )
            }
            4 | 5 => {
                let start = c.quarter_start().bucket();
                let q = (c.m - 1) / 3 + 1;
                (start, format!("q{q}"), String::new(), format!("peat {}-q{q}", c.y))
            }
            _ => {
                let start = Civil { y: c.y, m: 1, d: 1 }.bucket();
                (start, format!("{}", c.y), String::new(), format!("peat {}", c.y))
            }
        };
        let start = start.max(oldest);
        out.push(digest(
            day_rows,
            obs_per_day,
            start,
            end,
            label,
            span,
            handle,
        ));
        if start == 0 || start <= oldest {
            break;
        }
        end = start - 1;
        rung += 1;
    }
    out
}

// ---------------------------------------------------------------- windows

/// A parsed window target: the shape-dispatch grammar for places in time.
/// Strict on purpose — anything that doesn't match is search words.
///
///   2026-08-14   day        2026-07  month     2026      year
///   w33          ISO week (most recent ≤ now)  2026-w33  pinned week
///   q3           quarter  (most recent ≤ now)  2026-q3   pinned quarter
pub fn parse_window(s: &str, now_bucket: u64) -> Option<(u64, u64, String)> {
    let s = s.to_ascii_lowercase();
    // A..B range: both sides must themselves be windows
    if let Some((a, b)) = s.split_once("..") {
        let (s1, _, l1) = parse_window(a, now_bucket)?;
        let (_, e2, l2) = parse_window(b, now_bucket)?;
        (s1 <= e2).then_some(())?;
        return Some((s1, e2, format!("{l1} – {l2}")));
    }
    let now = Civil::from_bucket(now_bucket);
    let clamp = |start: u64, end: u64, label: String| {
        (start, end.min(now_bucket), label)
    };
    // YYYY-MM-DD / YYYY-MM / YYYY
    let parts: Vec<&str> = s.split('-').collect();
    let all_num = parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    if all_num {
        match parts.as_slice() {
            [y, m, d] if y.len() == 4 => {
                let c = Civil { y: y.parse().ok()?, m: m.parse().ok()?, d: d.parse().ok()? };
                (1..=12).contains(&c.m).then_some(())?;
                let b = c.bucket();
                return Some((b, b, format!("{} {}", MONTHS[c.m as usize - 1], c.d)));
            }
            [y, m] if y.len() == 4 => {
                let (y, m): (i64, u32) = (y.parse().ok()?, m.parse().ok()?);
                (1..=12).contains(&m).then_some(())?;
                let start = Civil { y, m, d: 1 };
                let next = if m == 12 { Civil { y: y + 1, m: 1, d: 1 } } else { Civil { y, m: m + 1, d: 1 } };
                return Some(clamp(start.bucket(), next.bucket() - 1, format!("{} {y}", MONTHS[m as usize - 1])));
            }
            [y] if y.len() == 4 => {
                let y: i64 = y.parse().ok()?;
                let start = Civil { y, m: 1, d: 1 }.bucket();
                let end = Civil { y: y + 1, m: 1, d: 1 }.bucket() - 1;
                return Some(clamp(start, end, format!("{y}")));
            }
            _ => return None,
        }
    }
    // [YYYY-]wNN and [YYYY-]qN
    let (year, tail) = match parts.as_slice() {
        [y, t] if y.len() == 4 && y.bytes().all(|b| b.is_ascii_digit()) => (Some(y.parse::<i64>().ok()?), *t),
        [t] => (None, *t),
        _ => return None,
    };
    if let Some(n) = tail.strip_prefix('w').and_then(|n| n.parse::<u32>().ok()) {
        (1..=53).contains(&n).then_some(())?;
        let y = year.unwrap_or_else(|| now.iso_week().0);
        // Monday of ISO week n: week 1 contains Jan 4
        let jan4 = Civil { y, m: 1, d: 4 };
        let week1_mon = jan4.bucket() - jan4.weekday() as u64;
        let start = week1_mon + (n as u64 - 1) * 7;
        if year.is_none() && start > now_bucket {
            // bare wNN in january referring to last year's tail
            let jan4 = Civil { y: y - 1, m: 1, d: 4 };
            let start = jan4.bucket() - jan4.weekday() as u64 + (n as u64 - 1) * 7;
            return Some(clamp(start, start + 6, format!("w{n}")));
        }
        return Some(clamp(start, start + 6, format!("w{n}")));
    }
    if let Some(n) = tail.strip_prefix('q').and_then(|n| n.parse::<u32>().ok()) {
        (1..=4).contains(&n).then_some(())?;
        let mut y = year.unwrap_or(now.y);
        if year.is_none() && n > (now.m - 1) / 3 + 1 {
            y -= 1; // bare qN later than the current quarter → last year's
        }
        let start = Civil { y, m: (n - 1) * 3 + 1, d: 1 };
        let end = if n == 4 { Civil { y: y + 1, m: 1, d: 1 } } else { Civil { y, m: n * 3 + 1, d: 1 } };
        return Some(clamp(start.bucket(), end.bucket() - 1, format!("q{n} {y}")));
    }
    None
}

/// Children of a window, one rung finer: year → months, quarter → months,
/// month → weeks, week → days, day → (caller renders sessions).
pub fn children(start: u64, end: u64) -> Vec<(u64, u64, String, String)> {
    let days = end - start + 1;
    let mut out = Vec::new();
    if days <= 1 {
        return out;
    }
    if days <= 7 {
        for b in start..=end {
            let c = Civil::from_bucket(b);
            out.push((b, b, format!("{} {}", MONTHS[c.m as usize - 1], c.d), format!("peat {:04}-{:02}-{:02}", c.y, c.m, c.d)));
        }
    } else if days <= 31 {
        let mut b = start;
        while b <= end {
            let c = Civil::from_bucket(b);
            let wk_end = (b + 6 - c.weekday() as u64).min(end);
            let (wy, wn) = c.iso_week();
            let s = Civil::from_bucket(b);
            let e = Civil::from_bucket(wk_end);
            out.push((b, wk_end, format!("w{wn} · {} {}–{}", MONTHS[s.m as usize - 1], s.d, e.d), format!("peat {wy}-w{wn}")));
            b = wk_end + 1;
        }
    } else {
        let mut b = start;
        while b <= end {
            let c = Civil::from_bucket(b).month_start();
            let next = if c.m == 12 { Civil { y: c.y + 1, m: 1, d: 1 } } else { Civil { y: c.y, m: c.m + 1, d: 1 } };
            let m_end = (next.bucket() - 1).min(end);
            out.push((b, m_end, format!("{} {}", MONTHS[c.m as usize - 1], c.y), format!("peat {:04}-{:02}", c.y, c.m)));
            b = m_end + 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(tools: i64) -> DayStats {
        DayStats { tools, ..Default::default() }
    }

    #[test]
    fn civil_round_trips() {
        for b in [0u64, 719_468, 20_000, 20_680, 20_681, 21_000] {
            assert_eq!(Civil::from_bucket(b).bucket(), b);
        }
        // 2026-08-17 is a Monday
        let c = Civil { y: 2026, m: 8, d: 17 };
        assert_eq!(c.weekday(), 0);
        assert_eq!(c.iso_week().1, 34);
    }

    #[test]
    fn bands_tile_the_past_exactly() {
        let mut rows = BTreeMap::new();
        let today = Civil { y: 2026, m: 8, d: 17 }.bucket();
        let oldest = today - 400;
        for b in (oldest..=today).step_by(3) {
            rows.insert(b, day(10));
        }
        let obs = BTreeMap::new();
        let bs = bands(&rows, &obs, today, 10);
        assert!(!bs.is_empty());
        // tiling: bands run newest→oldest, contiguous, no gaps or overlaps
        assert_eq!(bs[0].end, today - 1);
        for w in bs.windows(2) {
            assert_eq!(w[1].end, w[0].start - 1, "gap or overlap between bands");
        }
        assert_eq!(bs.last().unwrap().start, oldest);
        assert!(bs.len() <= 10);
    }

    #[test]
    fn budget_changes_reslice_only() {
        let mut rows = BTreeMap::new();
        let today = Civil { y: 2026, m: 8, d: 17 }.bucket();
        for b in (today - 700..=today).step_by(2) {
            rows.insert(b, day(1));
        }
        let obs = BTreeMap::new();
        for budget in [3usize, 5, 8, 12] {
            let bs = bands(&rows, &obs, today, budget);
            assert!(bs.len() <= budget, "budget {budget} produced {}", bs.len());
            assert_eq!(bs.last().unwrap().start, *rows.keys().next().unwrap());
        }
    }

    #[test]
    fn window_grammar() {
        let now = Civil { y: 2026, m: 8, d: 17 }.bucket();
        let (s, e, _) = parse_window("2026-08-14", now).unwrap();
        assert_eq!(s, e);
        let (s, e, _) = parse_window("w33", now).unwrap();
        assert_eq!(e - s, 6);
        assert_eq!(Civil::from_bucket(s).weekday(), 0);
        let (s, e, _) = parse_window("2026-07", now).unwrap();
        assert_eq!(Civil::from_bucket(s), Civil { y: 2026, m: 7, d: 1 });
        assert_eq!(Civil::from_bucket(e), Civil { y: 2026, m: 7, d: 31 });
        let (_, e, _) = parse_window("q3", now).unwrap();
        assert_eq!(e, now, "current quarter clamps at now");
        assert!(parse_window("fold", now).is_none());
        assert!(parse_window("w99", now).is_none());
        assert!(parse_window("2026-13", now).is_none());
    }

    #[test]
    fn dominance_filter_on_band_files() {
        let mut rows = BTreeMap::new();
        let today = Civil { y: 2026, m: 8, d: 17 }.bucket();
        let mut s = day(5);
        s.files.insert("src/a/dominant.rs".into(), 30);
        s.files.insert("src/a/minor1.rs".into(), 2);
        s.files.insert("src/a/minor2.rs".into(), 2);
        rows.insert(today - 10, s);
        let obs = BTreeMap::new();
        let bs = bands(&rows, &obs, today, 8);
        let with_files: Vec<&Band> = bs.iter().filter(|b| !b.files.is_empty()).collect();
        assert_eq!(with_files.len(), 1);
        assert_eq!(with_files[0].files, vec!["…/a/dominant.rs".to_string()]);
    }
}
