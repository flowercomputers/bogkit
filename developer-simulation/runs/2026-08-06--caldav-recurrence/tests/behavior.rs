use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use caldav_recurrence_prototype::{Config, Event, Occurrence, Override, run};
use fold::pipeline::terminal;

struct FixtureDir(PathBuf);

impl FixtureDir {
    fn new(label: &str) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "bogkit-caldav-{label}-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("fixture directory");
        Self(path)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    fn write_events(&self, name: &str, events: &[Event]) -> PathBuf {
        let path = self.path(name);
        let body = events
            .iter()
            .map(|event| serde_json::to_string(event).expect("event JSON"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&path, format!("{body}\n")).expect("events");
        path
    }

    fn write_edits(&self, name: &str, edits: &[serde_json::Value]) -> PathBuf {
        let path = self.path(name);
        let body = edits
            .iter()
            .map(|edit| serde_json::to_string(edit).expect("edit JSON"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&path, format!("{body}\n")).expect("edits");
        path
    }

    fn zones(&self) -> PathBuf {
        let path = self.path("zones.json");
        fs::write(
            &path,
            r#"{
  "zones": {
    "America/New_York": {
      "initial_offset_seconds": -18000,
      "transitions": [
        {"at_utc":"2026-03-08T07:00:00Z","offset_after_seconds":-14400},
        {"at_utc":"2026-11-01T06:00:00Z","offset_after_seconds":-18000}
      ]
    },
    "FLOATING": {
      "initial_offset_seconds": 0,
      "transitions": []
    }
  }
}
"#,
        )
        .expect("zones");
        path
    }

    fn config(&self, events: &Path, output: &str, state: &str) -> Config {
        Config {
            events: events.to_path_buf(),
            transitions: self.zones(),
            from: "2026-03-07T00:00:00Z".to_string(),
            to: "2026-03-12T00:00:00Z".to_string(),
            output: self.path(output),
            state_dir: self.path(state),
            edits: None,
            crash_after_uid: None,
        }
    }
}

impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn timed(uid: &str, start: &str, end: &str, tzid: Option<&str>, rrule: Option<&str>) -> Event {
    Event {
        uid: uid.to_string(),
        kind: "timed".to_string(),
        start: start.to_string(),
        end: end.to_string(),
        tzid: tzid.map(str::to_string),
        rrule: rrule.map(str::to_string),
        exdate: Vec::new(),
        overrides: Vec::new(),
    }
}

#[test]
fn dst_exceptions_all_day_and_floating_times_are_deterministic() {
    let fixture = FixtureDir::new("semantics");
    let mut recurring = timed(
        "meeting",
        "2026-03-07T09:00:00",
        "2026-03-07T10:00:00",
        Some("America/New_York"),
        Some("FREQ=DAILY;COUNT=4"),
    );
    recurring.exdate.push("2026-03-09T09:00:00".to_string());
    recurring.overrides.push(Override {
        recurrence_id: "2026-03-08T09:00:00".to_string(),
        status: None,
        start: Some("2026-03-08T11:00:00".to_string()),
        end: Some("2026-03-08T12:00:00".to_string()),
        tzid: None,
    });
    recurring.overrides.push(Override {
        recurrence_id: "2026-03-10T09:00:00".to_string(),
        status: Some("cancelled".to_string()),
        start: None,
        end: None,
        tzid: None,
    });

    let gap = timed(
        "gap",
        "2026-03-08T02:30:00",
        "2026-03-08T04:00:00",
        Some("America/New_York"),
        None,
    );
    let fold = timed(
        "fold",
        "2026-11-01T01:30:00",
        "2026-11-01T02:30:00",
        Some("America/New_York"),
        None,
    );
    let all_day = Event {
        uid: "all-day".to_string(),
        kind: "all_day".to_string(),
        start: "2026-03-08".to_string(),
        end: "2026-03-09".to_string(),
        tzid: None,
        rrule: None,
        exdate: Vec::new(),
        overrides: Vec::new(),
    };
    let floating = timed(
        "floating",
        "2026-03-08T09:00:00",
        "2026-03-08T10:00:00",
        None,
        None,
    );
    let events = vec![recurring, gap, fold, all_day, floating];
    let events_path = fixture.write_events("events.jsonl", &events);
    let mut config = fixture.config(&events_path, "out.jsonl", "state");
    config.to = "2026-11-02T00:00:00Z".to_string();
    let result = run(&config).expect("prototype run");
    assert_eq!(result.events, 5);

    let lines = fs::read_to_string(&config.output)
        .expect("output")
        .lines()
        .map(|line| serde_json::from_str::<Occurrence>(line).expect("occurrence JSON"))
        .collect::<Vec<_>>();
    let find = |uid: &str, recurrence_id: &str| {
        lines
            .iter()
            .find(|occurrence| occurrence.uid == uid && occurrence.recurrence_id == recurrence_id)
            .expect("occurrence")
    };
    assert_eq!(
        find("gap", "2026-03-08T02:30:00").start,
        "2026-03-08T07:30:00Z"
    );
    assert_eq!(
        find("fold", "2026-11-01T01:30:00").start,
        "2026-11-01T05:30:00Z"
    );
    assert_eq!(find("all-day", "2026-03-08").start, "2026-03-08");
    assert_eq!(
        find("floating", "2026-03-08T09:00:00").start,
        "2026-03-08T09:00:00Z"
    );
    assert!(lines.iter().all(|occurrence| {
        !(occurrence.uid == "meeting" && occurrence.recurrence_id == "2026-03-09T09:00:00")
    }));
    assert!(lines.iter().all(|occurrence| {
        !(occurrence.uid == "meeting" && occurrence.recurrence_id == "2026-03-10T09:00:00")
    }));
    assert!(lines.windows(2).all(|pair| {
        (pair[0].uid.as_str(), pair[0].recurrence_id.as_str())
            < (pair[1].uid.as_str(), pair[1].recurrence_id.as_str())
    }));
}

#[test]
fn input_order_and_host_environment_do_not_change_output() {
    let fixture = FixtureDir::new("ordering");
    let events = vec![
        timed(
            "z",
            "2026-03-07T09:00:00Z",
            "2026-03-07T10:00:00Z",
            Some("UTC"),
            Some("FREQ=DAILY;COUNT=2"),
        ),
        timed(
            "a",
            "2026-03-07T11:00:00Z",
            "2026-03-07T12:00:00Z",
            Some("UTC"),
            Some("FREQ=DAILY;COUNT=2"),
        ),
    ];
    let first_path = fixture.write_events("first.jsonl", &events);
    let reversed = vec![events[1].clone(), events[0].clone()];
    let second_path = fixture.write_events("second.jsonl", &reversed);
    let first = fixture.config(&first_path, "first.out", "first.state");
    let second = fixture.config(&second_path, "second.out", "second.state");
    run(&first).expect("first run");
    run(&second).expect("second run");
    assert_eq!(
        fs::read(&first.output).expect("first output"),
        fs::read(&second.output).expect("second output")
    );
}

#[test]
fn one_event_edit_rebuilds_one_shard_and_resume_recovers_after_interruption() {
    let fixture = FixtureDir::new("incremental");
    let events = vec![
        timed(
            "keep",
            "2026-03-07T09:00:00Z",
            "2026-03-07T10:00:00Z",
            Some("UTC"),
            Some("FREQ=DAILY;COUNT=2"),
        ),
        timed(
            "change",
            "2026-03-07T11:00:00Z",
            "2026-03-07T12:00:00Z",
            Some("UTC"),
            Some("FREQ=DAILY;COUNT=2"),
        ),
    ];
    let events_path = fixture.write_events("events.jsonl", &events);
    let edits_path = fixture.write_edits(
        "edits.jsonl",
        &[serde_json::json!({
            "uid":"change",
            "event": timed("change", "2026-03-07T15:00:00Z", "2026-03-07T16:00:00Z", Some("UTC"), Some("FREQ=DAILY;COUNT=2"))
        })],
    );
    let mut config = fixture.config(&events_path, "out.jsonl", "state");
    run(&config).expect("initial run");
    let keep_shard = fs::read_dir(config.state_dir.join("shards"))
        .expect("shards")
        .map(|entry| entry.expect("entry").path())
        .find(|path| {
            fs::read_to_string(path)
                .expect("shard")
                .contains("\"keep\"")
        })
        .expect("keep shard");
    let keep_before = fs::read(&keep_shard).expect("keep before");
    let old_output = fs::read(&config.output).expect("old output");

    config.edits = Some(edits_path.clone());
    config.crash_after_uid = Some("change".to_string());
    assert!(run(&config).is_err());
    assert_eq!(
        fs::read(&config.output).expect("output after crash"),
        old_output
    );

    config.crash_after_uid = None;
    let result = run(&config).expect("resumed run");
    assert_eq!(result.rebuilt_uids, 1);
    assert_eq!(result.reused_uids, 1);
    assert_eq!(fs::read(&keep_shard).expect("keep after"), keep_before);
    assert!(
        fs::read_to_string(&config.output)
            .expect("new output")
            .contains("2026-03-07T15:00:00Z")
    );
}

#[test]
fn malformed_input_does_not_publish_or_modify_existing_output() {
    let fixture = FixtureDir::new("validation");
    let valid = fixture.write_events(
        "valid.jsonl",
        &[timed(
            "one",
            "2026-03-07T09:00:00Z",
            "2026-03-07T10:00:00Z",
            Some("UTC"),
            None,
        )],
    );
    let mut config = fixture.config(&valid, "out.jsonl", "state");
    run(&config).expect("valid run");
    let before = fs::read(&config.output).expect("before");

    let invalid = fixture.path("invalid.jsonl");
    fs::write(&invalid, "{\"uid\":\"broken\",\"kind\":\"timed\"}\n").expect("invalid input");
    config.events = invalid;
    assert!(run(&config).is_err());
    assert_eq!(fs::read(&config.output).expect("after"), before);
}

#[test]
fn weekly_and_monthly_rules_use_calendar_steps() {
    let fixture = FixtureDir::new("calendar-steps");
    let weekly = timed(
        "weekly",
        "2026-03-02T09:00:00",
        "2026-03-02T10:00:00",
        Some("America/New_York"),
        Some("FREQ=WEEKLY;BYDAY=MO,WE;COUNT=4"),
    );
    let monthly = timed(
        "monthly",
        "2026-01-15T09:00:00Z",
        "2026-01-15T10:00:00Z",
        Some("UTC"),
        Some("FREQ=MONTHLY;BYMONTHDAY=15;COUNT=3"),
    );
    let events = fixture.write_events("events.jsonl", &[weekly, monthly]);
    let mut config = fixture.config(&events, "out.jsonl", "state");
    config.from = "2026-01-01T00:00:00Z".to_string();
    config.to = "2026-04-01T00:00:00Z".to_string();
    let result = run(&config).expect("calendar run");
    assert_eq!(result.occurrences, 7);
    let output = fs::read_to_string(&config.output).expect("output");
    assert!(output.contains("2026-03-09T13:00:00Z"));
    assert!(output.contains("2026-03-11T13:00:00Z"));
    assert!(output.contains("2026-01-15T09:00:00Z"));
    assert!(output.contains("2026-02-15T09:00:00Z"));
    assert!(output.contains("2026-03-15T09:00:00Z"));
}

#[test]
fn partial_day_query_includes_the_touched_all_day_date() {
    let fixture = FixtureDir::new("all-day-window");
    let events = fixture.write_events(
        "events.jsonl",
        &[Event {
            uid: "day".to_string(),
            kind: "all_day".to_string(),
            start: "2026-03-08".to_string(),
            end: "2026-03-09".to_string(),
            tzid: None,
            rrule: None,
            exdate: Vec::new(),
            overrides: Vec::new(),
        }],
    );
    let mut config = fixture.config(&events, "out.jsonl", "state");
    config.from = "2026-03-08T00:00:00Z".to_string();
    config.to = "2026-03-08T12:00:00Z".to_string();
    let result = run(&config).expect("partial-day query");
    assert_eq!(result.occurrences, 1);
    assert!(
        fs::read_to_string(&config.output)
            .expect("output")
            .contains("\"recurrence_id\":\"2026-03-08\"")
    );
}

#[test]
fn duplicate_canonical_override_ids_are_rejected() {
    let fixture = FixtureDir::new("canonical-overrides");
    let mut event = timed(
        "duplicate",
        "2026-03-08T09:00:00Z",
        "2026-03-08T10:00:00Z",
        Some("UTC"),
        Some("FREQ=DAILY;COUNT=2"),
    );
    event.overrides = vec![
        Override {
            recurrence_id: "2026-03-08T09:00:00Z".to_string(),
            status: None,
            start: Some("2026-03-08T11:00:00Z".to_string()),
            end: Some("2026-03-08T12:00:00Z".to_string()),
            tzid: None,
        },
        Override {
            recurrence_id: "2026-03-08T09:00:00+00:00".to_string(),
            status: None,
            start: Some("2026-03-08T13:00:00Z".to_string()),
            end: Some("2026-03-08T14:00:00Z".to_string()),
            tzid: None,
        },
    ];
    let events = fixture.write_events("events.jsonl", &[event]);
    let config = fixture.config(&events, "out.jsonl", "state");
    assert!(run(&config).is_err());
}

#[test]
fn override_for_an_unseen_recurrence_is_rejected() {
    let fixture = FixtureDir::new("unseen-override");
    let mut event = timed(
        "unseen",
        "2026-03-08T09:00:00Z",
        "2026-03-08T10:00:00Z",
        Some("UTC"),
        None,
    );
    event.overrides.push(Override {
        recurrence_id: "2026-03-09T09:00:00Z".to_string(),
        status: None,
        start: Some("2026-03-09T11:00:00Z".to_string()),
        end: Some("2026-03-09T12:00:00Z".to_string()),
        tzid: None,
    });
    let events = fixture.write_events("events.jsonl", &[event]);
    let config = fixture.config(&events, "out.jsonl", "state");
    assert!(run(&config).is_err());
}

#[test]
fn tampered_shards_are_rebuilt_before_reuse() {
    let fixture = FixtureDir::new("shard-integrity");
    let event = timed(
        "tamper",
        "2026-03-08T09:00:00Z",
        "2026-03-08T10:00:00Z",
        Some("UTC"),
        None,
    );
    let events = fixture.write_events("events.jsonl", &[event]);
    let config = fixture.config(&events, "out.jsonl", "state");
    run(&config).expect("initial run");
    let shard = fs::read_dir(config.state_dir.join("shards"))
        .expect("shards")
        .map(|entry| entry.expect("entry").path())
        .next()
        .expect("one shard");
    fs::write(
        &shard,
        br#"{"uid":"tamper","recurrence_id":"2026-03-08T09:00:00Z","kind":"timed","start":"2030-01-01T00:00:00Z","end":"2030-01-01T01:00:00Z"}
"#,
    )
    .expect("tampered shard");
    let result = run(&config).expect("rebuild run");
    assert_eq!(result.rebuilt_uids, 1);
    assert_eq!(result.reused_uids, 0);
    assert!(
        fs::read_to_string(&config.output)
            .expect("output")
            .contains("2026-03-08T09:00:00Z")
    );
}

#[test]
fn failed_expansion_does_not_mutate_the_durable_event_store() {
    let fixture = FixtureDir::new("preflight-store");
    let valid_event = timed(
        "stable",
        "2026-03-08T09:00:00",
        "2026-03-08T10:00:00",
        Some("America/New_York"),
        None,
    );
    let valid = fixture.write_events("valid.jsonl", std::slice::from_ref(&valid_event));
    let mut config = fixture.config(&valid, "out.jsonl", "state");
    run(&config).expect("valid run");

    let invalid_event = timed(
        "stable",
        "2026-03-08T02:30:00",
        "2026-03-08T03:00:00",
        Some("America/New_York"),
        None,
    );
    let invalid = fixture.write_events("invalid.jsonl", &[invalid_event]);
    config.events = invalid;
    assert!(run(&config).is_err());

    let store = caldav_recurrence_prototype::EventStore::new(
        config.state_dir.join("event-store"),
        terminal::Table::new("events"),
    );
    let stored_start = store.rtx(|table| {
        table
            .iter()
            .find(|(uid, _)| uid == "stable")
            .map(|(_, event)| event.start)
    });
    assert_eq!(stored_start.as_deref(), Some("2026-03-08T09:00:00"));
}
