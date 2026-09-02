//! Where the ledger lives and how it is opened: path policy, session-id
//! policy, and the shared-database open-with-retry.

use std::path::PathBuf;

use fold::pipeline::{Keyed, Push};
use fold::stream::KeyedStream;

use crate::event::{Envelope, EventId};
use crate::ui;

/// The database: `$PEAT_DB` if set, else `.peat/db` at the repo root —
/// unless `.peat/redirect` names another `.peat` directory (one line,
/// resolved relative to the repo root), in which case the ledger lives
/// there. The beads convention, borrowed: a worktree desk redirects to its
/// anchor so every seat reads and writes one shared memory, while
/// desk-local files (`current-session`, the once-per-session markers)
/// stay beside the redirect.
pub fn db_path() -> PathBuf {
    if let Ok(p) = std::env::var("PEAT_DB") {
        return PathBuf::from(p);
    }
    let dir = peat_dir();
    if let Ok(target) = std::fs::read_to_string(dir.join("redirect")) {
        let target = target.trim();
        if !target.is_empty() {
            let base = dir.parent().map(PathBuf::from).unwrap_or_default();
            return base.join(target).join("db");
        }
    }
    dir.join("db")
}

/// `.peat/` beside the nearest git/jj root above cwd, else cwd. Cached —
/// callers hit this several times per invocation and the answer is fixed.
pub fn peat_dir() -> PathBuf {
    static C: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    C.get_or_init(|| {
        let mut dir = std::env::current_dir().unwrap();
        loop {
            if dir.join(".git").exists() || dir.join(".jj").exists() {
                return dir.join(".peat");
            }
            if !dir.pop() {
                return std::env::current_dir().unwrap().join(".peat");
            }
        }
    })
    .clone()
}

/// Resolve the current session id: an explicit value wins, then this
/// worktree's `.peat/current-session`, then the file beside the shared db
/// (a desk is not the anchor — hooks may have written it there).
pub fn current_session(explicit: Option<String>) -> Option<String> {
    explicit
        .or_else(|| std::fs::read_to_string(peat_dir().join("current-session")).ok())
        .or_else(|| {
            let anchor = db_path().parent()?.join("current-session");
            std::fs::read_to_string(anchor).ok()
        })
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Restores the process panic hook when dropped, whichever way the open
/// loop exits — the hook is muted during retries because `catch_unwind`
/// does not silence it, and a caught-and-retried lock conflict must not
/// print a backtrace.
type PanicHook = Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Sync + Send + 'static>;

struct QuietPanics(Option<PanicHook>);

impl QuietPanics {
    fn engage() -> Self {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        QuietPanics(Some(prev))
    }
}

impl Drop for QuietPanics {
    fn drop(&mut self) {
        if let Some(hook) = self.0.take() {
            std::panic::set_hook(hook);
        }
    }
}

/// Open the ledger, waiting out lock contention.
///
/// fold is single-writer and fjall's lock is exclusive even for reads, so
/// with several worktree agents sharing one `PEAT_DB`, invocations can
/// collide; peat processes are short-lived, so waiting is correct. Retries
/// with backoff up to `PEAT_LOCK_WAIT_SECS` (default 120 — a bulk capture
/// can legitimately hold the lock for minutes), then exits `EX_TEMPFAIL`
/// with an explanation. Non-lock panics (corruption, schema) fail
/// immediately.
///
/// `make` re-creates the pipeline per attempt (the value is consumed by a
/// failed open); generic over `P` so the pipeline type stays inferred at
/// the call site and reader destructuring keeps compiling.
pub fn open<P>(path: PathBuf, make: impl Fn() -> P) -> KeyedStream<EventId, Envelope, P>
where
    P: Push<Keyed<EventId, Envelope>>,
{
    let wait_max_ms: u64 = std::env::var("PEAT_LOCK_WAIT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(120u64)
        * 1000;

    // the data dir ignores itself (cargo's target/ pattern): without this
    // jj snapshots the database into the working commit on the very next
    // command, and git accumulates it as untracked noise
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
        let marker = parent.join(".gitignore");
        if !marker.exists() {
            let _ = std::fs::write(&marker, "*
");
        }
    }

    let phase = ui::Phase::new("opening ledger");
    let quiet = QuietPanics::engage();
    let mut delay_ms = 200u64;
    let mut waited = 0u64;
    let st = loop {
        let path = path.clone();
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            KeyedStream::<EventId, Envelope, _>::new(path, make())
        })) {
            Ok(st) => break st,
            Err(p) => {
                let msg = p
                    .downcast_ref::<String>()
                    .map(String::as_str)
                    .or_else(|| p.downcast_ref::<&str>().copied())
                    .unwrap_or("");
                if !msg.contains("Locked") {
                    drop(quiet);
                    std::panic::resume_unwind(p);
                }
                if waited >= wait_max_ms {
                    drop(quiet);
                    ui::error(&format!(
                        "ledger still locked after {}s — another peat \
process holds it (reads are exclusive too; a bulk capture can hold it for \
minutes). Retry shortly, or raise PEAT_LOCK_WAIT_SECS.",
                        wait_max_ms / 1000
                    ));
                    std::process::exit(75); // EX_TEMPFAIL
                }
                if waited == 0 && !ui::fancy_err() {
                    // non-tty gets one plain line instead of a spinner
                    eprintln!("peat: ledger busy (another peat process); waiting…");
                }
                phase.tick(format!(
                    "ledger busy — another peat process · waiting {}s (gives up at {}s)",
                    waited / 1000,
                    wait_max_ms / 1000
                ));
                std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                waited += delay_ms;
                delay_ms = (delay_ms * 2).min(3_000);
            }
        }
    };
    phase.done();
    st
}
