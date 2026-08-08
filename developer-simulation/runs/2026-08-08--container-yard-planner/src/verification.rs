//! Executable artifact contract verification layered over transition replay.

use crate::model::{MoveKind, MovesOutput, PickupWave, Yard};
use crate::simulator;

const PICKUP_CHECKS: [&str; 4] = [
    "required_pickup_order",
    "source_top",
    "source_not_frozen",
    "customs_hold_clear",
];

/// Verify output metadata and explanations before replaying state transitions.
///
/// # Errors
///
/// Returns the first dishonest or inconsistent contract field, or the first
/// illegal transition found by the separate replay implementation.
pub fn verify_moves_output(
    yard: &Yard,
    wave: &PickupWave,
    output: &MovesOutput,
) -> Result<(), String> {
    if output.status != "executable" {
        return Err("status must be executable".to_string());
    }
    if !output.executable {
        return Err("executable flag must be true".to_string());
    }
    if output.planner.trim().is_empty() {
        return Err("planner name must not be empty".to_string());
    }
    if !output.simulator_verified {
        return Err("simulator_verified flag must be true".to_string());
    }

    let relocation_count = output
        .moves
        .iter()
        .filter(|planned| planned.kind == MoveKind::Relocate)
        .count();
    let pickup_count = output
        .moves
        .iter()
        .filter(|planned| planned.kind == MoveKind::Pickup)
        .count();
    if output.relocations != relocation_count {
        return Err(format!(
            "relocations count mismatch: declared {}, actual {relocation_count}",
            output.relocations
        ));
    }
    if output.pickups != pickup_count || pickup_count != wave.pickups.len() {
        return Err(format!(
            "pickups count mismatch: declared {}, actual {pickup_count}, required {}",
            output.pickups,
            wave.pickups.len()
        ));
    }

    let mut completed_pickups = 0_usize;
    for (index, planned) in output.moves.iter().enumerate() {
        let expected_step = index + 1;
        if planned.step != expected_step {
            return Err(format!(
                "step mismatch: expected {expected_step}, got {}",
                planned.step
            ));
        }
        let expected_rank = completed_pickups + 1;
        let expected_pickup = wave.pickups.get(completed_pickups).ok_or_else(|| {
            format!("step {expected_step}: move appears after the pickup wave completed")
        })?;
        if planned.pickup_rank != expected_rank {
            return Err(format!(
                "step {expected_step}: pickup_rank must be {expected_rank}, got {}",
                planned.pickup_rank
            ));
        }

        let (expected_reason, expected_checks) = match planned.kind {
            MoveKind::Relocate => (
                format!("expose pickup {expected_pickup}"),
                relocation_checks(yard, &planned.container_id)?,
            ),
            MoveKind::Pickup => {
                if planned.container_id != *expected_pickup {
                    return Err(format!(
                        "step {expected_step}: pickup metadata names {}, expected {expected_pickup}",
                        planned.container_id
                    ));
                }
                completed_pickups += 1;
                (
                    format!("fulfill pickup {expected_pickup}"),
                    PICKUP_CHECKS.iter().map(ToString::to_string).collect(),
                )
            }
        };
        if planned.needed_for.trim().is_empty() || planned.needed_for != expected_reason {
            return Err(format!(
                "step {expected_step}: needed_for must be {expected_reason:?}"
            ));
        }
        if planned.rule_checks != expected_checks {
            return Err(format!(
                "step {expected_step}: rule_checks do not match the required canonical checks"
            ));
        }
    }

    simulator::replay(yard, wave, &output.moves)
}

fn relocation_checks(yard: &Yard, container_id: &str) -> Result<Vec<String>, String> {
    let container = yard
        .stacks
        .iter()
        .flat_map(|stack| &stack.containers)
        .find(|candidate| candidate.id == container_id)
        .ok_or_else(|| format!("explanation names unknown container {container_id}"))?;
    Ok(vec![
        "source_top".to_string(),
        "source_not_frozen".to_string(),
        "customs_hold_clear".to_string(),
        "destination_not_frozen".to_string(),
        "capacity_below_maximum".to_string(),
        "heavy_below_light".to_string(),
        if container.reefer {
            "reefer_socket_present".to_string()
        } else {
            "reefer_socket_not_required".to_string()
        },
        "hazardous_neighbor_segregation".to_string(),
    ])
}
