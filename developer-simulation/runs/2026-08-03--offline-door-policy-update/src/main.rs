use std::collections::BTreeMap;
use std::fs;
use std::mem::MaybeUninit;
use std::os::unix::fs::MetadataExt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::time::Instant;

use fold::pipeline::terminal;
use fold::stream::{KeyedStream, Stream};
use serde::{Deserialize, Serialize};

const POLICY_BYTES: u64 = 16 * 1024 * 1024;
const CONTROLLER_DOOR: u16 = 7;
const INITIAL_GRANTS: u32 = 60_000;
const EMERGENCY_REVOCATIONS: u32 = 50_000;
const QUERY_CHECKS: usize = 20_000;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
enum PolicyKey {
    Meta,
    Grant { badge: u32, door: u16 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum PolicyRecord {
    Meta(PersistedStatus),
    Grant(Grant),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct PersistedStatus {
    active_version: u64,
    last_verified_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Grant {
    not_before: u64,
    not_after: u64,
}

#[derive(Clone, Debug)]
enum Change {
    Grant { badge: u32, door: u16, grant: Grant },
    Revoke { badge: u32, door: u16 },
}

#[derive(Clone, Debug)]
struct Bundle {
    based_on: u64,
    version: u64,
    verified_at: u64,
    changes: Vec<Change>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ApplyResult {
    Activated,
    SameVersionIgnoredUnverified,
    RejectedOld,
    BaseMismatch {
        expected_base: u64,
        received_base: u64,
    },
    Missing {
        expected: u64,
        received: u64,
    },
}

struct Controller {
    state: KeyedStream<PolicyKey, PolicyRecord, terminal::Count>,
    active_version: u64,
    last_verified_at: u64,
    missing: Option<(u64, u64)>,
}

impl Controller {
    fn open(path: &Path) -> Self {
        let state = KeyedStream::new(path, terminal::Count::new("record_count"));
        let persisted = match state.get(&PolicyKey::Meta) {
            Some(PolicyRecord::Meta(status)) => status,
            Some(PolicyRecord::Grant(_)) | None => PersistedStatus {
                active_version: 0,
                last_verified_at: 0,
            },
        };
        Self {
            state,
            active_version: persisted.active_version,
            last_verified_at: persisted.last_verified_at,
            missing: None,
        }
    }

    fn apply(&mut self, bundle: &Bundle) -> ApplyResult {
        if bundle.version == self.active_version {
            return ApplyResult::SameVersionIgnoredUnverified;
        }
        if bundle.version < self.active_version {
            return ApplyResult::RejectedOld;
        }

        let expected = self.active_version + 1;
        if bundle.version != expected {
            self.missing = Some((expected, bundle.version));
            return ApplyResult::Missing {
                expected,
                received: bundle.version,
            };
        }
        if bundle.based_on != self.active_version {
            self.missing = None;
            return ApplyResult::BaseMismatch {
                expected_base: self.active_version,
                received_base: bundle.based_on,
            };
        }

        self.state.wtx(|tx| {
            for change in &bundle.changes {
                match change {
                    Change::Grant { badge, door, grant } => {
                        tx.upsert(
                            &PolicyKey::Grant {
                                badge: *badge,
                                door: *door,
                            },
                            &PolicyRecord::Grant(grant.clone()),
                        );
                    }
                    Change::Revoke { badge, door } => {
                        tx.remove(&PolicyKey::Grant {
                            badge: *badge,
                            door: *door,
                        });
                    }
                }
            }
            tx.upsert(
                &PolicyKey::Meta,
                &PolicyRecord::Meta(PersistedStatus {
                    active_version: bundle.version,
                    last_verified_at: bundle.verified_at,
                }),
            );
        });

        self.active_version = bundle.version;
        self.last_verified_at = bundle.verified_at;
        self.missing = None;
        ApplyResult::Activated
    }

    fn authorize(&self, badge: u32, door: u16, at: u64) -> bool {
        match self.state.get(&PolicyKey::Grant { badge, door }) {
            Some(PolicyRecord::Grant(grant)) => grant.not_before <= at && at < grant.not_after,
            Some(PolicyRecord::Meta(_)) | None => false,
        }
    }

    fn checkpoint(&mut self) {
        self.state.checkpoint();
    }

    fn status(&self) -> String {
        match self.missing {
            Some((expected, received)) => format!(
                "active_version={} last_verified_at={} missing={}..{}",
                self.active_version,
                self.last_verified_at,
                expected,
                received.saturating_sub(1)
            ),
            None => format!(
                "active_version={} last_verified_at={} missing=none",
                self.active_version, self.last_verified_at
            ),
        }
    }
}

fn grant_key(badge: u32, door: u16) -> PolicyKey {
    PolicyKey::Grant { badge, door }
}

fn initial_snapshot() -> Bundle {
    let changes = (0..INITIAL_GRANTS)
        .map(|badge| Change::Grant {
            badge,
            door: CONTROLLER_DOOR,
            grant: Grant {
                not_before: 100,
                not_after: 10_000,
            },
        })
        .collect();
    Bundle {
        based_on: 0,
        version: 1,
        verified_at: 1_000,
        changes,
    }
}

fn revocation_bundle() -> Bundle {
    let changes = (0..EMERGENCY_REVOCATIONS)
        .map(|badge| Change::Revoke {
            badge,
            door: CONTROLLER_DOOR,
        })
        .collect();
    Bundle {
        based_on: 1,
        version: 2,
        verified_at: 1_010,
        changes,
    }
}

fn reference_authorize(
    reference: &BTreeMap<PolicyKey, Grant>,
    badge: u32,
    door: u16,
    at: u64,
) -> bool {
    reference
        .get(&grant_key(badge, door))
        .is_some_and(|grant| grant.not_before <= at && at < grant.not_after)
}

fn check_queries(controller: &Controller, reference: &BTreeMap<PolicyKey, Grant>) -> usize {
    let mut mismatches = 0;
    let mut state = 0x9e37_79b9_u64;
    for _ in 0..QUERY_CHECKS {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        let badge = ((state >> 16) % 70_000) as u32;
        let door = if state & 1 == 0 {
            CONTROLLER_DOOR
        } else {
            CONTROLLER_DOOR + 1
        };
        let at = (state >> 32) % 12_000;
        if controller.authorize(badge, door, at) != reference_authorize(reference, badge, door, at)
        {
            mismatches += 1;
        }
    }
    mismatches
}

fn store_stats(path: &Path) -> std::io::Result<(u64, u64, usize)> {
    let mut logical_bytes = 0;
    let mut allocated_bytes = 0;
    let mut files = 0;
    let mut pending = vec![path.to_path_buf()];
    while let Some(item) = pending.pop() {
        let metadata = fs::metadata(&item)?;
        if metadata.is_dir() {
            for entry in fs::read_dir(item)? {
                pending.push(entry?.path());
            }
        } else if metadata.is_file() {
            logical_bytes += metadata.len();
            allocated_bytes += metadata.blocks() * 512;
            files += 1;
        }
    }
    Ok((logical_bytes, allocated_bytes, files))
}

fn fixed_image_probe(path: &Path) -> std::io::Result<bool> {
    let image = path.join("fixed-flash.img");
    let file = fs::File::create(&image)?;
    file.set_len(POLICY_BYTES)?;
    drop(file);

    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let open_result = catch_unwind(AssertUnwindSafe(|| {
        let _stream: Stream<u32, terminal::Count> =
            Stream::new(&image, terminal::Count::new("probe"));
    }));
    std::panic::set_hook(previous_hook);
    Ok(open_result.is_err())
}

fn naive_baseline_mixes_generations_after_one_block() -> bool {
    let old_generation = [0x11_u8; 8 * 1024];
    let new_generation = [0x22_u8; 8 * 1024];
    let mut flash = old_generation;
    flash[..4 * 1024].copy_from_slice(&new_generation[..4 * 1024]);
    flash != old_generation && flash != new_generation
}

fn run_root() -> Result<PathBuf, String> {
    let mut args = std::env::args_os();
    let program = args
        .next()
        .unwrap_or_else(|| "offline-door-policy-fit-probe".into());
    let Some(root) = args.next() else {
        return Err(format!(
            "usage: {} <empty-state-directory>",
            Path::new(&program).display()
        ));
    };
    if args.next().is_some() {
        return Err(format!(
            "usage: {} <empty-state-directory>",
            Path::new(&program).display()
        ));
    }
    Ok(PathBuf::from(root))
}

fn peak_rss_bytes() -> Option<u64> {
    let mut usage = MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: getrusage initializes the provided rusage when it returns zero.
    let result = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    if result != 0 {
        return None;
    }
    // SAFETY: a successful getrusage call initialized usage above.
    let usage = unsafe { usage.assume_init() };
    #[cfg(target_os = "macos")]
    {
        Some(usage.ru_maxrss as u64)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Some((usage.ru_maxrss as u64) * 1024)
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = run_root()?;
    if root.exists() && fs::read_dir(&root)?.next().is_some() {
        return Err(format!("state directory must be empty: {}", root.display()).into());
    }
    fs::create_dir_all(&root)?;

    let baseline_mixed = naive_baseline_mixes_generations_after_one_block();
    let fixed_image_rejected = fixed_image_probe(&root)?;
    let store_path = root.join("fold-store");
    let mut controller = Controller::open(&store_path);
    let mut reference = BTreeMap::new();

    let snapshot = initial_snapshot();
    let snapshot_started = Instant::now();
    assert_eq!(controller.apply(&snapshot), ApplyResult::Activated);
    let snapshot_apply_ms = snapshot_started.elapsed().as_millis();
    for change in &snapshot.changes {
        if let Change::Grant { badge, door, grant } = change {
            reference.insert(grant_key(*badge, *door), grant.clone());
        }
    }
    let before_mismatches = check_queries(&controller, &reference);
    controller.checkpoint();

    let wrong_base_bundle = Bundle {
        based_on: 0,
        version: 2,
        verified_at: 1_005,
        changes: vec![Change::Revoke {
            badge: 55_000,
            door: CONTROLLER_DOOR,
        }],
    };
    let wrong_base = controller.apply(&wrong_base_bundle);
    let wrong_base_status = controller.status();
    let wrong_base_unchanged = controller.authorize(55_000, CONTROLLER_DOOR, 1_500);

    let revocations = revocation_bundle();
    let delta_started = Instant::now();
    assert_eq!(controller.apply(&revocations), ApplyResult::Activated);
    let delta_apply_ms = delta_started.elapsed().as_millis();
    let checkpoint_started = Instant::now();
    controller.checkpoint();
    let checkpoint_ms = checkpoint_started.elapsed().as_millis();
    for badge in 0..EMERGENCY_REVOCATIONS {
        reference.remove(&grant_key(badge, CONTROLLER_DOOR));
    }
    let after_mismatches = check_queries(&controller, &reference);

    let same_version = controller.apply(&revocations);
    let old = controller.apply(&snapshot);
    let version_four = Bundle {
        based_on: 3,
        version: 4,
        verified_at: 1_030,
        changes: vec![Change::Grant {
            badge: 99_999,
            door: CONTROLLER_DOOR,
            grant: Grant {
                not_before: 1_000,
                not_after: 2_000,
            },
        }],
    };
    let gap = controller.apply(&version_four);
    let gap_status = controller.status();
    let version_three = Bundle {
        based_on: 2,
        version: 3,
        verified_at: 1_020,
        changes: Vec::new(),
    };
    let repair = controller.apply(&version_three);
    let after_repair = controller.apply(&version_four);
    let final_status = controller.status();
    controller.checkpoint();

    let (open_logical_bytes, open_allocated_bytes, open_store_files) = store_stats(&store_path)?;
    let revoked_deny = !controller.authorize(0, CONTROLLER_DOOR, 1_500);
    let final_allow = controller.authorize(99_999, CONTROLLER_DOOR, 1_500);
    let expired_deny = !controller.authorize(99_999, CONTROLLER_DOOR, 2_000);
    drop(controller);
    let reopened = Controller::open(&store_path);
    let reopened_status = reopened.status();
    let reopened_allow = reopened.authorize(99_999, CONTROLLER_DOOR, 1_500);
    drop(reopened);
    let (closed_logical_bytes, closed_allocated_bytes, closed_store_files) =
        store_stats(&store_path)?;
    println!("baseline_mixed_after_first_4k_write={baseline_mixed}");
    println!("fixed_16mib_image_rejected={fixed_image_rejected}");
    println!(
        "snapshot_entries={} snapshot_apply_ms={snapshot_apply_ms}",
        snapshot.changes.len()
    );
    println!("queries_before={QUERY_CHECKS} mismatches={before_mismatches}");
    println!(
        "revocations={} delta_apply_ms={delta_apply_ms} checkpoint_ms={checkpoint_ms}",
        revocations.changes.len()
    );
    println!("queries_after={QUERY_CHECKS} mismatches={after_mismatches}");
    println!(
        "wrong_base={wrong_base:?} wrong_base_status={wrong_base_status} wrong_base_unchanged={wrong_base_unchanged}"
    );
    println!("same_version={same_version:?} old={old:?} gap={gap:?}");
    println!("gap_status={gap_status}");
    println!("repair={repair:?} after_repair={after_repair:?}");
    println!("final_status={final_status}");
    println!("reopened_status={reopened_status} reopened_query={reopened_allow}");
    println!(
        "fold_store_open_files={open_store_files} open_logical_bytes={open_logical_bytes} open_allocated_bytes={open_allocated_bytes}"
    );
    println!(
        "fold_store_closed_files={closed_store_files} closed_logical_bytes={closed_logical_bytes} closed_allocated_bytes={closed_allocated_bytes} policy_limit_bytes={POLICY_BYTES}"
    );
    let peak_rss = peak_rss_bytes().ok_or("peak RSS measurement unavailable")?;
    println!(
        "whole_probe_peak_rss_bytes={peak_rss} working_memory_limit_bytes={}",
        4 * 1024 * 1024
    );
    println!("signature_verification=not_provided_by_fold");
    println!("truncated_bundle_detection=not_provided_by_fold");
    println!("power_cut_at_each_4k_write=not_injectable_through_fold_api");

    assert!(baseline_mixed);
    assert!(fixed_image_rejected);
    assert_eq!(before_mismatches, 0);
    assert_eq!(after_mismatches, 0);
    assert_eq!(
        wrong_base,
        ApplyResult::BaseMismatch {
            expected_base: 1,
            received_base: 0,
        }
    );
    assert_eq!(
        wrong_base_status,
        "active_version=1 last_verified_at=1000 missing=none"
    );
    assert!(wrong_base_unchanged);
    assert_eq!(same_version, ApplyResult::SameVersionIgnoredUnverified);
    assert_eq!(old, ApplyResult::RejectedOld);
    assert_eq!(
        gap,
        ApplyResult::Missing {
            expected: 3,
            received: 4,
        }
    );
    assert_eq!(repair, ApplyResult::Activated);
    assert_eq!(after_repair, ApplyResult::Activated);
    assert!(revoked_deny);
    assert!(final_allow);
    assert!(expired_deny);
    assert_eq!(reopened_status, final_status);
    assert!(reopened_allow);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store(name: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .expect("system time must follow the Unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "offline-door-policy-{name}-{}-{nonce}",
            std::process::id()
        ))
    }

    #[test]
    fn sequence_rules_and_time_bounds_are_deterministic() {
        let path = test_store("sequence");
        if path.exists() {
            fs::remove_dir_all(&path).unwrap();
        }
        let mut controller = Controller::open(&path);
        let v1 = Bundle {
            based_on: 0,
            version: 1,
            verified_at: 10,
            changes: vec![Change::Grant {
                badge: 42,
                door: CONTROLLER_DOOR,
                grant: Grant {
                    not_before: 100,
                    not_after: 200,
                },
            }],
        };
        assert_eq!(controller.apply(&v1), ApplyResult::Activated);
        assert!(!controller.authorize(42, CONTROLLER_DOOR, 99));
        assert!(controller.authorize(42, CONTROLLER_DOOR, 100));
        assert!(!controller.authorize(42, CONTROLLER_DOOR, 200));
        assert_eq!(
            controller.apply(&v1),
            ApplyResult::SameVersionIgnoredUnverified
        );

        let v3 = Bundle {
            based_on: 2,
            version: 3,
            verified_at: 30,
            changes: Vec::new(),
        };
        assert_eq!(
            controller.apply(&v3),
            ApplyResult::Missing {
                expected: 2,
                received: 3,
            }
        );
        assert_eq!(
            controller.status(),
            "active_version=1 last_verified_at=10 missing=2..2"
        );
        drop(controller);
        fs::remove_dir_all(&path).unwrap();
    }

    #[test]
    fn contiguous_version_with_wrong_base_is_rejected_without_mutation() {
        let path = test_store("wrong-base");
        let mut controller = Controller::open(&path);
        let v1 = Bundle {
            based_on: 0,
            version: 1,
            verified_at: 10,
            changes: vec![Change::Grant {
                badge: 42,
                door: CONTROLLER_DOOR,
                grant: Grant {
                    not_before: 100,
                    not_after: 200,
                },
            }],
        };
        assert_eq!(controller.apply(&v1), ApplyResult::Activated);

        let wrong_base = Bundle {
            based_on: 0,
            version: 2,
            verified_at: 20,
            changes: vec![Change::Revoke {
                badge: 42,
                door: CONTROLLER_DOOR,
            }],
        };
        assert_eq!(
            controller.apply(&wrong_base),
            ApplyResult::BaseMismatch {
                expected_base: 1,
                received_base: 0,
            }
        );
        assert!(controller.authorize(42, CONTROLLER_DOOR, 150));
        assert_eq!(
            controller.status(),
            "active_version=1 last_verified_at=10 missing=none"
        );
        drop(controller);
        let reopened = Controller::open(&path);
        assert!(reopened.authorize(42, CONTROLLER_DOOR, 150));
        assert_eq!(
            reopened.status(),
            "active_version=1 last_verified_at=10 missing=none"
        );
        drop(reopened);
        fs::remove_dir_all(&path).unwrap();
    }
}
