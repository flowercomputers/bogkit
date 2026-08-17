//! The oracles peat's claims rest on, plus parser and idempotency checks.
//!
//! Oracle 1 (retraction): revising an indexed text must make the old text
//! unfindable in both the keyword and vector indexes. The `#[ignore]`d twin
//! proves the oracle is red-capable — it asserts the OPPOSITE and must fail
//! when run (`cargo test -p peat -- --ignored` shows exactly one failure).
//!
//! Oracle 2 (replay determinism): folding any prefix of the ledger must
//! yield exactly the views that an independent scan of that prefix
//! predicts. This is what makes time travel (`--asof`-style replay) a fact
//! rather than a hope.

use std::collections::BTreeMap;

use fold::stream::KeyedStream;

use crate::event::{Envelope, Event, EventId, OBS_SEQ_BASE};
use crate::pipeline::{DayStats, SubjStats, DAY_MS};

fn tmp() -> std::path::PathBuf {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "peat-test-{}-{n}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

// The pipeline type contains closures and cannot be named, so opening a
// stream is a macro expanded where the concrete type is needed.
macro_rules! open {
    ($path:expr) => {
        KeyedStream::<EventId, Envelope, _>::new($path, crate::peat_pipeline!())
    };
}

fn obs(session: &str, n: u32, ts: u64, subject: &str, text: &str) -> (EventId, Envelope) {
    (
        (session.to_string(), OBS_SEQ_BASE + n),
        Envelope::new(
            session,
            ts,
            Event::Obs {
                subject: subject.to_string(),
                text: text.to_string(),
                derived_from: vec![],
            },
        ),
    )
}

fn tool(session: &str, n: u32, ts: u64, ok: bool) -> (EventId, Envelope) {
    (
        (session.to_string(), n),
        Envelope::new(
            session,
            ts,
            Event::ToolCall {
                tool: "Bash".into(),
                detail: format!("cmd-{n}"),
                ok,
            },
        ),
    )
}

// ---------------------------------------------------------------- oracle 1

/// Keyword hits for `probe` (exact posting semantics — text-level).
macro_rules! find_kw {
    ($st:expr, $probe:expr) => {
        $st.rtx(|(_, _, (kw, _, _), _, _)| {
            kw.search($probe, 10)
                .into_iter()
                .map(|h| h.val)
                .collect::<Vec<EventId>>()
        })
    };
}

/// Nearest neighbor for `probe`. NOTE: HNSW always returns the nearest
/// vectors regardless of absolute distance, so "old text unfindable" is
/// only well-defined relative to a control document that actually carries
/// the old text — the revised doc must lose to the control.
macro_rules! nearest {
    ($st:expr, $probe:expr) => {
        $st.rtx(|(_, _, (_, vec, _), _, _)| {
            vec.search(&ese::encode_single($probe))
                .first()
                .map(|h| h.val.clone())
        })
    };
}

#[test]
fn retraction_makes_old_text_unfindable() {
    let path = tmp();
    let mut st = open!(&path);
    let id = ("s1".to_string(), OBS_SEQ_BASE);
    let control = ("s1".to_string(), OBS_SEQ_BASE + 1);

    st.wtx(|tx| {
        tx.upsert(&id, &obs("s1", 0, 1000, "staging", "runs on the raspberry pi").1);
        // control: keeps carrying the old text so vector-nearest is decidable
        tx.upsert(
            &control,
            &obs("s1", 1, 1000, "hardware", "a raspberry pi lives under the desk").1,
        );
    });
    assert!(
        find_kw!(st, "raspberry").contains(&id),
        "sanity: old text must be keyword-indexed before revision"
    );

    // revise: same event id, new text — one transaction
    st.wtx(|tx| {
        tx.upsert(&id, &obs("s1", 0, 2000, "staging", "moved to a cloud vm").1);
    });

    assert!(
        !find_kw!(st, "raspberry").contains(&id),
        "old text still keyword-findable after revision"
    );
    assert!(find_kw!(st, "cloud vm").contains(&id));
    assert_eq!(
        nearest!(st, "raspberry pi under a desk"),
        Some(control.clone()),
        "revised doc still wins vector search for its OLD text"
    );
    assert_eq!(nearest!(st, "moved to a cloud vm"), Some(id.clone()));

    // and the property must survive a reopen (hnsw rebuild path)
    drop(st);
    let st = open!(&path);
    assert!(!find_kw!(st, "raspberry").contains(&id));
    assert_eq!(nearest!(st, "raspberry pi under a desk"), Some(control));
    assert_eq!(nearest!(st, "moved to a cloud vm"), Some(id));
}

/// Red-capability proof for oracle 1: asserts the OPPOSITE of the oracle.
/// `cargo test -p peat -- --ignored` must show this failing — if it ever
/// passes, the retraction path is broken and the oracle above has gone
/// blind. (Kept `#[ignore]`d so the suite is green by default.)
#[test]
#[ignore = "red-capability proof: must FAIL when run explicitly"]
fn retraction_oracle_is_red_capable() {
    let path = tmp();
    let mut st = open!(&path);
    let id = ("s1".to_string(), OBS_SEQ_BASE);
    st.wtx(|tx| {
        tx.upsert(&id, &obs("s1", 0, 1000, "staging", "runs on the raspberry pi").1);
    });
    st.wtx(|tx| {
        tx.upsert(&id, &obs("s1", 0, 2000, "staging", "moved to a cloud vm").1);
    });
    assert!(
        find_kw!(st, "raspberry").contains(&id),
        "correct behavior: this assertion is meant to fail"
    );
}

// ---------------------------------------------------------------- oracle 2

/// Deterministic synthetic ledger: interleaved sessions, tools, failures,
/// obs revisions across days. No randomness, no wall clock.
fn ledger() -> Vec<(EventId, Envelope)> {
    let mut ev = Vec::new();
    for s in 0..3u32 {
        let sid = format!("s{s}");
        for n in 0..20u32 {
            let ts = u64::from(s) * DAY_MS / 2 + u64::from(n) * 3_600_000;
            ev.push(tool(&sid, n, ts, n % 5 != 0));
        }
        for n in 0..4u32 {
            let ts = u64::from(s) * DAY_MS / 2 + u64::from(n) * 7_200_000;
            ev.push(obs(
                &sid,
                n,
                ts,
                if n % 2 == 0 { "staging" } else { "gate" },
                &format!("claim {s}-{n}"),
            ));
        }
    }
    ev
}

/// What the views must contain after folding `prefix`, computed by an
/// independent plain scan (no fold involved).
fn predict(
    prefix: &[(EventId, Envelope)],
) -> (
    BTreeMap<u64, (i64, i64)>,
    BTreeMap<String, (String, i64, (u64, u32))>,
) {
    let mut days: BTreeMap<u64, (i64, i64)> = BTreeMap::new(); // day -> (tools, fails)
    // subject -> (text, count, (last_ms, last_seq))
    let mut subj: BTreeMap<String, (String, i64, (u64, u32))> = BTreeMap::new();
    for (id, e) in prefix {
        match &e.kind {
            Event::ToolCall { ok, .. } => {
                let d = days.entry(e.ts_ms / DAY_MS).or_default();
                d.0 += 1;
                d.1 += i64::from(!ok);
            }
            Event::Obs { subject, text, .. } => {
                let s = subj.entry(subject.clone()).or_default();
                s.1 += 1;
                if (e.ts_ms, id.1) >= s.2 {
                    s.0 = text.clone();
                    s.2 = (e.ts_ms, id.1);
                }
            }
            _ => {}
        }
    }
    (days, subj)
}

#[test]
fn replay_prefix_matches_independent_prediction() {
    let ev = ledger();
    for cut in [0, 1, 7, 24, ev.len()] {
        let prefix = &ev[..cut];
        let mut st = open!(tmp());
        st.wtx(|tx| {
            for (id, e) in prefix {
                tx.upsert(id, e);
            }
        });
        let (want_days, want_subj) = predict(prefix);

        let (got_days, got_subj) = st.rtx(|(days, _, _, (subjects, _), _)| {
            let d: BTreeMap<u64, (i64, i64)> = days
                .iter()
                .map(|(k, v): (u64, DayStats)| (k, (v.tools, v.fails)))
                .collect();
            let s: BTreeMap<String, (String, i64, (u64, u32))> = subjects
                .iter()
                .map(|(k, v): (String, SubjStats)| {
                    (k, (v.text, v.count, (v.last_ms, v.last_seq)))
                })
                .collect();
            (d, s)
        });
        assert_eq!(got_days, want_days, "day digest diverged at prefix {cut}");
        assert_eq!(got_subj, want_subj, "subjects diverged at prefix {cut}");
    }
}

/// One-transaction fold equals many-transaction fold: batching must not be
/// observable (the other half of replay determinism).
#[test]
fn batching_is_unobservable() {
    let ev = ledger();

    let mut one = open!(tmp());
    one.wtx(|tx| {
        for (id, e) in &ev {
            tx.upsert(id, e);
        }
    });

    let mut many = open!(tmp());
    for (id, e) in &ev {
        many.wtx(|tx| {
            tx.upsert(id, e);
        });
    }

    let a = one.rtx(|(days, _, _, (subjects, _), _)| {
        (
            days.iter().collect::<BTreeMap<u64, DayStats>>().len(),
            subjects
                .iter()
                .map(|(k, v): (String, SubjStats)| (k, (v.text, v.count)))
                .collect::<BTreeMap<_, _>>(),
        )
    });
    let b = many.rtx(|(days, _, _, (subjects, _), _)| {
        (
            days.iter().collect::<BTreeMap<u64, DayStats>>().len(),
            subjects
                .iter()
                .map(|(k, v): (String, SubjStats)| (k, (v.text, v.count)))
                .collect::<BTreeMap<_, _>>(),
        )
    });
    assert_eq!(a, b);
}

/// Two obs on one subject with the SAME timestamp in one transaction:
/// the winner must be the higher seq, not whichever drained last.
#[test]
fn equal_timestamp_obs_resolve_by_seq() {
    for _ in 0..8 {
        // repeated runs guard against hash-order flakiness going unseen
        let mut st = open!(tmp());
        st.wtx(|tx| {
            tx.upsert(
                &("s1".to_string(), OBS_SEQ_BASE),
                &obs("s1", 0, 5000, "staging", "first claim").1,
            );
            tx.upsert(
                &("s1".to_string(), OBS_SEQ_BASE + 1),
                &obs("s1", 1, 5000, "staging", "second claim").1,
            );
        });
        let text = st.rtx(|(_, _, _, (subjects, _), _)| {
            subjects.get(&"staging".to_string()).map(|s: SubjStats| s.text)
        });
        assert_eq!(text.as_deref(), Some("second claim"));
    }
}

// ------------------------------------------------------------ capture path

const FIXTURE: &str = include_str!("../tests/fixtures/transcript-nx-rs-planread.jsonl");

#[test]
fn capture_fixture_parses_and_is_idempotent() {
    let parsed = crate::transcript::parse(FIXTURE, None).expect("fixture has a session id");
    assert!(
        parsed.events.len() >= 10,
        "fixture should yield a real event stream, got {}",
        parsed.events.len()
    );
    // shape: exactly one SessionMeta, at least one ToolCall and one FinalMsg
    let count = |f: fn(&Event) -> bool| parsed.events.iter().filter(|(_, e)| f(&e.kind)).count();
    assert_eq!(count(|e| matches!(e, Event::SessionMeta { .. })), 1);
    assert!(count(|e| matches!(e, Event::ToolCall { .. })) >= 8);
    assert_eq!(count(|e| matches!(e, Event::FinalMsg { .. })), 1);
    // ids are unique (the idempotency key)
    let mut ids: Vec<&EventId> = parsed.events.iter().map(|(id, _)| id).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), parsed.events.len(), "duplicate event ids");

    // capturing twice must equal capturing once
    let mut st = open!(tmp());
    st.wtx(|tx| {
        for (id, e) in &parsed.events {
            tx.upsert(id, e);
        }
    });
    let once = st.rtx(|(days, _, _, _, sessions)| {
        (
            days.iter().collect::<BTreeMap<u64, DayStats>>().len(),
            sessions.iter().count(),
        )
    });
    st.wtx(|tx| {
        for (id, e) in &parsed.events {
            tx.upsert(id, e);
        }
    });
    let twice = st.rtx(|(days, _, _, _, sessions)| {
        (
            days.iter().collect::<BTreeMap<u64, DayStats>>().len(),
            sessions.iter().count(),
        )
    });
    assert_eq!(once, twice);
}

#[test]
fn unknown_lines_never_fail() {
    let weird = r#"{"type":"mode","mode":"normal","sessionId":"sX"}
not even json
{"type":"totally-new-thing","payload":{"deep":[1,2,3]}}
{"type":"user","sessionId":"sX","timestamp":"2026-08-16T20:00:00.000Z","message":{"content":"hello"}}
"#;
    let parsed = crate::transcript::parse(weird, None).unwrap();
    assert_eq!(parsed.session, "sX");
    assert!(parsed
        .events
        .iter()
        .any(|(_, e)| matches!(&e.kind, Event::UserMsg { text } if text == "hello")));
}

#[test]
fn iso_timestamps_round_trip() {
    let ms = crate::transcript::iso_to_ms("2026-08-10T19:18:11.311Z").unwrap();
    assert_eq!(ms, 1786389491311);
    assert_eq!(crate::transcript::iso_to_ms("garbage"), None);
    // date_label is the inverse's date part
    assert_eq!(super::date_label(ms), "2026-08-10");
}
