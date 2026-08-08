use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClaimLine {
    pub claim_line_id: String,
    pub claim_id: String,
    pub revision: u32,
    pub payer: String,
    pub provider: String,
    pub patient_key: String,
    pub service_date: i32,
    pub procedure_code: String,
    pub billed_cents: i64,
    pub open_balance_cents: i64,
    #[serde(default)]
    pub insurer_references: Vec<String>,
}

impl ClaimLine {
    pub fn key(&self) -> ClaimKey {
        ClaimKey {
            claim_line_id: self.claim_line_id.clone(),
            revision: self.revision,
        }
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.claim_line_id.is_empty()
            || self.claim_id.is_empty()
            || self.payer.is_empty()
            || self.provider.is_empty()
            || self.patient_key.is_empty()
            || self.procedure_code.is_empty()
        {
            return Err("missing_identity");
        }
        if self.billed_cents < 0
            || self.open_balance_cents < 0
            || self.open_balance_cents > self.billed_cents
        {
            return Err("amount_inconsistent");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransactionKind {
    Payment,
    Denial,
    Reversal,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemittanceLine {
    pub remittance_line_id: String,
    pub payer: String,
    pub provider: String,
    pub insurer_reference: Option<String>,
    pub patient_key: Option<String>,
    pub service_date_start: Option<i32>,
    pub service_date_end: Option<i32>,
    pub procedure_code: Option<String>,
    pub paid_cents: i64,
    pub adjustment_cents: i64,
    #[serde(default)]
    pub adjustment_codes: Vec<String>,
    pub transaction_kind: TransactionKind,
}

impl RemittanceLine {
    pub fn signed_amount(&self) -> i64 {
        self.paid_cents
            .checked_add(self.adjustment_cents)
            .expect("signed_amount is called only after validation")
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.remittance_line_id.is_empty() || self.payer.is_empty() || self.provider.is_empty() {
            return Err("missing_identity");
        }
        if let (Some(start), Some(end)) = (self.service_date_start, self.service_date_end) {
            if start > end {
                return Err("amount_inconsistent");
            }
            if end.saturating_sub(start) > 31 {
                return Err("unsupported_date_range");
            }
        }
        let Some(amount) = self.paid_cents.checked_add(self.adjustment_cents) else {
            return Err("amount_inconsistent");
        };
        match self.transaction_kind {
            TransactionKind::Payment | TransactionKind::Denial if amount <= 0 => {
                Err("amount_inconsistent")
            }
            TransactionKind::Reversal if amount >= 0 => Err("amount_inconsistent"),
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClaimKey {
    pub claim_line_id: String,
    pub revision: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RejectedCompetitor {
    pub claim_line_id: String,
    pub claim_revision: u32,
    pub reason_codes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AcceptedLink {
    pub remittance_line_id: String,
    pub claim_line_id: String,
    pub claim_revision: u32,
    pub applied_cents: i64,
    pub score: i32,
    pub facts: Vec<String>,
    pub rejected_competitors: Vec<RejectedCompetitor>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewRecord {
    pub remittance_line_id: String,
    pub physical_record_ordinal: usize,
    pub reason_codes: Vec<String>,
    pub candidate_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Summary {
    pub input_claim_records: usize,
    pub valid_current_claim_lines: usize,
    pub invalid_claim_records: usize,
    pub input_remittance_records: usize,
    pub accepted_remittance_lines: usize,
    pub accepted_links: usize,
    pub review_remittance_lines: usize,
    pub invalid_remittance_records: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct ExpectedLink {
    pub remittance_line_id: String,
    pub claim_line_id: String,
    pub claim_revision: u32,
    pub applied_cents: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GroundTruthRecord {
    pub remittance_line_id: String,
    pub unambiguous: bool,
    #[serde(default)]
    pub links: Vec<ExpectedLink>,
}
