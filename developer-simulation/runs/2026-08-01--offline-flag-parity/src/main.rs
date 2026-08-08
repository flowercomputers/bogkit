use std::collections::BTreeMap;
use std::env;
use std::fs::{self, File};
use std::hint::black_box;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use offline_flag_parity::{
    Condition, Context, EvaluationInput, Evaluator, Flag, GoldenCase, Operator, Percentage, Rule,
    Scalar, Snapshot, estimated_snapshot_heap, evaluate_snapshot, load_snapshot_file,
};
use serde::Serialize;
use serde_json::Value;

type CliResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn run() -> CliResult {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("generate") => generate(Path::new(&required_arg(&mut args, "output directory")?)),
        Some("verify") => verify(Path::new(&required_arg(&mut args, "fixture directory")?)),
        Some("demo") => demo(Path::new(&required_arg(&mut args, "fixture directory")?)),
        Some("eval") => evaluate_ndjson(
            Path::new(&required_arg(&mut args, "snapshot path")?),
            Path::new(&required_arg(&mut args, "NDJSON context path")?),
        ),
        Some("fingerprint") => {
            fingerprint(Path::new(&required_arg(&mut args, "fixture directory")?))
        }
        Some("generate-benchmark") => {
            generate_benchmark(Path::new(&required_arg(&mut args, "snapshot path")?))
        }
        Some("bench") => benchmark(Path::new(&required_arg(
            &mut args,
            "generated benchmark snapshot path",
        )?)),
        _ => {
            eprintln!(
                "usage: offline-flag-parity <generate DIR|verify DIR|demo DIR|eval SNAPSHOT NDJSON|fingerprint DIR|generate-benchmark SNAPSHOT|bench SNAPSHOT>"
            );
            std::process::exit(2);
        }
    }
}

fn required_arg(args: &mut impl Iterator<Item = String>, name: &str) -> CliResult<String> {
    args.next().ok_or_else(|| format!("missing {name}").into())
}

fn generate(directory: &Path) -> CliResult {
    fs::create_dir_all(directory)?;
    let malformed = directory.join("malformed");
    fs::create_dir_all(&malformed)?;

    let snapshot = demo_snapshot("demo-v1", false);
    write_json(directory.join("snapshot.json"), &snapshot)?;
    write_reordered_json(directory.join("snapshot-reordered.json"), &snapshot)?;
    write_json(
        directory.join("good-reload.json"),
        &demo_snapshot("demo-v2", true),
    )?;
    fs::write(
        directory.join("bad-reload.json"),
        br#"{"schema_version":1,"config_id":"bad","salt":"retail-kiosk-v1","flags":{"checkout_redesign":{"default":false,"rules":[{"id":"bad-rollout","conditions":[],"serve":true,"percentage":{"attribute":"user_id","basis_points":10001}}]}}}"#,
    )?;

    let evaluator = Evaluator::new(snapshot);
    let context_file = File::create(directory.join("contexts.ndjson"))?;
    let golden_file = File::create(directory.join("golden.json"))?;
    let mut contexts = BufWriter::new(context_file);
    let mut golden = Vec::with_capacity(250);
    for index in 0..250 {
        let input = golden_input(index);
        serde_json::to_writer(&mut contexts, &input)?;
        contexts.write_all(b"\n")?;
        let expected = evaluator.evaluate(&input.flag, &input.context)?;
        golden.push(GoldenCase { input, expected });
    }
    contexts.flush()?;
    serde_json::to_writer_pretty(BufWriter::new(golden_file), &golden)?;

    let malformed_cases = malformed_cases();
    for (name, contents) in &malformed_cases {
        fs::write(malformed.join(name), contents)?;
    }

    println!(
        "generated snapshot, reordered snapshot, 250 NDJSON contexts/golden cases, reload fixtures, and {} malformed snapshots in {}",
        malformed_cases.len(),
        directory.display()
    );
    Ok(())
}

fn verify(directory: &Path) -> CliResult {
    let snapshot = load_snapshot_file(directory.join("snapshot.json"))?;
    let reordered = load_snapshot_file(directory.join("snapshot-reordered.json"))?;
    let golden: Vec<GoldenCase> =
        serde_json::from_reader(File::open(directory.join("golden.json"))?)?;
    if golden.len() != 250 {
        return Err(format!("expected 250 golden cases, found {}", golden.len()).into());
    }

    for case in &golden {
        let actual = evaluate_snapshot(&snapshot, &case.input.flag, &case.input.context)?;
        if actual != case.expected {
            return Err(format!("golden mismatch for {}", case.input.case_id).into());
        }
        let reordered_actual =
            evaluate_snapshot(&reordered, &case.input.flag, &case.input.context)?;
        if reordered_actual != actual {
            return Err(format!("object-order mismatch for {}", case.input.case_id).into());
        }
    }

    let malformed_directory = directory.join("malformed");
    let mut malformed_paths: Vec<_> = fs::read_dir(&malformed_directory)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<_, _>>()?;
    malformed_paths.sort();
    for path in &malformed_paths {
        if load_snapshot_file(path).is_ok() {
            return Err(format!("malformed fixture activated: {}", path.display()).into());
        }
    }

    let mut evaluator = Evaluator::new(snapshot);
    let before = evaluator.evaluate("checkout_redesign", &golden[0].input.context)?;
    evaluator.reload_file(directory.join("good-reload.json"))?;
    let after_good = evaluator.evaluate("checkout_redesign", &golden[0].input.context)?;
    if evaluator.config_id() != "demo-v2" {
        return Err("good reload did not activate demo-v2".into());
    }
    let rejected = evaluator.reload_file(directory.join("bad-reload.json"));
    if rejected.is_ok() || evaluator.config_id() != "demo-v2" {
        return Err("bad reload did not preserve demo-v2".into());
    }
    let after_bad = evaluator.evaluate("checkout_redesign", &golden[0].input.context)?;
    if after_bad != after_good {
        return Err("bad reload partially changed active decisions".into());
    }

    println!(
        "verified {} golden cases; object ordering invariant; {} malformed snapshots rejected; good reload {} -> {}; bad reload preserved {}",
        golden.len(),
        malformed_paths.len(),
        before.value,
        after_good.value,
        after_bad.value
    );
    Ok(())
}

fn demo(directory: &Path) -> CliResult {
    let snapshot = load_snapshot_file(directory.join("snapshot.json"))?;
    let mut evaluator = Evaluator::new(snapshot);
    let inputs = read_inputs(&directory.join("contexts.ndjson"))?;

    println!("active config: {}", evaluator.config_id());
    for input in inputs.iter().take(4) {
        let decision = evaluator.evaluate(&input.flag, &input.context)?;
        println!("{}", serde_json::to_string(&decision)?);
    }

    evaluator.reload_file(directory.join("good-reload.json"))?;
    let after_good = evaluator.evaluate("checkout_redesign", &inputs[0].context)?;
    println!(
        "good reload activated config {}: {}",
        evaluator.config_id(),
        serde_json::to_string(&after_good)?
    );

    let error = evaluator
        .reload_file(directory.join("bad-reload.json"))
        .expect_err("bad reload should fail");
    let after_bad = evaluator.evaluate("checkout_redesign", &inputs[0].context)?;
    println!(
        "bad reload rejected ({error}); active config remains {}: {}",
        evaluator.config_id(),
        serde_json::to_string(&after_bad)?
    );
    Ok(())
}

fn evaluate_ndjson(snapshot_path: &Path, contexts_path: &Path) -> CliResult {
    let evaluator = Evaluator::new(load_snapshot_file(snapshot_path)?);
    for input in read_inputs(contexts_path)? {
        let decision = evaluator.evaluate(&input.flag, &input.context)?;
        println!("{}", serde_json::to_string(&decision)?);
    }
    Ok(())
}

fn fingerprint(directory: &Path) -> CliResult {
    let evaluator = Evaluator::new(load_snapshot_file(directory.join("snapshot.json"))?);
    let mut hash = 0xcbf29ce484222325u64;
    for input in read_inputs(&directory.join("contexts.ndjson"))? {
        let decision = evaluator.evaluate(&input.flag, &input.context)?;
        let bytes = serde_json::to_vec(&decision)?;
        for byte in bytes {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    println!("{hash:016x}");
    Ok(())
}

fn generate_benchmark(path: &Path) -> CliResult {
    let snapshot = benchmark_snapshot();
    serde_json::to_writer(BufWriter::new(File::create(path)?), &snapshot)?;
    let bytes = fs::metadata(path)?.len();
    println!(
        "generated 5000-flag/50000-rule benchmark snapshot: {} bytes at {}",
        bytes,
        path.display()
    );
    Ok(())
}

fn benchmark(path: &Path) -> CliResult {
    let snapshot_bytes = fs::metadata(path)?.len();
    let snapshot = load_snapshot_file(path)?;

    let flag_count = snapshot.flags.len();
    let rule_count: usize = snapshot.flags.values().map(|flag| flag.rules.len()).sum();
    let heap_estimate = estimated_snapshot_heap(&snapshot);
    let mut evaluator = Evaluator::new(snapshot);
    let (reload_result, peak_reload_rss_bytes) = sample_peak_rss(|| evaluator.reload_file(path));
    reload_result?;
    let mut checksum = 0u64;
    let mut p95_values = Vec::new();

    for round in 0..5u64 {
        let mut timings = Vec::with_capacity(20_000);
        for index in 0..20_000u64 {
            let flag = format!("flag-{}", (index * 2_653 + round * 977) % 5_000);
            let context = benchmark_context(index, round);
            let start = Instant::now();
            let decision = evaluator.evaluate(&flag, &context)?;
            let elapsed = start.elapsed();
            black_box(&decision);
            timings.push(elapsed);
            checksum = checksum.wrapping_add(decision.source.len() as u64);
        }
        timings.sort_unstable();
        let p95 = timings[(timings.len() * 95) / 100];
        p95_values.push(p95);
        println!("round {} p95_ns={}", round + 1, p95.as_nanos());
    }
    p95_values.sort_unstable();
    let median_p95 = p95_values[p95_values.len() / 2];
    let max_p95 = *p95_values.last().expect("five p95 values");
    let current_rss_bytes = current_rss_bytes().unwrap_or(0);
    println!(
        "benchmark flags={flag_count} rules={rule_count} evaluations=100000 snapshot_bytes={snapshot_bytes} estimated_active_heap_bytes={heap_estimate} sampled_peak_same_size_reload_rss_bytes={peak_reload_rss_bytes} current_rss_bytes={current_rss_bytes} median_p95_ns={} max_p95_ns={} checksum={checksum}",
        median_p95.as_nanos(),
        max_p95.as_nanos()
    );
    if max_p95 >= Duration::from_micros(250) {
        return Err(format!("maximum measured p95 {max_p95:?} exceeded 250us").into());
    }
    if peak_reload_rss_bytes >= 64 * 1024 * 1024 {
        return Err(
            format!("sampled peak reload RSS {peak_reload_rss_bytes} exceeded 64 MiB").into(),
        );
    }
    Ok(())
}

fn sample_peak_rss<T>(work: impl FnOnce() -> T) -> (T, u64) {
    let running = Arc::new(AtomicBool::new(true));
    let maximum = Arc::new(AtomicU64::new(current_rss_bytes().unwrap_or(0)));
    let sampler_running = Arc::clone(&running);
    let sampler_maximum = Arc::clone(&maximum);
    let sampler = thread::spawn(move || {
        while sampler_running.load(Ordering::Relaxed) {
            if let Some(rss) = current_rss_bytes() {
                sampler_maximum.fetch_max(rss, Ordering::Relaxed);
            }
            thread::sleep(Duration::from_millis(1));
        }
    });
    let result = work();
    if let Some(rss) = current_rss_bytes() {
        maximum.fetch_max(rss, Ordering::Relaxed);
    }
    running.store(false, Ordering::Relaxed);
    sampler.join().expect("RSS sampler thread should not panic");
    (result, maximum.load(Ordering::Relaxed))
}

fn current_rss_bytes() -> Option<u64> {
    let output = Command::new("/bin/ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let kib = String::from_utf8(output.stdout)
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    Some(kib * 1024)
}

fn demo_snapshot(config_id: &str, reloaded: bool) -> Snapshot {
    let checkout_rules = if reloaded {
        vec![Rule {
            id: "all-kiosks-after-reload".to_string(),
            conditions: vec![Condition {
                attribute: "kiosk".to_string(),
                op: Operator::Eq,
                value: Scalar::Bool(true),
            }],
            serve: Scalar::Bool(true),
            percentage: None,
        }]
    } else {
        vec![
            Rule {
                id: "employees-first".to_string(),
                conditions: vec![Condition {
                    attribute: "tier".to_string(),
                    op: Operator::Eq,
                    value: Scalar::String("employee".to_string()),
                }],
                serve: Scalar::Bool(true),
                percentage: None,
            },
            Rule {
                id: "kiosk-rollout".to_string(),
                conditions: vec![Condition {
                    attribute: "kiosk".to_string(),
                    op: Operator::Eq,
                    value: Scalar::Bool(true),
                }],
                serve: Scalar::Bool(true),
                percentage: Some(Percentage {
                    attribute: "user_id".to_string(),
                    basis_points: 3_500,
                }),
            },
        ]
    };

    Snapshot {
        schema_version: 1,
        config_id: config_id.to_string(),
        salt: "retail-kiosk-v1".to_string(),
        flags: BTreeMap::from([
            (
                "checkout_redesign".to_string(),
                Flag {
                    default: Scalar::Bool(false),
                    rules: checkout_rules,
                },
            ),
            (
                "max_cart_items".to_string(),
                Flag {
                    default: Scalar::Number(30.0),
                    rules: vec![Rule {
                        id: "large-store".to_string(),
                        conditions: vec![Condition {
                            attribute: "store_size".to_string(),
                            op: Operator::GreaterThan,
                            value: Scalar::Number(20_000.0),
                        }],
                        serve: Scalar::Number(50.0),
                        percentage: None,
                    }],
                },
            ),
            (
                "receipt_style".to_string(),
                Flag {
                    default: Scalar::String("compact".to_string()),
                    rules: vec![Rule {
                        id: "accessible-store".to_string(),
                        conditions: vec![Condition {
                            attribute: "accessibility_mode".to_string(),
                            op: Operator::Eq,
                            value: Scalar::Bool(true),
                        }],
                        serve: Scalar::String("large-print".to_string()),
                        percentage: None,
                    }],
                },
            ),
            (
                "support_prompt".to_string(),
                Flag {
                    default: Scalar::Bool(false),
                    rules: vec![Rule {
                        id: "non-guest".to_string(),
                        conditions: vec![Condition {
                            attribute: "tier".to_string(),
                            op: Operator::NotEq,
                            value: Scalar::String("guest".to_string()),
                        }],
                        serve: Scalar::Bool(true),
                        percentage: None,
                    }],
                },
            ),
        ]),
    }
}

fn golden_input(index: usize) -> EvaluationInput {
    let flags = [
        "checkout_redesign",
        "max_cart_items",
        "receipt_style",
        "support_prompt",
    ];
    let mut context = Context::new();
    context.insert(
        "user_id".to_string(),
        Scalar::String(format!("synthetic-user-{index:03}")),
    );
    context.insert("kiosk".to_string(), Scalar::Bool(!index.is_multiple_of(3)));
    context.insert(
        "tier".to_string(),
        Scalar::String(
            match index % 5 {
                0 => "employee",
                1 => "guest",
                _ => "member",
            }
            .to_string(),
        ),
    );
    context.insert(
        "store_size".to_string(),
        Scalar::Number((8_000 + (index * 257) % 25_000) as f64),
    );
    context.insert(
        "accessibility_mode".to_string(),
        Scalar::Bool(index.is_multiple_of(7)),
    );
    EvaluationInput {
        case_id: format!("golden-{index:03}"),
        flag: flags[index % flags.len()].to_string(),
        context,
    }
}

fn benchmark_snapshot() -> Snapshot {
    let mut flags = BTreeMap::new();
    for flag_index in 0..5_000usize {
        let mut rules = Vec::with_capacity(10);
        for rule_index in 0..10usize {
            rules.push(Rule {
                id: format!("r{rule_index}"),
                conditions: vec![Condition {
                    attribute: "segment".to_string(),
                    op: Operator::Eq,
                    value: Scalar::Number(rule_index as f64),
                }],
                serve: Scalar::Bool((flag_index + rule_index) % 2 == 0),
                percentage: if rule_index % 3 == 0 {
                    Some(Percentage {
                        attribute: "user_id".to_string(),
                        basis_points: 7_500,
                    })
                } else {
                    None
                },
            });
        }
        flags.insert(
            format!("flag-{flag_index}"),
            Flag {
                default: Scalar::Bool(false),
                rules,
            },
        );
    }
    Snapshot {
        schema_version: 1,
        config_id: "synthetic-5000x10".to_string(),
        salt: "benchmark-salt".to_string(),
        flags,
    }
}

fn benchmark_context(index: u64, round: u64) -> Context {
    BTreeMap::from([
        (
            "segment".to_string(),
            Scalar::Number(((index * 7 + round * 3) % 12) as f64),
        ),
        (
            "user_id".to_string(),
            Scalar::String(format!("bench-user-{index}-{round}")),
        ),
        ("kiosk".to_string(), Scalar::Bool(true)),
    ])
}

fn malformed_cases() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("01-syntax.json", b"{not-json".to_vec()),
        (
            "02-unknown-field.json",
            br#"{"schema_version":1,"config_id":"x","salt":"s","extra":true,"flags":{"f":{"default":false,"rules":[]}}}"#.to_vec(),
        ),
        (
            "03-wrong-schema.json",
            br#"{"schema_version":2,"config_id":"x","salt":"s","flags":{"f":{"default":false,"rules":[]}}}"#.to_vec(),
        ),
        (
            "04-empty-id.json",
            br#"{"schema_version":1,"config_id":"","salt":"s","flags":{"f":{"default":false,"rules":[]}}}"#.to_vec(),
        ),
        (
            "05-empty-salt.json",
            br#"{"schema_version":1,"config_id":"x","salt":"","flags":{"f":{"default":false,"rules":[]}}}"#.to_vec(),
        ),
        (
            "06-duplicate-flag-key.json",
            br#"{"schema_version":1,"config_id":"x","salt":"s","flags":{"f":{"default":false,"rules":[]},"f":{"default":true,"rules":[]}}}"#.to_vec(),
        ),
        (
            "07-duplicate-rule-id.json",
            br#"{"schema_version":1,"config_id":"x","salt":"s","flags":{"f":{"default":false,"rules":[{"id":"r","conditions":[{"attribute":"x","op":"eq","value":true}],"serve":true},{"id":"r","conditions":[{"attribute":"x","op":"eq","value":false}],"serve":false}]}}}"#.to_vec(),
        ),
        (
            "08-empty-rule.json",
            br#"{"schema_version":1,"config_id":"x","salt":"s","flags":{"f":{"default":false,"rules":[{"id":"r","conditions":[],"serve":true}]}}}"#.to_vec(),
        ),
        (
            "09-invalid-percentage.json",
            br#"{"schema_version":1,"config_id":"x","salt":"s","flags":{"f":{"default":false,"rules":[{"id":"r","conditions":[],"serve":true,"percentage":{"attribute":"user_id","basis_points":10001}}]}}}"#.to_vec(),
        ),
        (
            "10-invalid-operator-type.json",
            br#"{"schema_version":1,"config_id":"x","salt":"s","flags":{"f":{"default":false,"rules":[{"id":"r","conditions":[{"attribute":"age","op":"greater_than","value":"old"}],"serve":true}]}}}"#.to_vec(),
        ),
        (
            "11-null-scalar.json",
            br#"{"schema_version":1,"config_id":"x","salt":"s","flags":{"f":{"default":null,"rules":[]}}}"#.to_vec(),
        ),
        (
            "12-missing-flags.json",
            br#"{"schema_version":1,"config_id":"x","salt":"s"}"#.to_vec(),
        ),
    ]
}

fn read_inputs(path: &Path) -> CliResult<Vec<EvaluationInput>> {
    let reader = BufReader::new(File::open(path)?);
    let mut inputs = Vec::new();
    for (index, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let input = serde_json::from_str(&line)
            .map_err(|error| format!("{} line {}: {error}", path.display(), index + 1))?;
        inputs.push(input);
    }
    Ok(inputs)
}

fn write_json(path: PathBuf, value: &impl Serialize) -> CliResult {
    serde_json::to_writer_pretty(BufWriter::new(File::create(path)?), value)?;
    Ok(())
}

fn write_reordered_json(path: PathBuf, value: &impl Serialize) -> CliResult {
    let value = serde_json::to_value(value)?;
    let mut writer = BufWriter::new(File::create(path)?);
    write_value_reverse(&mut writer, &value)?;
    writer.write_all(b"\n")?;
    Ok(())
}

fn write_value_reverse(writer: &mut impl Write, value: &Value) -> CliResult {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
            serde_json::to_writer(writer, value)?;
        }
        Value::Array(values) => {
            writer.write_all(b"[")?;
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    writer.write_all(b",")?;
                }
                write_value_reverse(writer, value)?;
            }
            writer.write_all(b"]")?;
        }
        Value::Object(values) => {
            writer.write_all(b"{")?;
            for (index, (key, value)) in values.iter().rev().enumerate() {
                if index > 0 {
                    writer.write_all(b",")?;
                }
                serde_json::to_writer(&mut *writer, key)?;
                writer.write_all(b":")?;
                write_value_reverse(writer, value)?;
            }
            writer.write_all(b"}")?;
        }
    }
    Ok(())
}
