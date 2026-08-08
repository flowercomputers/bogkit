use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use container_yard_planner::demo_case;
use container_yard_planner::model::{
    Container, MoveKind, MovesOutput, PickupWave, PlanOutput, PlannedMove, ReviewOutput, Stack,
    Yard,
};
use container_yard_planner::{planner, run_plan_files, simulator, verification, write_plan};

const LIMIT: Duration = Duration::from_secs(10);
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[test]
fn repeated_micro_geometry_is_deterministic_and_replays_every_move() {
    let mut baseline_total = 0;
    let mut planned_total = 0;

    for suffix in 0..27 {
        let (yard, wave) = demo_case(suffix);
        let baseline = planner::baseline(&yard, &wave, LIMIT);
        let planned = planner::plan(&yard, &wave, LIMIT);
        let baseline_count = baseline.relocations().expect("baseline should complete");
        let planned_count = planned.relocations().expect("planner should complete");
        assert!(
            planned_count * 100 <= baseline_count * 105,
            "suffix repetition {suffix} is more than 5% worse"
        );
        baseline_total += baseline_count;
        planned_total += planned_count;
        let PlanOutput::Moves(moves) = planned else {
            panic!("suffix repetition {suffix} did not produce moves");
        };
        simulator::replay(&yard, &wave, &moves.moves).expect("transition replay should pass");
    }

    assert_eq!(baseline_total, 81);
    assert_eq!(planned_total, 54);
    assert!(planned_total * 100 <= baseline_total * 80);
    println!(
        "repeated micro-geometry: repetitions=27 mechanical_baseline_total={baseline_total} mechanical_planned_total={planned_total} per_geometry=3_to_2"
    );
}

#[test]
fn three_infeasible_cases_return_non_executable_review_without_partial_moves() {
    for (label, yard) in [
        ("held pickup", held_pickup()),
        ("frozen pickup", frozen_pickup()),
        ("capacity dead end", capacity_dead_end()),
    ] {
        let wave = PickupWave {
            pickups: vec!["P1".to_string()],
        };
        let output = planner::plan(&yard, &wave, LIMIT);
        let PlanOutput::Review(review) = output else {
            panic!("infeasible fixture returned executable moves");
        };
        assert!(!review.executable);
        assert_eq!(review.first_blocked_pickup.as_deref(), Some("P1"));
        assert!(!review.preventing_conditions.is_empty());
        println!("synthetic infeasible case '{label}': {}", review.reason);
    }
}

#[test]
fn timeout_is_a_review_and_never_a_truncated_executable_plan() {
    let (yard, wave) = demo_case(88);
    let output = planner::plan(&yard, &wave, Duration::ZERO);
    let PlanOutput::Review(review) = output else {
        panic!("zero timeout returned executable moves");
    };
    assert!(!review.executable);
    assert!(review.reason.contains("deadline elapsed"));
}

#[test]
fn canonical_output_is_identical_across_five_runs_and_key_orders() {
    let (yard, wave) = demo_case(7);
    let expected = planner::plan(&yard, &wave, LIMIT)
        .canonical_json()
        .expect("output should serialize");
    for _ in 0..5 {
        assert_eq!(
            planner::plan(&yard, &wave, LIMIT)
                .canonical_json()
                .expect("output should serialize"),
            expected
        );
    }

    let yard_variants = yard_key_order_variants();
    let wave_variants = [
        r#"{"pickups":["P1-7","P2-7"]}"#,
        r#"{ "pickups" : [ "P1-7", "P2-7" ] }"#,
        r#"{
            "pickups": ["P1-7", "P2-7"]
        }"#,
        r#"{"pickups" : ["P1-7", "P2-7"]}"#,
        r#"{ "pickups":["P1-7","P2-7"] }"#,
    ];
    for (yard_json, wave_json) in yard_variants.iter().zip(wave_variants) {
        let parsed_yard: Yard = serde_json::from_str(yard_json).expect("valid yard variant");
        let parsed_wave: PickupWave = serde_json::from_str(wave_json).expect("valid wave variant");
        assert_eq!(
            planner::plan(&parsed_yard, &parsed_wave, LIMIT)
                .canonical_json()
                .expect("output should serialize"),
            expected
        );
    }
}

#[test]
fn dense_representative_workload_finishes_under_deadline_and_replays() {
    let (yard, wave) = dense_yard();
    assert_eq!(yard.stacks.len(), 48 * 6);
    assert_eq!(
        yard.stacks
            .iter()
            .map(|stack| stack.containers.len())
            .sum::<usize>(),
        1_280
    );
    assert_eq!(wave.pickups.len(), 40);

    let baseline = planner::baseline(&yard, &wave, LIMIT);
    let baseline_relocations = baseline
        .relocations()
        .expect("dense baseline should complete");
    let started = Instant::now();
    let output = planner::plan(&yard, &wave, LIMIT);
    let elapsed = started.elapsed();
    let PlanOutput::Moves(moves) = output else {
        panic!("representative workload should be feasible");
    };
    assert!(elapsed < LIMIT, "planner took {elapsed:?}");
    simulator::replay(&yard, &wave, &moves.moves).expect("dense plan should replay");
    assert!(moves.relocations * 100 <= baseline_relocations * 105);
    println!(
        "dense workload: stacks=288 containers=1280 pickups=40 baseline_relocations={baseline_relocations} planned_relocations={} total_moves={} elapsed={elapsed:?}",
        moves.relocations,
        moves.moves.len()
    );
}

#[test]
fn separate_transition_replay_rejects_illegal_top_and_weight_moves() {
    let (yard, wave) = demo_case(0);
    let illegal_non_top = vec![PlannedMove {
        step: 1,
        kind: MoveKind::Relocate,
        container_id: "X1-0".to_string(),
        from: "A".to_string(),
        to: "C".to_string(),
        pickup_rank: 1,
        needed_for: "corruption test".to_string(),
        rule_checks: vec![],
    }];
    assert!(simulator::replay(&yard, &wave, &illegal_non_top).is_err());

    let illegal_weight = vec![
        PlannedMove {
            step: 1,
            kind: MoveKind::Relocate,
            container_id: "X2-0".to_string(),
            from: "A".to_string(),
            to: "B".to_string(),
            pickup_rank: 1,
            needed_for: "setup".to_string(),
            rule_checks: vec![],
        },
        PlannedMove {
            step: 2,
            kind: MoveKind::Relocate,
            container_id: "X1-0".to_string(),
            from: "A".to_string(),
            to: "B".to_string(),
            pickup_rank: 1,
            needed_for: "corruption test".to_string(),
            rule_checks: vec![],
        },
    ];
    let error = simulator::replay(&yard, &wave, &illegal_weight)
        .expect_err("weight inversion should fail replay");
    assert!(error.contains("weight inversion"));
}

#[test]
fn destination_filter_enforces_reefer_frozen_weight_and_hazard_rules() {
    let yard = constrained_destinations();
    let wave = PickupWave {
        pickups: vec!["P1".to_string()],
    };
    let output = planner::plan(&yard, &wave, LIMIT);
    let PlanOutput::Moves(moves) = output else {
        panic!("fixture has one legal reefer destination");
    };
    let relocation = moves
        .moves
        .iter()
        .find(|item| item.kind == MoveKind::Relocate)
        .expect("fixture requires relocation");
    assert_eq!(relocation.to, "LEGAL");
    assert!(
        relocation
            .rule_checks
            .contains(&"reefer_socket_present".to_string())
    );
    simulator::replay(&yard, &wave, &moves.moves).expect("constrained plan should replay");
}

#[test]
fn exact_reviewer_witness_is_a_safe_feasible_false_negative() {
    let (yard, wave, witness) = feasible_false_negative_witness();
    let proposal = planner::plan(&yard, &wave, LIMIT);
    let PlanOutput::Review(review) = proposal else {
        panic!("bounded proposal generator unexpectedly completed witness");
    };
    assert_eq!(review.first_blocked_pickup.as_deref(), Some("P"));
    assert_eq!(
        review.reason,
        "no legal destination exists for the top blocker"
    );
    verification::verify_moves_output(&yard, &wave, &witness)
        .expect("reviewer's three-move witness proves feasibility");
}

#[test]
fn verifier_rejects_dishonest_metadata_and_explanations() {
    let (yard, wave) = demo_case(0);
    let PlanOutput::Moves(valid) = planner::plan(&yard, &wave, LIMIT) else {
        panic!("demo must plan");
    };
    verification::verify_moves_output(&yard, &wave, &valid).expect("control must verify");

    let mut corruptions: Vec<(&str, MovesOutput)> = Vec::new();
    let mut changed = valid.clone();
    changed.status = "not_executable".to_string();
    corruptions.push(("status", changed));
    let mut changed = valid.clone();
    changed.executable = false;
    corruptions.push(("executable", changed));
    let mut changed = valid.clone();
    changed.relocations = 999;
    corruptions.push(("relocations", changed));
    let mut changed = valid.clone();
    changed.pickups = 999;
    corruptions.push(("pickups", changed));
    let mut changed = valid.clone();
    changed.simulator_verified = false;
    corruptions.push(("simulator flag", changed));
    let mut changed = valid.clone();
    changed.moves[0].step = 999;
    corruptions.push(("step", changed));
    let mut changed = valid.clone();
    changed.moves[0].pickup_rank = 999;
    corruptions.push(("pickup rank", changed));
    let mut changed = valid.clone();
    changed.moves[0].needed_for.clear();
    corruptions.push(("empty reason", changed));
    let mut changed = valid.clone();
    changed.moves[0].needed_for = "wrong reason".to_string();
    corruptions.push(("incorrect reason", changed));
    let mut changed = valid.clone();
    changed.moves[0].rule_checks.clear();
    corruptions.push(("missing checks", changed));
    let mut changed = valid;
    changed.moves[0].rule_checks[0] = "invented_check".to_string();
    corruptions.push(("incorrect checks", changed));

    for (label, corrupted) in corruptions {
        assert!(
            verification::verify_moves_output(&yard, &wave, &corrupted).is_err(),
            "verifier accepted corrupt {label}"
        );
    }
}

#[test]
fn reused_output_success_to_review_is_exclusive() {
    let fixture = FileFixture::new("success-review");
    fixture.write_demo_inputs();
    run_plan_files(&fixture.yard, &fixture.pickups, &fixture.output, LIMIT)
        .expect("success generation should publish");
    assert_only_current(&fixture.output, "moves.json");

    fs::write(&fixture.pickups, r#"{"pickups":["MISSING"]}"#).expect("write infeasible wave");
    let (review, _) = run_plan_files(&fixture.yard, &fixture.pickups, &fixture.output, LIMIT)
        .expect("review generation should publish");
    assert!(matches!(review, PlanOutput::Review(_)));
    assert_only_current(&fixture.output, "review.json");
}

#[test]
fn reused_output_review_to_success_is_exclusive() {
    let fixture = FileFixture::new("review-success");
    fixture.write_demo_inputs();
    fs::write(&fixture.pickups, r#"{"pickups":["MISSING"]}"#).expect("write infeasible wave");
    run_plan_files(&fixture.yard, &fixture.pickups, &fixture.output, LIMIT)
        .expect("review generation should publish");
    assert_only_current(&fixture.output, "review.json");

    fixture.write_demo_inputs();
    let (success, _) = run_plan_files(&fixture.yard, &fixture.pickups, &fixture.output, LIMIT)
        .expect("success generation should publish");
    assert!(matches!(success, PlanOutput::Moves(_)));
    assert_only_current(&fixture.output, "moves.json");
}

#[test]
fn reused_output_success_to_malformed_leaves_no_current_artifact() {
    let fixture = FileFixture::new("success-malformed");
    fixture.write_demo_inputs();
    run_plan_files(&fixture.yard, &fixture.pickups, &fixture.output, LIMIT)
        .expect("success generation should publish");
    fs::write(&fixture.pickups, "{").expect("write malformed wave");
    assert!(run_plan_files(&fixture.yard, &fixture.pickups, &fixture.output, LIMIT).is_err());
    assert_no_current(&fixture.output);
}

#[test]
fn reused_output_success_to_timeout_publishes_only_review() {
    let fixture = FileFixture::new("success-timeout");
    fixture.write_demo_inputs();
    run_plan_files(&fixture.yard, &fixture.pickups, &fixture.output, LIMIT)
        .expect("success generation should publish");
    let (timed_out, _) = run_plan_files(
        &fixture.yard,
        &fixture.pickups,
        &fixture.output,
        Duration::ZERO,
    )
    .expect("timeout review should publish");
    assert!(matches!(timed_out, PlanOutput::Review(_)));
    assert_only_current(&fixture.output, "review.json");
}

#[test]
fn reused_output_success_to_replay_rejection_publishes_only_review() {
    let fixture = FileFixture::new("success-replay-rejection");
    fixture.write_demo_inputs();
    run_plan_files(&fixture.yard, &fixture.pickups, &fixture.output, LIMIT)
        .expect("success generation should publish");
    let rejection = PlanOutput::Review(ReviewOutput {
        status: "review_required".to_string(),
        executable: false,
        planner: "bounded-lookahead-v1".to_string(),
        first_blocked_pickup: Some("P1-0".to_string()),
        reason: "separate transition replay rejected the candidate plan".to_string(),
        preventing_conditions: vec!["injected replay rejection".to_string()],
    });
    write_plan(&fixture.output, &rejection).expect("replay rejection review should publish");
    assert_only_current(&fixture.output, "review.json");
}

#[test]
fn review_output_is_byte_deterministic() {
    let (yard, wave, _) = feasible_false_negative_witness();
    let expected = planner::plan(&yard, &wave, LIMIT)
        .canonical_json()
        .expect("review should serialize");
    for _ in 0..5 {
        assert_eq!(
            planner::plan(&yard, &wave, LIMIT)
                .canonical_json()
                .expect("review should serialize"),
            expected
        );
    }
}

fn plain(id: &str, weight_class: u8) -> Container {
    Container {
        id: id.to_string(),
        weight_class,
        reefer: false,
        hazardous_group: None,
        customs_hold: false,
    }
}

fn base_stack(id: &str, x: i32, containers: Vec<Container>) -> Stack {
    Stack {
        id: id.to_string(),
        x,
        y: 0,
        reefer_socket: false,
        frozen: false,
        neighbors: vec![],
        containers,
    }
}

fn held_pickup() -> Yard {
    let mut pickup = plain("P1", 5);
    pickup.customs_hold = true;
    Yard {
        max_height: 5,
        hazardous_exclusions: BTreeMap::new(),
        stacks: vec![base_stack("A", 0, vec![pickup]), base_stack("B", 1, vec![])],
    }
}

fn frozen_pickup() -> Yard {
    let mut source = base_stack("A", 0, vec![plain("P1", 5)]);
    source.frozen = true;
    Yard {
        max_height: 5,
        hazardous_exclusions: BTreeMap::new(),
        stacks: vec![source, base_stack("B", 1, vec![])],
    }
}

fn capacity_dead_end() -> Yard {
    Yard {
        max_height: 2,
        hazardous_exclusions: BTreeMap::new(),
        stacks: vec![
            base_stack("A", 0, vec![plain("P1", 5), plain("X", 4)]),
            base_stack("B", 1, vec![plain("B1", 5), plain("B2", 4)]),
            base_stack("C", 2, vec![plain("C1", 5), plain("C2", 4)]),
        ],
    }
}

fn dense_yard() -> (Yard, PickupWave) {
    let mut stacks = Vec::new();
    let mut pickups = Vec::new();
    for index in 0_usize..288 {
        let height = if index < 128 { 5 } else { 4 };
        let mut containers = Vec::new();
        for level in 0..height {
            let id = if (128..168).contains(&index) && level == 2 {
                let id = format!("PICKUP-{index}");
                pickups.push(id.clone());
                id
            } else {
                format!("C-{index}-{level}")
            };
            containers.push(plain(&id, [5, 4, 3, 2, 1][level]));
        }
        stacks.push(Stack {
            id: format!("S-{index:03}"),
            x: i32::try_from(index / 6).expect("test coordinate fits i32"),
            y: i32::try_from(index % 6).expect("test coordinate fits i32"),
            reefer_socket: index % 3 == 0,
            frozen: false,
            neighbors: vec![],
            containers,
        });
    }
    (
        Yard {
            max_height: 5,
            hazardous_exclusions: BTreeMap::new(),
            stacks,
        },
        PickupWave { pickups },
    )
}

fn constrained_destinations() -> Yard {
    let mut reefer = plain("R", 2);
    reefer.reefer = true;
    reefer.hazardous_group = Some("A".to_string());
    let mut hazard = plain("HZ", 5);
    hazard.hazardous_group = Some("B".to_string());
    let mut frozen = base_stack("FROZEN", 1, vec![]);
    frozen.reefer_socket = true;
    frozen.frozen = true;
    let mut no_socket = base_stack("NO_SOCKET", 2, vec![]);
    no_socket.reefer_socket = false;
    let mut bad_weight = base_stack("BAD_WEIGHT", 3, vec![plain("LIGHT", 1)]);
    bad_weight.reefer_socket = true;
    let mut bad_hazard = base_stack("BAD_HAZARD", 4, vec![]);
    bad_hazard.reefer_socket = true;
    bad_hazard.neighbors = vec!["NEIGHBOR".to_string()];
    let mut neighbor = base_stack("NEIGHBOR", 5, vec![hazard]);
    neighbor.neighbors = vec!["BAD_HAZARD".to_string()];
    let mut legal = base_stack("LEGAL", 6, vec![]);
    legal.reefer_socket = true;
    let mut exclusions = BTreeMap::new();
    exclusions.insert("A".to_string(), vec!["B".to_string()]);
    let mut source = base_stack("SOURCE", 0, vec![plain("P1", 5), reefer]);
    source.reefer_socket = true;
    Yard {
        max_height: 5,
        hazardous_exclusions: exclusions,
        stacks: vec![
            source, frozen, no_socket, bad_weight, bad_hazard, neighbor, legal,
        ],
    }
}

fn yard_key_order_variants() -> [String; 5] {
    let (yard, _) = demo_case(7);
    let normal = serde_json::to_string(&yard).expect("yard should serialize");
    let stacks = serde_json::to_string(&yard.stacks).expect("stacks should serialize");
    let exclusions =
        serde_json::to_string(&yard.hazardous_exclusions).expect("exclusions should serialize");
    [
        normal,
        format!(r#"{{"stacks":{stacks},"max_height":5,"hazardous_exclusions":{exclusions}}}"#),
        format!(r#"{{"hazardous_exclusions":{exclusions},"stacks":{stacks},"max_height":5}}"#),
        format!(r#"{{"max_height":5,"stacks":{stacks},"hazardous_exclusions":{exclusions}}}"#),
        format!(
            r#"{{ "stacks" : {stacks}, "hazardous_exclusions" : {exclusions}, "max_height" : 5 }}"#
        ),
    ]
}

fn feasible_false_negative_witness() -> (Yard, PickupWave, MovesOutput) {
    let yard = Yard {
        max_height: 3,
        hazardous_exclusions: BTreeMap::new(),
        stacks: vec![
            base_stack("A", 0, vec![plain("P", 5), plain("X2", 4), plain("X1", 2)]),
            base_stack("B", 1, vec![plain("B0", 5)]),
            base_stack("C", 2, vec![plain("C0", 3)]),
        ],
    };
    let wave = PickupWave {
        pickups: vec!["P".to_string()],
    };
    let relocation_checks = vec![
        "source_top".to_string(),
        "source_not_frozen".to_string(),
        "customs_hold_clear".to_string(),
        "destination_not_frozen".to_string(),
        "capacity_below_maximum".to_string(),
        "heavy_below_light".to_string(),
        "reefer_socket_not_required".to_string(),
        "hazardous_neighbor_segregation".to_string(),
    ];
    let witness = MovesOutput {
        status: "executable".to_string(),
        executable: true,
        planner: "manual-reviewer-witness".to_string(),
        relocations: 2,
        pickups: 1,
        simulator_verified: true,
        moves: vec![
            PlannedMove {
                step: 1,
                kind: MoveKind::Relocate,
                container_id: "X1".to_string(),
                from: "A".to_string(),
                to: "C".to_string(),
                pickup_rank: 1,
                needed_for: "expose pickup P".to_string(),
                rule_checks: relocation_checks.clone(),
            },
            PlannedMove {
                step: 2,
                kind: MoveKind::Relocate,
                container_id: "X2".to_string(),
                from: "A".to_string(),
                to: "B".to_string(),
                pickup_rank: 1,
                needed_for: "expose pickup P".to_string(),
                rule_checks: relocation_checks,
            },
            PlannedMove {
                step: 3,
                kind: MoveKind::Pickup,
                container_id: "P".to_string(),
                from: "A".to_string(),
                to: "pickup_lane".to_string(),
                pickup_rank: 1,
                needed_for: "fulfill pickup P".to_string(),
                rule_checks: vec![
                    "required_pickup_order".to_string(),
                    "source_top".to_string(),
                    "source_not_frozen".to_string(),
                    "customs_hold_clear".to_string(),
                ],
            },
        ],
    };
    (yard, wave, witness)
}

struct FileFixture {
    root: PathBuf,
    yard: PathBuf,
    pickups: PathBuf,
    output: PathBuf,
}

impl FileFixture {
    fn new(label: &str) -> Self {
        let serial = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "container-yard-planner-{label}-{}-{serial}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create isolated fixture root");
        Self {
            yard: root.join("yard.json"),
            pickups: root.join("pickups.json"),
            output: root.join("output"),
            root,
        }
    }

    fn write_demo_inputs(&self) {
        let (yard, wave) = demo_case(0);
        fs::write(
            &self.yard,
            serde_json::to_vec(&yard).expect("serialize yard"),
        )
        .expect("write yard");
        fs::write(
            &self.pickups,
            serde_json::to_vec(&wave).expect("serialize wave"),
        )
        .expect("write wave");
    }
}

impl Drop for FileFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn assert_only_current(output: &Path, expected: &str) {
    assert!(output.join(expected).is_file());
    for other in ["moves.json", "review.json", ".yard-plan.tmp"] {
        if other != expected {
            assert!(!output.join(other).exists(), "stale artifact: {other}");
        }
    }
}

fn assert_no_current(output: &Path) {
    for artifact in ["moves.json", "review.json", ".yard-plan.tmp"] {
        assert!(
            !output.join(artifact).exists(),
            "stale artifact: {artifact}"
        );
    }
}
