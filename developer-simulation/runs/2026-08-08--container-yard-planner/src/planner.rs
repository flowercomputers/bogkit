use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::model::{
    Container, MoveKind, MovesOutput, PickupWave, PlanOutput, PlannedMove, ReviewOutput, Stack,
    Yard, hazardous_conflict, validate_snapshot,
};
use crate::simulator;

const LOOKAHEAD_PICKUPS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    NearestBaseline,
    BoundedLookahead,
}

impl Strategy {
    fn name(self) -> &'static str {
        match self {
            Self::NearestBaseline => "nearest-legal-slot-baseline",
            Self::BoundedLookahead => "bounded-lookahead-v1",
        }
    }
}

#[must_use]
pub fn plan(yard: &Yard, wave: &PickupWave, timeout: Duration) -> PlanOutput {
    plan_with_strategy(yard, wave, timeout, Strategy::BoundedLookahead)
}

#[must_use]
pub fn baseline(yard: &Yard, wave: &PickupWave, timeout: Duration) -> PlanOutput {
    plan_with_strategy(yard, wave, timeout, Strategy::NearestBaseline)
}

/// Compute a complete candidate using the selected deterministic strategy.
///
/// # Panics
///
/// Internal invariant failures can panic if a previously discovered blocker or
/// legal candidate disappears without an intervening state transition.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn plan_with_strategy(
    yard: &Yard,
    wave: &PickupWave,
    timeout: Duration,
    strategy: Strategy,
) -> PlanOutput {
    let started = Instant::now();
    if let Err(errors) = validate_snapshot(yard) {
        return review(
            strategy,
            wave.pickups.first().cloned(),
            "snapshot validation failed",
            errors,
        );
    }
    if started.elapsed() >= timeout {
        return timeout_review(strategy, wave.pickups.first().cloned());
    }

    let mut working = yard.clone();
    let mut moves = Vec::new();

    for (pickup_index, pickup_id) in wave.pickups.iter().enumerate() {
        loop {
            if started.elapsed() >= timeout {
                return timeout_review(strategy, Some(pickup_id.clone()));
            }
            let Some((source_index, level)) = locate_container(&working, pickup_id) else {
                return review(
                    strategy,
                    Some(pickup_id.clone()),
                    "requested container is absent from the snapshot",
                    vec![format!("container {pickup_id} was not found")],
                );
            };

            if working.stacks[source_index].frozen {
                return review(
                    strategy,
                    Some(pickup_id.clone()),
                    "the first blocked pickup is in a frozen cell",
                    vec![format!(
                        "source stack {} is frozen by maintenance",
                        working.stacks[source_index].id
                    )],
                );
            }
            if working.stacks[source_index].containers[level].customs_hold {
                return review(
                    strategy,
                    Some(pickup_id.clone()),
                    "the first blocked pickup is on customs hold",
                    vec![format!("container {pickup_id} cannot move or be picked")],
                );
            }

            let top_level = working.stacks[source_index].containers.len() - 1;
            if level == top_level {
                let source_id = working.stacks[source_index].id.clone();
                working.stacks[source_index].containers.pop();
                moves.push(PlannedMove {
                    step: moves.len() + 1,
                    kind: MoveKind::Pickup,
                    container_id: pickup_id.clone(),
                    from: source_id,
                    to: "pickup_lane".to_string(),
                    pickup_rank: pickup_index + 1,
                    needed_for: format!("fulfill pickup {pickup_id}"),
                    rule_checks: vec![
                        "required_pickup_order".to_string(),
                        "source_top".to_string(),
                        "source_not_frozen".to_string(),
                        "customs_hold_clear".to_string(),
                    ],
                });
                break;
            }

            let blocker = working.stacks[source_index]
                .containers
                .last()
                .expect("a buried pickup necessarily has a blocker")
                .clone();
            if blocker.customs_hold {
                return review(
                    strategy,
                    Some(pickup_id.clone()),
                    "a customs-held blocker cannot be moved",
                    vec![format!(
                        "container {} is above pickup {} and is on customs hold",
                        blocker.id, pickup_id
                    )],
                );
            }

            let candidates = legal_destinations(&working, source_index, &blocker);
            if candidates.is_empty() {
                return review(
                    strategy,
                    Some(pickup_id.clone()),
                    "no legal destination exists for the top blocker",
                    explain_no_destination(&working, source_index, &blocker),
                );
            }
            let destination_index = choose_destination(
                &working,
                source_index,
                &blocker,
                &candidates,
                &wave.pickups[pickup_index..],
                strategy,
            );
            let source_id = working.stacks[source_index].id.clone();
            let destination_id = working.stacks[destination_index].id.clone();
            working.stacks[source_index].containers.pop();
            working.stacks[destination_index]
                .containers
                .push(blocker.clone());
            moves.push(PlannedMove {
                step: moves.len() + 1,
                kind: MoveKind::Relocate,
                container_id: blocker.id.clone(),
                from: source_id,
                to: destination_id,
                pickup_rank: pickup_index + 1,
                needed_for: format!("expose pickup {pickup_id}"),
                rule_checks: relocation_checks(&blocker),
            });
        }
    }

    let relocations = moves
        .iter()
        .filter(|item| item.kind == MoveKind::Relocate)
        .count();
    let mut output = MovesOutput {
        status: "executable".to_string(),
        executable: true,
        planner: strategy.name().to_string(),
        relocations,
        pickups: wave.pickups.len(),
        simulator_verified: false,
        moves,
    };

    if started.elapsed() >= timeout {
        return timeout_review(strategy, wave.pickups.first().cloned());
    }
    let replay_budget = timeout.saturating_sub(started.elapsed());
    match simulator::replay_with_timeout(yard, wave, &output.moves, replay_budget) {
        Ok(()) => {
            output.simulator_verified = true;
            PlanOutput::Moves(output)
        }
        Err(error) if error == simulator::TIMEOUT_ERROR => {
            timeout_review(strategy, wave.pickups.first().cloned())
        }
        Err(error) => review(
            strategy,
            wave.pickups.first().cloned(),
            "separate transition replay rejected the candidate plan",
            vec![error],
        ),
    }
}

fn locate_container(yard: &Yard, id: &str) -> Option<(usize, usize)> {
    yard.stacks
        .iter()
        .enumerate()
        .find_map(|(stack_index, stack)| {
            stack
                .containers
                .iter()
                .position(|container| container.id == id)
                .map(|level| (stack_index, level))
        })
}

fn legal_destinations(yard: &Yard, source: usize, container: &Container) -> Vec<usize> {
    (0..yard.stacks.len())
        .filter(|&destination| {
            destination != source && destination_is_legal(yard, destination, container)
        })
        .collect()
}

fn destination_is_legal(yard: &Yard, destination: usize, container: &Container) -> bool {
    let stack = &yard.stacks[destination];
    if stack.frozen || stack.containers.len() >= yard.max_height {
        return false;
    }
    if container.reefer && !stack.reefer_socket {
        return false;
    }
    if stack
        .containers
        .last()
        .is_some_and(|top| top.weight_class < container.weight_class)
    {
        return false;
    }
    for neighbor_id in &stack.neighbors {
        let Some(neighbor) = yard.stacks.iter().find(|item| item.id == *neighbor_id) else {
            return false;
        };
        if neighbor
            .containers
            .iter()
            .any(|other| hazardous_conflict(yard, container, other))
        {
            return false;
        }
    }
    true
}

fn choose_destination(
    yard: &Yard,
    source: usize,
    blocker: &Container,
    candidates: &[usize],
    remaining_pickups: &[String],
    strategy: Strategy,
) -> usize {
    let source_stack = &yard.stacks[source];
    let future_ranks: BTreeMap<&str, usize> = remaining_pickups
        .iter()
        .take(LOOKAHEAD_PICKUPS)
        .enumerate()
        .map(|(rank, id)| (id.as_str(), rank))
        .collect();

    *candidates
        .iter()
        .min_by_key(|&&index| {
            let destination = &yard.stacks[index];
            let distance = manhattan(source_stack, destination);
            match strategy {
                Strategy::NearestBaseline => (0_usize, 0_usize, distance, destination.id.as_str()),
                Strategy::BoundedLookahead => {
                    let buried_rank = destination
                        .containers
                        .iter()
                        .filter_map(|container| future_ranks.get(container.id.as_str()).copied())
                        .min()
                        .map_or(0, |rank| LOOKAHEAD_PICKUPS + 1 - rank);
                    let constrained_top = usize::from(6_u8.saturating_sub(blocker.weight_class));
                    let height = destination.containers.len() + 1;
                    (
                        buried_rank,
                        height * 10 + constrained_top,
                        distance,
                        destination.id.as_str(),
                    )
                }
            }
        })
        .expect("candidate list is non-empty")
}

fn manhattan(left: &Stack, right: &Stack) -> usize {
    left.x.abs_diff(right.x) as usize + left.y.abs_diff(right.y) as usize
}

fn relocation_checks(container: &Container) -> Vec<String> {
    vec![
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
    ]
}

fn explain_no_destination(yard: &Yard, source: usize, container: &Container) -> Vec<String> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for (index, stack) in yard.stacks.iter().enumerate() {
        if index == source {
            continue;
        }
        let reason = if stack.frozen {
            "frozen destination cells"
        } else if stack.containers.len() >= yard.max_height {
            "capacity limits"
        } else if container.reefer && !stack.reefer_socket {
            "reefer socket requirement"
        } else if stack
            .containers
            .last()
            .is_some_and(|top| top.weight_class < container.weight_class)
        {
            "weight placement rule"
        } else {
            "hazardous neighbor exclusion"
        };
        *counts.entry(reason).or_default() += 1;
    }
    counts
        .into_iter()
        .map(|(reason, count)| format!("{count} candidate stacks blocked by {reason}"))
        .collect()
}

fn timeout_review(strategy: Strategy, pickup: Option<String>) -> PlanOutput {
    review(
        strategy,
        pickup,
        "planning deadline elapsed; no partial plan is executable",
        vec!["time limit reached".to_string()],
    )
}

fn review(
    strategy: Strategy,
    pickup: Option<String>,
    reason: &str,
    mut preventing_conditions: Vec<String>,
) -> PlanOutput {
    preventing_conditions.sort();
    PlanOutput::Review(ReviewOutput {
        status: "review_required".to_string(),
        executable: false,
        planner: strategy.name().to_string(),
        first_blocked_pickup: pickup,
        reason: reason.to_string(),
        preventing_conditions,
    })
}
