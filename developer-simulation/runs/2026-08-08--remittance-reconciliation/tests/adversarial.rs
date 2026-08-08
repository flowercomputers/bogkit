use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use remittance_reconciliation::engine::{greedy_baseline, reconcile};
use remittance_reconciliation::model::{
    AcceptedLink, ClaimLine, ExpectedLink, GroundTruthRecord, RemittanceLine, ReviewRecord,
    Summary, TransactionKind,
};
use remittance_reconciliation::verify::{VerificationReport, verify_results};
use serde::Serialize;

static CASE_NUMBER: AtomicUsize = AtomicUsize::new(1);

struct Outcome {
    accepted: Vec<AcceptedLink>,
    reviews: Vec<ReviewRecord>,
    summary: Summary,
    accepted_bytes: Vec<u8>,
    review_bytes: Vec<u8>,
    summary_bytes: Vec<u8>,
    verification: VerificationReport,
}

fn claim(id: &str, claim_id: &str, reference: &str, patient: &str) -> ClaimLine {
    ClaimLine {
        claim_line_id: id.to_string(),
        claim_id: claim_id.to_string(),
        revision: 1,
        payer: "payer".to_string(),
        provider: "provider".to_string(),
        patient_key: patient.to_string(),
        service_date: 20_000,
        procedure_code: "PROC".to_string(),
        billed_cents: 100,
        open_balance_cents: 100,
        insurer_references: vec![reference.to_string()],
    }
}

fn payment(id: &str, reference: Option<&str>, patient: &str, amount: i64) -> RemittanceLine {
    RemittanceLine {
        remittance_line_id: id.to_string(),
        payer: "payer".to_string(),
        provider: "provider".to_string(),
        insurer_reference: reference.map(str::to_string),
        patient_key: Some(patient.to_string()),
        service_date_start: Some(20_000),
        service_date_end: Some(20_000),
        procedure_code: Some("PROC".to_string()),
        paid_cents: amount,
        adjustment_cents: 0,
        adjustment_codes: Vec::new(),
        transaction_kind: TransactionKind::Payment,
    }
}

fn expected(remit_id: &str, claim_id: &str, amount: i64) -> GroundTruthRecord {
    GroundTruthRecord {
        remittance_line_id: remit_id.to_string(),
        unambiguous: true,
        links: vec![ExpectedLink {
            remittance_line_id: remit_id.to_string(),
            claim_line_id: claim_id.to_string(),
            claim_revision: 1,
            applied_cents: amount,
        }],
    }
}

fn reviewed_truth(remit_id: &str) -> GroundTruthRecord {
    GroundTruthRecord {
        remittance_line_id: remit_id.to_string(),
        unambiguous: false,
        links: Vec::new(),
    }
}

fn temp_case(label: &str) -> PathBuf {
    let number = CASE_NUMBER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "remittance-adversarial-{label}-{}-{number}",
        std::process::id()
    ));
    fs::create_dir_all(&path).expect("create test directory");
    path
}

fn write_jsonl<T: Serialize>(path: &Path, records: &[T]) {
    let mut writer = BufWriter::new(File::create(path).expect("create JSONL"));
    for record in records {
        serde_json::to_writer(&mut writer, record).expect("serialize JSONL record");
        writer.write_all(b"\n").expect("write newline");
    }
    writer.flush().expect("flush JSONL");
}

fn read_jsonl<T: for<'de> serde::Deserialize<'de>>(path: &Path) -> Vec<T> {
    fs::read_to_string(path)
        .expect("read JSONL")
        .lines()
        .map(|line| serde_json::from_str(line).expect("parse JSONL"))
        .collect()
}

fn run_solver(
    label: &str,
    claims: &[ClaimLine],
    remittances: &[RemittanceLine],
    truth: &[GroundTruthRecord],
) -> Outcome {
    let directory = temp_case(label);
    let claims_path = directory.join("claims.jsonl");
    let remittances_path = directory.join("remittances.jsonl");
    let truth_path = directory.join("ground-truth.jsonl");
    let results = directory.join("results");
    write_jsonl(&claims_path, claims);
    write_jsonl(&remittances_path, remittances);
    write_jsonl(&truth_path, truth);
    reconcile(&claims_path, &remittances_path, &results).expect("reconcile case");
    let verification = verify_results(&claims_path, &remittances_path, &truth_path, &results)
        .expect("verify case");
    let outcome = Outcome {
        accepted: read_jsonl(&results.join("accepted.jsonl")),
        reviews: read_jsonl(&results.join("review.jsonl")),
        summary: serde_json::from_reader(
            File::open(results.join("summary.json")).expect("open summary"),
        )
        .expect("parse summary"),
        accepted_bytes: fs::read(results.join("accepted.jsonl")).expect("read accepted bytes"),
        review_bytes: fs::read(results.join("review.jsonl")).expect("read review bytes"),
        summary_bytes: fs::read(results.join("summary.json")).expect("read summary bytes"),
        verification,
    };
    fs::remove_dir_all(directory).expect("clean test directory");
    outcome
}

fn baseline_target(claims: &[ClaimLine], remittance: &RemittanceLine) -> Option<String> {
    let directory = temp_case("baseline");
    let claims_path = directory.join("claims.jsonl");
    let remittances_path = directory.join("remittances.jsonl");
    let results = directory.join("results");
    write_jsonl(&claims_path, claims);
    write_jsonl(&remittances_path, std::slice::from_ref(remittance));
    greedy_baseline(&claims_path, &remittances_path, &results).expect("run baseline");
    let accepted = read_jsonl::<AcceptedLink>(&results.join("accepted.jsonl"));
    fs::remove_dir_all(directory).expect("clean test directory");
    accepted.first().map(|link| link.claim_line_id.clone())
}

fn run_manual_verifier(
    label: &str,
    claims: &[ClaimLine],
    remittances: &[RemittanceLine],
    truth: &[GroundTruthRecord],
    accepted: &[AcceptedLink],
    reviews: &[ReviewRecord],
    summary: &Summary,
) -> VerificationReport {
    let directory = temp_case(label);
    let claims_path = directory.join("claims.jsonl");
    let remittances_path = directory.join("remittances.jsonl");
    let truth_path = directory.join("ground-truth.jsonl");
    let results = directory.join("results");
    fs::create_dir_all(&results).expect("create results directory");
    write_jsonl(&claims_path, claims);
    write_jsonl(&remittances_path, remittances);
    write_jsonl(&truth_path, truth);
    write_jsonl(&results.join("accepted.jsonl"), accepted);
    write_jsonl(&results.join("review.jsonl"), reviews);
    let mut summary_writer =
        BufWriter::new(File::create(results.join("summary.json")).expect("create summary"));
    serde_json::to_writer_pretty(&mut summary_writer, summary).expect("serialize summary");
    summary_writer
        .write_all(b"\n")
        .expect("write summary newline");
    summary_writer.flush().expect("flush summary");
    let report = verify_results(&claims_path, &remittances_path, &truth_path, &results)
        .expect("run manual verifier");
    fs::remove_dir_all(directory).expect("clean test directory");
    report
}

fn accepted_link(remit_id: &str, claim_id: &str) -> AcceptedLink {
    AcceptedLink {
        remittance_line_id: remit_id.to_string(),
        claim_line_id: claim_id.to_string(),
        claim_revision: 1,
        applied_cents: 100,
        score: 100,
        facts: vec!["insurer_reference".to_string()],
        rejected_competitors: Vec::new(),
    }
}

fn assert_same_outputs(left: &Outcome, right: &Outcome) {
    assert_eq!(left.accepted_bytes, right.accepted_bytes);
    assert_eq!(left.review_bytes, right.review_bytes);
    assert_eq!(left.summary_bytes, right.summary_bytes);
}

#[test]
fn conflicting_identity_sources_review_in_all_orders_while_agreement_accepts() {
    let reference_claim = claim("C-REF", "CLAIM-REF", "REF-CONFLICT", "patient-ref");
    let identity_claim = claim(
        "C-IDENTITY",
        "CLAIM-IDENTITY",
        "REF-OTHER",
        "patient-identity",
    );
    let agreement_claim = claim("C-AGREE", "CLAIM-AGREE", "REF-AGREE", "patient-agree");
    let conflict = payment("R-CONFLICT", Some("REF-CONFLICT"), "patient-identity", 100);
    let agreement = payment("R-AGREE", Some("REF-AGREE"), "patient-agree", 100);
    let truth = vec![
        reviewed_truth("R-CONFLICT"),
        expected("R-AGREE", "C-AGREE", 100),
    ];

    let ordered = run_solver(
        "conflict-ordered",
        &[
            reference_claim.clone(),
            identity_claim.clone(),
            agreement_claim.clone(),
        ],
        &[conflict.clone(), agreement.clone()],
        &truth,
    );
    let shuffled = run_solver(
        "conflict-shuffled",
        &[
            agreement_claim,
            identity_claim.clone(),
            reference_claim.clone(),
        ],
        &[agreement, conflict],
        &truth,
    );
    let reshuffled = run_solver(
        "conflict-reshuffled",
        &[
            identity_claim,
            reference_claim,
            claim("C-AGREE", "CLAIM-AGREE", "REF-AGREE", "patient-agree"),
        ],
        &[
            payment("R-CONFLICT", Some("REF-CONFLICT"), "patient-identity", 100),
            payment("R-AGREE", Some("REF-AGREE"), "patient-agree", 100),
        ],
        &truth,
    );

    assert_same_outputs(&ordered, &shuffled);
    assert_same_outputs(&ordered, &reshuffled);
    assert!(
        ordered.verification.passed,
        "{:?}",
        ordered.verification.failures
    );
    assert_eq!(ordered.accepted.len(), 1);
    assert_eq!(ordered.accepted[0].remittance_line_id, "R-AGREE");
    assert_eq!(ordered.reviews.len(), 1);
    assert_eq!(
        ordered.reviews[0].reason_codes,
        ["conflicting_identity_sources"]
    );
}

#[test]
fn overlapping_identity_sources_require_a_feasible_shared_candidate_in_all_orders() {
    let mut reference_only = claim("C-A", "CLAIM-A", "REF-X", "patient-a");
    reference_only.service_date = 19_900;
    reference_only.procedure_code = "PROC-A".to_string();
    let mut common_underfunded = claim("C-B", "CLAIM-B", "REF-X", "patient-shared");
    common_underfunded.billed_cents = 50;
    common_underfunded.open_balance_cents = 50;
    let fallback_only = claim("C-C", "CLAIM-C", "REF-Y", "patient-shared");
    let unrelated_claim = claim("C-OK", "CLAIM-OK", "REF-OK", "patient-ok");
    let overlap = payment("R-OVERLAP", Some("REF-X"), "patient-shared", 100);
    let unrelated = payment("R-OK", Some("REF-OK"), "patient-ok", 100);
    let truth = vec![reviewed_truth("R-OVERLAP"), expected("R-OK", "C-OK", 100)];

    let ordered = run_solver(
        "overlap-ordered",
        &[
            reference_only.clone(),
            common_underfunded.clone(),
            fallback_only.clone(),
            unrelated_claim.clone(),
        ],
        &[overlap.clone(), unrelated.clone()],
        &truth,
    );
    let reversed = run_solver(
        "overlap-reversed",
        &[
            unrelated_claim.clone(),
            fallback_only.clone(),
            common_underfunded.clone(),
            reference_only.clone(),
        ],
        &[unrelated.clone(), overlap.clone()],
        &truth,
    );
    let shuffled = run_solver(
        "overlap-shuffled",
        &[
            fallback_only,
            reference_only,
            unrelated_claim,
            common_underfunded,
        ],
        &[overlap, unrelated],
        &truth,
    );

    assert_same_outputs(&ordered, &reversed);
    assert_same_outputs(&ordered, &shuffled);
    assert!(
        ordered.verification.passed,
        "{:?}",
        ordered.verification.failures
    );
    assert_eq!(ordered.accepted.len(), 1);
    assert_eq!(ordered.accepted[0].remittance_line_id, "R-OK");
    assert_eq!(ordered.reviews.len(), 1);
    assert_eq!(ordered.reviews[0].remittance_line_id, "R-OVERLAP");
    assert_eq!(
        ordered.reviews[0].reason_codes,
        ["conflicting_identity_sources"]
    );
    assert_eq!(ordered.reviews[0].candidate_count, 1);
}

#[test]
fn overlapping_identity_sources_accept_only_the_feasible_shared_candidate() {
    let mut reference_only = claim("C-A", "CLAIM-A", "REF-X", "patient-a");
    reference_only.service_date = 19_900;
    reference_only.procedure_code = "PROC-A".to_string();
    let common = claim("C-B", "CLAIM-B", "REF-X", "patient-shared");
    let fallback_only = claim("C-C", "CLAIM-C", "REF-Y", "patient-shared");
    let outcome = run_solver(
        "overlap-common-feasible",
        &[reference_only, common, fallback_only],
        &[payment("R-OVERLAP", Some("REF-X"), "patient-shared", 100)],
        &[expected("R-OVERLAP", "C-B", 100)],
    );

    assert!(
        outcome.verification.passed,
        "{:?}",
        outcome.verification.failures
    );
    assert!(outcome.reviews.is_empty());
    assert_eq!(outcome.accepted.len(), 1);
    assert_eq!(outcome.accepted[0].claim_line_id, "C-B");
}

#[test]
fn a_single_nonempty_identity_source_remains_eligible() {
    let reference_only = claim("C-REF", "CLAIM-REF", "REF-X", "patient-ref");
    let fallback_only = claim(
        "C-FALLBACK",
        "CLAIM-FALLBACK",
        "REF-FALLBACK",
        "patient-fallback",
    );
    let exact_remit = payment("R-REF", Some("REF-X"), "no-fallback-match", 100);
    let fallback_remit = payment("R-FALLBACK", Some("REF-NOT-FOUND"), "patient-fallback", 100);
    let outcome = run_solver(
        "single-source-controls",
        &[reference_only, fallback_only],
        &[fallback_remit, exact_remit],
        &[
            expected("R-REF", "C-REF", 100),
            expected("R-FALLBACK", "C-FALLBACK", 100),
        ],
    );

    assert!(
        outcome.verification.passed,
        "{:?}",
        outcome.verification.failures
    );
    assert!(outcome.reviews.is_empty());
    assert_eq!(outcome.accepted.len(), 2);
}

#[test]
fn overlapping_sources_cannot_reintroduce_reference_only_split_candidates() {
    let mut reference_only_a = claim("C-A1", "CLAIM-A", "REF-X", "patient-a1");
    reference_only_a.service_date = 19_900;
    reference_only_a.procedure_code = "PROC-A".to_string();
    reference_only_a.billed_cents = 50;
    reference_only_a.open_balance_cents = 50;
    let mut reference_only_b = claim("C-A2", "CLAIM-A", "REF-X", "patient-a2");
    reference_only_b.service_date = 19_901;
    reference_only_b.procedure_code = "PROC-A".to_string();
    reference_only_b.billed_cents = 50;
    reference_only_b.open_balance_cents = 50;
    let mut common_underfunded = claim("C-B", "CLAIM-B", "REF-X", "patient-shared");
    common_underfunded.billed_cents = 50;
    common_underfunded.open_balance_cents = 50;
    let fallback_only = claim("C-C", "CLAIM-C", "REF-Y", "patient-shared");
    let outcome = run_solver(
        "overlap-split-safeguard",
        &[
            reference_only_a,
            reference_only_b,
            common_underfunded,
            fallback_only,
        ],
        &[payment(
            "R-OVERLAP-SPLIT",
            Some("REF-X"),
            "patient-shared",
            100,
        )],
        &[reviewed_truth("R-OVERLAP-SPLIT")],
    );

    assert!(
        outcome.verification.passed,
        "{:?}",
        outcome.verification.failures
    );
    assert!(outcome.accepted.is_empty());
    assert_eq!(outcome.reviews.len(), 1);
    assert_eq!(
        outcome.reviews[0].reason_codes,
        ["conflicting_identity_sources"]
    );
    assert_eq!(outcome.reviews[0].candidate_count, 1);
}

#[test]
fn duplicate_remittance_rows_are_quarantined_in_all_orders() {
    let first_claim = claim("C-DUP-A", "CLAIM-A", "REF-A", "patient-a");
    let second_claim = claim("C-DUP-B", "CLAIM-B", "REF-B", "patient-b");
    let okay_claim = claim("C-OK", "CLAIM-OK", "REF-OK", "patient-ok");
    let duplicate_a = payment("R-DUP", Some("REF-A"), "patient-a", 100);
    let duplicate_b = payment("R-DUP", Some("REF-B"), "patient-b", 100);
    let okay = payment("R-OK", Some("REF-OK"), "patient-ok", 100);
    let truth = vec![reviewed_truth("R-DUP"), expected("R-OK", "C-OK", 100)];

    let adjacent = run_solver(
        "remit-duplicate-adjacent",
        &[
            first_claim.clone(),
            second_claim.clone(),
            okay_claim.clone(),
        ],
        &[duplicate_a.clone(), duplicate_b.clone(), okay.clone()],
        &truth,
    );
    let separated = run_solver(
        "remit-duplicate-separated",
        &[okay_claim, second_claim, first_claim],
        &[duplicate_b, okay, duplicate_a],
        &truth,
    );

    assert_same_outputs(&adjacent, &separated);
    assert!(
        adjacent.verification.passed,
        "{:?}",
        adjacent.verification.failures
    );
    assert_eq!(adjacent.accepted.len(), 1);
    assert_eq!(adjacent.reviews.len(), 2);
    assert_eq!(adjacent.summary.invalid_remittance_records, 2);
    assert_eq!(
        adjacent
            .reviews
            .iter()
            .map(|review| review.physical_record_ordinal)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    assert!(
        adjacent
            .reviews
            .iter()
            .all(|review| { review.reason_codes == ["duplicate_remittance_id"] })
    );
}

#[test]
fn verifier_rejects_an_accepted_duplicate_remittance_identity() {
    let first_claim = claim("C-A", "CLAIM-A", "REF-A", "patient-a");
    let second_claim = claim("C-B", "CLAIM-B", "REF-B", "patient-b");
    let first = payment("R-DUP", Some("REF-A"), "patient-a", 100);
    let second = payment("R-DUP", Some("REF-B"), "patient-b", 100);
    let report = run_manual_verifier(
        "verify-duplicate-remittance",
        &[first_claim, second_claim],
        &[first, second],
        &[reviewed_truth("R-DUP")],
        &[accepted_link("R-DUP", "C-A")],
        &[],
        &Summary {
            input_claim_records: 2,
            valid_current_claim_lines: 2,
            invalid_claim_records: 0,
            input_remittance_records: 2,
            accepted_remittance_lines: 1,
            accepted_links: 1,
            review_remittance_lines: 0,
            invalid_remittance_records: 2,
        },
    );
    assert!(!report.passed);
    assert!(
        report
            .failures
            .iter()
            .any(|failure| { failure.contains("duplicate remittance R-DUP was not quarantined") })
    );
}

#[test]
fn byte_identical_duplicate_remittances_are_not_idempotently_accepted() {
    let duplicate_claim = claim("C-DUP", "CLAIM-DUP", "REF-DUP", "patient-dup");
    let okay_claim = claim("C-OK", "CLAIM-OK", "REF-OK", "patient-ok");
    let duplicate = payment("R-DUP", Some("REF-DUP"), "patient-dup", 100);
    let okay = payment("R-OK", Some("REF-OK"), "patient-ok", 100);
    let truth = vec![reviewed_truth("R-DUP"), expected("R-OK", "C-OK", 100)];
    let adjacent = run_solver(
        "identical-remittance-duplicate-adjacent",
        &[duplicate_claim.clone(), okay_claim.clone()],
        &[duplicate.clone(), duplicate.clone(), okay.clone()],
        &truth,
    );
    let separated = run_solver(
        "identical-remittance-duplicate-separated",
        &[okay_claim, duplicate_claim],
        &[duplicate.clone(), okay, duplicate],
        &truth,
    );

    assert_same_outputs(&adjacent, &separated);
    assert!(
        adjacent.verification.passed,
        "{:?}",
        adjacent.verification.failures
    );
    assert_eq!(adjacent.reviews.len(), 2);
    assert_eq!(adjacent.summary.invalid_remittance_records, 2);
}

#[test]
fn duplicate_claim_keys_are_quarantined_in_all_orders() {
    let duplicate_a = claim("C-DUP", "CLAIM-DUP", "REF-A", "patient-a");
    let mut duplicate_b = claim("C-DUP", "CLAIM-DUP", "REF-B", "patient-b");
    duplicate_b.procedure_code = "PROC-B".to_string();
    let okay_claim = claim("C-OK", "CLAIM-OK", "REF-OK", "patient-ok");
    let related_a = payment("R-DUP-A", Some("REF-A"), "patient-a", 100);
    let mut related_b = payment("R-DUP-B", Some("REF-B"), "patient-b", 100);
    related_b.procedure_code = Some("PROC-B".to_string());
    let okay = payment("R-OK", Some("REF-OK"), "patient-ok", 100);
    let truth = vec![
        reviewed_truth("R-DUP-A"),
        reviewed_truth("R-DUP-B"),
        expected("R-OK", "C-OK", 100),
    ];

    let adjacent = run_solver(
        "claim-duplicate-adjacent",
        &[duplicate_a.clone(), duplicate_b.clone(), okay_claim.clone()],
        &[related_a.clone(), related_b.clone(), okay.clone()],
        &truth,
    );
    let separated = run_solver(
        "claim-duplicate-separated",
        &[duplicate_b, okay_claim, duplicate_a],
        &[okay, related_b, related_a],
        &truth,
    );

    assert_same_outputs(&adjacent, &separated);
    assert!(
        adjacent.verification.passed,
        "{:?}",
        adjacent.verification.failures
    );
    assert_eq!(adjacent.accepted.len(), 1);
    assert_eq!(adjacent.reviews.len(), 2);
    assert_eq!(adjacent.summary.invalid_claim_records, 2);
    assert!(
        adjacent
            .reviews
            .iter()
            .all(|review| { review.reason_codes == ["duplicate_claim_key"] })
    );
}

#[test]
fn verifier_rejects_capacity_from_a_duplicate_claim_key() {
    let first = claim("C-DUP", "CLAIM-DUP", "REF-A", "patient-a");
    let second = claim("C-DUP", "CLAIM-DUP", "REF-B", "patient-b");
    let remit = payment("R-DUP", Some("REF-A"), "patient-a", 100);
    let report = run_manual_verifier(
        "verify-duplicate-claim",
        &[first, second],
        std::slice::from_ref(&remit),
        &[reviewed_truth("R-DUP")],
        &[accepted_link("R-DUP", "C-DUP")],
        &[],
        &Summary {
            input_claim_records: 2,
            valid_current_claim_lines: 0,
            invalid_claim_records: 2,
            input_remittance_records: 1,
            accepted_remittance_lines: 1,
            accepted_links: 1,
            review_remittance_lines: 0,
            invalid_remittance_records: 0,
        },
    );
    assert!(!report.passed);
    assert!(
        report
            .failures
            .iter()
            .any(|failure| { failure.contains("reuses a duplicated logical claim key") })
    );
}

#[test]
fn byte_identical_duplicate_claim_keys_do_not_create_capacity() {
    let duplicate = claim("C-DUP", "CLAIM-DUP", "REF-DUP", "patient-dup");
    let okay_claim = claim("C-OK", "CLAIM-OK", "REF-OK", "patient-ok");
    let related = payment("R-DUP", Some("REF-DUP"), "patient-dup", 100);
    let okay = payment("R-OK", Some("REF-OK"), "patient-ok", 100);
    let truth = vec![reviewed_truth("R-DUP"), expected("R-OK", "C-OK", 100)];
    let adjacent = run_solver(
        "identical-claim-duplicate-adjacent",
        &[duplicate.clone(), duplicate.clone(), okay_claim.clone()],
        &[related.clone(), okay.clone()],
        &truth,
    );
    let separated = run_solver(
        "identical-claim-duplicate-separated",
        &[duplicate.clone(), okay_claim, duplicate],
        &[okay, related],
        &truth,
    );

    assert_same_outputs(&adjacent, &separated);
    assert!(
        adjacent.verification.passed,
        "{:?}",
        adjacent.verification.failures
    );
    assert_eq!(adjacent.accepted.len(), 1);
    assert_eq!(adjacent.summary.invalid_claim_records, 2);
}

#[test]
fn comparator_runs_exact_reference_before_fallback() {
    let reference_claim = claim("C-REF", "CLAIM-REF", "REF-X", "patient-ref");
    let fallback_claim = claim("C-FALLBACK", "CLAIM-FALLBACK", "REF-Y", "patient-fallback");
    let remit = payment("R-X", Some("REF-X"), "patient-fallback", 100);
    assert_eq!(
        baseline_target(&[fallback_claim.clone(), reference_claim.clone()], &remit),
        Some("C-REF".to_string())
    );
    assert_eq!(
        baseline_target(&[reference_claim, fallback_claim], &remit),
        Some("C-REF".to_string())
    );
}

#[test]
fn comparator_is_input_order_greedy_inside_duplicate_reference_stage() {
    let first = claim("C-A", "CLAIM-A", "REF-X", "patient-a");
    let second = claim("C-B", "CLAIM-B", "REF-X", "patient-b");
    let remit = payment("R-X", Some("REF-X"), "patient-b", 100);
    assert_eq!(
        baseline_target(&[first.clone(), second.clone()], &remit),
        Some("C-A".to_string())
    );
    assert_eq!(
        baseline_target(&[second, first], &remit),
        Some("C-B".to_string())
    );
}

#[test]
fn comparator_uses_fallback_when_reference_is_missing() {
    let first = claim("C-A", "CLAIM-A", "REF-A", "patient-shared");
    let second = claim("C-B", "CLAIM-B", "REF-B", "patient-shared");
    let remit = payment("R-X", None, "patient-shared", 100);
    assert_eq!(
        baseline_target(&[first.clone(), second.clone()], &remit),
        Some("C-A".to_string())
    );
    assert_eq!(
        baseline_target(&[second, first], &remit),
        Some("C-B".to_string())
    );
}

#[test]
fn comparator_uses_fallback_only_when_reference_stage_has_no_acceptable_capacity() {
    let mut reference_claim = claim("C-REF", "CLAIM-REF", "REF-X", "patient-ref");
    reference_claim.billed_cents = 50;
    reference_claim.open_balance_cents = 50;
    let fallback_claim = claim("C-FALLBACK", "CLAIM-FALLBACK", "REF-Y", "patient-fallback");
    let remit = payment("R-X", Some("REF-X"), "patient-fallback", 100);
    assert_eq!(
        baseline_target(&[reference_claim, fallback_claim], &remit),
        Some("C-FALLBACK".to_string())
    );
}

#[test]
fn unsupported_partial_split_and_standalone_reversal_have_accurate_reasons() {
    let first = claim("C-SPLIT-A", "CLAIM-SPLIT", "REF-SPLIT", "patient-split");
    let second = claim("C-SPLIT-B", "CLAIM-SPLIT", "REF-SPLIT", "patient-split");
    let split = payment("R-SPLIT", Some("REF-SPLIT"), "patient-split", 150);
    let mut reversal_claim = claim(
        "C-REVERSAL",
        "CLAIM-REVERSAL",
        "REF-REVERSAL",
        "patient-reversal",
    );
    reversal_claim.billed_cents = 500;
    reversal_claim.open_balance_cents = 400;
    let mut reversal = payment("R-REVERSAL", Some("REF-REVERSAL"), "patient-reversal", -100);
    reversal.transaction_kind = TransactionKind::Reversal;
    let truth = vec![reviewed_truth("R-SPLIT"), reviewed_truth("R-REVERSAL")];
    let outcome = run_solver(
        "narrow-settlement-scope",
        &[first, second, reversal_claim],
        &[split, reversal],
        &truth,
    );

    assert!(
        outcome.verification.passed,
        "{:?}",
        outcome.verification.failures
    );
    assert_eq!(outcome.accepted.len(), 0);
    assert!(outcome.reviews.iter().any(|review| {
        review.remittance_line_id == "R-SPLIT"
            && review.reason_codes == ["unsupported_partial_split"]
    }));
    assert!(outcome.reviews.iter().any(|review| {
        review.remittance_line_id == "R-REVERSAL"
            && review.reason_codes == ["unsupported_standalone_reversal"]
    }));
}

#[test]
fn dense_end_to_end_cluster_exhausts_to_review() {
    let claims = (0..12)
        .map(|index| {
            claim(
                &format!("C-{index:02}"),
                &format!("CLAIM-{index:02}"),
                "REF-DENSE",
                "patient-dense",
            )
        })
        .collect::<Vec<_>>();
    let remittances = (0..12)
        .map(|index| {
            payment(
                &format!("R-{index:02}"),
                Some("REF-DENSE"),
                "patient-dense",
                100,
            )
        })
        .collect::<Vec<_>>();
    let truth = remittances
        .iter()
        .map(|remit| reviewed_truth(&remit.remittance_line_id))
        .collect::<Vec<_>>();
    let outcome = run_solver("dense-budget", &claims, &remittances, &truth);

    assert!(
        outcome.verification.passed,
        "{:?}",
        outcome.verification.failures
    );
    assert!(outcome.accepted.is_empty());
    assert_eq!(outcome.reviews.len(), 12);
    assert!(
        outcome
            .reviews
            .iter()
            .all(|review| { review.reason_codes == ["search_budget_exhausted"] })
    );
}
