use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

use fold::pipeline::{Filter, Keyed, terminal};
use fold::stream::KeyedStream;
use serde::{Deserialize, Serialize};

const LEASE_MS: u64 = 30_000;
const FORCED_EXIT_CODE: i32 = 77;

type JobTable = terminal::Table<u32, Job>;
type JobFilter = Filter<Keyed<u32, Job>, fn(&Keyed<u32, Job>) -> bool, JobTable>;
type JobPipeline = (JobTable, JobFilter, JobFilter);
type JobStore = KeyedStream<u32, Job, JobPipeline>;

#[derive(Clone, Debug, Deserialize, Serialize)]
enum Status {
    Pending,
    Ready,
    Leased { owner: u32, deadline_ms: u64 },
    Completed { attempt: u32, result_key: String },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Job {
    build_id: u32,
    dependencies: Vec<u32>,
    dependents: Vec<u32>,
    attempt: u32,
    status: Status,
    last_heartbeat_message_id: u64,
    terminal_message_id: Option<u64>,
    version: u64,
    reason: String,
}

#[derive(Debug)]
struct Outcome {
    changed: bool,
    detail: String,
}

#[derive(Clone, Copy)]
struct Heartbeat {
    job_id: u32,
    worker: u32,
    attempt: u32,
    message_id: u64,
    coordinator_now_ms: u64,
}

struct Coordinator {
    store: JobStore,
}

impl Coordinator {
    fn open(path: &Path) -> Self {
        let pipeline = (
            terminal::Table::new("jobs"),
            Filter::new(
                is_ready as fn(&Keyed<u32, Job>) -> bool,
                terminal::Table::new("ready"),
            ),
            Filter::new(
                is_leased as fn(&Keyed<u32, Job>) -> bool,
                terminal::Table::new("leased"),
            ),
        );
        Self {
            store: KeyedStream::new(path, pipeline),
        }
    }

    fn seed_pairs(&mut self, pairs: u32) {
        self.store.wtx(|tx| {
            for pair in 0..pairs {
                let root = pair * 2;
                let child = root + 1;
                tx.upsert(
                    &root,
                    &Job {
                        build_id: pair,
                        dependencies: vec![],
                        dependents: vec![child],
                        attempt: 0,
                        status: Status::Ready,
                        last_heartbeat_message_id: 0,
                        terminal_message_id: None,
                        version: 1,
                        reason: "ready: no dependencies".to_string(),
                    },
                );
                tx.upsert(
                    &child,
                    &Job {
                        build_id: pair,
                        dependencies: vec![root],
                        dependents: vec![],
                        attempt: 0,
                        status: Status::Pending,
                        last_heartbeat_message_id: 0,
                        terminal_message_id: None,
                        version: 1,
                        reason: format!("blocked by unfinished job {root}"),
                    },
                );
            }
            let fencing_job = pairs * 2;
            tx.upsert(
                &fencing_job,
                &Job {
                    build_id: pairs,
                    dependencies: vec![],
                    dependents: vec![],
                    attempt: 0,
                    status: Status::Ready,
                    last_heartbeat_message_id: 0,
                    terminal_message_id: None,
                    version: 1,
                    reason: "ready: no dependencies".to_string(),
                },
            );
        });
    }

    fn seed_benchmark_graph(&mut self, builds: u32, jobs_per_build: u32) {
        self.store.wtx(|tx| {
            for build in 0..builds {
                for offset in 0..jobs_per_build {
                    let id = build * jobs_per_build + offset;
                    let dependencies = if offset > 0 { vec![id - 1] } else { Vec::new() };
                    let dependents = if offset + 1 < jobs_per_build {
                        vec![id + 1]
                    } else {
                        Vec::new()
                    };
                    let (status, reason) = if dependencies.is_empty() {
                        (Status::Ready, "ready: no dependencies".to_string())
                    } else {
                        (
                            Status::Pending,
                            format!("blocked by unfinished job {}", id - 1),
                        )
                    };
                    tx.upsert(
                        &id,
                        &Job {
                            build_id: build,
                            dependencies,
                            dependents,
                            attempt: 0,
                            status,
                            last_heartbeat_message_id: 0,
                            terminal_message_id: None,
                            version: 1,
                            reason,
                        },
                    );
                }
            }
        });
    }

    fn job(&self, id: u32) -> Job {
        self.store
            .get(&id)
            .unwrap_or_else(|| panic!("missing job {id}"))
    }

    fn next_ready(&self) -> Option<u32> {
        self.store
            .rtx(|(_, ready, _)| ready.iter().next().map(|(id, _)| id))
    }

    fn lease_next(&mut self, worker: u32, now_ms: u64) -> Option<(u32, Outcome)> {
        let id = self.next_ready()?;
        Some((id, self.lease_job(id, worker, now_ms)))
    }

    fn lease_job(&mut self, id: u32, worker: u32, now_ms: u64) -> Outcome {
        self.store.wtx(|tx| {
            let Some(mut job) = tx.get(&id) else {
                return Outcome {
                    changed: false,
                    detail: format!("rejected: job {id} does not exist"),
                };
            };
            if !matches!(job.status, Status::Ready) {
                return Outcome {
                    changed: false,
                    detail: rejection_for_non_ready(id, &job),
                };
            }
            job.attempt += 1;
            job.last_heartbeat_message_id = 0;
            job.status = Status::Leased {
                owner: worker,
                deadline_ms: now_ms + LEASE_MS,
            };
            job.version += 1;
            job.reason = format!(
                "leased to worker {worker} as attempt {} until {} by coordinator time",
                job.attempt,
                now_ms + LEASE_MS
            );
            let attempt = job.attempt;
            tx.upsert(&id, &job);
            Outcome {
                changed: true,
                detail: format!("leased job {id}: worker {worker}, attempt {attempt}"),
            }
        })
    }

    fn heartbeat(&mut self, heartbeat: Heartbeat) -> Outcome {
        self.heartbeat_batch(&[heartbeat])
            .into_iter()
            .next()
            .expect("one heartbeat outcome")
    }

    fn heartbeat_batch(&mut self, heartbeats: &[Heartbeat]) -> Vec<Outcome> {
        self.store.wtx(|tx| {
            heartbeats
                .iter()
                .map(|heartbeat| {
                    let Some(mut job) = tx.get(&heartbeat.job_id) else {
                        return Outcome {
                            changed: false,
                            detail: format!(
                                "rejected heartbeat: job {} does not exist",
                                heartbeat.job_id
                            ),
                        };
                    };
                    let Status::Leased { owner, deadline_ms } = job.status else {
                        return Outcome {
                            changed: false,
                            detail: rejection_for_message("heartbeat", heartbeat.job_id, &job),
                        };
                    };
                    if owner != heartbeat.worker || job.attempt != heartbeat.attempt {
                        return Outcome {
                            changed: false,
                            detail: format!(
                                "rejected heartbeat for job {}: active fence is worker {owner}, attempt {}; rule requires both to match",
                                heartbeat.job_id, job.attempt
                            ),
                        };
                    }
                    if deadline_ms <= heartbeat.coordinator_now_ms {
                        return Outcome {
                            changed: false,
                            detail: format!(
                                "rejected heartbeat for job {}: attempt {} expired at {deadline_ms}; coordinator observed {}; rule forbids reviving an expired lease",
                                heartbeat.job_id,
                                job.attempt,
                                heartbeat.coordinator_now_ms
                            ),
                        };
                    }
                    if heartbeat.message_id <= job.last_heartbeat_message_id {
                        return Outcome {
                            changed: false,
                            detail: format!(
                                "rejected heartbeat for job {}: message {} did not advance winner {} for attempt {}; rule requires a stable increasing per-attempt message id",
                                heartbeat.job_id,
                                heartbeat.message_id,
                                job.last_heartbeat_message_id,
                                job.attempt
                            ),
                        };
                    }
                    let new_deadline =
                        deadline_ms.max(heartbeat.coordinator_now_ms.saturating_add(LEASE_MS));
                    job.last_heartbeat_message_id = heartbeat.message_id;
                    job.status = Status::Leased {
                        owner,
                        deadline_ms: new_deadline,
                    };
                    job.version += 1;
                    job.reason = format!(
                        "lease renewed by coordinator message {} until {new_deadline}",
                        heartbeat.message_id
                    );
                    tx.upsert(&heartbeat.job_id, &job);
                    Outcome {
                        changed: true,
                        detail: format!(
                            "renewed job {} attempt {} until {new_deadline}",
                            heartbeat.job_id, heartbeat.attempt
                        ),
                    }
                })
                .collect()
        })
    }

    fn complete(
        &mut self,
        id: u32,
        worker: u32,
        attempt: u32,
        message_id: u64,
        coordinator_now_ms: u64,
        result_key: &str,
    ) -> Outcome {
        self.store.wtx(|tx| {
            let Some(mut job) = tx.get(&id) else {
                return Outcome {
                    changed: false,
                    detail: format!("rejected completion: job {id} does not exist"),
                };
            };
            let Status::Leased { owner, deadline_ms } = &job.status else {
                return Outcome {
                    changed: false,
                    detail: rejection_for_message("completion", id, &job),
                };
            };
            if *owner != worker || job.attempt != attempt {
                return Outcome {
                    changed: false,
                    detail: format!(
                        "rejected completion for job {id}: active fence is worker {owner}, attempt {}; rule requires both to match",
                        job.attempt
                    ),
                };
            }
            if *deadline_ms <= coordinator_now_ms {
                return Outcome {
                    changed: false,
                    detail: format!(
                        "rejected completion for job {id}: attempt {} expired at {deadline_ms}; coordinator observed {coordinator_now_ms}; rule forbids an expired attempt from winning",
                        job.attempt
                    ),
                };
            }

            let dependents = job.dependents.clone();
            job.status = Status::Completed {
                attempt,
                result_key: result_key.to_string(),
            };
            job.terminal_message_id = Some(message_id);
            job.version += 1;
            job.reason = format!(
                "terminal result committed by worker {worker}, attempt {attempt}, message {message_id}"
            );
            tx.upsert(&id, &job);

            for dependent_id in dependents {
                let Some(mut dependent) = tx.get(&dependent_id) else {
                    continue;
                };
                if !matches!(dependent.status, Status::Pending) {
                    continue;
                }
                let mut blocked = Vec::new();
                for dependency_id in &dependent.dependencies {
                    let complete = tx
                        .get(dependency_id)
                        .is_some_and(|parent| matches!(parent.status, Status::Completed { .. }));
                    if !complete {
                        blocked.push(*dependency_id);
                    }
                }
                dependent.version += 1;
                if blocked.is_empty() {
                    dependent.status = Status::Ready;
                    dependent.reason =
                        format!("ready: all dependencies completed after job {id}");
                } else {
                    dependent.reason = format!("blocked by unfinished jobs {blocked:?}");
                }
                tx.upsert(&dependent_id, &dependent);
            }

            Outcome {
                changed: true,
                detail: format!(
                    "committed terminal winner for job {id}: attempt {attempt}, result {result_key}"
                ),
            }
        })
    }

    fn reap_expired(&mut self, now_ms: u64) -> usize {
        let expired: Vec<u32> = self.store.rtx(|(_, _, leased)| {
            leased
                .iter()
                .filter_map(|(id, job)| match job.status {
                    Status::Leased { deadline_ms, .. } if deadline_ms <= now_ms => Some(id),
                    _ => None,
                })
                .collect()
        });
        self.store.wtx(|tx| {
            for id in &expired {
                let Some(mut job) = tx.get(id) else {
                    continue;
                };
                let Status::Leased { deadline_ms, .. } = job.status else {
                    continue;
                };
                if deadline_ms > now_ms {
                    continue;
                }
                job.status = Status::Ready;
                job.version += 1;
                job.reason = format!(
                    "retry ready: attempt {} expired at {deadline_ms}; coordinator observed {now_ms}",
                    job.attempt
                );
                tx.upsert(id, &job);
            }
        });
        expired.len()
    }

    fn checkpoint(&mut self) {
        self.store.checkpoint();
    }
}

fn is_ready(job: &Keyed<u32, Job>) -> bool {
    matches!(job.val.status, Status::Ready)
}

fn is_leased(job: &Keyed<u32, Job>) -> bool {
    matches!(job.val.status, Status::Leased { .. })
}

fn rejection_for_non_ready(id: u32, job: &Job) -> String {
    match &job.status {
        Status::Pending => format!("rejected lease for job {id}: {}", job.reason),
        Status::Leased { owner, .. } => format!(
            "rejected lease for job {id}: winner is active worker {owner}, attempt {}; rule forbids a second live lease",
            job.attempt
        ),
        Status::Completed {
            attempt,
            result_key,
        } => format!(
            "rejected lease for job {id}: terminal winner is attempt {attempt}, result {result_key}; rule forbids retry after completion"
        ),
        Status::Ready => unreachable!("caller checks ready"),
    }
}

fn rejection_for_message(kind: &str, id: u32, job: &Job) -> String {
    match &job.status {
        Status::Completed {
            attempt,
            result_key,
        } => format!(
            "rejected {kind} for job {id}: terminal winner is attempt {attempt}, result {result_key}; rule says terminal results are immutable"
        ),
        Status::Pending => format!("rejected {kind} for job {id}: {}", job.reason),
        Status::Ready => format!(
            "rejected {kind} for job {id}: no active lease; rule requires a matching active attempt"
        ),
        Status::Leased { .. } => unreachable!("caller handles leased"),
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct SnapshotJob {
    attempt: u32,
    lease_owner: Option<u32>,
}

fn baseline_demo() {
    let durable_snapshot = serde_json::to_vec(&vec![SnapshotJob {
        attempt: 0,
        lease_owner: None,
    }])
    .expect("encode baseline snapshot");
    let mut in_memory: Vec<SnapshotJob> =
        serde_json::from_slice(&durable_snapshot).expect("load baseline snapshot");
    in_memory[0].attempt += 1;
    in_memory[0].lease_owner = Some(7);
    let first_ack = in_memory[0].clone();

    let mut after_crash: Vec<SnapshotJob> =
        serde_json::from_slice(&durable_snapshot).expect("reload baseline snapshot");
    after_crash[0].attempt += 1;
    after_crash[0].lease_owner = Some(8);
    let second_ack = after_crash[0].clone();

    println!(
        "baseline failure: acknowledged worker {:?}; restart leased {:?}; both received fencing attempt {}",
        first_ack.lease_owner, second_ack.lease_owner, second_ack.attempt
    );
}

fn crash_child(args: &[String]) -> ExitCode {
    let path = Path::new(&args[2]);
    let operation = &args[3];
    let id: u32 = args[4].parse().expect("job id");
    let worker: u32 = args[5].parse().expect("worker");
    let mut coordinator = Coordinator::open(path);
    let outcome = match operation.as_str() {
        "lease" => {
            let now_ms: u64 = args[6].parse().expect("coordinator time");
            coordinator.lease_job(id, worker, now_ms)
        }
        "heartbeat" => {
            let attempt: u32 = args[6].parse().expect("attempt");
            let message_id: u64 = args[7].parse().expect("message id");
            let now_ms: u64 = args[8].parse().expect("coordinator time");
            coordinator.heartbeat(Heartbeat {
                job_id: id,
                worker,
                attempt,
                message_id,
                coordinator_now_ms: now_ms,
            })
        }
        "complete" => {
            let attempt: u32 = args[6].parse().expect("attempt");
            let message_id: u64 = args[7].parse().expect("message id");
            let now_ms: u64 = args[8].parse().expect("coordinator time");
            coordinator.complete(id, worker, attempt, message_id, now_ms, &args[9])
        }
        other => panic!("unknown crash operation {other}"),
    };
    assert!(
        outcome.changed,
        "crash step must mutate: {}",
        outcome.detail
    );
    println!("ACK {}", outcome.detail);
    std::io::stdout().flush().expect("flush ack");
    std::process::exit(FORCED_EXIT_CODE);
}

fn run_crash_step(path: &Path, operation: &[String]) {
    let output = Command::new(std::env::current_exe().expect("current executable"))
        .arg("crash-step")
        .arg(path)
        .args(operation)
        .output()
        .expect("spawn forced-termination child");
    assert_eq!(output.status.code(), Some(FORCED_EXIT_CODE));
    let stdout = String::from_utf8(output.stdout).expect("child stdout");
    assert!(stdout.starts_with("ACK "), "missing child ack: {stdout}");
}

fn fault_demo(path: &Path) {
    reset_dir(path);
    {
        let mut coordinator = Coordinator::open(path);
        coordinator.seed_pairs(25);
    }

    let mut terminations = 0;
    for pair in 0..25u32 {
        let root = pair * 2;
        let child = root + 1;
        let root_worker = 1_000 + root;
        let child_worker = 2_000 + child;
        let heartbeat_id = 10_000 + u64::from(root);

        run_crash_step(
            path,
            &[
                "lease".to_string(),
                root.to_string(),
                root_worker.to_string(),
                "100".to_string(),
            ],
        );
        terminations += 1;
        {
            let mut coordinator = Coordinator::open(path);
            assert!(matches!(coordinator.job(child).status, Status::Pending));
            let before = coordinator.job(root).version;
            let duplicate = coordinator.lease_job(root, root_worker, 100);
            assert!(!duplicate.changed);
            assert_eq!(coordinator.job(root).version, before);
        }

        run_crash_step(
            path,
            &[
                "heartbeat".to_string(),
                root.to_string(),
                root_worker.to_string(),
                "1".to_string(),
                heartbeat_id.to_string(),
                "1_000".replace('_', ""),
            ],
        );
        terminations += 1;
        {
            let mut coordinator = Coordinator::open(path);
            let before = coordinator.job(root).version;
            let duplicate = coordinator.heartbeat(Heartbeat {
                job_id: root,
                worker: root_worker,
                attempt: 1,
                message_id: heartbeat_id,
                coordinator_now_ms: 2_000,
            });
            assert!(!duplicate.changed);
            assert_eq!(coordinator.job(root).version, before);
        }

        run_crash_step(
            path,
            &[
                "complete".to_string(),
                root.to_string(),
                root_worker.to_string(),
                "1".to_string(),
                (20_000 + u64::from(root)).to_string(),
                "2_500".replace('_', ""),
                format!("objects/build-{pair}/root"),
            ],
        );
        terminations += 1;
        {
            let mut coordinator = Coordinator::open(path);
            assert!(matches!(coordinator.job(child).status, Status::Ready));
            let before = coordinator.job(root).version;
            let replay = coordinator.complete(
                root,
                root_worker,
                1,
                20_000 + u64::from(root),
                2_500,
                &format!("objects/build-{pair}/root"),
            );
            assert!(!replay.changed);
            assert!(replay.detail.contains("terminal winner"));
            let reordered_heartbeat = coordinator.heartbeat(Heartbeat {
                job_id: root,
                worker: root_worker,
                attempt: 1,
                message_id: heartbeat_id + 1,
                coordinator_now_ms: 3_000,
            });
            assert!(!reordered_heartbeat.changed);
            assert_eq!(coordinator.job(root).version, before);
        }

        run_crash_step(
            path,
            &[
                "lease".to_string(),
                child.to_string(),
                child_worker.to_string(),
                "4_000".replace('_', ""),
            ],
        );
        terminations += 1;
        {
            let mut coordinator = Coordinator::open(path);
            let before = coordinator.job(child).version;
            let duplicate = coordinator.lease_job(child, child_worker, 4_000);
            assert!(!duplicate.changed);
            assert_eq!(coordinator.job(child).version, before);
        }
    }
    assert_eq!(terminations, 100);

    let fencing_job = 50;
    let mut coordinator = Coordinator::open(path);
    assert!(coordinator.lease_job(fencing_job, 41, 0).changed);
    let before_expired_messages = coordinator.job(fencing_job).version;
    let expired_heartbeat = coordinator.heartbeat(Heartbeat {
        job_id: fencing_job,
        worker: 41,
        attempt: 1,
        message_id: 90_000,
        coordinator_now_ms: 30_001,
    });
    assert!(!expired_heartbeat.changed);
    assert!(expired_heartbeat.detail.contains("expired at 30000"));
    let expired_completion = coordinator.complete(
        fencing_job,
        41,
        1,
        90_001,
        30_001,
        "objects/fencing/expired",
    );
    assert!(!expired_completion.changed);
    assert!(expired_completion.detail.contains("expired at 30000"));
    assert_eq!(
        coordinator.job(fencing_job).version,
        before_expired_messages
    );
    assert_eq!(coordinator.reap_expired(30_001), 1);
    let retry_ready_after_ms = 1;
    assert!(coordinator.lease_job(fencing_job, 42, 30_001).changed);
    assert_eq!(coordinator.job(fencing_job).attempt, 2);
    let winner = coordinator.complete(fencing_job, 42, 2, 90_002, 30_002, "objects/fencing/winner");
    assert!(winner.changed);
    let before = coordinator.job(fencing_job).version;
    let late = coordinator.complete(fencing_job, 41, 1, 90_003, 30_003, "objects/fencing/late");
    assert!(!late.changed);
    assert!(late.detail.contains("attempt 2"));
    assert!(late.detail.contains("objects/fencing/winner"));
    assert_eq!(coordinator.job(fencing_job).version, before);

    println!(
        "fault test passed: {terminations} forced process exits after ACK; duplicate/reordered messages were no-ops; heartbeat and completion observed after expiry were rejected without mutation; expiry became retry-ready in {retry_ready_after_ms} ms; late attempt rejection: {}",
        late.detail
    );
}

fn multi_writer_demo(path: &Path) {
    reset_dir(path);
    let _first = Coordinator::open(path);
    let old_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let second = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| Coordinator::open(path)));
    std::panic::set_hook(old_hook);
    match second {
        Ok(_) => println!(
            "multi-writer check: a second handle opened, but Fold exposes no replica consensus, leader fencing, or cross-process compare-and-swap contract; requirement remains unproven"
        ),
        Err(_) => println!(
            "multi-writer check: second concurrent Fold opener was rejected; this embedded single-writer component cannot host three active coordinator replicas"
        ),
    }
}

fn benchmark(path: &Path) {
    reset_dir(path);
    let json_path = path.with_extension("json");
    let seed_jobs = benchmark_jobs(10_000, 10);
    let mut json_durations = Vec::new();
    let mut json_bytes = 0;
    for _ in 0..3 {
        let started = Instant::now();
        let file = File::create(&json_path).expect("create JSON snapshot");
        let mut writer = BufWriter::new(file);
        serde_json::to_writer(&mut writer, &seed_jobs).expect("write JSON snapshot");
        writer.flush().expect("flush JSON");
        writer.get_ref().sync_all().expect("sync JSON snapshot");
        json_durations.push(started.elapsed());
        json_bytes = writer.get_ref().metadata().expect("JSON metadata").len();
    }
    println!(
        "JSON full rewrite (100,000 jobs, {} bytes), repeated sync times: {}",
        json_bytes,
        display_durations(&json_durations)
    );

    let seed_started = Instant::now();
    let mut coordinator = Coordinator::open(path);
    coordinator.seed_benchmark_graph(10_000, 10);
    coordinator.checkpoint();
    let seed_elapsed = seed_started.elapsed();
    drop(coordinator);

    let mut recovery_times = Vec::new();
    for worker in 0..5 {
        let started = Instant::now();
        let mut recovered = Coordinator::open(path);
        let lease = recovered
            .lease_next(50_000 + worker, 100)
            .expect("ready work after recovery");
        assert!(lease.1.changed);
        recovery_times.push(started.elapsed());
    }

    let mut coordinator = Coordinator::open(path);
    let mut leased = Vec::new();
    for worker in 0..500 {
        let (id, outcome) = coordinator
            .lease_next(60_000 + worker, 1_000)
            .expect("500 ready benchmark jobs");
        assert!(outcome.changed);
        leased.push((id, 60_000 + worker, coordinator.job(id).attempt));
    }

    let mut all_message_latencies = Vec::new();
    for pass in 0..3u64 {
        let pass_started = Instant::now();
        let mut pass_latencies = Vec::with_capacity(2_000);
        for batch_start in (0..2_000usize).step_by(32) {
            let batch_end = (batch_start + 32).min(2_000);
            let mut batch = Vec::with_capacity(batch_end - batch_start);
            for update in batch_start..batch_end {
                let (job_id, worker, attempt) = leased[update % leased.len()];
                batch.push(Heartbeat {
                    job_id,
                    worker,
                    attempt,
                    message_id: pass * 2_000 + update as u64 + 1,
                    coordinator_now_ms: 2_000 + pass,
                });
            }
            let started = Instant::now();
            let outcomes = coordinator.heartbeat_batch(&batch);
            let elapsed = started.elapsed();
            assert!(outcomes.iter().all(|outcome| outcome.changed));
            pass_latencies.extend(std::iter::repeat_n(elapsed, batch.len()));
        }
        let wall = pass_started.elapsed();
        let rate = 2_000.0 / wall.as_secs_f64();
        let p99 = percentile_99(&mut pass_latencies);
        println!(
            "batched heartbeat pass {}: 2,000 updates in {:.3}s = {:.0} updates/s; message p99 commit latency {:.3} ms",
            pass + 1,
            wall.as_secs_f64(),
            rate,
            p99.as_secs_f64() * 1_000.0
        );
        all_message_latencies.extend(pass_latencies);
    }
    coordinator.checkpoint();
    drop(coordinator);

    let state_bytes = directory_size(path);
    let overall_p99 = percentile_99(&mut all_message_latencies);
    println!(
        "Fold upper-bound sample: seed/checkpoint {:.3}s; five reopen+lease times {}; overall message p99 {:.3} ms; persistent directory {} bytes",
        seed_elapsed.as_secs_f64(),
        display_durations(&recovery_times),
        overall_p99.as_secs_f64() * 1_000.0,
        state_bytes
    );
}

fn benchmark_jobs(builds: u32, jobs_per_build: u32) -> Vec<Job> {
    let mut jobs = Vec::with_capacity((builds * jobs_per_build) as usize);
    for build in 0..builds {
        for offset in 0..jobs_per_build {
            let id = build * jobs_per_build + offset;
            jobs.push(Job {
                build_id: build,
                dependencies: if offset > 0 { vec![id - 1] } else { Vec::new() },
                dependents: if offset + 1 < jobs_per_build {
                    vec![id + 1]
                } else {
                    Vec::new()
                },
                attempt: 0,
                status: if offset == 0 {
                    Status::Ready
                } else {
                    Status::Pending
                },
                last_heartbeat_message_id: 0,
                terminal_message_id: None,
                version: 1,
                reason: if offset == 0 {
                    "ready: no dependencies".to_string()
                } else {
                    format!("blocked by unfinished job {}", id - 1)
                },
            });
        }
    }
    jobs
}

fn percentile_99(durations: &mut [Duration]) -> Duration {
    durations.sort_unstable();
    durations[(durations.len() * 99 / 100).min(durations.len() - 1)]
}

fn display_durations(durations: &[Duration]) -> String {
    durations
        .iter()
        .map(|duration| format!("{:.3}s", duration.as_secs_f64()))
        .collect::<Vec<_>>()
        .join(", ")
}

fn directory_size(path: &Path) -> u64 {
    fs::read_dir(path)
        .expect("read state directory")
        .map(|entry| {
            let entry = entry.expect("state entry");
            let metadata = entry.metadata().expect("state metadata");
            if metadata.is_dir() {
                directory_size(&entry.path())
            } else {
                metadata.len()
            }
        })
        .sum()
}

fn reset_dir(path: &Path) {
    if path.exists() {
        fs::remove_dir_all(path).expect("remove prior demonstration state");
    }
}

fn demo_paths() -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir();
    (
        base.join("bogkit-ci-lease-fault-demo.db"),
        base.join("bogkit-ci-lease-multi-writer.db"),
    )
}

fn usage() {
    eprintln!(
        "usage: ci-lease-coordinator <demo|fault [path]|baseline|multi-writer [path]|bench [path]>"
    );
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("crash-step") => crash_child(&args),
        Some("baseline") => {
            baseline_demo();
            ExitCode::SUCCESS
        }
        Some("fault") => {
            let path = args
                .get(2)
                .map(PathBuf::from)
                .unwrap_or_else(|| demo_paths().0);
            fault_demo(&path);
            ExitCode::SUCCESS
        }
        Some("multi-writer") => {
            let path = args
                .get(2)
                .map(PathBuf::from)
                .unwrap_or_else(|| demo_paths().1);
            multi_writer_demo(&path);
            ExitCode::SUCCESS
        }
        Some("bench") => {
            let path = args
                .get(2)
                .map(PathBuf::from)
                .unwrap_or_else(|| std::env::temp_dir().join("bogkit-ci-lease-benchmark.db"));
            benchmark(&path);
            ExitCode::SUCCESS
        }
        Some("demo") => {
            let (fault_path, multi_writer_path) = demo_paths();
            baseline_demo();
            fault_demo(&fault_path);
            multi_writer_demo(&multi_writer_path);
            ExitCode::SUCCESS
        }
        _ => {
            usage();
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DB: AtomicU64 = AtomicU64::new(0);

    fn test_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "bogkit-ci-{name}-{}-{}",
            std::process::id(),
            NEXT_DB.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn dependency_only_becomes_ready_after_parent_terminal_commit() {
        let path = test_path("dependency");
        let mut coordinator = Coordinator::open(&path);
        coordinator.seed_pairs(1);
        assert!(matches!(coordinator.job(1).status, Status::Pending));
        assert!(coordinator.lease_job(0, 7, 0).changed);
        assert!(matches!(coordinator.job(1).status, Status::Pending));
        assert!(
            coordinator
                .complete(0, 7, 1, 1, 1_000, "objects/parent")
                .changed
        );
        assert!(matches!(coordinator.job(1).status, Status::Ready));
    }

    #[test]
    fn duplicate_and_reordered_messages_do_not_mutate() {
        let path = test_path("replay");
        let mut coordinator = Coordinator::open(&path);
        coordinator.seed_pairs(1);
        assert!(coordinator.lease_job(0, 7, 0).changed);
        let heartbeat = Heartbeat {
            job_id: 0,
            worker: 7,
            attempt: 1,
            message_id: 10,
            coordinator_now_ms: 1_000,
        };
        assert!(coordinator.heartbeat(heartbeat).changed);
        let after_first = coordinator.job(0).version;
        assert!(!coordinator.heartbeat(heartbeat).changed);
        assert_eq!(coordinator.job(0).version, after_first);
        assert!(
            coordinator
                .complete(0, 7, 1, 11, 2_000, "objects/winner")
                .changed
        );
        let after_complete = coordinator.job(0).version;
        assert!(
            !coordinator
                .heartbeat(Heartbeat {
                    message_id: 12,
                    ..heartbeat
                })
                .changed
        );
        assert!(
            !coordinator
                .complete(0, 7, 1, 11, 2_000, "objects/winner")
                .changed
        );
        assert_eq!(coordinator.job(0).version, after_complete);
    }

    #[test]
    fn expired_attempt_cannot_overwrite_winner() {
        let path = test_path("fence");
        let mut coordinator = Coordinator::open(&path);
        coordinator.seed_pairs(0);
        assert!(coordinator.lease_job(0, 7, 0).changed);
        assert_eq!(coordinator.reap_expired(30_001), 1);
        assert!(coordinator.lease_job(0, 8, 30_001).changed);
        assert!(
            coordinator
                .complete(0, 8, 2, 2, 30_002, "objects/winner")
                .changed
        );
        let before = coordinator.job(0).version;
        let late = coordinator.complete(0, 7, 1, 1, 30_003, "objects/late");
        assert!(!late.changed);
        assert!(late.detail.contains("attempt 2"));
        assert!(late.detail.contains("objects/winner"));
        assert_eq!(coordinator.job(0).version, before);
    }

    #[test]
    fn expired_messages_cannot_revive_or_complete_a_lease() {
        let path = test_path("expired-messages");
        let mut coordinator = Coordinator::open(&path);
        coordinator.seed_pairs(0);
        assert!(coordinator.lease_job(0, 7, 0).changed);
        let leased_version = coordinator.job(0).version;

        let at_deadline = coordinator.heartbeat(Heartbeat {
            job_id: 0,
            worker: 7,
            attempt: 1,
            message_id: 1,
            coordinator_now_ms: LEASE_MS,
        });
        assert!(!at_deadline.changed);
        assert!(at_deadline.detail.contains("expired at 30000"));
        assert_eq!(coordinator.job(0).version, leased_version);

        let far_past = coordinator.complete(0, 7, 1, 2, 999_999, "objects/expired-attempt");
        assert!(!far_past.changed);
        assert!(far_past.detail.contains("expired at 30000"));
        assert_eq!(coordinator.job(0).version, leased_version);

        assert_eq!(coordinator.reap_expired(999_999), 1);
        assert!(coordinator.lease_job(0, 8, 999_999).changed);
        assert_eq!(coordinator.job(0).attempt, 2);
        let reassigned_version = coordinator.job(0).version;

        let stale_heartbeat = coordinator.heartbeat(Heartbeat {
            job_id: 0,
            worker: 7,
            attempt: 1,
            message_id: 3,
            coordinator_now_ms: 1_000_000,
        });
        assert!(!stale_heartbeat.changed);
        let stale_completion = coordinator.complete(0, 7, 1, 4, 1_000_000, "objects/stale-attempt");
        assert!(!stale_completion.changed);
        assert_eq!(coordinator.job(0).version, reassigned_version);
    }
}
