pub mod model;
pub mod planner;
pub mod simulator;
pub mod verification;

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;

use model::{PickupWave, PlanOutput, Yard};

/// Read and decode one yard snapshot and pickup wave.
///
/// # Errors
///
/// Returns a contextual message for unreadable files or malformed JSON.
pub fn read_inputs(yard_path: &Path, pickups_path: &Path) -> Result<(Yard, PickupWave), String> {
    let yard_bytes = fs::read(yard_path)
        .map_err(|error| format!("cannot read {}: {error}", yard_path.display()))?;
    let pickup_bytes = fs::read(pickups_path)
        .map_err(|error| format!("cannot read {}: {error}", pickups_path.display()))?;
    let yard = serde_json::from_slice(&yard_bytes)
        .map_err(|error| format!("invalid yard JSON: {error}"))?;
    let wave = serde_json::from_slice(&pickup_bytes)
        .map_err(|error| format!("invalid pickups JSON: {error}"))?;
    Ok((yard, wave))
}

const MOVES_FILE: &str = "moves.json";
const REVIEW_FILE: &str = "review.json";
const TEMP_FILE: &str = ".yard-plan.tmp";

/// Remove artifacts belonging to the previous generation.
///
/// # Errors
///
/// Returns a contextual message if the output directory cannot be created or a
/// previous owned artifact cannot be invalidated.
pub fn invalidate_output(output_dir: &Path) -> Result<(), String> {
    fs::create_dir_all(output_dir)
        .map_err(|error| format!("cannot create {}: {error}", output_dir.display()))?;
    for name in [MOVES_FILE, REVIEW_FILE, TEMP_FILE] {
        remove_owned_path(&output_dir.join(name))?;
    }
    Ok(())
}

/// Atomically publish either `moves.json` or `review.json` in canonical form.
/// Any previous current result is invalidated first, so exactly one canonical
/// artifact exists after success and none exists after publication failure.
///
/// # Errors
///
/// Returns a contextual message if invalidation, serialization, writing,
/// syncing, or atomic publication fails.
pub fn write_plan(output_dir: &Path, output: &PlanOutput) -> Result<std::path::PathBuf, String> {
    invalidate_output(output_dir)?;
    publish_plan_after_invalidation(output_dir, output, |_| Ok(()))
}

fn publish_plan_after_invalidation<F>(
    output_dir: &Path,
    output: &PlanOutput,
    before_rename: F,
) -> Result<std::path::PathBuf, String>
where
    F: FnOnce(&Path) -> io::Result<()>,
{
    let file_name = match output {
        PlanOutput::Moves(_) => MOVES_FILE,
        PlanOutput::Review(_) => REVIEW_FILE,
    };
    let destination = output_dir.join(file_name);
    let temporary = output_dir.join(TEMP_FILE);
    let bytes = output
        .canonical_json()
        .map_err(|error| format!("cannot serialize output: {error}"))?;

    let publication = (|| -> io::Result<()> {
        let mut file = open_new(&temporary)?;
        file.write_all(bytes.as_bytes())?;
        file.sync_all()?;
        before_rename(&temporary)?;
        drop(file);
        fs::rename(&temporary, &destination)?;
        File::open(output_dir)?.sync_all()?;
        Ok(())
    })();

    match publication {
        Ok(()) => Ok(destination),
        Err(error) => {
            let _ = remove_owned_path(&temporary);
            let _ = remove_owned_path(&destination);
            Err(format!(
                "cannot atomically publish {}: {error}",
                destination.display()
            ))
        }
    }
}

/// Run the planner from JSON files and write its advisory artifact.
///
/// # Errors
///
/// Returns a contextual input or output file error.
pub fn run_plan_files(
    yard_path: &Path,
    pickups_path: &Path,
    output_dir: &Path,
    timeout: Duration,
) -> Result<(PlanOutput, std::path::PathBuf), String> {
    // Invalidate first: malformed input and all later failures must not leave a
    // previous executable generation at this current output location.
    invalidate_output(output_dir)?;
    let (yard, wave) = read_inputs(yard_path, pickups_path)?;
    let output = planner::plan(&yard, &wave, timeout);
    let destination = publish_plan_after_invalidation(output_dir, &output, |_| Ok(()))?;
    Ok((output, destination))
}

fn open_new(path: &Path) -> io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

fn remove_owned_path(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path)
            .map_err(|error| format!("cannot invalidate {}: {error}", path.display())),
        Ok(_) => fs::remove_file(path)
            .map_err(|error| format!("cannot invalidate {}: {error}", path.display())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("cannot inspect {}: {error}", path.display())),
    }
}

#[must_use]
pub fn demo_case(suffix: usize) -> (Yard, PickupWave) {
    use std::collections::BTreeMap;

    use model::{Container, Stack};
    let container = |id: &str, weight_class| Container {
        id: format!("{id}-{suffix}"),
        weight_class,
        reefer: false,
        hazardous_group: None,
        customs_hold: false,
    };
    let stacks = vec![
        Stack {
            id: "A".to_string(),
            x: 0,
            y: 0,
            reefer_socket: false,
            frozen: false,
            neighbors: vec![],
            containers: vec![container("P1", 5), container("X1", 4), container("X2", 3)],
        },
        Stack {
            id: "B".to_string(),
            x: 1,
            y: 0,
            reefer_socket: false,
            frozen: false,
            neighbors: vec![],
            containers: vec![container("P2", 5)],
        },
        Stack {
            id: "C".to_string(),
            x: 2,
            y: 0,
            reefer_socket: false,
            frozen: false,
            neighbors: vec![],
            containers: vec![],
        },
        Stack {
            id: "D".to_string(),
            x: 3,
            y: 0,
            reefer_socket: false,
            frozen: false,
            neighbors: vec![],
            containers: vec![],
        },
    ];
    (
        Yard {
            max_height: 5,
            hazardous_exclusions: BTreeMap::new(),
            stacks,
        },
        PickupWave {
            pickups: vec![format!("P1-{suffix}"), format!("P2-{suffix}")],
        },
    )
}

#[cfg(test)]
mod publication_tests {
    use std::io;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static SERIAL: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn injected_atomic_write_failure_leaves_no_current_or_partial_artifact() {
        let serial = SERIAL.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "container-yard-planner-write-failure-{}-{serial}",
            std::process::id()
        ));
        let (yard, wave) = demo_case(0);
        let success = planner::plan(&yard, &wave, Duration::from_secs(10));
        write_plan(&root, &success).expect("control success should publish");
        assert!(root.join(MOVES_FILE).is_file());

        invalidate_output(&root).expect("new generation should invalidate success");
        let rejection = PlanOutput::Review(model::ReviewOutput {
            status: "review_required".to_string(),
            executable: false,
            planner: "bounded-lookahead-v1".to_string(),
            first_blocked_pickup: Some("P1-0".to_string()),
            reason: "injected publication failure".to_string(),
            preventing_conditions: vec!["write failure".to_string()],
        });
        let result = publish_plan_after_invalidation(&root, &rejection, |_| {
            Err(io::Error::other("injected before atomic rename"))
        });
        assert!(result.is_err());
        for artifact in [MOVES_FILE, REVIEW_FILE, TEMP_FILE] {
            assert!(!root.join(artifact).exists(), "stale artifact: {artifact}");
        }
        fs::remove_dir_all(&root).expect("clean test fixture");
    }
}
