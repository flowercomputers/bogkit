use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Container {
    pub id: String,
    pub weight_class: u8,
    #[serde(default)]
    pub reefer: bool,
    #[serde(default)]
    pub hazardous_group: Option<String>,
    #[serde(default)]
    pub customs_hold: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stack {
    pub id: String,
    pub x: i32,
    pub y: i32,
    #[serde(default)]
    pub reefer_socket: bool,
    #[serde(default)]
    pub frozen: bool,
    #[serde(default)]
    pub neighbors: Vec<String>,
    /// Containers in bottom-to-top order.
    #[serde(default)]
    pub containers: Vec<Container>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Yard {
    pub max_height: usize,
    #[serde(default)]
    pub hazardous_exclusions: BTreeMap<String, Vec<String>>,
    pub stacks: Vec<Stack>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PickupWave {
    pub pickups: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MoveKind {
    Relocate,
    Pickup,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedMove {
    pub step: usize,
    pub kind: MoveKind,
    pub container_id: String,
    pub from: String,
    pub to: String,
    pub pickup_rank: usize,
    pub needed_for: String,
    pub rule_checks: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MovesOutput {
    pub status: String,
    pub executable: bool,
    pub planner: String,
    pub relocations: usize,
    pub pickups: usize,
    pub simulator_verified: bool,
    pub moves: Vec<PlannedMove>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewOutput {
    pub status: String,
    pub executable: bool,
    pub planner: String,
    pub first_blocked_pickup: Option<String>,
    pub reason: String,
    pub preventing_conditions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PlanOutput {
    Moves(MovesOutput),
    Review(ReviewOutput),
}

impl PlanOutput {
    /// Serialize a plan with stable field ordering and a trailing newline.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization fails.
    pub fn canonical_json(&self) -> Result<String, serde_json::Error> {
        let mut json = serde_json::to_string_pretty(self)?;
        json.push('\n');
        Ok(json)
    }

    #[must_use]
    pub fn relocations(&self) -> Option<usize> {
        match self {
            Self::Moves(output) => Some(output.relocations),
            Self::Review(_) => None,
        }
    }
}

/// Check all static constraints on the authoritative input snapshot.
///
/// # Errors
///
/// Returns every detected validation problem, sorted and deduplicated.
pub fn validate_snapshot(yard: &Yard) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    if yard.max_height == 0 {
        errors.push("max_height must be greater than zero".to_string());
    }

    let stack_ids: BTreeSet<_> = yard.stacks.iter().map(|stack| stack.id.as_str()).collect();
    if stack_ids.len() != yard.stacks.len() {
        errors.push("stack IDs must be unique".to_string());
    }

    let mut container_ids = BTreeSet::new();
    for stack in &yard.stacks {
        if stack.containers.len() > yard.max_height {
            errors.push(format!(
                "stack {} exceeds maximum height {}",
                stack.id, yard.max_height
            ));
        }
        for neighbor in &stack.neighbors {
            if !stack_ids.contains(neighbor.as_str()) {
                errors.push(format!(
                    "stack {} names unknown neighbor {}",
                    stack.id, neighbor
                ));
            } else if yard
                .stacks
                .iter()
                .find(|candidate| candidate.id == *neighbor)
                .is_some_and(|candidate| !candidate.neighbors.contains(&stack.id))
            {
                errors.push(format!(
                    "neighbor relation between {} and {} must be reciprocal",
                    stack.id, neighbor
                ));
            }
        }
        for container in &stack.containers {
            if !(1..=5).contains(&container.weight_class) {
                errors.push(format!(
                    "container {} has weight class outside 1..=5",
                    container.id
                ));
            }
            if !container_ids.insert(container.id.as_str()) {
                errors.push(format!("container ID {} is duplicated", container.id));
            }
            if container.reefer && !stack.reefer_socket {
                errors.push(format!(
                    "reefer container {} is in stack {} without a socket",
                    container.id, stack.id
                ));
            }
        }
        for pair in stack.containers.windows(2) {
            if pair[0].weight_class < pair[1].weight_class {
                errors.push(format!(
                    "stack {} has heavier container {} above lighter container {}",
                    stack.id, pair[1].id, pair[0].id
                ));
            }
        }
    }

    for stack in &yard.stacks {
        for neighbor_id in &stack.neighbors {
            let Some(neighbor) = yard.stacks.iter().find(|item| item.id == *neighbor_id) else {
                continue;
            };
            for left in &stack.containers {
                for right in &neighbor.containers {
                    if hazardous_conflict(yard, left, right) {
                        errors.push(format!(
                            "hazardous groups conflict between container {} in {} and container {} in {}",
                            left.id, stack.id, right.id, neighbor.id
                        ));
                    }
                }
            }
        }
    }

    errors.sort();
    errors.dedup();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

#[must_use]
pub fn hazardous_conflict(yard: &Yard, left: &Container, right: &Container) -> bool {
    let (Some(left_group), Some(right_group)) = (
        left.hazardous_group.as_ref(),
        right.hazardous_group.as_ref(),
    ) else {
        return false;
    };
    yard.hazardous_exclusions
        .get(left_group)
        .is_some_and(|groups| groups.contains(right_group))
        || yard
            .hazardous_exclusions
            .get(right_group)
            .is_some_and(|groups| groups.contains(left_group))
}
