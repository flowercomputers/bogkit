//! Terminal presentation. One rule governs this module: **stdout is an
//! API** — the `SessionStart` hook injects `peat brief` stdout into an
//! agent's context verbatim, and `--json` is machine-read. Everything
//! animated or decorative therefore targets stderr, and only when stderr
//! is a real terminal; color reaches stdout only when stdout is one.
//! Under a hook both gates fail (the streams are pipes), so hook and
//! piped output stay byte-identical to the unstyled form with no
//! special-casing.

use std::io::IsTerminal;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use indicatif::{ProgressBar, ProgressStyle};

fn color_ok() -> bool {
    std::env::var_os("NO_COLOR").is_none()
        && std::env::var_os("TERM").is_none_or(|t| t != "dumb")
}

// Stream capability cannot change within a process, and the style helpers
// run in per-row loops — probe once, not per styled string.

/// Decoration allowed on stderr (spinners, phase timings).
pub fn fancy_err() -> bool {
    static C: OnceLock<bool> = OnceLock::new();
    *C.get_or_init(|| std::io::stderr().is_terminal() && color_ok())
}

/// Color allowed on stdout (the rendered brief at an interactive prompt).
pub fn fancy_out() -> bool {
    static C: OnceLock<bool> = OnceLock::new();
    *C.get_or_init(|| std::io::stdout().is_terminal() && color_ok())
}

/// Whether stdout is a terminal at all (paging is independent of color:
/// `NO_COLOR` suppresses ANSI but should not suppress `less`).
pub fn stdout_is_tty() -> bool {
    static C: OnceLock<bool> = OnceLock::new();
    *C.get_or_init(|| std::io::stdout().is_terminal())
}

/// A spinner for one named phase of work, RAII-style: create it with a
/// gerund, drop it and it vanishes. Phases that turn out slow (>300ms)
/// leave one dim line with the measured time — numbers are receipts.
/// When stderr is not a terminal every method is a no-op.
pub struct Phase {
    bar: Option<ProgressBar>,
    label: String,
    started: Instant,
}

impl Phase {
    pub fn new(label: &str) -> Self {
        let bar = fancy_err().then(|| {
            let b = ProgressBar::new_spinner()
                .with_style(
                    ProgressStyle::with_template("{spinner} {msg}")
                        .unwrap()
                        .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏·"),
                )
                .with_message(format!("{label}…"));
            b.enable_steady_tick(Duration::from_millis(80));
            b
        });
        Phase {
            bar,
            label: label.to_string(),
            started: Instant::now(),
        }
    }

    /// Update the live message (elapsed shown by the lock-wait phase).
    pub fn tick(&self, msg: String) {
        if let Some(b) = &self.bar {
            b.set_message(msg);
        }
    }

    /// End the phase; slow ones report their measured cost.
    pub fn done(self) {
        if let Some(b) = &self.bar {
            let took = self.started.elapsed();
            if took > Duration::from_millis(300) {
                b.finish_and_clear();
                eprintln!(
                    "{}",
                    console::style(format!("{} ({:.1}s)", self.label, took.as_secs_f64())).dim()
                );
            } else {
                b.finish_and_clear();
            }
        }
    }
}

impl Drop for Phase {
    fn drop(&mut self) {
        if let Some(b) = self.bar.take() {
            b.finish_and_clear();
        }
    }
}

// ---- the one style vocabulary, shared by every verb ----
//
// Four roles, used identically in the brief template and in direct verb
// output: `h1` bold headers · `accent` cyan identities (subjects, session
// ids) · `dim` receded metadata (tags, parentheticals, receipts) · `warn`
// red distrust signals (uncited, failures, errors). All identity when the
// target stream is not a terminal.

fn paint(on: bool, f: fn(console::Style) -> console::Style, s: &str) -> String {
    if on {
        f(console::Style::new()).apply_to(s).to_string()
    } else {
        s.to_string()
    }
}

/// Bold header, stdout.
pub fn h1(s: &str) -> String {
    paint(fancy_out(), |c| c.bold(), s)
}
/// Cyan identity (subject, session), stdout.
pub fn accent(s: &str) -> String {
    paint(fancy_out(), |c| c.cyan(), s)
}
/// Dim metadata, stdout.
pub fn dim(s: &str) -> String {
    paint(fancy_out(), |c| c.dim(), s)
}
/// Red distrust signal, stdout.
pub fn warn(s: &str) -> String {
    paint(fancy_out(), |c| c.red(), s)
}

/// Dim receipt line on stderr (`peat: captured 161 events …`).
pub fn note(msg: &str) {
    eprintln!("{}", paint(fancy_err(), |c| c.dim(), msg));
}

/// Error line on stderr: the `peat:` prefix in red, message plain.
pub fn error(msg: &str) {
    eprintln!("{} {msg}", paint(fancy_err(), |c| c.red(), "peat:"));
}

/// Register the style filters the brief template may use. Identity unless
/// stdout is a terminal, so templated output under hooks, tests, and pipes
/// is byte-for-byte what the template says.
pub fn add_style_filters(env: &mut minijinja::Environment<'_>) {
    let on = fancy_out();
    let style = move |f: fn(console::Style) -> console::Style| {
        move |s: String| -> String { paint(on, f, &s) }
    };
    env.add_filter("h1", style(|s| s.bold()));
    env.add_filter("dim", style(|s| s.dim()));
    env.add_filter("warn", style(|s| s.red()));
    env.add_filter("accent", style(|s| s.cyan()));
    // display-time truncation lives HERE, not in the JSON — --json is the
    // API and carries full text; templates opt into clipping
    env.add_filter("clip", |s: String, n: usize| clip(&s, n));
}

// ---- display formatting, shared by every verb ----

/// Whitespace-collapse and truncate to `max` chars with an ellipsis.
/// Display-only: JSON output never clips.
pub fn clip(s: &str, max: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= max {
        return s;
    }
    let cut: String = s.chars().take(max).collect();
    format!("{cut}…")
}

/// Last two path components, elided: `…/dir/file.rs`.
pub fn short_path(p: &str) -> String {
    let parts: Vec<&str> = p.rsplitn(3, '/').collect();
    match parts.len() {
        3 => format!("…/{}/{}", parts[1], parts[0]),
        _ => p.to_string(),
    }
}

/// First 8 chars of a session uuid.
pub fn short_sess(s: &str) -> String {
    s.chars().take(8).collect()
}

/// Humanized age: `<1h`, `7h`, `33d`.
pub fn age_label(now: u64, ts: u64) -> String {
    const DAY_MS: u64 = 86_400_000;
    let d = now.saturating_sub(ts);
    match d {
        _ if d < 3_600_000 => "<1h".into(),
        _ if d < DAY_MS => format!("{}h", d / 3_600_000),
        _ => format!("{}d", d / DAY_MS),
    }
}

/// Write through `less -RFX` on a terminal (quit-if-one-screen, keep
/// ANSI), plain stdout otherwise.
pub fn page(text: &str) {
    use std::io::Write;
    if stdout_is_tty()
        && let Ok(mut p) = std::process::Command::new("less")
            .args(["-RFX"])
            .stdin(std::process::Stdio::piped())
            .spawn()
        {
            if let Some(stdin) = p.stdin.as_mut() {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = p.wait();
            return;
        }
    print!("{text}");
}
