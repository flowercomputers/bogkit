use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::Write as _;
use std::ops::Bound;
use std::path::Path;

pub const MINUTE_MS: u64 = 60_000;
pub const MAX_LATENESS_MS: u64 = 15 * MINUTE_MS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Currency([u8; 3]);

impl Currency {
    fn parse(value: &str) -> Result<Self, String> {
        let bytes: [u8; 3] = value
            .as_bytes()
            .try_into()
            .map_err(|_| format!("currency must contain three ASCII bytes: {value}"))?;
        if !bytes.iter().all(u8::is_ascii_uppercase) {
            return Err(format!("currency must be uppercase ASCII: {value}"));
        }
        Ok(Self(bytes))
    }

    fn as_str(self) -> &'static str {
        match &self.0 {
            b"USD" => "USD",
            b"EUR" => "EUR",
            b"GBP" => "GBP",
            _ => "UNK",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorizationOutcome {
    Approved,
    Declined,
}

impl AuthorizationOutcome {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "approved" => Ok(Self::Approved),
            "declined" => Ok(Self::Declined),
            _ => Err(format!("unknown authorization outcome: {value}")),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::Declined => "declined",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub event_id: u64,
    pub event_time_ms: u64,
    pub arrival_time_ms: u64,
    pub account_id: u64,
    pub salted_card_fingerprint: u64,
    pub device_id: u64,
    pub ip_prefix: u64,
    pub merchant_event_id: u64,
    pub amount_minor: i64,
    pub currency: Currency,
    pub authorization_outcome: AuthorizationOutcome,
}

impl Event {
    fn parse_tsv(line: &str, line_number: usize) -> Result<Self, String> {
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() != 11 {
            return Err(format!(
                "line {line_number}: expected 11 tab-separated fields, got {}",
                fields.len()
            ));
        }
        let number = |index: usize, name: &str| {
            fields[index]
                .parse::<u64>()
                .map_err(|error| format!("line {line_number}: invalid {name}: {error}"))
        };
        let amount_minor = fields[8]
            .parse::<i64>()
            .map_err(|error| format!("line {line_number}: invalid amount_minor: {error}"))?;
        if amount_minor < 0 {
            return Err(format!(
                "line {line_number}: amount_minor must be non-negative"
            ));
        }
        Ok(Self {
            event_id: number(0, "event_id")?,
            event_time_ms: number(1, "event_time_ms")?,
            arrival_time_ms: number(2, "arrival_time_ms")?,
            account_id: number(3, "account_id")?,
            salted_card_fingerprint: number(4, "salted_card_fingerprint")?,
            device_id: number(5, "device_id")?,
            ip_prefix: number(6, "ip_prefix")?,
            merchant_event_id: number(7, "merchant_event_id")?,
            amount_minor,
            currency: Currency::parse(fields[9])?,
            authorization_outcome: AuthorizationOutcome::parse(fields[10])?,
        })
    }

    fn to_tsv(&self) -> String {
        format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            self.event_id,
            self.event_time_ms,
            self.arrival_time_ms,
            self.account_id,
            self.salted_card_fingerprint,
            self.device_id,
            self.ip_prefix,
            self.merchant_event_id,
            self.amount_minor,
            self.currency.as_str(),
            self.authorization_outcome.as_str()
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct EventOrder {
    event_time_ms: u64,
    event_id: u64,
}

impl From<&Event> for EventOrder {
    fn from(event: &Event) -> Self {
        Self {
            event_time_ms: event.event_time_ms,
            event_id: event.event_id,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum KeyKind {
    Account,
    Card,
    Device,
    IpPrefix,
}

impl KeyKind {
    fn name(self) -> &'static str {
        match self {
            Self::Account => "account",
            Self::Card => "card",
            Self::Device => "device",
            Self::IpPrefix => "ip-prefix",
        }
    }

    fn value(self, event: &Event) -> u64 {
        match self {
            Self::Account => event.account_id,
            Self::Card => event.salted_card_fingerprint,
            Self::Device => event.device_id,
            Self::IpPrefix => event.ip_prefix,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Rule {
    id: &'static str,
    key_kind: KeyKind,
    window_ms: u64,
    minimum_count: u32,
    minimum_amount_minor: i64,
}

const RULES: [Rule; 4] = [
    Rule {
        id: "account-1m-count-3",
        key_kind: KeyKind::Account,
        window_ms: MINUTE_MS,
        minimum_count: 3,
        minimum_amount_minor: 0,
    },
    Rule {
        id: "card-10m-amount-100000",
        key_kind: KeyKind::Card,
        window_ms: 10 * MINUTE_MS,
        minimum_count: 2,
        minimum_amount_minor: 100_000,
    },
    Rule {
        id: "device-10m-count-4",
        key_kind: KeyKind::Device,
        window_ms: 10 * MINUTE_MS,
        minimum_count: 4,
        minimum_amount_minor: 0,
    },
    Rule {
        id: "ip-prefix-24h-count-6",
        key_kind: KeyKind::IpPrefix,
        window_ms: 24 * 60 * MINUTE_MS,
        minimum_count: 6,
        minimum_amount_minor: 0,
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct IndexKey {
    kind: KeyKind,
    value: u64,
    currency: Currency,
}

impl IndexKey {
    fn new(rule: Rule, event: &Event) -> Self {
        Self {
            kind: rule.key_kind,
            value: rule.key_kind.value(event),
            currency: event.currency,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleOutcome {
    rule_id: &'static str,
    key_kind: KeyKind,
    window_ms: u64,
    count: u32,
    amount_minor: i64,
    currency: Currency,
    alert: bool,
    contributing_event_ids: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LatestDecision {
    revision: u32,
    outcomes: Vec<RuleOutcome>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionRecord {
    event_id: u64,
    revision: u32,
    correction_of_revision: Option<u32>,
    outcomes: Vec<RuleOutcome>,
}

impl DecisionRecord {
    fn append_canonical(&self, output: &mut String) {
        let record_kind = if self.correction_of_revision.is_some() {
            'C'
        } else {
            'I'
        };
        write!(output, "{record_kind}|{}|{}|", self.event_id, self.revision)
            .expect("writing to String cannot fail");
        match self.correction_of_revision {
            Some(revision) => {
                write!(output, "{}:{revision}", self.event_id)
                    .expect("writing to String cannot fail");
            }
            None => output.push('-'),
        }
        for outcome in &self.outcomes {
            write!(
                output,
                "|{}:{}:{}:{}:{}:{}:{}:",
                outcome.rule_id,
                outcome.key_kind.name(),
                outcome.window_ms,
                outcome.count,
                outcome.amount_minor,
                outcome.currency.as_str(),
                u8::from(outcome.alert)
            )
            .expect("writing to String cannot fail");
            for (index, event_id) in outcome.contributing_event_ids.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                write!(output, "{event_id}").expect("writing to String cannot fail");
            }
        }
        output.push('\n');
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestResult {
    Accepted { correction_count: usize },
    ExactDuplicate,
    ConflictingDuplicate,
}

#[derive(Debug)]
pub struct DeletionReceipt {
    account_id: u64,
    event_ids: BTreeSet<u64>,
    card_fingerprints: BTreeSet<u64>,
}

impl DeletionReceipt {
    pub fn event_count(&self) -> usize {
        self.event_ids.len()
    }
}

#[derive(Default)]
pub struct Engine {
    events: BTreeMap<EventOrder, Event>,
    event_order_by_id: HashMap<u64, EventOrder>,
    event_id_by_merchant_id: HashMap<u64, u64>,
    indexes: HashMap<IndexKey, BTreeMap<EventOrder, i64>>,
    latest: HashMap<u64, LatestDecision>,
    records: Vec<DecisionRecord>,
}

impl Engine {
    pub fn ingest(&mut self, event: Event) -> IngestResult {
        if let Some(order) = self.event_order_by_id.get(&event.event_id) {
            let existing = self
                .events
                .get(order)
                .expect("event ID index must point to an event");
            return if existing == &event
                || (existing.merchant_event_id == event.merchant_event_id
                    && existing.event_time_ms == event.event_time_ms
                    && existing.account_id == event.account_id
                    && existing.salted_card_fingerprint == event.salted_card_fingerprint
                    && existing.device_id == event.device_id
                    && existing.ip_prefix == event.ip_prefix
                    && existing.amount_minor == event.amount_minor
                    && existing.currency == event.currency
                    && existing.authorization_outcome == event.authorization_outcome)
            {
                IngestResult::ExactDuplicate
            } else {
                IngestResult::ConflictingDuplicate
            };
        }
        if let Some(existing_event_id) = self.event_id_by_merchant_id.get(&event.merchant_event_id)
        {
            return if self
                .events
                .get(
                    self.event_order_by_id
                        .get(existing_event_id)
                        .expect("merchant ID index must point to an event ID"),
                )
                .is_some_and(|existing| {
                    existing.event_time_ms == event.event_time_ms
                        && existing.account_id == event.account_id
                        && existing.salted_card_fingerprint == event.salted_card_fingerprint
                        && existing.device_id == event.device_id
                        && existing.ip_prefix == event.ip_prefix
                        && existing.amount_minor == event.amount_minor
                        && existing.currency == event.currency
                        && existing.authorization_outcome == event.authorization_outcome
                }) {
                IngestResult::ExactDuplicate
            } else {
                IngestResult::ConflictingDuplicate
            };
        }

        let order = EventOrder::from(&event);
        let mut affected = BTreeSet::new();
        for rule in RULES {
            let key = IndexKey::new(rule, &event);
            if key.value == 0 {
                continue;
            }
            if let Some(index) = self.indexes.get(&key) {
                let end_time = event.event_time_ms.saturating_add(rule.window_ms);
                let end = EventOrder {
                    event_time_ms: end_time,
                    event_id: 0,
                };
                for (later_order, _) in index.range((Bound::Excluded(order), Bound::Excluded(end)))
                {
                    affected.insert(*later_order);
                }
            }
        }

        self.event_order_by_id.insert(event.event_id, order);
        self.event_id_by_merchant_id
            .insert(event.merchant_event_id, event.event_id);
        for rule in RULES {
            let key = IndexKey::new(rule, &event);
            if key.value != 0 {
                self.indexes
                    .entry(key)
                    .or_default()
                    .insert(order, event.amount_minor);
            }
        }
        let event_id = event.event_id;
        self.events.insert(order, event);

        let outcomes = self.outcomes_from_indexes(order);
        self.latest.insert(
            event_id,
            LatestDecision {
                revision: 0,
                outcomes: outcomes.clone(),
            },
        );
        self.records.push(DecisionRecord {
            event_id,
            revision: 0,
            correction_of_revision: None,
            outcomes,
        });

        let mut correction_count = 0;
        for target_order in affected {
            let target_id = target_order.event_id;
            let corrected = self.outcomes_from_indexes(target_order);
            let latest = self
                .latest
                .get_mut(&target_id)
                .expect("affected event must already have a decision");
            if latest.outcomes != corrected {
                let previous_revision = latest.revision;
                latest.revision += 1;
                latest.outcomes = corrected.clone();
                self.records.push(DecisionRecord {
                    event_id: target_id,
                    revision: latest.revision,
                    correction_of_revision: Some(previous_revision),
                    outcomes: corrected,
                });
                correction_count += 1;
            }
        }

        IngestResult::Accepted { correction_count }
    }

    fn outcomes_from_indexes(&self, target_order: EventOrder) -> Vec<RuleOutcome> {
        let target = self
            .events
            .get(&target_order)
            .expect("target order must point to an event");
        RULES
            .into_iter()
            .map(|rule| {
                let key = IndexKey::new(rule, target);
                let mut count = 0_u32;
                let mut amount_minor = 0_i64;
                let mut contributing_event_ids = Vec::new();
                if key.value != 0
                    && let Some(index) = self.indexes.get(&key)
                {
                    let lower = lower_bound(target_order.event_time_ms, rule.window_ms);
                    for (order, amount) in index.range((lower, Bound::Included(target_order))) {
                        count = count.saturating_add(1);
                        amount_minor = amount_minor.saturating_add(*amount);
                        contributing_event_ids.push(order.event_id);
                    }
                }
                let alert =
                    count >= rule.minimum_count && amount_minor >= rule.minimum_amount_minor;
                if !alert {
                    contributing_event_ids.clear();
                }
                RuleOutcome {
                    rule_id: rule.id,
                    key_kind: rule.key_kind,
                    window_ms: rule.window_ms,
                    count,
                    amount_minor,
                    currency: target.currency,
                    alert,
                    contributing_event_ids,
                }
            })
            .collect()
    }

    fn outcomes_from_naive_scan(&self, target_order: EventOrder) -> Vec<RuleOutcome> {
        let target = self
            .events
            .get(&target_order)
            .expect("target order must point to an event");
        RULES
            .into_iter()
            .map(|rule| {
                let target_key = rule.key_kind.value(target);
                let mut contributing_event_ids = Vec::new();
                let mut amount_minor = 0_i64;
                for (order, candidate) in self.events.range(..=target_order) {
                    if candidate.currency == target.currency
                        && rule.key_kind.value(candidate) == target_key
                        && target_key != 0
                        && in_window(
                            order.event_time_ms,
                            target_order.event_time_ms,
                            rule.window_ms,
                        )
                    {
                        contributing_event_ids.push(candidate.event_id);
                        amount_minor = amount_minor.saturating_add(candidate.amount_minor);
                    }
                }
                let count = u32::try_from(contributing_event_ids.len()).unwrap_or(u32::MAX);
                let alert =
                    count >= rule.minimum_count && amount_minor >= rule.minimum_amount_minor;
                if !alert {
                    contributing_event_ids.clear();
                }
                RuleOutcome {
                    rule_id: rule.id,
                    key_kind: rule.key_kind,
                    window_ms: rule.window_ms,
                    count,
                    amount_minor,
                    currency: target.currency,
                    alert,
                    contributing_event_ids,
                }
            })
            .collect()
    }

    pub fn assert_matches_reference(&self) -> Result<(), String> {
        for order in self.events.keys().copied() {
            let target_id = order.event_id;
            let optimized = &self
                .latest
                .get(&target_id)
                .ok_or_else(|| format!("event {target_id} has no latest decision"))?
                .outcomes;
            let reference = self.outcomes_from_naive_scan(order);
            if optimized != &reference {
                return Err(format!(
                    "event {target_id} differs from the independent naive reference"
                ));
            }
        }
        Ok(())
    }

    pub fn decision_bytes(&self) -> Vec<u8> {
        let mut output = String::new();
        for record in &self.records {
            record.append_canonical(&mut output);
        }
        output.into_bytes()
    }

    pub fn record_count(&self) -> usize {
        self.records.len()
    }

    pub fn event_count(&self) -> usize {
        self.events.len()
    }

    pub fn alert_count(&self) -> usize {
        self.records
            .iter()
            .flat_map(|record| &record.outcomes)
            .filter(|outcome| outcome.alert)
            .count()
    }

    pub fn correction_count(&self) -> usize {
        self.records
            .iter()
            .filter(|record| record.correction_of_revision.is_some())
            .count()
    }

    pub fn digest(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.decision_bytes().hash(&mut hasher);
        hasher.finish()
    }

    pub fn latest_rule_count(&self, event_id: u64, rule_id: &str) -> Option<u32> {
        self.latest.get(&event_id).and_then(|decision| {
            decision
                .outcomes
                .iter()
                .find(|outcome| outcome.rule_id == rule_id)
                .map(|outcome| outcome.count)
        })
    }

    pub fn delete_customer(&mut self, account_id: u64) -> Result<DeletionReceipt, String> {
        let deleted_orders: Vec<_> = self
            .events
            .iter()
            .filter_map(|(order, event)| (event.account_id == account_id).then_some(*order))
            .collect();
        let deleted_ids: BTreeSet<_> = deleted_orders.iter().map(|order| order.event_id).collect();
        let deleted_cards: BTreeSet<_> = deleted_orders
            .iter()
            .filter_map(|order| self.events.get(order))
            .map(|event| event.salted_card_fingerprint)
            .collect();
        let receipt = DeletionReceipt {
            account_id,
            event_ids: deleted_ids.clone(),
            card_fingerprints: deleted_cards,
        };
        if deleted_ids.is_empty() {
            return Ok(receipt);
        }

        for order in &deleted_orders {
            let event = self
                .events
                .get(order)
                .expect("deletion order must point to event")
                .clone();
            for rule in RULES {
                if matches!(rule.key_kind, KeyKind::Account | KeyKind::Card) {
                    let key = IndexKey::new(rule, &event);
                    if let Some(index) = self.indexes.get_mut(&key) {
                        index.remove(order);
                    }
                }
            }
            let event = self
                .events
                .get_mut(order)
                .expect("deletion order must still point to event");
            event.account_id = 0;
            event.salted_card_fingerprint = 0;
        }
        self.indexes.retain(|_, index| !index.is_empty());

        self.latest
            .retain(|event_id, _| !deleted_ids.contains(event_id));
        self.records
            .retain(|record| !deleted_ids.contains(&record.event_id));
        for record in &mut self.records {
            record.outcomes.retain(|outcome| {
                !matches!(outcome.key_kind, KeyKind::Account | KeyKind::Card)
                    || !outcome
                        .contributing_event_ids
                        .iter()
                        .any(|event_id| deleted_ids.contains(event_id))
            });
        }

        let retained_orders: Vec<_> = self
            .events
            .keys()
            .copied()
            .filter(|order| !deleted_ids.contains(&order.event_id))
            .collect();
        for order in retained_orders {
            let corrected = self.outcomes_from_indexes(order);
            let latest = self
                .latest
                .get_mut(&order.event_id)
                .expect("retained event must have a latest decision");
            if latest.outcomes != corrected {
                let previous_revision = latest.revision;
                latest.revision += 1;
                latest.outcomes = corrected.clone();
                self.records.push(DecisionRecord {
                    event_id: order.event_id,
                    revision: latest.revision,
                    correction_of_revision: Some(previous_revision),
                    outcomes: corrected,
                });
            }
        }

        self.scan_customer(&receipt)?;
        self.reconstruct_all_alerts()?;
        Ok(receipt)
    }

    pub fn scan_customer(&self, receipt: &DeletionReceipt) -> Result<(), String> {
        if self
            .events
            .values()
            .any(|event| event.account_id == receipt.account_id)
        {
            return Err(format!(
                "account {} remains in retained events",
                receipt.account_id
            ));
        }
        if self
            .indexes
            .keys()
            .any(|key| key.kind == KeyKind::Account && key.value == receipt.account_id)
        {
            return Err(format!(
                "account {} remains in an account index",
                receipt.account_id
            ));
        }
        for event_id in &receipt.event_ids {
            let order = self
                .event_order_by_id
                .get(event_id)
                .ok_or_else(|| format!("deleted event {event_id} lost its audit row"))?;
            let event = self
                .events
                .get(order)
                .ok_or_else(|| format!("deleted event {event_id} lost its retained record"))?;
            if event.account_id != 0 || event.salted_card_fingerprint != 0 {
                return Err(format!(
                    "deleted event {event_id} retains an account or card identifier"
                ));
            }
            for (key, index) in &self.indexes {
                if matches!(key.kind, KeyKind::Account | KeyKind::Card)
                    && (key.kind == KeyKind::Account
                        || receipt.card_fingerprints.contains(&key.value))
                    && index.contains_key(order)
                {
                    return Err(format!(
                        "deleted event {event_id} remains in a {} index",
                        key.kind.name()
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn reconstruct_all_alerts(&self) -> Result<(), String> {
        for record in &self.records {
            let target_order = self
                .event_order_by_id
                .get(&record.event_id)
                .ok_or_else(|| format!("missing target event {}", record.event_id))?;
            let target = self
                .events
                .get(target_order)
                .ok_or_else(|| format!("missing target record {}", record.event_id))?;
            for outcome in &record.outcomes {
                if !outcome.alert {
                    continue;
                }
                let rule = RULES
                    .into_iter()
                    .find(|rule| rule.id == outcome.rule_id)
                    .ok_or_else(|| format!("unknown retained rule {}", outcome.rule_id))?;
                let target_key = rule.key_kind.value(target);
                let mut amount_minor = 0_i64;
                for event_id in &outcome.contributing_event_ids {
                    let contribution_order = self
                        .event_order_by_id
                        .get(event_id)
                        .ok_or_else(|| format!("missing contribution event {event_id}"))?;
                    let contribution = self.events.get(contribution_order).ok_or_else(|| {
                        format!("missing retained contribution record {event_id}")
                    })?;
                    if *contribution_order > *target_order
                        || contribution.currency != outcome.currency
                        || rule.key_kind.value(contribution) != target_key
                        || !in_window(
                            contribution.event_time_ms,
                            target.event_time_ms,
                            rule.window_ms,
                        )
                    {
                        return Err(format!(
                            "event {event_id} cannot reconstruct {} for target {}",
                            rule.id, record.event_id
                        ));
                    }
                    amount_minor = amount_minor.saturating_add(contribution.amount_minor);
                }
                let count = u32::try_from(outcome.contributing_event_ids.len()).unwrap_or(u32::MAX);
                if count != outcome.count || amount_minor != outcome.amount_minor {
                    return Err(format!(
                        "retained explanation mismatch for event {} rule {}",
                        record.event_id, rule.id
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn approximate_payload_bytes(&self) -> usize {
        let event_payload = self.events.len() * std::mem::size_of::<Event>();
        let index_rows: usize = self.indexes.values().map(BTreeMap::len).sum();
        let index_payload =
            index_rows * (std::mem::size_of::<EventOrder>() + std::mem::size_of::<i64>());
        event_payload + index_payload + self.decision_bytes().len()
    }
}

fn lower_bound(target_time_ms: u64, window_ms: u64) -> Bound<EventOrder> {
    if target_time_ms < window_ms {
        Bound::Unbounded
    } else {
        Bound::Excluded(EventOrder {
            event_time_ms: target_time_ms - window_ms,
            event_id: u64::MAX,
        })
    }
}

fn in_window(candidate_time_ms: u64, target_time_ms: u64, window_ms: u64) -> bool {
    candidate_time_ms <= target_time_ms
        && target_time_ms.saturating_sub(candidate_time_ms) < window_ms
}

pub fn load_fixture(path: &Path) -> Result<Vec<Event>, String> {
    let contents = std::fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let mut events = Vec::new();
    for (line_index, line) in contents.lines().enumerate() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        events.push(Event::parse_tsv(line, line_index + 1)?);
    }
    events.sort_by(|left, right| {
        left.arrival_time_ms
            .cmp(&right.arrival_time_ms)
            .then_with(|| left.event_id.cmp(&right.event_id))
    });
    Ok(events)
}

pub struct PersistentEngine {
    engine: Engine,
    ledger: File,
}

impl PersistentEngine {
    pub fn open(directory: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(directory)
            .map_err(|error| format!("failed to create {}: {error}", directory.display()))?;
        let ledger_path = directory.join("accepted-events.tsv");
        let mut engine = Engine::default();
        if ledger_path.exists() {
            for event in load_fixture(&ledger_path)? {
                if !matches!(engine.ingest(event), IngestResult::Accepted { .. }) {
                    return Err("persisted ledger contains a duplicate".to_string());
                }
            }
        }
        let ledger = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&ledger_path)
            .map_err(|error| format!("failed to open {}: {error}", ledger_path.display()))?;
        Ok(Self { engine, ledger })
    }

    pub fn ingest(&mut self, event: Event) -> Result<IngestResult, String> {
        let result = self.engine.ingest(event.clone());
        if matches!(result, IngestResult::Accepted { .. }) {
            self.ledger
                .write_all(event.to_tsv().as_bytes())
                .map_err(|error| format!("failed to append event ledger: {error}"))?;
            self.ledger
                .sync_data()
                .map_err(|error| format!("failed to sync event ledger: {error}"))?;
        }
        Ok(result)
    }

    pub fn decision_bytes(&self) -> Vec<u8> {
        self.engine.decision_bytes()
    }
}

#[derive(Default)]
pub struct RedisStyleBaseline {
    account: HashMap<u64, (u64, u64)>,
    card: HashMap<u64, (u64, u64)>,
}

impl RedisStyleBaseline {
    fn apply(&mut self, event: &Event) -> (u64, u64) {
        let account = increment_with_independent_ttl(
            &mut self.account,
            event.account_id,
            event.arrival_time_ms,
        );
        let card = increment_with_independent_ttl(
            &mut self.card,
            event.salted_card_fingerprint,
            event.arrival_time_ms,
        );
        (account, card)
    }
}

fn increment_with_independent_ttl(
    counters: &mut HashMap<u64, (u64, u64)>,
    key: u64,
    arrival_time_ms: u64,
) -> u64 {
    let entry = counters
        .entry(key)
        .or_insert((0, arrival_time_ms + MINUTE_MS));
    if arrival_time_ms >= entry.1 {
        *entry = (0, arrival_time_ms + MINUTE_MS);
    }
    entry.0 += 1;
    entry.0
}

pub fn baseline_observations() -> Vec<String> {
    let original = Event {
        event_id: 10,
        event_time_ms: 10_000,
        arrival_time_ms: 10_000,
        account_id: 7,
        salted_card_fingerprint: 9,
        device_id: 11,
        ip_prefix: 12,
        merchant_event_id: 1010,
        amount_minor: 10_000,
        currency: Currency(*b"USD"),
        authorization_outcome: AuthorizationOutcome::Approved,
    };
    let duplicate = Event {
        arrival_time_ms: 11_000,
        ..original.clone()
    };
    let late = Event {
        event_id: 11,
        event_time_ms: 20_000,
        arrival_time_ms: 80_000,
        merchant_event_id: 1011,
        ..original.clone()
    };
    let mut baseline = RedisStyleBaseline::default();
    let first = baseline.apply(&original);
    let after_duplicate = baseline.apply(&duplicate);
    let after_late = baseline.apply(&late);

    assert_eq!(first, (1, 1));
    assert_eq!(after_duplicate, (2, 2));
    assert_eq!(after_late, (1, 1));
    assert!(late.event_time_ms - original.event_time_ms < MINUTE_MS);

    vec![
        format!("duplicate event 10 changed both counts: {first:?} -> {after_duplicate:?}"),
        format!(
            "late event 11 belongs with event 10 in event time, but arrival-time TTL returned {after_late:?}"
        ),
        "no retained contribution list can explain either result".to_string(),
    ]
}

pub fn generated_event(unique_index: u64) -> Event {
    let nominal_time = unique_index.saturating_mul(40);
    let is_late = unique_index % 20 == 19;
    let delay = if is_late {
        ((unique_index.wrapping_mul(7_919) % MAX_LATENESS_MS) + 1).min(nominal_time)
    } else {
        0
    };
    Event {
        event_id: unique_index + 1,
        event_time_ms: nominal_time - delay,
        arrival_time_ms: nominal_time,
        account_id: 1 + unique_index % 100_000,
        salted_card_fingerprint: 1 + unique_index.wrapping_mul(17) % 200_000,
        device_id: 1 + unique_index.wrapping_mul(31) % 50_000,
        ip_prefix: 1 + unique_index.wrapping_mul(43) % 10_000,
        merchant_event_id: 1_000_000_000 + unique_index,
        amount_minor: 1_000 + i64::try_from(unique_index % 90_000).unwrap_or(0),
        currency: Currency(*b"USD"),
        authorization_outcome: if unique_index.is_multiple_of(10) {
            AuthorizationOutcome::Declined
        } else {
            AuthorizationOutcome::Approved
        },
    }
}

pub fn percentile_nanos(samples: &mut [u64], percentile: usize) -> u64 {
    samples.sort_unstable();
    let rank = samples
        .len()
        .saturating_mul(percentile)
        .div_ceil(100)
        .saturating_sub(1);
    samples[rank.min(samples.len().saturating_sub(1))]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Vec<Event> {
        load_fixture(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures")
                .join("demo.tsv"),
        )
        .expect("fixture should parse")
    }

    #[test]
    fn baseline_reproducer_exposes_three_defects() {
        let observations = baseline_observations();
        assert_eq!(observations.len(), 3);
    }

    #[test]
    fn fixture_matches_naive_reference_and_duplicates_are_inert() {
        let mut engine = Engine::default();
        let mut duplicate_count = 0;
        for event in fixture() {
            match engine.ingest(event) {
                IngestResult::Accepted { .. } => {}
                IngestResult::ExactDuplicate => duplicate_count += 1,
                IngestResult::ConflictingDuplicate => panic!("unexpected conflict"),
            }
        }
        assert_eq!(duplicate_count, 1);
        assert_eq!(engine.event_count(), 9);
        engine
            .assert_matches_reference()
            .expect("indexed results should equal independent scan");
        engine
            .reconstruct_all_alerts()
            .expect("every alert should reconstruct");
    }

    #[test]
    fn late_event_emits_linked_corrections() {
        let mut engine = Engine::default();
        let mut late_corrections = 0;
        for event in fixture() {
            let is_late = event.event_id == 8;
            if let IngestResult::Accepted { correction_count } = engine.ingest(event)
                && is_late
            {
                late_corrections = correction_count;
            }
        }
        assert!(late_corrections > 0);
        assert!(engine.correction_count() >= late_corrections);
        let output = String::from_utf8(engine.decision_bytes()).expect("ASCII decision records");
        assert!(output.lines().any(|line| line.starts_with("C|")));
    }

    #[test]
    fn normal_close_reopen_replay_is_byte_identical() {
        let root = std::env::temp_dir().join(format!(
            "fraud-velocity-restart-test-{}",
            std::process::id()
        ));
        let direct_path = root.join("direct");
        let restarted_path = root.join("restarted");
        let _ = std::fs::remove_dir_all(&root);
        let events = fixture();

        let direct = {
            let mut persistent = PersistentEngine::open(&direct_path).expect("open direct ledger");
            for event in &events {
                persistent.ingest(event.clone()).expect("direct ingest");
            }
            persistent.decision_bytes()
        };
        let restarted = {
            let split = events.len() / 2;
            {
                let mut persistent =
                    PersistentEngine::open(&restarted_path).expect("open first ledger");
                for event in &events[..split] {
                    persistent.ingest(event.clone()).expect("first ingest");
                }
            }
            let mut persistent = PersistentEngine::open(&restarted_path).expect("reopen ledger");
            for event in &events[split..] {
                persistent.ingest(event.clone()).expect("second ingest");
            }
            persistent.decision_bytes()
        };
        assert_eq!(direct, restarted);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn customer_deletion_scrubs_owned_keys_and_preserves_shared_windows() {
        let mut engine = Engine::default();
        for event in fixture() {
            engine.ingest(event);
        }
        let before_device = engine.latest_rule_count(7, "device-10m-count-4");
        let before_ip = engine.latest_rule_count(7, "ip-prefix-24h-count-6");
        let receipt = engine.delete_customer(100).expect("customer deletion");
        assert_eq!(receipt.event_count(), 4);
        assert_eq!(
            before_device,
            engine.latest_rule_count(7, "device-10m-count-4")
        );
        assert_eq!(
            before_ip,
            engine.latest_rule_count(7, "ip-prefix-24h-count-6")
        );
        engine
            .scan_customer(&receipt)
            .expect("account and card scan");
        engine
            .reconstruct_all_alerts()
            .expect("retained alerts reconstruct");
    }
}
