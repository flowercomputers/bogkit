use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::PathBuf;

use caldav_recurrence_prototype::Event;

fn main() {
    let output_dir = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("fixtures/generated"));
    fs::create_dir_all(&output_dir).expect("create fixture directory");

    let events_path = output_dir.join("workload-events.jsonl");
    let file = File::create(&events_path).expect("create workload events");
    let mut writer = BufWriter::new(file);
    for index in 0..5_000 {
        let event = Event {
            uid: format!("workload-{index:04}"),
            kind: "timed".to_string(),
            start: "2026-01-01T09:00:00Z".to_string(),
            end: "2026-01-01T10:00:00Z".to_string(),
            tzid: Some("UTC".to_string()),
            rrule: Some("FREQ=DAILY;COUNT=400".to_string()),
            exdate: Vec::new(),
            overrides: Vec::new(),
        };
        serde_json::to_writer(&mut writer, &event).expect("encode workload event");
        writer.write_all(b"\n").expect("write workload event");
    }
    writer.flush().expect("flush workload events");

    let mut zones = String::from("{\n  \"zones\": {\n");
    for index in 0..100 {
        if index > 0 {
            zones.push_str(",\n");
        }
        write!(
            zones,
            "    \"Fixture/Zone{index:03}\": {{\"initial_offset_seconds\":0,\"transitions\":[]}}"
        )
        .expect("format fixture zone");
    }
    zones.push_str(
        ",\n    \"FLOATING\": {\"initial_offset_seconds\":0,\"transitions\":[]}\n  }\n}\n",
    );
    fs::write(output_dir.join("workload-zones.json"), zones).expect("write workload zones");
    println!("generated 5000 masters and 2000000 candidate occurrences");
}
