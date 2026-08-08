use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkflowState {
    Absent,
    Created,
    Requesting,
    Ambiguous,
    Purchased,
    Failed,
    NeedsReview,
}

impl WorkflowState {
    const fn code(self) -> char {
        match self {
            Self::Absent => '0',
            Self::Created => 'C',
            Self::Requesting => 'Q',
            Self::Ambiguous => 'A',
            Self::Purchased => 'P',
            Self::Failed => 'F',
            Self::NeedsReview => 'R',
        }
    }

    fn from_code(value: &str) -> Result<Self, String> {
        match value {
            "0" => Ok(Self::Absent),
            "C" => Ok(Self::Created),
            "Q" => Ok(Self::Requesting),
            "A" => Ok(Self::Ambiguous),
            "P" => Ok(Self::Purchased),
            "F" => Ok(Self::Failed),
            "R" => Ok(Self::NeedsReview),
            _ => Err(format!("unknown workflow state {value:?}")),
        }
    }

    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Purchased | Self::Failed | Self::NeedsReview)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StateRow {
    pub state: WorkflowState,
    pub carrier_tx: u64,
    pub price_cents: u32,
    pub attempts: u8,
    pub final_at: u32,
    pub saw_timeout: bool,
}

impl Default for StateRow {
    fn default() -> Self {
        Self {
            state: WorkflowState::Absent,
            carrier_tx: 0,
            price_cents: 0,
            attempts: 0,
            final_at: 0,
            saw_timeout: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventKind {
    ShipmentCreated,
    AttemptStarted,
    PurchaseConfirmed,
    PurchaseRejected,
    PurchaseTimedOut,
    CallbackPending,
    CallbackActive,
    ReconcileFound,
    ReconcileUnknown,
}

impl EventKind {
    const fn code(self) -> char {
        match self {
            Self::ShipmentCreated => 'C',
            Self::AttemptStarted => 'A',
            Self::PurchaseConfirmed => 'P',
            Self::PurchaseRejected => 'F',
            Self::PurchaseTimedOut => 'T',
            Self::CallbackPending => 'B',
            Self::CallbackActive => 'V',
            Self::ReconcileFound => 'R',
            Self::ReconcileUnknown => 'U',
        }
    }

    fn from_code(value: &str) -> Result<Self, String> {
        match value {
            "C" => Ok(Self::ShipmentCreated),
            "A" => Ok(Self::AttemptStarted),
            "P" => Ok(Self::PurchaseConfirmed),
            "F" => Ok(Self::PurchaseRejected),
            "T" => Ok(Self::PurchaseTimedOut),
            "B" => Ok(Self::CallbackPending),
            "V" => Ok(Self::CallbackActive),
            "R" => Ok(Self::ReconcileFound),
            "U" => Ok(Self::ReconcileUnknown),
            _ => Err(format!("unknown event kind {value:?}")),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Event {
    pub kind: EventKind,
    pub shipment: u32,
    pub at: u32,
    pub carrier_tx: u64,
    pub price_cents: u32,
}

impl Event {
    pub const fn new(kind: EventKind, shipment: u32, at: u32) -> Self {
        Self {
            kind,
            shipment,
            at,
            carrier_tx: 0,
            price_cents: 0,
        }
    }

    pub const fn carrier(mut self, tx: u64, price_cents: u32) -> Self {
        self.carrier_tx = tx;
        self.price_cents = price_cents;
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StoredEvent {
    event: Event,
    after: StateRow,
}

fn set_purchased(row: &mut StateRow, event: Event) -> Result<(), String> {
    if event.carrier_tx == 0 {
        return Err("a purchased decision requires a carrier transaction id".to_string());
    }
    if row.carrier_tx != 0 && row.carrier_tx != event.carrier_tx {
        return Err(format!(
            "shipment {} changed carrier transaction from {} to {}",
            event.shipment, row.carrier_tx, event.carrier_tx
        ));
    }
    let was_purchased = row.state == WorkflowState::Purchased;
    row.state = WorkflowState::Purchased;
    row.carrier_tx = event.carrier_tx;
    if event.price_cents != 0 {
        row.price_cents = event.price_cents;
    }
    if !was_purchased {
        row.final_at = event.at;
    }
    Ok(())
}

fn reduce(previous: StateRow, event: Event) -> Result<StateRow, String> {
    let mut row = previous;
    match event.kind {
        EventKind::ShipmentCreated => {
            if row.state != WorkflowState::Absent {
                return Err(format!("shipment {} was created twice", event.shipment));
            }
            row.state = WorkflowState::Created;
        }
        EventKind::AttemptStarted => {
            if row.state != WorkflowState::Created || row.attempts != 0 {
                return Err(format!(
                    "shipment {} attempted purchase from {:?} with {} prior attempts",
                    event.shipment, row.state, row.attempts
                ));
            }
            row.state = WorkflowState::Requesting;
            row.attempts = 1;
        }
        EventKind::PurchaseConfirmed | EventKind::CallbackActive | EventKind::ReconcileFound => {
            set_purchased(&mut row, event)?
        }
        EventKind::PurchaseRejected => {
            if row.state != WorkflowState::Purchased {
                row.state = WorkflowState::Failed;
                if row.final_at == 0 {
                    row.final_at = event.at;
                }
            }
        }
        EventKind::PurchaseTimedOut => {
            row.saw_timeout = true;
            if row.state != WorkflowState::Purchased {
                row.state = WorkflowState::Ambiguous;
                row.final_at = 0;
            }
        }
        EventKind::CallbackPending => {
            if row.state == WorkflowState::Absent {
                return Err(format!(
                    "shipment {} received a callback before creation",
                    event.shipment
                ));
            }
            if row.carrier_tx != 0 && row.carrier_tx != event.carrier_tx {
                return Err(format!(
                    "shipment {} received a conflicting callback transaction",
                    event.shipment
                ));
            }
            row.carrier_tx = event.carrier_tx;
            if event.price_cents != 0 {
                row.price_cents = event.price_cents;
            }
        }
        EventKind::ReconcileUnknown => {
            if row.state != WorkflowState::Purchased {
                row.state = WorkflowState::NeedsReview;
                row.final_at = event.at;
            }
        }
    }
    Ok(row)
}

fn checksum(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn encode(record: StoredEvent) -> String {
    let mut payload = String::with_capacity(96);
    write!(
        payload,
        "1|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
        record.event.kind.code(),
        record.event.shipment,
        record.event.at,
        record.event.carrier_tx,
        record.event.price_cents,
        record.after.state.code(),
        record.after.carrier_tx,
        record.after.price_cents,
        record.after.attempts,
        record.after.final_at,
        u8::from(record.after.saw_timeout)
    )
    .expect("writing to a String cannot fail");
    let digest = checksum(payload.as_bytes());
    format!("{payload}|{digest:016x}\n")
}

fn parse_num<T: std::str::FromStr>(value: &str, name: &str) -> Result<T, String> {
    value
        .parse()
        .map_err(|_| format!("invalid {name} value {value:?}"))
}

fn decode(line: &str) -> Result<StoredEvent, String> {
    let (payload, digest) = line
        .rsplit_once('|')
        .ok_or_else(|| "journal record has no checksum".to_string())?;
    let expected =
        u64::from_str_radix(digest, 16).map_err(|_| format!("invalid checksum {digest:?}"))?;
    let actual = checksum(payload.as_bytes());
    if expected != actual {
        return Err(format!(
            "journal checksum mismatch: expected {expected:016x}, got {actual:016x}"
        ));
    }
    let mut fields = payload.split('|');
    let version = fields.next().unwrap_or_default();
    let event_kind = fields.next().unwrap_or_default();
    let shipment = fields.next().unwrap_or_default();
    let event_time = fields.next().unwrap_or_default();
    let event_tx = fields.next().unwrap_or_default();
    let event_price = fields.next().unwrap_or_default();
    let state = fields.next().unwrap_or_default();
    let stored_tx = fields.next().unwrap_or_default();
    let stored_price = fields.next().unwrap_or_default();
    let attempts = fields.next().unwrap_or_default();
    let final_time = fields.next().unwrap_or_default();
    let timeout = fields.next().unwrap_or_default();
    if version != "1" || fields.next().is_some() {
        return Err(format!("unsupported journal record {payload:?}"));
    }
    Ok(StoredEvent {
        event: Event {
            kind: EventKind::from_code(event_kind)?,
            shipment: parse_num(shipment, "shipment")?,
            at: parse_num(event_time, "event time")?,
            carrier_tx: parse_num(event_tx, "carrier transaction")?,
            price_cents: parse_num(event_price, "price")?,
        },
        after: StateRow {
            state: WorkflowState::from_code(state)?,
            carrier_tx: parse_num(stored_tx, "stored carrier transaction")?,
            price_cents: parse_num(stored_price, "stored price")?,
            attempts: parse_num(attempts, "attempt count")?,
            final_at: parse_num(final_time, "final time")?,
            saw_timeout: match timeout {
                "0" => false,
                "1" => true,
                value => return Err(format!("invalid timeout marker {value:?}")),
            },
        },
    })
}

pub struct Journal {
    path: PathBuf,
    states: Vec<StateRow>,
    attempt_records: Vec<u8>,
    record_count: usize,
}

impl Journal {
    pub fn create(path: &Path, shipments: usize) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)
            .map_err(|error| format!("create {}: {error}", path.display()))?;
        Ok(Self {
            path: path.to_path_buf(),
            states: vec![StateRow::default(); shipments],
            attempt_records: vec![0; shipments],
            record_count: 0,
        })
    }

    pub fn open(path: &Path, shipments: usize) -> Result<Self, String> {
        let file = File::open(path).map_err(|error| format!("open {}: {error}", path.display()))?;
        let mut states = vec![StateRow::default(); shipments];
        let mut attempt_records = vec![0_u8; shipments];
        let mut record_count = 0;
        let mut reader = BufReader::new(file);
        let mut bytes = Vec::new();
        let mut valid_len = 0_u64;
        loop {
            bytes.clear();
            let read = reader
                .read_until(b'\n', &mut bytes)
                .map_err(|error| format!("read {}: {error}", path.display()))?;
            if read == 0 {
                break;
            }
            if !bytes.ends_with(b"\n") {
                break;
            }
            valid_len = valid_len
                .checked_add(
                    u64::try_from(read).map_err(|_| "journal length overflow".to_string())?,
                )
                .ok_or_else(|| "journal length overflow".to_string())?;
            bytes.pop();
            let line = std::str::from_utf8(&bytes)
                .map_err(|error| format!("journal is not UTF-8: {error}"))?;
            let record = decode(line)?;
            let index = usize::try_from(record.event.shipment)
                .map_err(|_| "shipment index overflow".to_string())?;
            let previous = *states.get(index).ok_or_else(|| {
                format!("shipment {} is outside the fixture", record.event.shipment)
            })?;
            let reconstructed = reduce(previous, record.event)?;
            if reconstructed != record.after {
                return Err(format!(
                    "audit mismatch for shipment {}: reconstructed {reconstructed:?}, stored {:?}",
                    record.event.shipment, record.after
                ));
            }
            states[index] = record.after;
            if record.event.kind == EventKind::AttemptStarted {
                attempt_records[index] = attempt_records[index]
                    .checked_add(1)
                    .ok_or_else(|| "attempt audit counter overflow".to_string())?;
            }
            record_count += 1;
        }
        let file_len = fs::metadata(path)
            .map_err(|error| format!("stat {}: {error}", path.display()))?
            .len();
        if file_len != valid_len {
            let file = OpenOptions::new()
                .write(true)
                .open(path)
                .map_err(|error| format!("repair {}: {error}", path.display()))?;
            file.set_len(valid_len)
                .map_err(|error| format!("truncate {}: {error}", path.display()))?;
            file.sync_data()
                .map_err(|error| format!("sync repaired {}: {error}", path.display()))?;
        }
        Ok(Self {
            path: path.to_path_buf(),
            states,
            attempt_records,
            record_count,
        })
    }

    pub fn commit_batch(&mut self, events: &[Event]) -> Result<(), String> {
        let mut next_states = self.states.clone();
        let mut next_attempt_records = self.attempt_records.clone();
        let mut encoded = String::with_capacity(events.len().saturating_mul(64));
        for event in events {
            let index = usize::try_from(event.shipment)
                .map_err(|_| "shipment index overflow".to_string())?;
            let previous = *next_states
                .get(index)
                .ok_or_else(|| format!("shipment {} is outside the fixture", event.shipment))?;
            let after = reduce(previous, *event)?;
            let record = StoredEvent {
                event: *event,
                after,
            };
            encoded.push_str(&encode(record));
            next_states[index] = after;
            if event.kind == EventKind::AttemptStarted {
                next_attempt_records[index] = next_attempt_records[index]
                    .checked_add(1)
                    .ok_or_else(|| "attempt audit counter overflow".to_string())?;
            }
        }

        let file = OpenOptions::new()
            .append(true)
            .open(&self.path)
            .map_err(|error| format!("append {}: {error}", self.path.display()))?;
        let mut writer = BufWriter::new(file);
        writer
            .write_all(encoded.as_bytes())
            .map_err(|error| format!("write {}: {error}", self.path.display()))?;
        writer
            .flush()
            .map_err(|error| format!("flush {}: {error}", self.path.display()))?;
        writer
            .get_ref()
            .sync_data()
            .map_err(|error| format!("sync {}: {error}", self.path.display()))?;
        self.states = next_states;
        self.attempt_records = next_attempt_records;
        self.record_count += events.len();
        Ok(())
    }

    pub fn row(&self, shipment: usize) -> StateRow {
        self.states[shipment]
    }

    pub fn rows(&self) -> &[StateRow] {
        &self.states
    }

    pub fn record_count(&self) -> usize {
        self.record_count
    }

    pub fn attempt_record_count(&self, shipment: usize) -> u8 {
        self.attempt_records[shipment]
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CarrierLabel {
    tx: u64,
    price_cents: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PlannedOutcome {
    Confirmed,
    Rejected,
    TimedOutCharged,
    TimedOutUnknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PurchaseOutcome {
    Confirmed(CarrierLabel),
    Rejected,
    TimedOut,
}

fn mix(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn planned_outcome(seed: u64, shipment: u32) -> PlannedOutcome {
    if (u64::from(shipment) + seed).is_multiple_of(10) {
        if mix(seed.rotate_left(17) ^ u64::from(shipment)) & 1 == 0 {
            PlannedOutcome::TimedOutCharged
        } else {
            PlannedOutcome::TimedOutUnknown
        }
    } else if mix(seed ^ u64::from(shipment).wrapping_mul(0x9e37_79b9)).is_multiple_of(20) {
        PlannedOutcome::Rejected
    } else {
        PlannedOutcome::Confirmed
    }
}

fn price_for(seed: u64, shipment: u32) -> u32 {
    500 + u32::try_from(mix(seed ^ u64::from(shipment)) % 2_000).expect("bounded price")
}

struct CarrierSimulator {
    seed: u64,
    labels: Vec<Option<CarrierLabel>>,
    calls: Vec<u8>,
}

impl CarrierSimulator {
    fn new(seed: u64, shipments: usize) -> Self {
        Self {
            seed,
            labels: vec![None; shipments],
            calls: vec![0; shipments],
        }
    }

    fn purchase(&mut self, shipment: u32, price_cents: u32) -> Result<PurchaseOutcome, String> {
        let index = usize::try_from(shipment).map_err(|_| "shipment index overflow".to_string())?;
        self.calls[index] = self.calls[index]
            .checked_add(1)
            .ok_or_else(|| "carrier call counter overflow".to_string())?;
        if self.calls[index] != 1 {
            return Err(format!(
                "unsafe automatic second purchase for shipment {shipment}"
            ));
        }
        let label = CarrierLabel {
            tx: self
                .seed
                .wrapping_mul(1_000_000)
                .wrapping_add(u64::from(shipment) + 1),
            price_cents,
        };
        match planned_outcome(self.seed, shipment) {
            PlannedOutcome::Confirmed => {
                self.labels[index] = Some(label);
                Ok(PurchaseOutcome::Confirmed(label))
            }
            PlannedOutcome::Rejected => Ok(PurchaseOutcome::Rejected),
            PlannedOutcome::TimedOutCharged => {
                self.labels[index] = Some(label);
                Ok(PurchaseOutcome::TimedOut)
            }
            PlannedOutcome::TimedOutUnknown => Ok(PurchaseOutcome::TimedOut),
        }
    }

    fn lookup(&self, shipment: usize) -> Option<CarrierLabel> {
        self.labels[shipment]
    }
}

#[derive(Clone, Debug)]
pub struct SeedMetrics {
    pub seed: u64,
    pub shipments: usize,
    pub purchased: usize,
    pub failed: usize,
    pub needs_review: usize,
    pub ambiguous_timeouts: usize,
    pub paid_labels: usize,
    pub callbacks: usize,
    pub injected_restarts: usize,
    pub decision_records: usize,
    pub max_final_at: u32,
    pub journal_bytes: u64,
}

fn reopen(journal: Journal, shipments: usize) -> Result<Journal, String> {
    let path = journal.path.clone();
    drop(journal);
    Journal::open(&path, shipments)
}

fn callback_sequence(seed: u64, shipment: u32, label: CarrierLabel, start_at: u32) -> [Event; 3] {
    let pending = Event::new(EventKind::CallbackPending, shipment, start_at)
        .carrier(label.tx, label.price_cents);
    let active = Event::new(EventKind::CallbackActive, shipment, start_at + 1)
        .carrier(label.tx, label.price_cents);
    if mix(seed ^ u64::from(shipment)) & 1 == 0 {
        [active, pending, active]
    } else {
        [pending, active, pending]
    }
}

fn find_confirmed(seed: u64, shipments: usize) -> Result<u32, String> {
    (0..u32::try_from(shipments).map_err(|_| "fixture is too large".to_string())?)
        .find(|shipment| planned_outcome(seed, *shipment) == PlannedOutcome::Confirmed)
        .ok_or_else(|| "fixture has no confirmed purchase for the network crash".to_string())
}

pub fn run_seed(root: &Path, seed: u64, shipments: usize) -> Result<SeedMetrics, String> {
    if shipments < 10 {
        return Err("fixture must contain at least 10 shipments".to_string());
    }
    let seed_dir = root.join(format!("seed-{seed:02}"));
    fs::create_dir(&seed_dir).map_err(|error| format!("create {}: {error}", seed_dir.display()))?;
    let journal_path = seed_dir.join("workflow.journal");
    let mut journal = Journal::create(&journal_path, shipments)?;
    let mut carrier = CarrierSimulator::new(seed, shipments);
    let mut restarts = 0;

    let shipment_count =
        u32::try_from(shipments).map_err(|_| "fixture is too large".to_string())?;
    let created: Vec<_> = (0..shipment_count)
        .map(|shipment| Event::new(EventKind::ShipmentCreated, shipment, 0))
        .collect();
    journal.commit_batch(&created)?;

    // Crash before attempt persistence: the shipment rows survive and no network call has happened.
    journal = reopen(journal, shipments)?;
    restarts += 1;

    // Write-ahead intent is durable for every shipment before any carrier request.
    let attempts: Vec<_> = (0..shipment_count)
        .map(|shipment| Event::new(EventKind::AttemptStarted, shipment, 1))
        .collect();
    journal.commit_batch(&attempts)?;
    journal = reopen(journal, shipments)?;
    restarts += 1;

    let crash_after_network = find_confirmed(seed, shipments)?;
    let mut results = Vec::with_capacity(shipments);
    for shipment in 0..shipment_count {
        if journal.row(shipment as usize).attempts != 1 {
            return Err(format!(
                "shipment {shipment} reached the carrier without a durable attempt"
            ));
        }
        let price = price_for(seed, shipment);
        let outcome = carrier.purchase(shipment, price)?;
        if shipment == crash_after_network {
            // The carrier has charged and created the label. The local result is intentionally lost.
            journal.commit_batch(&results)?;
            results.clear();
            journal = reopen(journal, shipments)?;
            restarts += 1;
            continue;
        }
        let event = match outcome {
            PurchaseOutcome::Confirmed(label) => {
                Event::new(EventKind::PurchaseConfirmed, shipment, 2)
                    .carrier(label.tx, label.price_cents)
            }
            PurchaseOutcome::Rejected => Event::new(EventKind::PurchaseRejected, shipment, 2),
            PurchaseOutcome::TimedOut => Event::new(EventKind::PurchaseTimedOut, shipment, 2),
        };
        results.push(event);
    }
    journal.commit_batch(&results)?;

    // Crash after the local result commit: terminal rows must resume without another purchase.
    journal = reopen(journal, shipments)?;
    restarts += 1;

    let mut callbacks = 0;
    let mut early_callbacks = Vec::new();
    let mut late_callbacks = Vec::new();
    for shipment in 0..shipment_count {
        let Some(label) = carrier.lookup(shipment as usize) else {
            continue;
        };
        let group = mix(seed.rotate_right(9) ^ u64::from(shipment)) % 3;
        let sequence = callback_sequence(seed, shipment, label, if group == 0 { 5 } else { 35 });
        callbacks += sequence.len();
        match group {
            0 => early_callbacks.extend(sequence),
            1 => late_callbacks.extend(sequence),
            _ => {
                early_callbacks.push(
                    Event::new(EventKind::CallbackPending, shipment, 5)
                        .carrier(label.tx, label.price_cents),
                );
                early_callbacks.push(
                    Event::new(EventKind::CallbackPending, shipment, 6)
                        .carrier(label.tx, label.price_cents),
                );
                late_callbacks.push(
                    Event::new(EventKind::CallbackActive, shipment, 35)
                        .carrier(label.tx, label.price_cents),
                );
            }
        }
    }
    journal.commit_batch(&early_callbacks)?;

    let mut reconciliation = Vec::new();
    for shipment in 0..shipment_count {
        let row = journal.row(shipment as usize);
        if row.state.is_terminal() {
            continue;
        }
        if let Some(label) = carrier.lookup(shipment as usize) {
            reconciliation.push(
                Event::new(EventKind::ReconcileFound, shipment, 30)
                    .carrier(label.tx, label.price_cents),
            );
        } else {
            reconciliation.push(Event::new(EventKind::ReconcileUnknown, shipment, 30));
        }
    }
    journal.commit_batch(&reconciliation)?;
    journal.commit_batch(&late_callbacks)?;

    // A final reopen is the audit: each stored post-state is checked against a fresh replay.
    journal = reopen(journal, shipments)?;

    let mut purchased = 0;
    let mut failed = 0;
    let mut needs_review = 0;
    let mut max_final_at = 0;
    let mut ambiguous_timeouts = 0;
    let mut paid_labels = 0;
    for shipment in 0..shipments {
        let row = journal.row(shipment);
        if row.attempts != 1 || carrier.calls[shipment] != 1 {
            return Err(format!(
                "shipment {shipment} did not preserve exactly one durable attempt and one carrier call"
            ));
        }
        if journal.attempt_record_count(shipment) != 1 {
            return Err(format!(
                "shipment {shipment} audit lost its purchase attempt"
            ));
        }
        let label = carrier.lookup(shipment);
        if let Some(label) = label {
            paid_labels += 1;
            if row.state != WorkflowState::Purchased || row.carrier_tx != label.tx {
                return Err(format!(
                    "shipment {shipment} did not converge to its authoritative carrier label"
                ));
            }
        } else {
            match planned_outcome(seed, shipment as u32) {
                PlannedOutcome::Rejected => {
                    if row.state != WorkflowState::Failed {
                        return Err(format!("shipment {shipment} lost a carrier rejection"));
                    }
                }
                PlannedOutcome::TimedOutUnknown => {
                    if row.state != WorkflowState::NeedsReview || !row.saw_timeout {
                        return Err(format!(
                            "shipment {shipment} did not expose an inconclusive timeout for review"
                        ));
                    }
                }
                PlannedOutcome::Confirmed | PlannedOutcome::TimedOutCharged => {
                    return Err(format!(
                        "shipment {shipment} is missing an authoritative label"
                    ));
                }
            }
        }
        match row.state {
            WorkflowState::Purchased => purchased += 1,
            WorkflowState::Failed => failed += 1,
            WorkflowState::NeedsReview => needs_review += 1,
            other => {
                return Err(format!(
                    "shipment {shipment} remained nonterminal in {other:?}"
                ));
            }
        }
        if row.final_at > 60 {
            return Err(format!(
                "shipment {shipment} converged after the 60-second deadline"
            ));
        }
        max_final_at = max_final_at.max(row.final_at);
        if (u64::try_from(shipment).expect("usize fits u64") + seed).is_multiple_of(10) {
            ambiguous_timeouts += 1;
            if carrier.calls[shipment] != 1 {
                return Err(format!("ambiguous shipment {shipment} was purchased twice"));
            }
        }
    }
    if ambiguous_timeouts * 10 != shipments {
        return Err(format!(
            "expected exactly 10% timeouts, got {ambiguous_timeouts}/{shipments}"
        ));
    }
    if purchased + failed + needs_review != shipments {
        return Err("terminal-state counts do not cover the fixture".to_string());
    }

    let decision_records = journal.record_count();
    let journal_bytes = fs::metadata(&journal_path)
        .map_err(|error| format!("stat {}: {error}", journal_path.display()))?
        .len();
    Ok(SeedMetrics {
        seed,
        shipments,
        purchased,
        failed,
        needs_review,
        ambiguous_timeouts,
        paid_labels,
        callbacks,
        injected_restarts: restarts,
        decision_records,
        max_final_at,
        journal_bytes,
    })
}

pub fn run_fixture(root: &Path, shipments: usize, seeds: u64) -> Result<Vec<SeedMetrics>, String> {
    fs::create_dir(root).map_err(|error| format!("create {}: {error}", root.display()))?;
    let mut metrics = Vec::with_capacity(usize::try_from(seeds).unwrap_or(0));
    for seed in 0..seeds {
        metrics.push(run_seed(root, seed, shipments)?);
    }
    Ok(metrics)
}

fn write_carrier_label(path: &Path) -> Result<CarrierLabel, String> {
    if path.exists() {
        return Err("a second carrier purchase was attempted".to_string());
    }
    let label = CarrierLabel {
        tx: 424_242,
        price_cents: 1_299,
    };
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(|error| format!("create {}: {error}", path.display()))?;
    writeln!(file, "{}|{}", label.tx, label.price_cents)
        .map_err(|error| format!("write {}: {error}", path.display()))?;
    file.sync_data()
        .map_err(|error| format!("sync {}: {error}", path.display()))?;
    Ok(label)
}

fn read_carrier_label(path: &Path) -> Result<Option<CarrierLabel>, String> {
    match fs::read_to_string(path) {
        Ok(contents) => {
            let (tx, price) = contents
                .trim()
                .split_once('|')
                .ok_or_else(|| "invalid carrier authority record".to_string())?;
            Ok(Some(CarrierLabel {
                tx: parse_num(tx, "carrier transaction")?,
                price_cents: parse_num(price, "carrier price")?,
            }))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("read {}: {error}", path.display())),
    }
}

pub fn run_crash_child(dir: &Path, stage: &str) -> Result<(), String> {
    let journal_path = dir.join("workflow.journal");
    let carrier_path = dir.join("carrier-authority.log");
    let mut journal = Journal::open(&journal_path, 1)?;
    match stage {
        "before-network" => {}
        "after-carrier" => {
            write_carrier_label(&carrier_path)?;
        }
        "after-confirm" => {
            let label = write_carrier_label(&carrier_path)?;
            journal
                .commit_batch(&[Event::new(EventKind::PurchaseConfirmed, 0, 2)
                    .carrier(label.tx, label.price_cents)])?;
        }
        "after-callback" => {
            let label = write_carrier_label(&carrier_path)?;
            journal
                .commit_batch(&[Event::new(EventKind::CallbackActive, 0, 3)
                    .carrier(label.tx, label.price_cents)])?;
        }
        other => return Err(format!("unknown crash stage {other:?}")),
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct CrashMetrics {
    pub stage: String,
    pub final_state: WorkflowState,
    pub carrier_purchases: usize,
    pub attempts: u8,
}

pub fn prepare_crash_scenario(dir: &Path) -> Result<(), String> {
    fs::create_dir(dir).map_err(|error| format!("create {}: {error}", dir.display()))?;
    let mut journal = Journal::create(&dir.join("workflow.journal"), 1)?;
    journal.commit_batch(&[
        Event::new(EventKind::ShipmentCreated, 0, 0),
        Event::new(EventKind::AttemptStarted, 0, 1),
    ])
}

pub fn resume_crash_scenario(dir: &Path, stage: &str) -> Result<CrashMetrics, String> {
    let journal_path = dir.join("workflow.journal");
    let carrier_path = dir.join("carrier-authority.log");
    let mut journal = Journal::open(&journal_path, 1)?;
    let label = read_carrier_label(&carrier_path)?;
    if !journal.row(0).state.is_terminal() {
        let event = match label {
            Some(label) => {
                Event::new(EventKind::ReconcileFound, 0, 30).carrier(label.tx, label.price_cents)
            }
            None => Event::new(EventKind::ReconcileUnknown, 0, 30),
        };
        journal.commit_batch(&[event])?;
    }
    let journal = Journal::open(&journal_path, 1)?;
    let row = journal.row(0);
    let expected = if label.is_some() {
        WorkflowState::Purchased
    } else {
        WorkflowState::NeedsReview
    };
    if row.state != expected || row.attempts != 1 {
        return Err(format!(
            "crash stage {stage} resumed as {:?} with {} attempts",
            row.state, row.attempts
        ));
    }
    Ok(CrashMetrics {
        stage: stage.to_string(),
        final_state: row.state,
        carrier_purchases: usize::from(label.is_some()),
        attempts: row.attempts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "carrier-label-ambiguity-{name}-{}-{nonce}",
            std::process::id()
        ))
    }

    #[test]
    fn reordered_duplicate_callbacks_are_monotonic() {
        let start = StateRow {
            state: WorkflowState::Ambiguous,
            attempts: 1,
            saw_timeout: true,
            ..StateRow::default()
        };
        let pending = Event::new(EventKind::CallbackPending, 0, 5).carrier(9, 700);
        let active = Event::new(EventKind::CallbackActive, 0, 6).carrier(9, 700);
        let ordered = reduce(reduce(start, pending).expect("pending"), active).expect("active");
        let reordered = reduce(
            reduce(reduce(start, active).expect("active"), pending).expect("pending"),
            active,
        )
        .expect("duplicate active");
        assert_eq!(ordered.state, WorkflowState::Purchased);
        assert_eq!(ordered.state, reordered.state);
        assert_eq!(ordered.carrier_tx, reordered.carrier_tx);
    }

    #[test]
    fn journal_repairs_a_partial_tail_before_later_commits() {
        let dir = temp_path("partial-tail");
        fs::create_dir(&dir).expect("temp directory");
        let path = dir.join("workflow.journal");
        let mut journal = Journal::create(&path, 1).expect("create journal");
        journal
            .commit_batch(&[
                Event::new(EventKind::ShipmentCreated, 0, 0),
                Event::new(EventKind::AttemptStarted, 0, 1),
            ])
            .expect("commit");
        let mut file = OpenOptions::new().append(true).open(&path).expect("append");
        file.write_all(b"partial-without-newline")
            .expect("partial write");
        file.sync_data().expect("sync partial");
        let mut recovered = Journal::open(&path, 1).expect("recover journal");
        assert_eq!(recovered.row(0).state, WorkflowState::Requesting);
        assert_eq!(recovered.record_count(), 2);
        recovered
            .commit_batch(&[Event::new(EventKind::PurchaseTimedOut, 0, 2)])
            .expect("commit after recovery");
        drop(recovered);
        let reopened = Journal::open(&path, 1).expect("reopen repaired journal");
        assert_eq!(reopened.row(0).state, WorkflowState::Ambiguous);
        assert_eq!(reopened.record_count(), 3);
        fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn realistic_rules_hold_on_small_fixture() {
        let dir = temp_path("small-fixture");
        let metrics = run_fixture(&dir, 100, 3).expect("fixture passes");
        assert_eq!(metrics.len(), 3);
        assert!(metrics.iter().all(|seed| seed.ambiguous_timeouts == 10));
        assert!(metrics.iter().all(|seed| seed.injected_restarts == 4));
        fs::remove_dir_all(dir).expect("cleanup");
    }
}
