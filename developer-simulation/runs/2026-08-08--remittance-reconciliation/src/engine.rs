use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::error::Error;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

use crate::model::{
    AcceptedLink, ClaimKey, ClaimLine, RejectedCompetitor, RemittanceLine, ReviewRecord, Summary,
    TransactionKind,
};

pub type AnyError = Box<dyn Error + Send + Sync>;

const MAX_CLUSTER_NODES: usize = 24;
const MAX_SPLIT_CANDIDATES: usize = 12;
const MAX_SEARCH_NODES: usize = 1_000_000;

#[derive(Debug)]
struct ClaimInput {
    records: Vec<ClaimLine>,
    quarantined_duplicates: Vec<ClaimLine>,
    total: usize,
    invalid: usize,
}

#[derive(Debug)]
struct RemitInput {
    records: Vec<RemittanceLine>,
    pre_reviews: Vec<ReviewRecord>,
    total: usize,
    invalid: usize,
}

#[derive(Debug, Clone)]
struct Candidate {
    claim_idx: usize,
    score: i32,
    facts: Vec<String>,
}

#[derive(Debug, Clone)]
struct CandidateEvaluation {
    candidates: Vec<Candidate>,
    reference_claims: BTreeSet<usize>,
    fallback_claims: BTreeSet<usize>,
}

impl CandidateEvaluation {
    fn has_both_identity_sources(&self) -> bool {
        !self.reference_claims.is_empty() && !self.fallback_claims.is_empty()
    }

    fn identity_sources_disagree(&self) -> bool {
        self.has_both_identity_sources() && self.reference_claims != self.fallback_claims
    }

    fn agreement_candidates(&self) -> Vec<Candidate> {
        self.candidates
            .iter()
            .filter(|candidate| {
                self.reference_claims.contains(&candidate.claim_idx)
                    && self.fallback_claims.contains(&candidate.claim_idx)
            })
            .cloned()
            .collect()
    }

    fn has_any(&self) -> bool {
        !self.candidates.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Allocation {
    claim_idx: usize,
    applied_cents: i64,
    score: i32,
    facts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AssignmentOption {
    allocations: Vec<Allocation>,
    score: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Objective {
    accepted: usize,
    score: i64,
}

impl Ord for Objective {
    fn cmp(&self, other: &Self) -> Ordering {
        self.accepted
            .cmp(&other.accepted)
            .then_with(|| self.score.cmp(&other.score))
    }
}

impl PartialOrd for Objective {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Default)]
struct SearchBest {
    objective: Option<Objective>,
    assignments: Vec<Option<AssignmentOption>>,
    tied: bool,
}

#[derive(Debug)]
struct ClaimIndex {
    by_reference: HashMap<(String, String, String), Vec<usize>>,
    by_identity: HashMap<(String, String, String, i32, String), Vec<usize>>,
}

impl ClaimIndex {
    fn new(claims: &[ClaimLine]) -> Self {
        let mut by_reference: HashMap<(String, String, String), Vec<usize>> = HashMap::new();
        let mut by_identity: HashMap<(String, String, String, i32, String), Vec<usize>> =
            HashMap::new();
        for (idx, claim) in claims.iter().enumerate() {
            for reference in &claim.insurer_references {
                by_reference
                    .entry((
                        claim.payer.clone(),
                        claim.provider.clone(),
                        reference.clone(),
                    ))
                    .or_default()
                    .push(idx);
            }
            by_identity
                .entry((
                    claim.payer.clone(),
                    claim.provider.clone(),
                    claim.patient_key.clone(),
                    claim.service_date,
                    claim.procedure_code.clone(),
                ))
                .or_default()
                .push(idx);
        }
        Self {
            by_reference,
            by_identity,
        }
    }

    fn evaluate(&self, remit: &RemittanceLine, claims: &[ClaimLine]) -> CandidateEvaluation {
        let mut reference_claims: BTreeSet<usize> = BTreeSet::new();
        if let Some(reference) = &remit.insurer_reference
            && let Some(matches) = self.by_reference.get(&(
                remit.payer.clone(),
                remit.provider.clone(),
                reference.clone(),
            ))
        {
            reference_claims.extend(matches);
        }
        let mut fallback_claims: BTreeSet<usize> = BTreeSet::new();
        if let (Some(patient), Some(start), Some(end), Some(procedure)) = (
            &remit.patient_key,
            remit.service_date_start,
            remit.service_date_end,
            &remit.procedure_code,
        ) {
            for day in start..=end.min(start.saturating_add(31)) {
                if let Some(matches) = self.by_identity.get(&(
                    remit.payer.clone(),
                    remit.provider.clone(),
                    patient.clone(),
                    day,
                    procedure.clone(),
                )) {
                    fallback_claims.extend(matches);
                }
            }
        }

        let indices = reference_claims
            .union(&fallback_claims)
            .copied()
            .collect::<BTreeSet<_>>();
        let amount = remit.signed_amount().unsigned_abs();
        let mut candidates = indices
            .into_iter()
            .map(|claim_idx| {
                let claim = &claims[claim_idx];
                let reference_match = remit.insurer_reference.as_ref().is_some_and(|reference| {
                    claim
                        .insurer_references
                        .iter()
                        .any(|item| item == reference)
                });
                let patient_match = remit
                    .patient_key
                    .as_ref()
                    .is_some_and(|patient| patient == &claim.patient_key);
                let date_match = remit
                    .service_date_start
                    .zip(remit.service_date_end)
                    .is_some_and(|(start, end)| (start..=end).contains(&claim.service_date));
                let procedure_match = remit
                    .procedure_code
                    .as_ref()
                    .is_some_and(|procedure| procedure == &claim.procedure_code);

                let mut facts = Vec::new();
                let mut score = 0;
                if reference_match {
                    facts.push("insurer_reference".to_string());
                    score += 100;
                }
                if patient_match {
                    facts.push("patient_key_exact".to_string());
                    score += 30;
                }
                if date_match {
                    facts.push("service_date_in_range".to_string());
                    score += 20;
                }
                if procedure_match {
                    facts.push("procedure_code_exact".to_string());
                    score += 20;
                }
                if amount == claim.open_balance_cents.unsigned_abs() {
                    facts.push("amount_exact".to_string());
                    score += 10;
                }
                Candidate {
                    claim_idx,
                    score,
                    facts,
                }
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            let left_claim = &claims[left.claim_idx];
            let right_claim = &claims[right.claim_idx];
            left_claim
                .claim_line_id
                .cmp(&right_claim.claim_line_id)
                .then_with(|| left_claim.revision.cmp(&right_claim.revision))
        });
        CandidateEvaluation {
            candidates,
            reference_claims,
            fallback_claims,
        }
    }
}

pub fn reconcile(
    claims_path: &Path,
    remits_path: &Path,
    output_dir: &Path,
) -> Result<Summary, AnyError> {
    let claim_input = read_claims(claims_path)?;
    let remit_input = read_remittances(remits_path)?;
    let claims = current_revisions(&claim_input.records);
    let index = ClaimIndex::new(&claims);
    let quarantined_index = ClaimIndex::new(&claim_input.quarantined_duplicates);
    let mut accepted = Vec::new();
    let mut reviews = remit_input.pre_reviews;
    let mut active_remits = Vec::new();
    let mut candidate_lists = Vec::new();
    let mut identity_sources_disagree = Vec::new();
    for remit in remit_input.records {
        let quarantined = quarantined_index.evaluate(&remit, &claim_input.quarantined_duplicates);
        let evaluation = index.evaluate(&remit, &claims);
        if quarantined.has_any() {
            reviews.push(ReviewRecord {
                remittance_line_id: remit.remittance_line_id,
                physical_record_ordinal: 1,
                reason_codes: vec!["duplicate_claim_key".to_string()],
                candidate_count: quarantined.candidates.len(),
            });
        } else {
            let has_both_sources = evaluation.has_both_identity_sources();
            let sources_disagree = evaluation.identity_sources_disagree();
            let candidates = if has_both_sources {
                evaluation.agreement_candidates()
            } else {
                evaluation.candidates
            };
            active_remits.push(remit);
            candidate_lists.push(candidates);
            identity_sources_disagree.push(sources_disagree);
        }
    }
    resolve_components(
        &claims,
        &active_remits,
        &candidate_lists,
        &identity_sources_disagree,
        &mut accepted,
        &mut reviews,
    );
    write_results(
        output_dir,
        accepted,
        reviews,
        claim_input.total,
        claims.len(),
        claim_input.invalid,
        remit_input.total,
        remit_input.invalid,
    )
}

pub fn greedy_baseline(
    claims_path: &Path,
    remits_path: &Path,
    output_dir: &Path,
) -> Result<Summary, AnyError> {
    let claim_input = read_claims(claims_path)?;
    let remit_input = read_remittances(remits_path)?;
    let claims = claim_input.records;
    let current_count = current_revisions(&claims).len();
    let index = ClaimIndex::new(&claims);
    let mut positive_used = vec![0_i64; claims.len()];
    let mut negative_used = vec![0_i64; claims.len()];
    let mut accepted = Vec::new();
    let mut reviews = remit_input.pre_reviews;
    let quarantined_index = ClaimIndex::new(&claim_input.quarantined_duplicates);

    for remit in &remit_input.records {
        let quarantined = quarantined_index.evaluate(remit, &claim_input.quarantined_duplicates);
        if quarantined.has_any() {
            reviews.push(ReviewRecord {
                remittance_line_id: remit.remittance_line_id.clone(),
                physical_record_ordinal: 1,
                reason_codes: vec!["duplicate_claim_key".to_string()],
                candidate_count: quarantined.candidates.len(),
            });
            continue;
        }
        let evaluation = index.evaluate(remit, &claims);
        let candidates = &evaluation.candidates;
        let signed = remit.signed_amount();
        let acceptable = |candidate: &&Candidate| {
            candidate_is_acceptable(candidate, remit, &claims, &positive_used, &negative_used)
        };
        let reference_choice = candidates
            .iter()
            .filter(|candidate| evaluation.reference_claims.contains(&candidate.claim_idx))
            .filter(acceptable)
            .min_by_key(|candidate| candidate.claim_idx);
        let chosen = reference_choice.or_else(|| {
            candidates
                .iter()
                .filter(|candidate| evaluation.fallback_claims.contains(&candidate.claim_idx))
                .filter(acceptable)
                .min_by_key(|candidate| candidate.claim_idx)
        });

        if let Some(candidate) = chosen {
            if signed > 0 {
                positive_used[candidate.claim_idx] += signed;
            } else {
                negative_used[candidate.claim_idx] += signed.unsigned_abs() as i64;
            }
            let claim = &claims[candidate.claim_idx];
            let rejected_competitors = competitors(
                candidates,
                &[candidate.claim_idx],
                &claims,
                "greedy_candidate_not_first",
            );
            accepted.push(AcceptedLink {
                remittance_line_id: remit.remittance_line_id.clone(),
                claim_line_id: claim.claim_line_id.clone(),
                claim_revision: claim.revision,
                applied_cents: signed,
                score: candidate.score,
                facts: candidate.facts.clone(),
                rejected_competitors,
            });
        } else {
            reviews.push(ReviewRecord {
                remittance_line_id: remit.remittance_line_id.clone(),
                physical_record_ordinal: 1,
                reason_codes: reason_for_no_assignment(remit, candidates, &claims, false),
                candidate_count: candidates.len(),
            });
        }
    }

    write_results(
        output_dir,
        accepted,
        reviews,
        claim_input.total,
        current_count,
        claim_input.invalid,
        remit_input.total,
        remit_input.invalid,
    )
}

fn candidate_is_acceptable(
    candidate: &Candidate,
    remit: &RemittanceLine,
    claims: &[ClaimLine],
    positive_used: &[i64],
    negative_used: &[i64],
) -> bool {
    let claim = &claims[candidate.claim_idx];
    let signed = remit.signed_amount();
    let amount = signed.unsigned_abs();
    if amount > claim.open_balance_cents.unsigned_abs() {
        return false;
    }
    if matches!(remit.transaction_kind, TransactionKind::Reversal)
        && !candidate
            .facts
            .iter()
            .any(|fact| fact == "insurer_reference")
    {
        return false;
    }
    if signed > 0 {
        positive_used[candidate.claim_idx] + signed <= claim.open_balance_cents
    } else {
        negative_used[candidate.claim_idx] + signed.unsigned_abs() as i64
            <= claim.open_balance_cents
    }
}

fn read_claims(path: &Path) -> Result<ClaimInput, AnyError> {
    let reader = BufReader::new(File::open(path)?);
    let mut records = Vec::new();
    let mut invalid = 0;
    let mut total = 0;
    let mut line = String::new();
    let mut reader = reader;
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        total += 1;
        match serde_json::from_str::<ClaimLine>(&line) {
            Ok(record) if record.validate().is_ok() => records.push(record),
            _ => invalid += 1,
        }
    }
    let mut counts = HashMap::new();
    for record in &records {
        *counts.entry(record.key()).or_insert(0_usize) += 1;
    }
    let mut quarantined_duplicates = Vec::new();
    let mut unique = Vec::new();
    for record in records {
        if counts.get(&record.key()).copied().unwrap_or_default() > 1 {
            invalid += 1;
            quarantined_duplicates.push(record);
        } else {
            unique.push(record);
        }
    }
    Ok(ClaimInput {
        records: unique,
        quarantined_duplicates,
        total,
        invalid,
    })
}

fn read_remittances(path: &Path) -> Result<RemitInput, AnyError> {
    let reader = BufReader::new(File::open(path)?);
    struct Row {
        id: String,
        record: Option<RemittanceLine>,
        invalid_reason: Option<String>,
    }

    let mut rows = Vec::new();
    let mut pre_reviews = Vec::new();
    let mut invalid = 0;
    let mut total = 0;
    let mut line = String::new();
    let mut reader = reader;
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        total += 1;
        let parsed_value = serde_json::from_str::<Value>(&line).ok();
        let extracted_id = parsed_value
            .as_ref()
            .and_then(|value| value.get("remittance_line_id"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| "quarantine-remittance-missing-id".to_string());
        match serde_json::from_str::<RemittanceLine>(&line) {
            Ok(record) => match record.validate() {
                Ok(()) => rows.push(Row {
                    id: extracted_id,
                    record: Some(record),
                    invalid_reason: None,
                }),
                Err(reason) => rows.push(Row {
                    id: extracted_id,
                    record: None,
                    invalid_reason: Some(reason.to_string()),
                }),
            },
            Err(_) => rows.push(Row {
                id: extracted_id,
                record: None,
                invalid_reason: Some("malformed_record".to_string()),
            }),
        }
    }
    let mut counts = HashMap::new();
    for row in &rows {
        *counts.entry(row.id.clone()).or_insert(0_usize) += 1;
    }
    let mut emitted_duplicates = HashSet::new();
    let mut records = Vec::new();
    for row in rows {
        let count = counts.get(&row.id).copied().unwrap_or_default();
        if count > 1 {
            if emitted_duplicates.insert(row.id.clone()) {
                invalid += count;
                for ordinal in 1..=count {
                    pre_reviews.push(ReviewRecord {
                        remittance_line_id: row.id.clone(),
                        physical_record_ordinal: ordinal,
                        reason_codes: vec!["duplicate_remittance_id".to_string()],
                        candidate_count: 0,
                    });
                }
            }
        } else if let Some(record) = row.record {
            records.push(record);
        } else {
            invalid += 1;
            pre_reviews.push(ReviewRecord {
                remittance_line_id: row.id,
                physical_record_ordinal: 1,
                reason_codes: vec![
                    row.invalid_reason
                        .unwrap_or_else(|| "malformed_record".to_string()),
                ],
                candidate_count: 0,
            });
        }
    }
    Ok(RemitInput {
        records,
        pre_reviews,
        total,
        invalid,
    })
}

fn current_revisions(records: &[ClaimLine]) -> Vec<ClaimLine> {
    let mut maximums: HashMap<&str, u32> = HashMap::new();
    for claim in records {
        maximums
            .entry(&claim.claim_line_id)
            .and_modify(|revision| *revision = (*revision).max(claim.revision))
            .or_insert(claim.revision);
    }
    let mut current = records
        .iter()
        .filter(|claim| maximums.get(claim.claim_line_id.as_str()) == Some(&claim.revision))
        .cloned()
        .collect::<Vec<_>>();
    current.sort_by_key(ClaimLine::key);
    current
}

fn resolve_components(
    claims: &[ClaimLine],
    remits: &[RemittanceLine],
    candidate_lists: &[Vec<Candidate>],
    identity_sources_disagree: &[bool],
    accepted: &mut Vec<AcceptedLink>,
    reviews: &mut Vec<ReviewRecord>,
) {
    let mut claim_to_remits = vec![Vec::new(); claims.len()];
    for (remit_idx, candidates) in candidate_lists.iter().enumerate() {
        for candidate in candidates {
            claim_to_remits[candidate.claim_idx].push(remit_idx);
        }
    }
    let mut visited = vec![false; remits.len()];
    for start in 0..remits.len() {
        if visited[start] {
            continue;
        }
        if candidate_lists[start].is_empty() {
            visited[start] = true;
            reviews.push(ReviewRecord {
                remittance_line_id: remits[start].remittance_line_id.clone(),
                physical_record_ordinal: 1,
                reason_codes: reason_for_no_assignment(
                    &remits[start],
                    &[],
                    claims,
                    identity_sources_disagree[start],
                ),
                candidate_count: 0,
            });
            continue;
        }

        let mut queue = VecDeque::from([start]);
        let mut component_remits = BTreeSet::new();
        let mut component_claims = BTreeSet::new();
        while let Some(remit_idx) = queue.pop_front() {
            if !component_remits.insert(remit_idx) {
                continue;
            }
            visited[remit_idx] = true;
            for candidate in &candidate_lists[remit_idx] {
                if component_claims.insert(candidate.claim_idx) {
                    for neighbor in &claim_to_remits[candidate.claim_idx] {
                        if !component_remits.contains(neighbor) {
                            queue.push_back(*neighbor);
                        }
                    }
                }
            }
        }
        let component_remits = component_remits.into_iter().collect::<Vec<_>>();
        if component_remits.len() + component_claims.len() > MAX_CLUSTER_NODES {
            for remit_idx in component_remits {
                reviews.push(ReviewRecord {
                    remittance_line_id: remits[remit_idx].remittance_line_id.clone(),
                    physical_record_ordinal: 1,
                    reason_codes: vec!["unsupported_bundle".to_string()],
                    candidate_count: candidate_lists[remit_idx].len(),
                });
            }
            continue;
        }
        resolve_component(
            claims,
            remits,
            candidate_lists,
            identity_sources_disagree,
            &component_remits,
            accepted,
            reviews,
        );
    }
}

fn resolve_component(
    claims: &[ClaimLine],
    remits: &[RemittanceLine],
    candidate_lists: &[Vec<Candidate>],
    identity_sources_disagree: &[bool],
    component_remits: &[usize],
    accepted: &mut Vec<AcceptedLink>,
    reviews: &mut Vec<ReviewRecord>,
) {
    let mut ordered_remits = component_remits.to_vec();
    ordered_remits.sort_by(|left, right| {
        remits[*left]
            .remittance_line_id
            .cmp(&remits[*right].remittance_line_id)
    });
    let options = ordered_remits
        .iter()
        .map(|remit_idx| {
            assignment_options(&remits[*remit_idx], &candidate_lists[*remit_idx], claims)
        })
        .collect::<Vec<_>>();
    let mut best = SearchBest {
        assignments: vec![None; ordered_remits.len()],
        ..SearchBest::default()
    };
    let mut working = vec![None; ordered_remits.len()];
    let mut positive = vec![0_i64; claims.len()];
    let mut negative = vec![0_i64; claims.len()];
    let mut remaining_nodes = MAX_SEARCH_NODES;
    let mut exhausted = false;
    search_assignments(
        0,
        &options,
        claims,
        &mut working,
        &mut positive,
        &mut negative,
        Objective {
            accepted: 0,
            score: 0,
        },
        &mut best,
        &mut remaining_nodes,
        &mut exhausted,
    );

    if exhausted {
        for remit_idx in ordered_remits {
            reviews.push(ReviewRecord {
                remittance_line_id: remits[remit_idx].remittance_line_id.clone(),
                physical_record_ordinal: 1,
                reason_codes: vec!["search_budget_exhausted".to_string()],
                candidate_count: candidate_lists[remit_idx].len(),
            });
        }
        return;
    }

    if best.tied {
        for remit_idx in ordered_remits {
            reviews.push(ReviewRecord {
                remittance_line_id: remits[remit_idx].remittance_line_id.clone(),
                physical_record_ordinal: 1,
                reason_codes: vec!["conflicting_strong_candidates".to_string()],
                candidate_count: candidate_lists[remit_idx].len(),
            });
        }
        return;
    }

    for (position, remit_idx) in ordered_remits.iter().enumerate() {
        let remit = &remits[*remit_idx];
        let candidates = &candidate_lists[*remit_idx];
        if let Some(option) = &best.assignments[position] {
            let chosen = option
                .allocations
                .iter()
                .map(|allocation| allocation.claim_idx)
                .collect::<Vec<_>>();
            let rejected_competitors =
                competitors(candidates, &chosen, claims, "lower_global_score");
            for allocation in &option.allocations {
                let claim = &claims[allocation.claim_idx];
                accepted.push(AcceptedLink {
                    remittance_line_id: remit.remittance_line_id.clone(),
                    claim_line_id: claim.claim_line_id.clone(),
                    claim_revision: claim.revision,
                    applied_cents: allocation.applied_cents,
                    score: allocation.score,
                    facts: allocation.facts.clone(),
                    rejected_competitors: rejected_competitors.clone(),
                });
            }
        } else {
            reviews.push(ReviewRecord {
                remittance_line_id: remit.remittance_line_id.clone(),
                physical_record_ordinal: 1,
                reason_codes: reason_for_no_assignment(
                    remit,
                    candidates,
                    claims,
                    identity_sources_disagree[*remit_idx],
                ),
                candidate_count: candidates.len(),
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn search_assignments(
    position: usize,
    options: &[Vec<AssignmentOption>],
    claims: &[ClaimLine],
    working: &mut [Option<AssignmentOption>],
    positive: &mut [i64],
    negative: &mut [i64],
    objective: Objective,
    best: &mut SearchBest,
    remaining_nodes: &mut usize,
    exhausted: &mut bool,
) {
    if *remaining_nodes == 0 {
        *exhausted = true;
        return;
    }
    *remaining_nodes -= 1;
    if position == options.len() {
        let valid_net = positive.iter().zip(negative.iter()).zip(claims).all(
            |((positive, negative), claim)| {
                positive >= negative && positive - negative <= claim.open_balance_cents
            },
        );
        if !valid_net {
            return;
        }
        match best.objective {
            None => {
                best.objective = Some(objective);
                best.assignments.clone_from_slice(working);
            }
            Some(current) if objective > current => {
                best.objective = Some(objective);
                best.assignments.clone_from_slice(working);
                best.tied = false;
            }
            Some(current) if objective == current && best.assignments != working => {
                best.tied = true;
            }
            _ => {}
        }
        return;
    }

    for option in &options[position] {
        if apply_option(option, claims, positive, negative) {
            working[position] = Some(option.clone());
            search_assignments(
                position + 1,
                options,
                claims,
                working,
                positive,
                negative,
                Objective {
                    accepted: objective.accepted + 1,
                    score: objective.score + i64::from(option.score),
                },
                best,
                remaining_nodes,
                exhausted,
            );
            undo_option(option, positive, negative);
            if *exhausted {
                return;
            }
        }
    }
    working[position] = None;
    search_assignments(
        position + 1,
        options,
        claims,
        working,
        positive,
        negative,
        objective,
        best,
        remaining_nodes,
        exhausted,
    );
}

fn apply_option(
    option: &AssignmentOption,
    claims: &[ClaimLine],
    positive: &mut [i64],
    negative: &mut [i64],
) -> bool {
    for (applied, allocation) in option.allocations.iter().enumerate() {
        if allocation.applied_cents > 0 {
            positive[allocation.claim_idx] += allocation.applied_cents;
            if positive[allocation.claim_idx] > claims[allocation.claim_idx].open_balance_cents {
                rollback_allocations(&option.allocations[..=applied], positive, negative);
                return false;
            }
        } else {
            negative[allocation.claim_idx] += allocation.applied_cents.unsigned_abs() as i64;
            if negative[allocation.claim_idx] > claims[allocation.claim_idx].open_balance_cents {
                rollback_allocations(&option.allocations[..=applied], positive, negative);
                return false;
            }
        }
    }
    true
}

fn undo_option(option: &AssignmentOption, positive: &mut [i64], negative: &mut [i64]) {
    rollback_allocations(&option.allocations, positive, negative);
}

fn rollback_allocations(allocations: &[Allocation], positive: &mut [i64], negative: &mut [i64]) {
    for allocation in allocations {
        if allocation.applied_cents > 0 {
            positive[allocation.claim_idx] -= allocation.applied_cents;
        } else {
            negative[allocation.claim_idx] -= allocation.applied_cents.unsigned_abs() as i64;
        }
    }
}

fn assignment_options(
    remit: &RemittanceLine,
    candidates: &[Candidate],
    claims: &[ClaimLine],
) -> Vec<AssignmentOption> {
    let signed = remit.signed_amount();
    let amount = signed.unsigned_abs();
    let mut options = Vec::new();
    for candidate in candidates {
        let claim = &claims[candidate.claim_idx];
        let has_reference = candidate
            .facts
            .iter()
            .any(|fact| fact == "insurer_reference");
        if amount <= claim.open_balance_cents.unsigned_abs()
            && (!matches!(remit.transaction_kind, TransactionKind::Reversal) || has_reference)
        {
            options.push(AssignmentOption {
                allocations: vec![Allocation {
                    claim_idx: candidate.claim_idx,
                    applied_cents: signed,
                    score: candidate.score,
                    facts: candidate.facts.clone(),
                }],
                score: candidate.score,
            });
        }
    }

    if signed > 0 && candidates.len() <= MAX_SPLIT_CANDIDATES {
        let mut by_claim: BTreeMap<&str, Vec<&Candidate>> = BTreeMap::new();
        for candidate in candidates {
            by_claim
                .entry(&claims[candidate.claim_idx].claim_id)
                .or_default()
                .push(candidate);
        }
        for same_claim_candidates in by_claim.values() {
            let mut selected = Vec::new();
            enumerate_split_options(
                0,
                amount,
                same_claim_candidates,
                claims,
                &mut selected,
                &mut options,
            );
        }
    }

    options.sort_by_key(|option| option_signature(option, claims));
    options.dedup();
    options
}

fn enumerate_split_options(
    position: usize,
    remaining: u64,
    candidates: &[&Candidate],
    claims: &[ClaimLine],
    selected: &mut Vec<usize>,
    options: &mut Vec<AssignmentOption>,
) {
    if remaining == 0 {
        if selected.len() >= 2 {
            let allocations = selected
                .iter()
                .map(|position| {
                    let candidate = candidates[*position];
                    let claim = &claims[candidate.claim_idx];
                    Allocation {
                        claim_idx: candidate.claim_idx,
                        applied_cents: claim.open_balance_cents,
                        score: candidate.score,
                        facts: candidate.facts.clone(),
                    }
                })
                .collect::<Vec<_>>();
            options.push(AssignmentOption {
                score: allocations
                    .iter()
                    .map(|allocation| allocation.score)
                    .sum::<i32>()
                    + 25,
                allocations,
            });
        }
        return;
    }
    if position == candidates.len() {
        return;
    }
    enumerate_split_options(
        position + 1,
        remaining,
        candidates,
        claims,
        selected,
        options,
    );
    let candidate = candidates[position];
    let capacity = claims[candidate.claim_idx]
        .open_balance_cents
        .unsigned_abs();
    if capacity <= remaining {
        selected.push(position);
        enumerate_split_options(
            position + 1,
            remaining - capacity,
            candidates,
            claims,
            selected,
            options,
        );
        selected.pop();
    }
}

fn option_signature(option: &AssignmentOption, claims: &[ClaimLine]) -> Vec<(ClaimKey, i64)> {
    option
        .allocations
        .iter()
        .map(|allocation| (claims[allocation.claim_idx].key(), allocation.applied_cents))
        .collect()
}

fn competitors(
    candidates: &[Candidate],
    chosen: &[usize],
    claims: &[ClaimLine],
    reason: &str,
) -> Vec<RejectedCompetitor> {
    candidates
        .iter()
        .filter(|candidate| !chosen.contains(&candidate.claim_idx))
        .map(|candidate| {
            let claim = &claims[candidate.claim_idx];
            RejectedCompetitor {
                claim_line_id: claim.claim_line_id.clone(),
                claim_revision: claim.revision,
                reason_codes: vec![reason.to_string()],
            }
        })
        .collect()
}

fn reason_for_no_assignment(
    remit: &RemittanceLine,
    candidates: &[Candidate],
    claims: &[ClaimLine],
    identity_sources_disagree: bool,
) -> Vec<String> {
    if identity_sources_disagree {
        return vec!["conflicting_identity_sources".to_string()];
    }
    if matches!(remit.transaction_kind, TransactionKind::Reversal) {
        return vec!["unsupported_standalone_reversal".to_string()];
    }
    if remit.insurer_reference.is_none()
        && (remit.patient_key.is_none()
            || remit.service_date_start.is_none()
            || remit.service_date_end.is_none()
            || remit.procedure_code.is_none())
    {
        return vec!["missing_identity".to_string()];
    }
    if candidates.is_empty() {
        return vec!["no_plausible_candidate".to_string()];
    }
    let amount = remit.signed_amount().unsigned_abs();
    let total_capacity = candidates.iter().fold(0_u64, |total, candidate| {
        total.saturating_add(
            claims[candidate.claim_idx]
                .open_balance_cents
                .unsigned_abs(),
        )
    });
    let distinct_claims = candidates
        .iter()
        .map(|candidate| claims[candidate.claim_idx].claim_id.as_str())
        .collect::<HashSet<_>>();
    if total_capacity == amount && distinct_claims.len() > 1 {
        return vec!["unsupported_bundle".to_string()];
    }
    if candidates.len() >= 2 && distinct_claims.len() == 1 && total_capacity >= amount {
        return vec!["unsupported_partial_split".to_string()];
    }
    vec!["amount_inconsistent".to_string()]
}

#[allow(clippy::too_many_arguments)]
fn write_results(
    output_dir: &Path,
    mut accepted: Vec<AcceptedLink>,
    mut reviews: Vec<ReviewRecord>,
    input_claim_records: usize,
    valid_current_claim_lines: usize,
    invalid_claim_records: usize,
    input_remittance_records: usize,
    invalid_remittance_records: usize,
) -> Result<Summary, AnyError> {
    fs::create_dir_all(output_dir)?;
    accepted.sort_by(|left, right| {
        left.remittance_line_id
            .cmp(&right.remittance_line_id)
            .then_with(|| left.claim_line_id.cmp(&right.claim_line_id))
            .then_with(|| left.claim_revision.cmp(&right.claim_revision))
            .then_with(|| left.applied_cents.cmp(&right.applied_cents))
    });
    reviews.sort_by(|left, right| {
        left.remittance_line_id
            .cmp(&right.remittance_line_id)
            .then_with(|| {
                left.physical_record_ordinal
                    .cmp(&right.physical_record_ordinal)
            })
    });

    write_jsonl(&output_dir.join("accepted.jsonl"), &accepted)?;
    write_jsonl(&output_dir.join("review.jsonl"), &reviews)?;
    let accepted_remittance_lines = accepted
        .iter()
        .map(|link| link.remittance_line_id.as_str())
        .collect::<HashSet<_>>()
        .len();
    let summary = Summary {
        input_claim_records,
        valid_current_claim_lines,
        invalid_claim_records,
        input_remittance_records,
        accepted_remittance_lines,
        accepted_links: accepted.len(),
        review_remittance_lines: reviews.len(),
        invalid_remittance_records,
    };
    let summary_file = File::create(output_dir.join("summary.json"))?;
    let mut writer = BufWriter::new(summary_file);
    serde_json::to_writer_pretty(&mut writer, &summary)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(summary)
}

fn write_jsonl<T: Serialize>(path: &Path, records: &[T]) -> Result<(), AnyError> {
    let mut writer = BufWriter::new(File::create(path)?);
    for record in records {
        serde_json::to_writer(&mut writer, record)?;
        writer.write_all(b"\n")?;
    }
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim(id: &str, claim_id: &str, balance: i64) -> ClaimLine {
        ClaimLine {
            claim_line_id: id.to_string(),
            claim_id: claim_id.to_string(),
            revision: 1,
            payer: "payer".to_string(),
            provider: "provider".to_string(),
            patient_key: "patient".to_string(),
            service_date: 20_000,
            procedure_code: "PROC".to_string(),
            billed_cents: balance,
            open_balance_cents: balance,
            insurer_references: vec!["REF".to_string()],
        }
    }

    fn remit(id: &str, amount: i64) -> RemittanceLine {
        RemittanceLine {
            remittance_line_id: id.to_string(),
            payer: "payer".to_string(),
            provider: "provider".to_string(),
            insurer_reference: Some("REF".to_string()),
            patient_key: Some("patient".to_string()),
            service_date_start: Some(20_000),
            service_date_end: Some(20_000),
            procedure_code: Some("PROC".to_string()),
            paid_cents: amount,
            adjustment_cents: 0,
            adjustment_codes: Vec::new(),
            transaction_kind: TransactionKind::Payment,
        }
    }

    #[test]
    fn split_requires_same_claim() {
        let claims = vec![claim("a", "one", 100), claim("b", "two", 150)];
        let candidates = ClaimIndex::new(&claims)
            .evaluate(&remit("r", 250), &claims)
            .candidates;
        assert!(assignment_options(&remit("r", 250), &candidates, &claims).is_empty());
    }

    #[test]
    fn split_is_available_within_one_claim() {
        let claims = vec![claim("a", "one", 100), claim("b", "one", 150)];
        let candidates = ClaimIndex::new(&claims)
            .evaluate(&remit("r", 250), &claims)
            .candidates;
        let options = assignment_options(&remit("r", 250), &candidates, &claims);
        assert_eq!(options.len(), 1);
        assert_eq!(options[0].allocations.len(), 2);
    }

    #[test]
    fn only_latest_revision_is_eligible() {
        let mut old = claim("line", "claim", 100);
        let mut current = old.clone();
        old.revision = 1;
        current.revision = 2;
        assert_eq!(current_revisions(&[old, current])[0].revision, 2);
    }

    #[test]
    fn dense_search_stops_at_the_budget() {
        let claims = (0..12)
            .map(|index| claim(&format!("line-{index}"), &format!("claim-{index}"), 100))
            .collect::<Vec<_>>();
        let one_remit_options = (0..12)
            .map(|claim_idx| AssignmentOption {
                allocations: vec![Allocation {
                    claim_idx,
                    applied_cents: 100,
                    score: 100,
                    facts: vec!["insurer_reference".to_string()],
                }],
                score: 100,
            })
            .collect::<Vec<_>>();
        let options = vec![one_remit_options; 12];
        let mut working = vec![None; 12];
        let mut positive = vec![0; 12];
        let mut negative = vec![0; 12];
        let mut best = SearchBest {
            assignments: vec![None; 12],
            ..SearchBest::default()
        };
        let mut remaining_nodes = 1_000;
        let mut exhausted = false;
        search_assignments(
            0,
            &options,
            &claims,
            &mut working,
            &mut positive,
            &mut negative,
            Objective {
                accepted: 0,
                score: 0,
            },
            &mut best,
            &mut remaining_nodes,
            &mut exhausted,
        );
        assert!(exhausted);
        assert_eq!(remaining_nodes, 0);
    }
}
