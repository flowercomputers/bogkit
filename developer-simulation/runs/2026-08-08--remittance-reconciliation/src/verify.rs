use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::error::Error;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{
    AcceptedLink, ClaimKey, ClaimLine, ExpectedLink, GroundTruthRecord, RemittanceLine,
    ReviewRecord, Summary, TransactionKind,
};

type AnyError = Box<dyn Error + Send + Sync>;

struct RemittanceInput {
    records: HashMap<String, RemittanceLine>,
    physical_counts: BTreeMap<String, usize>,
    patient_keys: HashSet<String>,
    total: usize,
    invalid: usize,
}

struct ClaimInput {
    records: Vec<ClaimLine>,
    duplicate_keys: HashSet<ClaimKey>,
    patient_keys: HashSet<String>,
    total: usize,
    invalid: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationReport {
    pub passed: bool,
    pub precision: f64,
    pub recall: f64,
    pub correct_links: usize,
    pub accepted_links: usize,
    pub expected_unambiguous_links: usize,
    pub invariant_failures: usize,
    pub failures: Vec<String>,
}

pub fn verify_results(
    claims_path: &Path,
    remits_path: &Path,
    ground_truth_path: &Path,
    results_dir: &Path,
) -> Result<VerificationReport, AnyError> {
    let claim_input = read_valid_claims(claims_path)?;
    let current_claims = current_claim_map(&claim_input.records);
    let remit_input = read_remittances(remits_path)?;
    let accepted = read_jsonl::<AcceptedLink>(&results_dir.join("accepted.jsonl"))?;
    let reviews = read_jsonl::<ReviewRecord>(&results_dir.join("review.jsonl"))?;
    let truth = read_jsonl::<GroundTruthRecord>(ground_truth_path)?;
    let summary: Summary = serde_json::from_reader(File::open(results_dir.join("summary.json"))?)?;
    let serialized_output_strings = read_output_strings(results_dir)?;

    let mut failures = Vec::new();
    verify_canonical_order(&accepted, &reviews, &mut failures);
    verify_decision_partition(
        &accepted,
        &reviews,
        &remit_input.physical_counts,
        &mut failures,
    );
    verify_reason_codes(&reviews, &mut failures);
    verify_explanations(&accepted, &mut failures);
    verify_value_rules(
        &accepted,
        &remit_input.records,
        &current_claims,
        &claim_input.duplicate_keys,
        &mut failures,
    );
    verify_privacy(
        &claim_input.patient_keys,
        &remit_input.patient_keys,
        &serialized_output_strings,
        &mut failures,
    );
    verify_summary(
        &summary,
        claim_input.total,
        current_claims.len(),
        claim_input.invalid,
        remit_input.total,
        remit_input.invalid,
        &accepted,
        &reviews,
        &mut failures,
    );

    let actual = accepted
        .iter()
        .map(|link| ExpectedLink {
            remittance_line_id: link.remittance_line_id.clone(),
            claim_line_id: link.claim_line_id.clone(),
            claim_revision: link.claim_revision,
            applied_cents: link.applied_cents,
        })
        .collect::<BTreeSet<_>>();
    let expected = truth
        .iter()
        .filter(|record| record.unambiguous)
        .flat_map(|record| record.links.iter().cloned())
        .collect::<BTreeSet<_>>();
    let correct_links = actual.intersection(&expected).count();
    let precision = ratio(correct_links, actual.len());
    let recall = ratio(correct_links, expected.len());
    if precision < 0.9995 {
        failures.push(format!(
            "precision {precision:.6} is below the required 0.999500"
        ));
    }
    if recall < 0.97 {
        failures.push(format!("recall {recall:.6} is below the required 0.970000"));
    }
    let invariant_failures = failures
        .iter()
        .filter(|failure| !failure.starts_with("precision ") && !failure.starts_with("recall "))
        .count();
    Ok(VerificationReport {
        passed: failures.is_empty(),
        precision,
        recall,
        correct_links,
        accepted_links: actual.len(),
        expected_unambiguous_links: expected.len(),
        invariant_failures,
        failures,
    })
}

fn read_valid_claims(path: &Path) -> Result<ClaimInput, AnyError> {
    let mut records = Vec::new();
    let mut total = 0;
    let mut invalid = 0;
    for line in BufReader::new(File::open(path)?).lines() {
        total += 1;
        match serde_json::from_str::<ClaimLine>(&line?) {
            Ok(record) if record.validate().is_ok() => records.push(record),
            _ => invalid += 1,
        }
    }
    let patient_keys = records
        .iter()
        .map(|record| record.patient_key.clone())
        .collect();
    let mut counts = HashMap::new();
    for record in &records {
        *counts.entry(record.key()).or_insert(0_usize) += 1;
    }
    let duplicate_keys = counts
        .iter()
        .filter(|(_, count)| **count > 1)
        .map(|(key, _)| key.clone())
        .collect::<HashSet<_>>();
    let duplicate_rows = records
        .iter()
        .filter(|record| duplicate_keys.contains(&record.key()))
        .count();
    invalid += duplicate_rows;
    records.retain(|record| !duplicate_keys.contains(&record.key()));
    Ok(ClaimInput {
        records,
        duplicate_keys,
        patient_keys,
        total,
        invalid,
    })
}

fn read_remittances(path: &Path) -> Result<RemittanceInput, AnyError> {
    struct Row {
        id: String,
        record: Option<RemittanceLine>,
    }

    let mut rows = Vec::new();
    let mut patient_keys = HashSet::new();
    let mut total = 0;
    let mut invalid = 0;
    for line in BufReader::new(File::open(path)?).lines() {
        total += 1;
        let line = line?;
        let value = serde_json::from_str::<Value>(&line).ok();
        if let Some(patient_key) = value
            .as_ref()
            .and_then(|item| item.get("patient_key"))
            .and_then(Value::as_str)
        {
            patient_keys.insert(patient_key.to_string());
        }
        let id = value
            .as_ref()
            .and_then(|item| item.get("remittance_line_id"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| "quarantine-remittance-missing-id".to_string());
        let record = serde_json::from_str::<RemittanceLine>(&line)
            .ok()
            .filter(|record| record.validate().is_ok());
        rows.push(Row { id, record });
    }
    let mut physical_counts = BTreeMap::new();
    for row in &rows {
        *physical_counts.entry(row.id.clone()).or_insert(0_usize) += 1;
    }
    let mut records = HashMap::new();
    for row in rows {
        let count = physical_counts.get(&row.id).copied().unwrap_or_default();
        if count > 1 {
            invalid += 1;
        } else if let Some(record) = row.record {
            records.insert(row.id, record);
        } else {
            invalid += 1;
        }
    }
    Ok(RemittanceInput {
        records,
        physical_counts,
        patient_keys,
        total,
        invalid,
    })
}

fn current_claim_map(records: &[ClaimLine]) -> BTreeMap<ClaimKey, ClaimLine> {
    let mut max_revisions = HashMap::new();
    for claim in records {
        max_revisions
            .entry(claim.claim_line_id.as_str())
            .and_modify(|revision: &mut u32| *revision = (*revision).max(claim.revision))
            .or_insert(claim.revision);
    }
    records
        .iter()
        .filter(|claim| max_revisions.get(claim.claim_line_id.as_str()) == Some(&claim.revision))
        .cloned()
        .map(|claim| (claim.key(), claim))
        .collect()
}

fn verify_canonical_order(
    accepted: &[AcceptedLink],
    reviews: &[ReviewRecord],
    failures: &mut Vec<String>,
) {
    let accepted_keys = accepted
        .iter()
        .map(|link| {
            (
                &link.remittance_line_id,
                &link.claim_line_id,
                link.claim_revision,
                link.applied_cents,
            )
        })
        .collect::<Vec<_>>();
    if !accepted_keys.windows(2).all(|pair| pair[0] <= pair[1]) {
        failures.push("accepted.jsonl is not in canonical order".to_string());
    }
    let review_keys = reviews
        .iter()
        .map(|record| (&record.remittance_line_id, record.physical_record_ordinal))
        .collect::<Vec<_>>();
    if !review_keys.windows(2).all(|pair| pair[0] <= pair[1]) {
        failures.push("review.jsonl is not in canonical order".to_string());
    }
}

fn verify_decision_partition(
    accepted: &[AcceptedLink],
    reviews: &[ReviewRecord],
    physical_counts: &BTreeMap<String, usize>,
    failures: &mut Vec<String>,
) {
    let accepted_ids = accepted
        .iter()
        .map(|link| link.remittance_line_id.as_str())
        .collect::<HashSet<_>>();
    let mut review_ordinals: HashMap<&str, BTreeSet<usize>> = HashMap::new();
    for review in reviews {
        let inserted = review_ordinals
            .entry(review.remittance_line_id.as_str())
            .or_default()
            .insert(review.physical_record_ordinal);
        if !inserted {
            failures.push(format!(
                "review {} repeats physical ordinal {}",
                review.remittance_line_id, review.physical_record_ordinal
            ));
        }
    }
    for (id, physical_count) in physical_counts {
        let accepted_count = usize::from(accepted_ids.contains(id.as_str()));
        let ordinals = review_ordinals
            .get(id.as_str())
            .cloned()
            .unwrap_or_default();
        if *physical_count > 1 {
            let expected = (1..=*physical_count).collect::<BTreeSet<_>>();
            let reasons_valid = reviews
                .iter()
                .filter(|review| review.remittance_line_id == *id)
                .all(|review| {
                    review
                        .reason_codes
                        .iter()
                        .any(|reason| reason == "duplicate_remittance_id")
                });
            if accepted_count != 0 || ordinals != expected || !reasons_valid {
                failures.push(format!(
                    "duplicate remittance {id} was not quarantined as {physical_count} physical rows"
                ));
            }
        } else if accepted_count + ordinals.len() != 1
            || ordinals.iter().any(|ordinal| *ordinal != 1)
        {
            failures.push(format!(
                "remittance {id} has {} decisions instead of exactly one",
                accepted_count + ordinals.len()
            ));
        }
    }
    for id in accepted_ids.iter().chain(review_ordinals.keys()).copied() {
        if !physical_counts.contains_key(id) {
            failures.push(format!("output contains unknown remittance {id}"));
        }
    }
    let duplicate_links = accepted
        .iter()
        .map(|link| {
            (
                &link.remittance_line_id,
                &link.claim_line_id,
                link.claim_revision,
                link.applied_cents,
            )
        })
        .collect::<Vec<_>>();
    if duplicate_links.iter().collect::<HashSet<_>>().len() != duplicate_links.len() {
        failures.push("accepted output contains a duplicated application".to_string());
    }
}

fn verify_reason_codes(reviews: &[ReviewRecord], failures: &mut Vec<String>) {
    let allowed = HashSet::from([
        "missing_identity",
        "conflicting_strong_candidates",
        "amount_inconsistent",
        "unsupported_bundle",
        "no_plausible_candidate",
        "malformed_record",
        "unsupported_date_range",
        "conflicting_identity_sources",
        "duplicate_remittance_id",
        "duplicate_claim_key",
        "unsupported_partial_split",
        "unsupported_standalone_reversal",
        "search_budget_exhausted",
    ]);
    for review in reviews {
        if review.reason_codes.is_empty()
            || review
                .reason_codes
                .iter()
                .any(|reason| !allowed.contains(reason.as_str()))
        {
            failures.push(format!(
                "review {} has missing or unstable reason codes",
                review.remittance_line_id
            ));
        }
    }
}

fn verify_explanations(accepted: &[AcceptedLink], failures: &mut Vec<String>) {
    for link in accepted {
        if link.facts.is_empty()
            || link
                .rejected_competitors
                .iter()
                .any(|competitor| competitor.reason_codes.is_empty())
        {
            failures.push(format!(
                "accepted link {} -> {} lacks a machine-checkable explanation",
                link.remittance_line_id, link.claim_line_id
            ));
        }
    }
}

fn verify_value_rules(
    accepted: &[AcceptedLink],
    remits: &HashMap<String, RemittanceLine>,
    current_claims: &BTreeMap<ClaimKey, ClaimLine>,
    duplicate_claim_keys: &HashSet<ClaimKey>,
    failures: &mut Vec<String>,
) {
    let mut by_remit: HashMap<&str, Vec<&AcceptedLink>> = HashMap::new();
    let mut claim_positive: BTreeMap<ClaimKey, i64> = BTreeMap::new();
    let mut claim_negative: BTreeMap<ClaimKey, i64> = BTreeMap::new();
    for link in accepted {
        by_remit
            .entry(link.remittance_line_id.as_str())
            .or_default()
            .push(link);
        let key = ClaimKey {
            claim_line_id: link.claim_line_id.clone(),
            revision: link.claim_revision,
        };
        if duplicate_claim_keys.contains(&key) {
            failures.push(format!(
                "accepted link {} reuses a duplicated logical claim key",
                link.remittance_line_id
            ));
        }
        if !current_claims.contains_key(&key) {
            failures.push(format!(
                "accepted link {} targets a missing or non-current claim revision",
                link.remittance_line_id
            ));
        }
        if link.applied_cents > 0 {
            *claim_positive.entry(key).or_default() += link.applied_cents;
        } else {
            *claim_negative.entry(key).or_default() += link.applied_cents.unsigned_abs() as i64;
        }
    }

    for (id, links) in by_remit {
        let Some(remit) = remits.get(id) else {
            failures.push(format!("accepted unknown or malformed remittance {id}"));
            continue;
        };
        let total = links.iter().map(|link| link.applied_cents).sum::<i64>();
        if total != remit.signed_amount() {
            failures.push(format!("remittance {id} does not conserve cents"));
        }
        let sign_valid = match remit.transaction_kind {
            TransactionKind::Payment | TransactionKind::Denial => {
                links.iter().all(|link| link.applied_cents > 0)
            }
            TransactionKind::Reversal => links.iter().all(|link| link.applied_cents < 0),
        };
        if !sign_valid {
            failures.push(format!("remittance {id} has an allocation sign error"));
        }
        let claim_ids = links
            .iter()
            .filter_map(|link| {
                current_claims
                    .get(&ClaimKey {
                        claim_line_id: link.claim_line_id.clone(),
                        revision: link.claim_revision,
                    })
                    .map(|claim| claim.claim_id.as_str())
            })
            .collect::<HashSet<_>>();
        if links.len() > 1 && claim_ids.len() != 1 {
            failures.push(format!(
                "remittance {id} uses an unsupported cross-claim split"
            ));
        }
    }

    for (key, claim) in current_claims {
        let positive = claim_positive.get(key).copied().unwrap_or_default();
        let negative = claim_negative.get(key).copied().unwrap_or_default();
        if positive > claim.open_balance_cents
            || negative > claim.open_balance_cents
            || positive < negative
        {
            failures.push(format!(
                "claim {} revision {} is over-applied or reversal-inconsistent",
                key.claim_line_id, key.revision
            ));
        }
    }
}

fn verify_privacy(
    claim_patient_keys: &HashSet<String>,
    remittance_patient_keys: &HashSet<String>,
    serialized_output_strings: &HashSet<String>,
    failures: &mut Vec<String>,
) {
    if claim_patient_keys
        .iter()
        .chain(remittance_patient_keys)
        .any(|key| serialized_output_strings.contains(key))
    {
        failures.push("output contains a source patient key".to_string());
    }
}

fn read_output_strings(results_dir: &Path) -> Result<HashSet<String>, AnyError> {
    let mut strings = HashSet::new();
    for filename in ["accepted.jsonl", "review.jsonl", "summary.json"] {
        let contents = fs::read_to_string(results_dir.join(filename))?;
        if filename.ends_with(".jsonl") {
            for line in contents.lines() {
                collect_json_strings(&serde_json::from_str::<Value>(line)?, &mut strings);
            }
        } else {
            collect_json_strings(&serde_json::from_str::<Value>(&contents)?, &mut strings);
        }
    }
    Ok(strings)
}

fn collect_json_strings(value: &Value, strings: &mut HashSet<String>) {
    match value {
        Value::String(string) => {
            strings.insert(string.clone());
        }
        Value::Array(items) => {
            for item in items {
                collect_json_strings(item, strings);
            }
        }
        Value::Object(object) => {
            for (key, item) in object {
                strings.insert(key.clone());
                collect_json_strings(item, strings);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn verify_summary(
    summary: &Summary,
    claim_total: usize,
    current_claim_count: usize,
    invalid_claims: usize,
    remit_total: usize,
    invalid_remits: usize,
    accepted: &[AcceptedLink],
    reviews: &[ReviewRecord],
    failures: &mut Vec<String>,
) {
    let accepted_remittances = accepted
        .iter()
        .map(|link| link.remittance_line_id.as_str())
        .collect::<HashSet<_>>()
        .len();
    let expected = Summary {
        input_claim_records: claim_total,
        valid_current_claim_lines: current_claim_count,
        invalid_claim_records: invalid_claims,
        input_remittance_records: remit_total,
        accepted_remittance_lines: accepted_remittances,
        accepted_links: accepted.len(),
        review_remittance_lines: reviews.len(),
        invalid_remittance_records: invalid_remits,
    };
    if summary != &expected {
        failures.push("summary.json does not match the canonical outputs".to_string());
    }
}

fn read_jsonl<T>(path: &Path) -> Result<Vec<T>, AnyError>
where
    T: for<'de> Deserialize<'de>,
{
    BufReader::new(File::open(path)?)
        .lines()
        .filter(|line| line.as_ref().map_or(true, |value| !value.is_empty()))
        .map(|line| Ok(serde_json::from_str(&line?)?))
        .collect()
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        1.0
    } else {
        numerator as f64 / denominator as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratio_handles_empty_denominator() {
        assert_eq!(ratio(0, 0), 1.0);
    }
}
