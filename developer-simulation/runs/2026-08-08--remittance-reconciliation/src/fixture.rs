use std::error::Error;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;

use serde::Serialize;

use crate::model::{ClaimLine, ExpectedLink, GroundTruthRecord, RemittanceLine, TransactionKind};

type AnyError = Box<dyn Error + Send + Sync>;

const EDGE_GROUPS: usize = 12;
const UNSUPPORTED_GROUPS: usize = 4;

pub fn generate_fixture(
    output_dir: &Path,
    claim_count: usize,
    remittance_count: usize,
) -> Result<(), AnyError> {
    let edge_remittance_count = EDGE_GROUPS * 12 + UNSUPPORTED_GROUPS + 1;
    let edge_claim_count = EDGE_GROUPS * 14 + UNSUPPORTED_GROUPS * 2;
    let minimum_claim_count = remittance_count - edge_remittance_count + edge_claim_count + 1;
    if remittance_count <= edge_remittance_count || claim_count < minimum_claim_count {
        return Err("fixture sizes are too small for the representative edge cases".into());
    }

    let bulk_count = remittance_count - edge_remittance_count;
    let mut claim_lines = Vec::with_capacity(claim_count);
    let mut remittance_lines = Vec::with_capacity(remittance_count);
    let mut truth = Vec::with_capacity(remittance_count);

    for index in 0..bulk_count {
        let case = format!("BULK-{index:06}");
        let amount = 1_000 + i64::try_from(index % 1_000)?;
        let claim = synthetic_claim(&case, &case, 1, amount, &format!("REF-{case}"), index);
        let remit = synthetic_remit(
            &case,
            amount,
            TransactionKind::Payment,
            Some(format!("REF-{case}")),
            Some(claim.patient_key.clone()),
            Some(claim.service_date),
            Some(claim.procedure_code.clone()),
            index,
        );
        push_serialized(&mut claim_lines, &claim)?;
        push_truth_for_single(&mut truth, &remit, &claim, amount);
        push_serialized(&mut remittance_lines, &remit)?;
    }

    for group in 0..EDGE_GROUPS {
        add_global_greedy_trap(group, &mut claim_lines, &mut remittance_lines, &mut truth)?;
        add_same_claim_split(group, &mut claim_lines, &mut remittance_lines, &mut truth)?;
        add_multiple_payments(group, &mut claim_lines, &mut remittance_lines, &mut truth)?;
        add_revision_case(group, &mut claim_lines, &mut remittance_lines, &mut truth)?;
        add_duplicate_reference_case(group, &mut claim_lines, &mut remittance_lines, &mut truth)?;
        add_payment_reversal(group, &mut claim_lines, &mut remittance_lines, &mut truth)?;
        add_denial(group, &mut claim_lines, &mut remittance_lines, &mut truth)?;
        add_missing_optional_fields(group, &mut claim_lines, &mut remittance_lines, &mut truth)?;
        add_ambiguous_case(group, &mut claim_lines, &mut remittance_lines, &mut truth)?;
    }
    for group in 0..UNSUPPORTED_GROUPS {
        add_unsupported_bundle(group, &mut claim_lines, &mut remittance_lines, &mut truth)?;
    }

    let invalid_id = "R-MALFORMED";
    remittance_lines.push(format!(
        "{{\"remittance_line_id\":\"{invalid_id}\",\"payer\":\"payer-malformed\",\"patient_key\":\"remittance-only-secret-000\"}}"
    ));
    truth.push(GroundTruthRecord {
        remittance_line_id: invalid_id.to_string(),
        unambiguous: false,
        links: Vec::new(),
    });

    while claim_lines.len() + 1 < claim_count {
        let index = claim_lines.len();
        let case = format!("UNMATCHED-{index:06}");
        let claim = synthetic_claim(&case, &case, 1, 777, &format!("REF-{case}"), index);
        push_serialized(&mut claim_lines, &claim)?;
    }
    claim_lines.push("{\"claim_line_id\":\"C-MALFORMED\",\"revision\":1}".to_string());

    if claim_lines.len() != claim_count || remittance_lines.len() != remittance_count {
        return Err(format!(
            "fixture construction count mismatch: claims={} remittances={}",
            claim_lines.len(),
            remittance_lines.len()
        )
        .into());
    }

    fs::create_dir_all(output_dir)?;
    write_lines(&output_dir.join("claims.jsonl"), &claim_lines)?;
    write_lines(&output_dir.join("remittances.jsonl"), &remittance_lines)?;
    truth.sort_by(|left, right| left.remittance_line_id.cmp(&right.remittance_line_id));
    write_jsonl(&output_dir.join("ground-truth.jsonl"), &truth)?;
    Ok(())
}

pub fn shuffle_inputs(
    claims_path: &Path,
    remittances_path: &Path,
    output_dir: &Path,
    seed: u64,
) -> Result<(), AnyError> {
    fs::create_dir_all(output_dir)?;
    let mut claims = read_lines(claims_path)?;
    let mut remittances = read_lines(remittances_path)?;
    let mut rng = XorShift64::new(seed);
    shuffle(&mut claims, &mut rng);
    shuffle(&mut remittances, &mut rng);
    write_lines(&output_dir.join("claims.jsonl"), &claims)?;
    write_lines(&output_dir.join("remittances.jsonl"), &remittances)?;
    Ok(())
}

fn add_global_greedy_trap(
    group: usize,
    claims: &mut Vec<String>,
    remits: &mut Vec<String>,
    truth: &mut Vec<GroundTruthRecord>,
) -> Result<(), AnyError> {
    let base = format!("TRAP-{group:03}");
    let mut first = synthetic_claim(
        &format!("{base}-A"),
        &format!("{base}-CLAIM-A"),
        1,
        100,
        &format!("REF-{base}-STRONG"),
        group,
    );
    let mut second = synthetic_claim(
        &format!("{base}-B"),
        &format!("{base}-CLAIM-B"),
        1,
        100,
        &format!("REF-{base}-OTHER"),
        group,
    );
    second.patient_key.clone_from(&first.patient_key);
    second.service_date = first.service_date;
    second.procedure_code.clone_from(&first.procedure_code);
    first.insurer_references = vec![format!("REF-{base}-STRONG")];
    let weak = synthetic_remit(
        &format!("{base}-WEAK"),
        100,
        TransactionKind::Payment,
        None,
        Some(first.patient_key.clone()),
        Some(first.service_date),
        Some(first.procedure_code.clone()),
        group,
    );
    let strong = synthetic_remit(
        &format!("{base}-STRONG"),
        100,
        TransactionKind::Payment,
        Some(format!("REF-{base}-STRONG")),
        None,
        None,
        None,
        group,
    );
    push_serialized(claims, &first)?;
    push_serialized(claims, &second)?;
    push_truth_for_single(truth, &weak, &second, 100);
    push_truth_for_single(truth, &strong, &first, 100);
    push_serialized(remits, &weak)?;
    push_serialized(remits, &strong)?;
    Ok(())
}

fn add_same_claim_split(
    group: usize,
    claims: &mut Vec<String>,
    remits: &mut Vec<String>,
    truth: &mut Vec<GroundTruthRecord>,
) -> Result<(), AnyError> {
    let base = format!("SPLIT-{group:03}");
    let reference = format!("REF-{base}");
    let first = synthetic_claim(&format!("{base}-A"), &base, 1, 100, &reference, group);
    let second = synthetic_claim(&format!("{base}-B"), &base, 1, 150, &reference, group);
    let remit = synthetic_remit(
        &base,
        250,
        TransactionKind::Payment,
        Some(reference),
        None,
        None,
        None,
        group,
    );
    push_serialized(claims, &first)?;
    push_serialized(claims, &second)?;
    push_serialized(remits, &remit)?;
    truth.push(GroundTruthRecord {
        remittance_line_id: remit.remittance_line_id.clone(),
        unambiguous: true,
        links: vec![
            expected(&remit, &first, 100),
            expected(&remit, &second, 150),
        ],
    });
    Ok(())
}

fn add_multiple_payments(
    group: usize,
    claims: &mut Vec<String>,
    remits: &mut Vec<String>,
    truth: &mut Vec<GroundTruthRecord>,
) -> Result<(), AnyError> {
    let base = format!("MULTI-{group:03}");
    let reference = format!("REF-{base}");
    let claim = synthetic_claim(&base, &base, 1, 300, &reference, group);
    let first = synthetic_remit(
        &format!("{base}-A"),
        100,
        TransactionKind::Payment,
        Some(reference.clone()),
        Some(claim.patient_key.clone()),
        Some(claim.service_date),
        Some(claim.procedure_code.clone()),
        group,
    );
    let second = synthetic_remit(
        &format!("{base}-B"),
        200,
        TransactionKind::Payment,
        Some(reference),
        Some(claim.patient_key.clone()),
        Some(claim.service_date),
        Some(claim.procedure_code.clone()),
        group,
    );
    push_serialized(claims, &claim)?;
    push_truth_for_single(truth, &first, &claim, 100);
    push_truth_for_single(truth, &second, &claim, 200);
    push_serialized(remits, &first)?;
    push_serialized(remits, &second)?;
    Ok(())
}

fn add_revision_case(
    group: usize,
    claims: &mut Vec<String>,
    remits: &mut Vec<String>,
    truth: &mut Vec<GroundTruthRecord>,
) -> Result<(), AnyError> {
    let base = format!("REVISION-{group:03}");
    let reference = format!("REF-{base}");
    let old = synthetic_claim(&base, &base, 1, 100, &reference, group);
    let mut current = old.clone();
    current.revision = 2;
    let remit = synthetic_remit(
        &base,
        100,
        TransactionKind::Payment,
        Some(reference),
        Some(current.patient_key.clone()),
        Some(current.service_date),
        Some(current.procedure_code.clone()),
        group,
    );
    push_serialized(claims, &old)?;
    push_serialized(claims, &current)?;
    push_truth_for_single(truth, &remit, &current, 100);
    push_serialized(remits, &remit)?;
    Ok(())
}

fn add_duplicate_reference_case(
    group: usize,
    claims: &mut Vec<String>,
    remits: &mut Vec<String>,
    truth: &mut Vec<GroundTruthRecord>,
) -> Result<(), AnyError> {
    let base = format!("DUPREF-{group:03}");
    let reference = format!("REF-{base}");
    let wrong = synthetic_claim(
        &format!("{base}-A"),
        &format!("{base}-A"),
        1,
        125,
        &reference,
        group,
    );
    let mut right = synthetic_claim(
        &format!("{base}-B"),
        &format!("{base}-B"),
        1,
        125,
        &reference,
        group,
    );
    right.service_date += 1;
    right.procedure_code = format!("PROC-DUP-{group:03}");
    let remit = synthetic_remit(
        &base,
        125,
        TransactionKind::Payment,
        Some(reference),
        Some(right.patient_key.clone()),
        Some(right.service_date),
        Some(right.procedure_code.clone()),
        group,
    );
    push_serialized(claims, &wrong)?;
    push_serialized(claims, &right)?;
    push_truth_for_single(truth, &remit, &right, 125);
    push_serialized(remits, &remit)?;
    Ok(())
}

fn add_payment_reversal(
    group: usize,
    claims: &mut Vec<String>,
    remits: &mut Vec<String>,
    truth: &mut Vec<GroundTruthRecord>,
) -> Result<(), AnyError> {
    let base = format!("REVERSAL-{group:03}");
    let reference = format!("REF-{base}");
    let claim = synthetic_claim(&base, &base, 1, 500, &reference, group);
    let payment = synthetic_remit(
        &format!("{base}-PAY"),
        500,
        TransactionKind::Payment,
        Some(reference.clone()),
        Some(claim.patient_key.clone()),
        Some(claim.service_date),
        Some(claim.procedure_code.clone()),
        group,
    );
    let reversal = synthetic_remit(
        &format!("{base}-REVERSE"),
        -100,
        TransactionKind::Reversal,
        Some(reference),
        Some(claim.patient_key.clone()),
        Some(claim.service_date),
        Some(claim.procedure_code.clone()),
        group,
    );
    push_serialized(claims, &claim)?;
    push_truth_for_single(truth, &payment, &claim, 500);
    push_truth_for_single(truth, &reversal, &claim, -100);
    push_serialized(remits, &payment)?;
    push_serialized(remits, &reversal)?;
    Ok(())
}

fn add_denial(
    group: usize,
    claims: &mut Vec<String>,
    remits: &mut Vec<String>,
    truth: &mut Vec<GroundTruthRecord>,
) -> Result<(), AnyError> {
    let base = format!("DENIAL-{group:03}");
    let reference = format!("REF-{base}");
    let claim = synthetic_claim(&base, &base, 1, 100, &reference, group);
    let mut remit = synthetic_remit(
        &base,
        100,
        TransactionKind::Denial,
        Some(reference),
        Some(claim.patient_key.clone()),
        Some(claim.service_date),
        Some(claim.procedure_code.clone()),
        group,
    );
    remit.paid_cents = 0;
    remit.adjustment_cents = 100;
    remit.adjustment_codes = vec!["contractual_adjustment".to_string()];
    push_serialized(claims, &claim)?;
    push_truth_for_single(truth, &remit, &claim, 100);
    push_serialized(remits, &remit)?;
    Ok(())
}

fn add_missing_optional_fields(
    group: usize,
    claims: &mut Vec<String>,
    remits: &mut Vec<String>,
    truth: &mut Vec<GroundTruthRecord>,
) -> Result<(), AnyError> {
    let base = format!("MISSING-{group:03}");
    let reference = format!("REF-{base}");
    let claim = synthetic_claim(&base, &base, 1, 90, &reference, group);
    let remit = synthetic_remit(
        &base,
        90,
        TransactionKind::Payment,
        Some(reference),
        None,
        None,
        None,
        group,
    );
    push_serialized(claims, &claim)?;
    push_truth_for_single(truth, &remit, &claim, 90);
    push_serialized(remits, &remit)?;
    Ok(())
}

fn add_ambiguous_case(
    group: usize,
    claims: &mut Vec<String>,
    remits: &mut Vec<String>,
    truth: &mut Vec<GroundTruthRecord>,
) -> Result<(), AnyError> {
    let base = format!("AMBIG-{group:03}");
    let reference = format!("REF-{base}");
    let first = synthetic_claim(
        &format!("{base}-A"),
        &format!("{base}-A"),
        1,
        110,
        &reference,
        group,
    );
    let mut second = synthetic_claim(
        &format!("{base}-B"),
        &format!("{base}-B"),
        1,
        110,
        &reference,
        group,
    );
    second.patient_key.clone_from(&first.patient_key);
    second.service_date = first.service_date;
    second.procedure_code.clone_from(&first.procedure_code);
    let remit = synthetic_remit(
        &base,
        110,
        TransactionKind::Payment,
        Some(reference),
        Some(first.patient_key.clone()),
        Some(first.service_date),
        Some(first.procedure_code.clone()),
        group,
    );
    push_serialized(claims, &first)?;
    push_serialized(claims, &second)?;
    push_serialized(remits, &remit)?;
    truth.push(GroundTruthRecord {
        remittance_line_id: remit.remittance_line_id,
        unambiguous: false,
        links: Vec::new(),
    });
    Ok(())
}

fn add_unsupported_bundle(
    group: usize,
    claims: &mut Vec<String>,
    remits: &mut Vec<String>,
    truth: &mut Vec<GroundTruthRecord>,
) -> Result<(), AnyError> {
    let base = format!("UNSUPPORTED-{group:03}");
    let reference = format!("REF-{base}");
    let first = synthetic_claim(
        &format!("{base}-A"),
        &format!("{base}-A"),
        1,
        100,
        &reference,
        group,
    );
    let second = synthetic_claim(
        &format!("{base}-B"),
        &format!("{base}-B"),
        1,
        150,
        &reference,
        group,
    );
    let remit = synthetic_remit(
        &base,
        250,
        TransactionKind::Payment,
        Some(reference),
        None,
        None,
        None,
        group,
    );
    push_serialized(claims, &first)?;
    push_serialized(claims, &second)?;
    push_serialized(remits, &remit)?;
    truth.push(GroundTruthRecord {
        remittance_line_id: remit.remittance_line_id,
        unambiguous: false,
        links: Vec::new(),
    });
    Ok(())
}

fn synthetic_claim(
    line_suffix: &str,
    claim_suffix: &str,
    revision: u32,
    amount: i64,
    reference: &str,
    variation: usize,
) -> ClaimLine {
    ClaimLine {
        claim_line_id: format!("C-{line_suffix}"),
        claim_id: format!("CLAIM-{claim_suffix}"),
        revision,
        payer: format!("payer-{:02}", variation % 17),
        provider: format!("provider-{:02}", variation % 29),
        patient_key: format!("patient-key-{line_suffix}"),
        service_date: 20_000 + i32::try_from(variation % 365).unwrap_or_default(),
        procedure_code: format!("PROC-{:03}", variation % 101),
        billed_cents: amount,
        open_balance_cents: amount,
        insurer_references: vec![reference.to_string()],
    }
}

#[allow(clippy::too_many_arguments)]
fn synthetic_remit(
    suffix: &str,
    amount: i64,
    transaction_kind: TransactionKind,
    insurer_reference: Option<String>,
    patient_key: Option<String>,
    service_date: Option<i32>,
    procedure_code: Option<String>,
    variation: usize,
) -> RemittanceLine {
    RemittanceLine {
        remittance_line_id: format!("R-{suffix}"),
        payer: format!("payer-{:02}", variation % 17),
        provider: format!("provider-{:02}", variation % 29),
        insurer_reference,
        patient_key,
        service_date_start: service_date,
        service_date_end: service_date,
        procedure_code,
        paid_cents: amount,
        adjustment_cents: 0,
        adjustment_codes: Vec::new(),
        transaction_kind,
    }
}

fn push_truth_for_single(
    truth: &mut Vec<GroundTruthRecord>,
    remit: &RemittanceLine,
    claim: &ClaimLine,
    amount: i64,
) {
    truth.push(GroundTruthRecord {
        remittance_line_id: remit.remittance_line_id.clone(),
        unambiguous: true,
        links: vec![expected(remit, claim, amount)],
    });
}

fn expected(remit: &RemittanceLine, claim: &ClaimLine, amount: i64) -> ExpectedLink {
    ExpectedLink {
        remittance_line_id: remit.remittance_line_id.clone(),
        claim_line_id: claim.claim_line_id.clone(),
        claim_revision: claim.revision,
        applied_cents: amount,
    }
}

fn push_serialized<T: Serialize>(lines: &mut Vec<String>, value: &T) -> Result<(), AnyError> {
    lines.push(serde_json::to_string(value)?);
    Ok(())
}

fn read_lines(path: &Path) -> Result<Vec<String>, AnyError> {
    Ok(BufReader::new(File::open(path)?)
        .lines()
        .collect::<Result<Vec<_>, _>>()?)
}

fn write_lines(path: &Path, lines: &[String]) -> Result<(), AnyError> {
    let mut writer = BufWriter::new(File::create(path)?);
    for line in lines {
        writer.write_all(line.as_bytes())?;
        writer.write_all(b"\n")?;
    }
    writer.flush()?;
    Ok(())
}

fn write_jsonl<T: Serialize>(path: &Path, values: &[T]) -> Result<(), AnyError> {
    let mut writer = BufWriter::new(File::create(path)?);
    for value in values {
        serde_json::to_writer(&mut writer, value)?;
        writer.write_all(b"\n")?;
    }
    writer.flush()?;
    Ok(())
}

#[derive(Debug)]
struct XorShift64(u64);

impl XorShift64 {
    fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    fn next(&mut self) -> u64 {
        let mut value = self.0;
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        self.0 = value;
        value
    }
}

fn shuffle<T>(items: &mut [T], rng: &mut XorShift64) {
    for index in (1..items.len()).rev() {
        let upper = u64::try_from(index + 1).unwrap_or(u64::MAX);
        let chosen = usize::try_from(rng.next() % upper).unwrap_or_default();
        items.swap(index, chosen);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shuffle_is_deterministic() {
        let mut first = (0..100).collect::<Vec<_>>();
        let mut second = first.clone();
        shuffle(&mut first, &mut XorShift64::new(42));
        shuffle(&mut second, &mut XorShift64::new(42));
        assert_eq!(first, second);
        assert_ne!(first, (0..100).collect::<Vec<_>>());
    }
}
