//! Separate state-transition replay.
//!
//! This module deliberately does not call the planner's legality helpers. It
//! performs transition checks again while replaying emitted moves, but shares
//! the model's static snapshot and hazardous-conflict definitions.

use std::time::{Duration, Instant};

use crate::model::{
    MoveKind, PickupWave, PlannedMove, Yard, hazardous_conflict, validate_snapshot,
};

pub const TIMEOUT_ERROR: &str = "transition replay deadline elapsed";

/// Replay a proposed plan using checks separate from the planner.
///
/// # Errors
///
/// Returns the first illegal transition or incomplete pickup sequence.
#[allow(clippy::too_many_lines)]
pub fn replay(yard: &Yard, wave: &PickupWave, moves: &[PlannedMove]) -> Result<(), String> {
    replay_until(yard, wave, moves, None)
}

/// Replay a proposed plan within a strict validation budget.
///
/// # Errors
///
/// Returns the first illegal transition, incomplete pickup sequence, or a
/// deadline error.
pub fn replay_with_timeout(
    yard: &Yard,
    wave: &PickupWave,
    moves: &[PlannedMove],
    timeout: Duration,
) -> Result<(), String> {
    replay_until(yard, wave, moves, Some(Instant::now() + timeout))
}

#[allow(clippy::too_many_lines)]
fn replay_until(
    yard: &Yard,
    wave: &PickupWave,
    moves: &[PlannedMove],
    deadline: Option<Instant>,
) -> Result<(), String> {
    validate_snapshot(yard).map_err(|errors| format!("invalid initial snapshot: {errors:?}"))?;
    let mut state = yard.clone();
    let mut next_pickup = 0_usize;

    for (index, planned) in moves.iter().enumerate() {
        if deadline.is_some_and(|limit| Instant::now() >= limit) {
            return Err(TIMEOUT_ERROR.to_string());
        }
        let expected_step = index + 1;
        if planned.step != expected_step {
            return Err(format!(
                "step numbering error: expected {expected_step}, got {}",
                planned.step
            ));
        }
        let source_index = stack_index(&state, &planned.from)
            .ok_or_else(|| format!("step {expected_step}: unknown source {}", planned.from))?;
        if state.stacks[source_index].frozen {
            return Err(format!("step {expected_step}: source cell is frozen"));
        }
        let top = state.stacks[source_index]
            .containers
            .last()
            .ok_or_else(|| format!("step {expected_step}: source stack is empty"))?;
        if top.id != planned.container_id {
            return Err(format!(
                "step {expected_step}: {} is not the top container",
                planned.container_id
            ));
        }
        if top.customs_hold {
            return Err(format!(
                "step {expected_step}: container {} is on customs hold",
                top.id
            ));
        }

        match planned.kind {
            MoveKind::Pickup => {
                let expected = wave.pickups.get(next_pickup).ok_or_else(|| {
                    format!("step {expected_step}: plan contains an extra pickup")
                })?;
                if &planned.container_id != expected || planned.to != "pickup_lane" {
                    return Err(format!(
                        "step {expected_step}: pickup order violation, expected {expected}"
                    ));
                }
                state.stacks[source_index].containers.pop();
                next_pickup += 1;
            }
            MoveKind::Relocate => {
                let destination_index = stack_index(&state, &planned.to).ok_or_else(|| {
                    format!("step {expected_step}: unknown destination {}", planned.to)
                })?;
                if destination_index == source_index {
                    return Err(format!("step {expected_step}: source equals destination"));
                }
                if state.stacks[destination_index].frozen {
                    return Err(format!("step {expected_step}: destination cell is frozen"));
                }
                if state.stacks[destination_index].containers.len() >= state.max_height {
                    return Err(format!(
                        "step {expected_step}: destination exceeds capacity"
                    ));
                }
                if top.reefer && !state.stacks[destination_index].reefer_socket {
                    return Err(format!(
                        "step {expected_step}: reefer destination has no socket"
                    ));
                }
                if state.stacks[destination_index]
                    .containers
                    .last()
                    .is_some_and(|below| below.weight_class < top.weight_class)
                {
                    return Err(format!(
                        "step {expected_step}: move creates a weight inversion"
                    ));
                }
                for neighbor_id in &state.stacks[destination_index].neighbors {
                    let neighbor_index = stack_index(&state, neighbor_id).ok_or_else(|| {
                        format!("step {expected_step}: destination has unknown neighbor")
                    })?;
                    if state.stacks[neighbor_index]
                        .containers
                        .iter()
                        .any(|other| hazardous_conflict(&state, top, other))
                    {
                        return Err(format!(
                            "step {expected_step}: hazardous neighbor exclusion violated"
                        ));
                    }
                }
                let transferred = state.stacks[source_index]
                    .containers
                    .pop()
                    .ok_or_else(|| format!("step {expected_step}: source became empty"))?;
                state.stacks[destination_index].containers.push(transferred);
            }
        }

        validate_snapshot(&state).map_err(|errors| {
            format!("step {expected_step}: intermediate state invalid: {errors:?}")
        })?;
    }

    if next_pickup != wave.pickups.len() {
        return Err(format!(
            "plan ended after {next_pickup} pickups; {} required",
            wave.pickups.len()
        ));
    }
    if deadline.is_some_and(|limit| Instant::now() >= limit) {
        return Err(TIMEOUT_ERROR.to_string());
    }
    Ok(())
}

fn stack_index(yard: &Yard, id: &str) -> Option<usize> {
    yard.stacks.iter().position(|stack| stack.id == id)
}
