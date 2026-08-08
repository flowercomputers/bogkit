//! A dependency-free reference model for the cache-revalidation scenario.
//!
//! This is deliberately a state-machine model, not an HTTP implementation.
//! It consumes normalized trace records, fake origin outcomes, and explicit
//! commit-point crash injections. The model is also used by the CLI demo and
//! unit tests so the evidence does not depend on network access or a running
//! proxy.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;
use std::fs;
use std::path::Path;

const SHA256_K: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

const SHA256_H0: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];

/// Stable SHA-256 identifier used in all emitted evidence.
pub fn hash_id(value: &str) -> String {
    let digest = sha256(value.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn sha256(input: &[u8]) -> [u8; 32] {
    let bit_len = (input.len() as u64) * 8;
    let mut message = input.to_vec();
    message.push(0x80);
    while !(message.len() + 8).is_multiple_of(64) {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());

    let mut state = SHA256_H0;
    for chunk in message.chunks_exact(64) {
        let mut words = [0u32; 64];
        for (i, word) in words.iter_mut().take(16).enumerate() {
            let start = i * 4;
            *word = u32::from_be_bytes([
                chunk[start],
                chunk[start + 1],
                chunk[start + 2],
                chunk[start + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = words[i - 15].rotate_right(7)
                ^ words[i - 15].rotate_right(18)
                ^ (words[i - 15] >> 3);
            let s1 = words[i - 2].rotate_right(17)
                ^ words[i - 2].rotate_right(19)
                ^ (words[i - 2] >> 10);
            words[i] = words[i - 16]
                .wrapping_add(s0)
                .wrapping_add(words[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(SHA256_K[i])
                .wrapping_add(words[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }

    let mut output = [0u8; 32];
    for (i, word) in state.iter().enumerate() {
        output[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    output
}

/// The cache key includes tenant and the supplied Vary fingerprint. URL
/// parsing is intentionally out of scope; the model only trims whitespace and
/// removes a fragment, which is not sent in an HTTP request.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CacheKey {
    pub tenant: String,
    pub method: String,
    pub url: String,
    pub vary: String,
}

impl CacheKey {
    pub fn new(tenant: &str, method: &str, url: &str, vary: &str) -> Self {
        let mut normalized_url = url.trim().to_string();
        if let Some(fragment) = normalized_url.find('#') {
            normalized_url.truncate(fragment);
        }
        Self {
            tenant: tenant.trim().to_string(),
            method: method.trim().to_ascii_uppercase(),
            url: normalized_url,
            vary: vary.trim().to_ascii_lowercase(),
        }
    }

    pub fn id(&self) -> String {
        hash_id(&format!(
            "tenant:{}:{}\0method:{}:{}\0url:{}:{}\0vary:{}:{}",
            self.tenant.len(),
            self.tenant,
            self.method.len(),
            self.method,
            self.url.len(),
            self.url,
            self.vary.len(),
            self.vary
        ))
    }
}

fn normalize_tags(tags: &[String]) -> Vec<String> {
    let mut result: Vec<String> = tags
        .iter()
        .flat_map(|tag| tag.split(','))
        .map(str::trim)
        .filter(|tag| !tag.is_empty() && *tag != "-")
        .map(str::to_ascii_lowercase)
        .collect();
    result.sort();
    result.dedup();
    result.truncate(16);
    result
}

fn parse_tags(raw: &str) -> Result<Vec<String>, String> {
    if raw == "-" {
        Ok(Vec::new())
    } else {
        let mut result: Vec<String> = raw
            .split(',')
            .map(str::trim)
            .filter(|tag| !tag.is_empty() && *tag != "-")
            .map(str::to_ascii_lowercase)
            .collect();
        result.sort();
        result.dedup();
        if result.len() > 16 {
            return Err("trace contains more than 16 tags".to_string());
        }
        Ok(result)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    pub fresh_until: u64,
    pub stale_until: u64,
    pub validator: String,
    pub body_digest: String,
    pub body_size: u64,
    pub tags: Vec<String>,
    pub last_access: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlobManifest {
    pub digest: String,
    pub size: u64,
    pub verified: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OriginOutcome {
    Error {
        code: String,
    },
    NotModified {
        fresh_for: u64,
        stale_for: u64,
        validator: String,
    },
    Modified {
        digest: String,
        size: u64,
        fresh_for: u64,
        stale_for: u64,
        tags: Vec<String>,
        validator: String,
        verified: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CrashPoint {
    None,
    AfterPrepare,
    AfterBodyCommit,
    AfterMetadataCommit,
}

impl CrashPoint {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "none" => Ok(Self::None),
            "after_prepare" => Ok(Self::AfterPrepare),
            "after_body" => Ok(Self::AfterBodyCommit),
            "after_metadata" => Ok(Self::AfterMetadataCommit),
            _ => Err(format!("unknown crash point: {value}")),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum CommitPhase {
    Prepared,
    BodyCommitted,
    MetadataCommitted,
}

#[derive(Clone, Debug)]
struct Blob {
    size: u64,
    verified: bool,
    refs: u64,
}

#[derive(Clone, Debug)]
struct Journal {
    request_id: String,
    key: CacheKey,
    digest: String,
    phase: CommitPhase,
}

#[derive(Clone, Debug)]
struct Lease {
    request_id: String,
    key: CacheKey,
    started_at: u64,
    tenant_epoch: u64,
    origin_id: String,
    allow_stale_if_error: bool,
    worker: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Metrics {
    pub requests: u64,
    pub fresh_hits: u64,
    pub stale_responses: u64,
    pub misses: u64,
    pub revalidation_starts: u64,
    pub revalidation_waits: u64,
    pub purges_applied: u64,
    pub purges_ignored: u64,
    pub recovery_rollbacks: u64,
    pub recovery_commits: u64,
    pub quota_evictions: u64,
    pub unsafe_body_serves: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Decision {
    pub at: u64,
    pub event: &'static str,
    pub reason: &'static str,
    pub tenant_id: Option<String>,
    pub key_id: Option<String>,
    pub actor_id: Option<String>,
    pub lease_id: Option<String>,
    pub body_id: Option<String>,
    pub committed_usage_bytes: u64,
}

impl Decision {
    pub fn line(&self) -> String {
        let tenant = self.tenant_id.as_deref().unwrap_or("-");
        let key = self.key_id.as_deref().unwrap_or("-");
        let actor = self.actor_id.as_deref().unwrap_or("-");
        let lease = self.lease_id.as_deref().unwrap_or("-");
        let body = self.body_id.as_deref().unwrap_or("-");
        format!(
            "at={} event={} reason={} tenant_id={} key_id={} actor_id={} lease_id={} body_id={} committed_usage_bytes={}",
            self.at,
            self.event,
            self.reason,
            tenant,
            key,
            actor,
            lease,
            body,
            self.committed_usage_bytes
        )
    }
}

#[derive(Clone, Debug, Default)]
struct State {
    entries: HashMap<CacheKey, Entry>,
    blobs: HashMap<String, Blob>,
    tag_index: HashMap<(String, String), BTreeSet<CacheKey>>,
}

impl State {
    fn add_tag_postings(&mut self, key: &CacheKey, tags: &[String]) {
        for tag in tags {
            self.tag_index
                .entry((key.tenant.clone(), tag.clone()))
                .or_default()
                .insert(key.clone());
        }
    }

    fn remove_tag_postings(&mut self, key: &CacheKey, tags: &[String]) {
        for tag in tags {
            let index_key = (key.tenant.clone(), tag.clone());
            if let Some(postings) = self.tag_index.get_mut(&index_key) {
                postings.remove(key);
                if postings.is_empty() {
                    self.tag_index.remove(&index_key);
                }
            }
        }
    }

    fn attach(&mut self, key: CacheKey, entry: Entry) {
        if let Some(old) = self.detach(&key) {
            debug_assert!(self.blobs.contains_key(&old.body_digest));
        }
        let blob = self
            .blobs
            .get_mut(&entry.body_digest)
            .expect("attach requires a manifest");
        blob.refs += 1;
        self.add_tag_postings(&key, &entry.tags);
        self.entries.insert(key, entry);
    }

    fn detach(&mut self, key: &CacheKey) -> Option<Entry> {
        let entry = self.entries.remove(key)?;
        self.remove_tag_postings(key, &entry.tags);
        let blob = self
            .blobs
            .get_mut(&entry.body_digest)
            .expect("entry body must have a manifest");
        debug_assert!(blob.refs > 0);
        blob.refs = blob.refs.saturating_sub(1);
        Some(entry)
    }

    fn body_is_servable(&self, entry: &Entry) -> bool {
        self.blobs
            .get(&entry.body_digest)
            .is_some_and(|blob| blob.verified && blob.size == entry.body_size)
    }

    fn rebuild_refs(&mut self) {
        for blob in self.blobs.values_mut() {
            blob.refs = 0;
        }
        self.tag_index.clear();
        let entries: Vec<(CacheKey, Entry)> = self
            .entries
            .iter()
            .map(|(key, entry)| (key.clone(), entry.clone()))
            .collect();
        for (key, entry) in entries {
            if let Some(blob) = self.blobs.get_mut(&entry.body_digest) {
                blob.refs += 1;
            }
            self.add_tag_postings(&key, &entry.tags);
        }
    }

    fn garbage_collect(&mut self) -> u64 {
        let before = self.blobs.len();
        self.blobs.retain(|_, blob| blob.refs > 0);
        (before - self.blobs.len()) as u64
    }

    fn metadata_bytes(&self) -> u64 {
        self.entries
            .values()
            .map(|entry| 192 + (entry.tags.len() as u64 * 32) + entry.validator.len() as u64)
            .sum()
    }

    fn committed_usage_bytes(&self) -> u64 {
        let bodies = self
            .blobs
            .values()
            .filter(|blob| blob.refs > 0 && blob.verified)
            .map(|blob| blob.size)
            .sum::<u64>();
        bodies.saturating_add(self.metadata_bytes())
    }
}

/// The acceptance-oriented reference model.
pub struct ReferenceEngine {
    state: State,
    quota_bytes: u64,
    origins: HashMap<String, OriginOutcome>,
    leases: HashMap<CacheKey, Lease>,
    journal: Option<Journal>,
    seen_purges: HashSet<(String, u64, String)>,
    tag_versions: HashMap<(String, String), u64>,
    tenant_epochs: HashMap<String, u64>,
    decisions: Vec<Decision>,
    pub metrics: Metrics,
}

impl ReferenceEngine {
    pub fn new(quota_bytes: u64) -> Self {
        Self {
            state: State::default(),
            quota_bytes,
            origins: HashMap::new(),
            leases: HashMap::new(),
            journal: None,
            seen_purges: HashSet::new(),
            tag_versions: HashMap::new(),
            tenant_epochs: HashMap::new(),
            decisions: Vec::new(),
            metrics: Metrics::default(),
        }
    }

    pub fn add_blob(&mut self, manifest: BlobManifest) {
        self.state.blobs.insert(
            manifest.digest,
            Blob {
                size: manifest.size,
                verified: manifest.verified,
                refs: 0,
            },
        );
    }

    pub fn add_initial_entry(&mut self, key: CacheKey, mut entry: Entry, at: u64) {
        entry.tags = normalize_tags(&entry.tags);
        let valid = self
            .state
            .blobs
            .get(&entry.body_digest)
            .is_some_and(|blob| blob.verified && blob.size == entry.body_size);
        if valid {
            self.state.attach(key, entry);
        } else {
            self.emit(
                at,
                "recovery",
                "RECOVERY_DROP_UNVERIFIED_INITIAL",
                None,
                None,
                None,
                None,
                None,
            );
        }
    }

    /// Finish loading the initial index and manifest before processing runtime
    /// events. This keeps manifests declared before their entries alive while
    /// still applying the quota to an already-populated cache.
    pub fn finalize_initial(&mut self, at: u64) {
        self.state.garbage_collect();
        self.enforce_quota(at);
    }

    pub fn add_origin(&mut self, id: String, outcome: OriginOutcome) {
        self.origins.insert(id, outcome);
    }

    pub fn request(
        &mut self,
        request_id: String,
        at: u64,
        worker: String,
        key: CacheKey,
        allow_stale_if_error: bool,
        origin_id: String,
    ) {
        self.metrics.requests += 1;
        if let Some(snapshot) = self.state.entries.get(&key).cloned() {
            if !self.state.body_is_servable(&snapshot) {
                self.metrics.unsafe_body_serves += 1;
                let _ = self.state.detach(&key);
                self.emit(
                    at,
                    "request",
                    "MISS_UNVERIFIED_BODY",
                    Some(&key),
                    Some(&worker),
                    None,
                    None,
                    None,
                );
            } else if at < snapshot.fresh_until {
                if let Some(entry) = self.state.entries.get_mut(&key) {
                    entry.last_access = at;
                }
                self.metrics.fresh_hits += 1;
                self.emit(
                    at,
                    "request",
                    "FRESH_HIT",
                    Some(&key),
                    Some(&worker),
                    None,
                    None,
                    None,
                );
                return;
            }
        }

        if let Some(lease_request_id) = self.leases.get(&key).map(|lease| lease.request_id.clone())
        {
            self.metrics.revalidation_waits += 1;
            self.emit(
                at,
                "request",
                "REVALIDATION_WAIT",
                Some(&key),
                Some(&worker),
                Some(&lease_request_id),
                None,
                None,
            );
            return;
        }

        let tenant_epoch = self.tenant_epochs.get(&key.tenant).copied().unwrap_or(0);
        self.leases.insert(
            key.clone(),
            Lease {
                request_id: request_id.clone(),
                key: key.clone(),
                started_at: at,
                tenant_epoch,
                origin_id,
                allow_stale_if_error,
                worker: worker.clone(),
            },
        );
        self.metrics.revalidation_starts += 1;
        let reason = if self.state.entries.contains_key(&key) {
            "REVALIDATION_STARTED"
        } else {
            self.metrics.misses += 1;
            "MISS_REVALIDATION_STARTED"
        };
        self.emit(
            at,
            "request",
            reason,
            Some(&key),
            Some(&worker),
            Some(&request_id),
            None,
            None,
        );
    }

    pub fn complete(&mut self, request_id: &str, at: u64, crash: CrashPoint) -> Result<(), String> {
        let key = self
            .leases
            .iter()
            .find_map(|(key, lease)| (lease.request_id == request_id).then(|| key.clone()))
            .ok_or_else(|| "completion has no active lease".to_string())?;
        let lease = self
            .leases
            .remove(&key)
            .expect("lease found immediately before removal");

        if at < lease.started_at {
            return Err("completion precedes request time".to_string());
        }
        let current_epoch = self.tenant_epochs.get(&key.tenant).copied().unwrap_or(0);
        if current_epoch != lease.tenant_epoch {
            self.emit(
                at,
                "revalidation",
                "REVALIDATION_REJECTED_PURGE",
                Some(&key),
                Some(&lease.worker),
                Some(request_id),
                None,
                None,
            );
            return Ok(());
        }

        let outcome = self
            .origins
            .get(&lease.origin_id)
            .cloned()
            .ok_or_else(|| "completion references unknown origin outcome".to_string())?;
        match outcome {
            OriginOutcome::Error { .. } => {
                let stale_servable = self.state.entries.get(&key).is_some_and(|entry| {
                    lease.allow_stale_if_error
                        && at < entry.stale_until
                        && self.state.body_is_servable(entry)
                });
                if stale_servable {
                    if let Some(entry) = self.state.entries.get_mut(&key) {
                        entry.last_access = at;
                    }
                    self.metrics.stale_responses += 1;
                    self.emit(
                        at,
                        "revalidation",
                        "STALE_IF_ERROR",
                        Some(&key),
                        Some(&lease.worker),
                        Some(request_id),
                        None,
                        None,
                    );
                } else {
                    self.metrics.misses += 1;
                    self.emit(
                        at,
                        "revalidation",
                        "MISS_ORIGIN_ERROR",
                        Some(&key),
                        Some(&lease.worker),
                        Some(request_id),
                        None,
                        None,
                    );
                }
            }
            OriginOutcome::NotModified {
                fresh_for,
                stale_for,
                validator,
            } => {
                let servable = self
                    .state
                    .entries
                    .get(&key)
                    .is_some_and(|entry| self.state.body_is_servable(entry));
                if servable {
                    if let Some(entry) = self.state.entries.get_mut(&key) {
                        entry.fresh_until = at.saturating_add(fresh_for);
                        entry.stale_until = at.saturating_add(stale_for);
                        entry.validator = validator;
                        entry.last_access = at;
                    }
                    self.metrics.recovery_commits += 1;
                    self.emit(
                        at,
                        "revalidation",
                        "REVALIDATION_COMMITTED_304",
                        Some(&key),
                        Some(&lease.worker),
                        Some(request_id),
                        None,
                        None,
                    );
                } else {
                    self.metrics.unsafe_body_serves += 1;
                    self.emit(
                        at,
                        "revalidation",
                        "REVALIDATION_REJECTED_UNVERIFIED",
                        Some(&key),
                        Some(&lease.worker),
                        Some(request_id),
                        None,
                        None,
                    );
                }
            }
            outcome @ OriginOutcome::Modified { .. } => {
                self.commit_modified(lease, at, outcome, crash);
            }
        }
        Ok(())
    }

    fn commit_modified(
        &mut self,
        lease: Lease,
        at: u64,
        outcome: OriginOutcome,
        crash: CrashPoint,
    ) {
        let OriginOutcome::Modified {
            digest,
            size,
            fresh_for,
            stale_for,
            tags,
            validator,
            verified,
        } = outcome
        else {
            unreachable!("commit_modified only accepts modified outcomes");
        };
        let key = lease.key.clone();
        self.journal = Some(Journal {
            request_id: lease.request_id.clone(),
            key: key.clone(),
            digest: digest.clone(),
            phase: CommitPhase::Prepared,
        });

        let manifest_matches = self
            .state
            .blobs
            .get(&digest)
            .is_none_or(|blob| blob.size == size);
        if !manifest_matches {
            self.journal = None;
            self.emit(
                at,
                "revalidation",
                "REVALIDATION_REJECTED_BODY_SIZE",
                Some(&key),
                Some(&lease.worker),
                Some(&lease.request_id),
                Some(&digest),
                None,
            );
            return;
        }
        self.state.blobs.entry(digest.clone()).or_insert(Blob {
            size,
            verified: false,
            refs: 0,
        });
        if crash == CrashPoint::AfterPrepare {
            self.recover(at);
            return;
        }

        let already_verified = self
            .state
            .blobs
            .get(&digest)
            .is_some_and(|blob| blob.verified && blob.size == size);
        if !already_verified && let Some(blob) = self.state.blobs.get_mut(&digest) {
            blob.verified = verified;
        }
        if let Some(journal) = self.journal.as_mut() {
            journal.phase = CommitPhase::BodyCommitted;
        }
        if crash == CrashPoint::AfterBodyCommit {
            self.recover(at);
            return;
        }

        if !verified {
            self.journal = None;
            self.state.garbage_collect();
            self.metrics.unsafe_body_serves += 1;
            self.emit(
                at,
                "revalidation",
                "REVALIDATION_REJECTED_UNVERIFIED",
                Some(&key),
                Some(&lease.worker),
                Some(&lease.request_id),
                Some(&digest),
                None,
            );
            return;
        }

        let entry = Entry {
            fresh_until: at.saturating_add(fresh_for),
            stale_until: at.saturating_add(stale_for),
            validator,
            body_digest: digest.clone(),
            body_size: size,
            tags: normalize_tags(&tags),
            last_access: at,
        };
        self.state.attach(key.clone(), entry);
        if let Some(journal) = self.journal.as_mut() {
            journal.phase = CommitPhase::MetadataCommitted;
        }
        if crash == CrashPoint::AfterMetadataCommit {
            self.recover(at);
            return;
        }

        self.journal = None;
        self.state.garbage_collect();
        self.enforce_quota(at);
        self.metrics.recovery_commits += 1;
        self.emit(
            at,
            "revalidation",
            "REVALIDATION_COMMITTED_200",
            Some(&key),
            Some(&lease.worker),
            Some(&lease.request_id),
            Some(&digest),
            None,
        );
    }

    pub fn purge(&mut self, at: u64, tenant: String, seq: u64, tag: String) {
        let normalized_tag = tag.trim().to_ascii_lowercase();
        let purge_key = (tenant.clone(), seq, normalized_tag.clone());
        if !self.seen_purges.insert(purge_key) {
            self.metrics.purges_ignored += 1;
            self.emit(
                at,
                "purge",
                "PURGE_DUPLICATE_IGNORED",
                None,
                None,
                None,
                None,
                None,
            );
            return;
        }

        let previous = self
            .tag_versions
            .get(&(tenant.clone(), normalized_tag.clone()))
            .copied()
            .unwrap_or(0);
        if seq < previous {
            self.metrics.purges_ignored += 1;
            self.emit(
                at,
                "purge",
                "PURGE_REORDERED_IGNORED",
                None,
                None,
                None,
                None,
                None,
            );
            return;
        }
        if seq > previous {
            self.tag_versions
                .insert((tenant.clone(), normalized_tag.clone()), seq);
        }
        let epoch = self.tenant_epochs.entry(tenant.clone()).or_default();
        *epoch += 1;

        let index_key = (tenant.clone(), normalized_tag.clone());
        let victims: Vec<CacheKey> = self
            .state
            .tag_index
            .get(&index_key)
            .map(|keys| keys.iter().cloned().collect())
            .unwrap_or_default();
        for key in &victims {
            let _ = self.state.detach(key);
        }
        self.state.garbage_collect();
        self.metrics.purges_applied += 1;
        self.emit(at, "purge", "PURGE_APPLIED", None, None, None, None, None);
    }

    pub fn recover(&mut self, at: u64) {
        let Some(journal) = self.journal.take() else {
            self.audit_references(at);
            return;
        };
        match journal.phase {
            CommitPhase::Prepared | CommitPhase::BodyCommitted => {
                if self
                    .state
                    .blobs
                    .get(&journal.digest)
                    .is_some_and(|blob| blob.refs == 0)
                {
                    self.state.blobs.remove(&journal.digest);
                }
                self.state.rebuild_refs();
                self.metrics.recovery_rollbacks += 1;
                self.emit(
                    at,
                    "recovery",
                    "RECOVERY_ROLLBACK_UNCOMMITTED",
                    Some(&journal.key),
                    None,
                    Some(&journal.request_id),
                    Some(&journal.digest),
                    None,
                );
            }
            CommitPhase::MetadataCommitted => {
                self.audit_references(at);
                self.metrics.recovery_commits += 1;
                self.emit(
                    at,
                    "recovery",
                    "RECOVERY_RETAIN_COMMITTED",
                    Some(&journal.key),
                    None,
                    Some(&journal.request_id),
                    Some(&journal.digest),
                    None,
                );
            }
        }
        self.state.garbage_collect();
        self.enforce_quota(at);
    }

    fn audit_references(&mut self, at: u64) {
        let invalid: Vec<CacheKey> = self
            .state
            .entries
            .iter()
            .filter_map(|(key, entry)| (!self.state.body_is_servable(entry)).then_some(key.clone()))
            .collect();
        for key in invalid {
            let _ = self.state.detach(&key);
            self.metrics.unsafe_body_serves += 1;
            self.emit(
                at,
                "recovery",
                "RECOVERY_DROP_UNVERIFIED",
                Some(&key),
                None,
                None,
                None,
                None,
            );
        }
        self.state.rebuild_refs();
    }

    fn enforce_quota(&mut self, at: u64) {
        self.state.garbage_collect();
        while self.state.committed_usage_bytes() > self.quota_bytes {
            let Some(victim) = self
                .state
                .entries
                .iter()
                .min_by_key(|(key, entry)| (entry.last_access, key.id()))
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            let body = self
                .state
                .entries
                .get(&victim)
                .map(|entry| entry.body_digest.clone());
            let _ = self.state.detach(&victim);
            self.state.garbage_collect();
            self.metrics.quota_evictions += 1;
            self.emit(
                at,
                "eviction",
                "QUOTA_EVICT_LRU",
                Some(&victim),
                None,
                None,
                body.as_deref(),
                None,
            );
        }
    }

    pub fn drain_decisions(&mut self) -> Vec<Decision> {
        std::mem::take(&mut self.decisions)
    }

    pub fn final_index(&self) -> Vec<FinalEntry> {
        let mut result: Vec<FinalEntry> = self
            .state
            .entries
            .iter()
            .map(|(key, entry)| FinalEntry {
                tenant_id: hash_id(&key.tenant),
                key_id: key.id(),
                body_id: hash_id(&entry.body_digest),
                fresh_until: entry.fresh_until,
                stale_until: entry.stale_until,
                body_size: entry.body_size,
                tags: entry.tags.iter().map(|tag| hash_id(tag)).collect(),
            })
            .collect();
        result.sort_by(|a, b| a.key_id.cmp(&b.key_id));
        result
    }

    pub fn committed_usage_bytes(&self) -> u64 {
        self.state.committed_usage_bytes()
    }

    pub fn quota_bytes(&self) -> u64 {
        self.quota_bytes
    }

    pub fn active_lease_count(&self) -> usize {
        self.leases.len()
    }

    pub fn modeled_reference_integrity_holds(&self) -> bool {
        self.state
            .entries
            .values()
            .all(|entry| self.state.body_is_servable(entry))
    }

    #[allow(clippy::too_many_arguments)]
    fn emit(
        &mut self,
        at: u64,
        event: &'static str,
        reason: &'static str,
        key: Option<&CacheKey>,
        actor: Option<&str>,
        lease: Option<&str>,
        body: Option<&str>,
        _tag: Option<&str>,
    ) {
        self.decisions.push(Decision {
            at,
            event,
            reason,
            tenant_id: key.map(|key| hash_id(&key.tenant)),
            key_id: key.map(CacheKey::id),
            actor_id: actor.map(hash_id),
            lease_id: lease.map(hash_id),
            body_id: body.map(hash_id),
            committed_usage_bytes: self.state.committed_usage_bytes(),
        });
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FinalEntry {
    pub tenant_id: String,
    pub key_id: String,
    pub body_id: String,
    pub fresh_until: u64,
    pub stale_until: u64,
    pub body_size: u64,
    pub tags: Vec<String>,
}

/// A deliberately small model of the stated baseline. It is only used to
/// reproduce the four known failure classes; it is not a claim about the
/// unavailable production gateway.
#[derive(Clone, Debug, Default)]
pub struct BaselineReport {
    pub wrong_variant_served: bool,
    pub cross_tenant_collision: bool,
    pub duplicate_revalidations: u64,
    pub purge_left_entry_servable: bool,
    pub unverified_body_served_after_crash: bool,
    pub missing_body_served_after_crash: bool,
}

#[derive(Clone, Debug)]
struct BaselineEntry {
    tenant: String,
    vary: String,
    fresh_until: u64,
    digest: String,
    size: u64,
    tags: Vec<String>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct BaselineKey {
    method: String,
    url: String,
}

impl BaselineKey {
    fn new(method: &str, url: &str) -> Self {
        Self {
            method: method.trim().to_ascii_uppercase(),
            url: url.trim().to_string(),
        }
    }
}

/// Replays the baseline's independent failure modes without pretending they
/// are one coherent implementation.
pub fn baseline_reproduction() -> BaselineReport {
    let mut report = BaselineReport::default();
    let key = BaselineKey::new("GET", "https://cache.test/item");
    let mut entries = HashMap::new();
    entries.insert(
        key.clone(),
        BaselineEntry {
            tenant: "tenant-a".to_string(),
            vary: "en".to_string(),
            fresh_until: 100,
            digest: "body-en".to_string(),
            size: 10,
            tags: vec!["article".to_string()],
        },
    );
    entries.insert(
        key.clone(),
        BaselineEntry {
            tenant: "tenant-b".to_string(),
            vary: "fr".to_string(),
            fresh_until: 100,
            digest: "body-fr".to_string(),
            size: 10,
            tags: vec!["article".to_string()],
        },
    );
    report.cross_tenant_collision = entries
        .get(&key)
        .is_some_and(|entry| entry.tenant == "tenant-b");
    report.wrong_variant_served = entries.get(&key).is_some_and(|entry| entry.vary != "en");

    let mut active_revalidations = 0u64;
    for _ in 0..2 {
        if entries
            .get(&key)
            .is_some_and(|entry| entry.fresh_until <= 100)
        {
            active_revalidations += 1;
        }
    }
    report.duplicate_revalidations = active_revalidations;

    // The baseline only knows exact URLs. A tag purge cannot find this row.
    report.purge_left_entry_servable = entries.get(&key).is_some_and(|entry| {
        entry.tags.iter().any(|tag| tag == "article") && entry.fresh_until > 50
    });

    // Split commit failure A: metadata points at a body before verification.
    let mut bodies = HashMap::from([(String::from("body-fr"), true)]);
    let mut metadata = entries.remove(&key).expect("baseline fixture entry");
    metadata.digest = "body-new".to_string();
    metadata.size = 11;
    bodies.insert("body-new".to_string(), false);
    report.unverified_body_served_after_crash =
        !bodies.get(&metadata.digest).copied().unwrap_or(false);

    // Split commit failure B: old body is removed before metadata switches.
    let old_digest = String::from("body-old");
    let mut old_bodies = HashSet::from([old_digest.clone()]);
    old_bodies.remove(&old_digest);
    report.missing_body_served_after_crash = !old_bodies.contains(&old_digest);
    report
}

#[derive(Clone, Debug)]
pub enum Event {
    Blob(BlobManifest),
    Entry {
        at: u64,
        key: CacheKey,
        entry: Entry,
    },
    Origin {
        id: String,
        outcome: OriginOutcome,
    },
    Request {
        id: String,
        at: u64,
        worker: String,
        key: CacheKey,
        allow_stale_if_error: bool,
        origin_id: String,
    },
    Complete {
        request_id: String,
        at: u64,
        crash: CrashPoint,
    },
    Purge {
        at: u64,
        tenant: String,
        seq: u64,
        tag: String,
    },
    Recover {
        at: u64,
    },
}

#[derive(Debug)]
pub struct ParseError {
    pub line: usize,
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "trace line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for ParseError {}

fn token<'a>(
    tokens: &'a [&str],
    index: usize,
    line: usize,
    name: &str,
) -> Result<&'a str, ParseError> {
    tokens.get(index).copied().ok_or_else(|| ParseError {
        line,
        message: format!("missing {name}"),
    })
}

fn parse_u64(value: &str, line: usize, name: &str) -> Result<u64, ParseError> {
    value.parse().map_err(|_| ParseError {
        line,
        message: format!("invalid {name}"),
    })
}

fn parse_bool(value: &str, line: usize, name: &str) -> Result<bool, ParseError> {
    match value {
        "allow" | "verified" => Ok(true),
        "deny" | "unverified" => Ok(false),
        _ => Err(ParseError {
            line,
            message: format!("invalid {name}"),
        }),
    }
}

/// Parse the line-oriented trace format documented in the trial README.
pub fn parse_trace(input: &str) -> Result<(u64, Vec<Event>), ParseError> {
    let mut quota_bytes = 64 * 1024 * 1024 * 1024;
    let mut events = Vec::new();
    for (line_index, raw_line) in input.lines().enumerate() {
        let line = line_index + 1;
        let content = raw_line
            .split_once('#')
            .map_or(raw_line, |(head, _)| head)
            .trim();
        if content.is_empty() {
            continue;
        }
        let tokens: Vec<&str> = content.split_whitespace().collect();
        let expected_tokens = match tokens.first().copied() {
            Some("quota") => Some(2),
            Some("blob") => Some(4),
            Some("entry") => Some(12),
            Some("origin") => match tokens.get(2).copied() {
                Some("error") => Some(4),
                Some("not_modified") => Some(6),
                Some("modified") => Some(10),
                _ => None,
            },
            Some("request") => Some(10),
            Some("complete") => Some(4),
            Some("purge") => Some(5),
            Some("recover") => Some(2),
            _ => None,
        };
        if let Some(expected_tokens) = expected_tokens
            && tokens.len() != expected_tokens
        {
            return Err(ParseError {
                line,
                message: format!(
                    "event has {} fields, expected {expected_tokens}",
                    tokens.len()
                ),
            });
        }
        match token(&tokens, 0, line, "event")? {
            "quota" => {
                quota_bytes = parse_u64(token(&tokens, 1, line, "quota")?, line, "quota")?;
            }
            "blob" => {
                let digest = token(&tokens, 1, line, "digest")?.to_string();
                let size = parse_u64(token(&tokens, 2, line, "size")?, line, "size")?;
                let verified = parse_bool(
                    token(&tokens, 3, line, "verification")?,
                    line,
                    "verification",
                )?;
                events.push(Event::Blob(BlobManifest {
                    digest,
                    size,
                    verified,
                }));
            }
            "entry" => {
                let tenant = token(&tokens, 1, line, "tenant")?;
                let method = token(&tokens, 2, line, "method")?;
                let url = token(&tokens, 3, line, "url")?;
                let vary = token(&tokens, 4, line, "vary")?;
                let fresh_until =
                    parse_u64(token(&tokens, 5, line, "fresh_until")?, line, "fresh_until")?;
                let stale_until =
                    parse_u64(token(&tokens, 6, line, "stale_until")?, line, "stale_until")?;
                let digest = token(&tokens, 7, line, "digest")?.to_string();
                let body_size =
                    parse_u64(token(&tokens, 8, line, "body_size")?, line, "body_size")?;
                let last_access =
                    parse_u64(token(&tokens, 9, line, "last_access")?, line, "last_access")?;
                let tags = parse_tags(token(&tokens, 10, line, "tags")?)
                    .map_err(|message| ParseError { line, message })?;
                let validator = token(&tokens, 11, line, "validator")?.to_string();
                events.push(Event::Entry {
                    at: 0,
                    key: CacheKey::new(tenant, method, url, vary),
                    entry: Entry {
                        fresh_until,
                        stale_until,
                        validator,
                        body_digest: digest,
                        body_size,
                        tags,
                        last_access,
                    },
                });
            }
            "origin" => {
                let id = token(&tokens, 1, line, "origin id")?.to_string();
                let status = token(&tokens, 2, line, "origin status")?;
                let outcome = match status {
                    "error" => OriginOutcome::Error {
                        code: token(&tokens, 3, line, "origin error")?.to_string(),
                    },
                    "not_modified" => OriginOutcome::NotModified {
                        fresh_for: parse_u64(
                            token(&tokens, 3, line, "fresh_for")?,
                            line,
                            "fresh_for",
                        )?,
                        stale_for: parse_u64(
                            token(&tokens, 4, line, "stale_for")?,
                            line,
                            "stale_for",
                        )?,
                        validator: token(&tokens, 5, line, "validator")?.to_string(),
                    },
                    "modified" => OriginOutcome::Modified {
                        digest: token(&tokens, 3, line, "digest")?.to_string(),
                        size: parse_u64(token(&tokens, 4, line, "size")?, line, "size")?,
                        fresh_for: parse_u64(
                            token(&tokens, 5, line, "fresh_for")?,
                            line,
                            "fresh_for",
                        )?,
                        stale_for: parse_u64(
                            token(&tokens, 6, line, "stale_for")?,
                            line,
                            "stale_for",
                        )?,
                        tags: parse_tags(token(&tokens, 7, line, "tags")?)
                            .map_err(|message| ParseError { line, message })?,
                        validator: token(&tokens, 8, line, "validator")?.to_string(),
                        verified: parse_bool(
                            token(&tokens, 9, line, "verification")?,
                            line,
                            "verification",
                        )?,
                    },
                    _ => {
                        return Err(ParseError {
                            line,
                            message: "unknown origin status".to_string(),
                        });
                    }
                };
                events.push(Event::Origin { id, outcome });
            }
            "request" => {
                let id = token(&tokens, 1, line, "request id")?.to_string();
                let at = parse_u64(
                    token(&tokens, 2, line, "request time")?,
                    line,
                    "request time",
                )?;
                let worker = token(&tokens, 3, line, "worker")?.to_string();
                let tenant = token(&tokens, 4, line, "tenant")?;
                let method = token(&tokens, 5, line, "method")?;
                let url = token(&tokens, 6, line, "url")?;
                let vary = token(&tokens, 7, line, "vary")?;
                let allow_stale_if_error = parse_bool(
                    token(&tokens, 8, line, "stale policy")?,
                    line,
                    "stale policy",
                )?;
                let origin_id = token(&tokens, 9, line, "origin id")?.to_string();
                events.push(Event::Request {
                    id,
                    at,
                    worker,
                    key: CacheKey::new(tenant, method, url, vary),
                    allow_stale_if_error,
                    origin_id,
                });
            }
            "complete" => {
                events.push(Event::Complete {
                    request_id: token(&tokens, 1, line, "request id")?.to_string(),
                    at: parse_u64(
                        token(&tokens, 2, line, "completion time")?,
                        line,
                        "completion time",
                    )?,
                    crash: CrashPoint::parse(token(&tokens, 3, line, "crash point")?)
                        .map_err(|message| ParseError { line, message })?,
                });
            }
            "purge" => {
                events.push(Event::Purge {
                    at: parse_u64(token(&tokens, 1, line, "purge time")?, line, "purge time")?,
                    tenant: token(&tokens, 2, line, "tenant")?.to_string(),
                    seq: parse_u64(token(&tokens, 3, line, "sequence")?, line, "sequence")?,
                    tag: token(&tokens, 4, line, "tag")?.to_string(),
                });
            }
            "recover" => events.push(Event::Recover {
                at: parse_u64(
                    token(&tokens, 1, line, "recovery time")?,
                    line,
                    "recovery time",
                )?,
            }),
            _ => {
                return Err(ParseError {
                    line,
                    message: "unknown event".to_string(),
                });
            }
        }
    }
    Ok((quota_bytes, events))
}

/// Apply a parsed trace to the reference model. Decision records remain
/// buffered until the caller drains them, allowing the CLI to stream output.
pub fn apply_events(engine: &mut ReferenceEngine, events: &[Event]) -> Result<(), String> {
    let mut finalized_initial = false;
    for event in events {
        if !finalized_initial
            && matches!(
                event,
                Event::Request { .. }
                    | Event::Complete { .. }
                    | Event::Purge { .. }
                    | Event::Recover { .. }
            )
        {
            engine.finalize_initial(0);
            finalized_initial = true;
        }
        match event {
            Event::Blob(blob) => engine.add_blob(blob.clone()),
            Event::Entry { at, key, entry } => {
                engine.add_initial_entry(key.clone(), entry.clone(), *at)
            }
            Event::Origin { id, outcome } => engine.add_origin(id.clone(), outcome.clone()),
            Event::Request {
                id,
                at,
                worker,
                key,
                allow_stale_if_error,
                origin_id,
            } => engine.request(
                id.clone(),
                *at,
                worker.clone(),
                key.clone(),
                *allow_stale_if_error,
                origin_id.clone(),
            ),
            Event::Complete {
                request_id,
                at,
                crash,
            } => engine.complete(request_id, *at, *crash)?,
            Event::Purge {
                at,
                tenant,
                seq,
                tag,
            } => engine.purge(*at, tenant.clone(), *seq, tag.clone()),
            Event::Recover { at } => engine.recover(*at),
        }
    }
    if !finalized_initial {
        engine.finalize_initial(0);
    }
    Ok(())
}

/// Run a line-oriented trace file, returning the model and the original
/// parsed events for reporting.
pub fn run_trace_file(path: &Path) -> Result<(ReferenceEngine, Vec<Event>), String> {
    let input = fs::read_to_string(path).map_err(|_| "could not read trace file".to_string())?;
    let (quota, events) = parse_trace(&input).map_err(|error| error.to_string())?;
    let mut engine = ReferenceEngine::new(quota);
    apply_events(&mut engine, &events)?;
    Ok((engine, events))
}

/// A compact allocation-limited shape harness. It does not replace semantic
/// tests; it exercises the stated object/request/purge counts without storing
/// URLs, bodies, or a million output records in memory.
#[derive(Clone, Copy)]
struct CompactEntry {
    body_size: u32,
    fresh_until: u64,
    stale_until: u64,
    last_access: u32,
    tag_mask: u16,
    alive: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkloadReport {
    pub objects: usize,
    pub requests: usize,
    pub purges: usize,
    pub request_hits: u64,
    pub request_misses: u64,
    pub committed_usage_bytes: u64,
    pub quota_bytes: u64,
}

pub fn run_shape_workload(objects: usize, requests: usize, purges: usize) -> WorkloadReport {
    let quota_bytes = 64 * 1024 * 1024 * 1024;
    let mut entries = Vec::with_capacity(objects);
    for index in 0..objects {
        entries.push(CompactEntry {
            body_size: 4 * 1024,
            fresh_until: 100,
            stale_until: 200,
            last_access: 0,
            tag_mask: 1u16 << (index % 16),
            alive: true,
        });
    }

    let mut purged_tags = 0u16;
    for index in 0..purges {
        purged_tags |= 1u16 << (index % 16);
    }

    let mut request_hits = 0u64;
    let mut request_misses = 0u64;
    if objects != 0 {
        for request in 0..requests {
            let index = request.wrapping_mul(1_103_515_245).wrapping_add(12_345) % objects;
            let entry = &mut entries[index];
            if entry.alive
                && (entry.tag_mask & purged_tags) == 0
                && request as u64 % 257 < entry.fresh_until
            {
                entry.last_access = request as u32;
                request_hits += 1;
            } else {
                request_misses += 1;
                if request as u64 >= entry.stale_until {
                    entry.fresh_until = request as u64 + 100;
                    entry.stale_until = request as u64 + 200;
                }
            }
        }
    }

    let live_objects = entries.iter().filter(|entry| entry.alive).count() as u64;
    let body_bytes = entries
        .iter()
        .filter(|entry| entry.alive)
        .map(|entry| u64::from(entry.body_size))
        .sum::<u64>();
    let metadata_bytes = live_objects * 224;
    WorkloadReport {
        objects,
        requests,
        purges,
        request_hits,
        request_misses,
        committed_usage_bytes: body_bytes + metadata_bytes,
        quota_bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(tenant: &str, vary: &str) -> CacheKey {
        CacheKey::new(tenant, "get", "https://cache.test/item#ignored", vary)
    }

    fn seed_engine(quota: u64) -> ReferenceEngine {
        let mut engine = ReferenceEngine::new(quota);
        engine.add_blob(BlobManifest {
            digest: "body-a".to_string(),
            size: 10,
            verified: true,
        });
        engine.add_initial_entry(
            key("tenant-a", "en"),
            Entry {
                fresh_until: 10,
                stale_until: 30,
                validator: "etag-a".to_string(),
                body_digest: "body-a".to_string(),
                body_size: 10,
                tags: vec!["article".to_string()],
                last_access: 0,
            },
            0,
        );
        engine
    }

    #[test]
    fn sha256_identifier_is_stable() {
        assert_eq!(
            hash_id("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn key_normalization_includes_tenant_and_vary() {
        let a = CacheKey::new("tenant-a", "get", " https://x.test/a#fragment", "EN");
        let b = CacheKey::new("tenant-a", "GET", "https://x.test/a", "en");
        let c = CacheKey::new("tenant-b", "GET", "https://x.test/a", "en");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_ne!(
            a.id(),
            CacheKey::new("tenant-a", "GET", "https://x.test/a", "fr").id()
        );
    }

    #[test]
    fn concurrent_expiry_has_one_active_lease() {
        let mut engine = seed_engine(1_000_000);
        engine.add_origin(
            "slow".to_string(),
            OriginOutcome::NotModified {
                fresh_for: 100,
                stale_for: 200,
                validator: "etag-b".to_string(),
            },
        );
        let cache_key = key("tenant-a", "en");
        engine.request(
            "r1".to_string(),
            10,
            "worker-1".to_string(),
            cache_key.clone(),
            true,
            "slow".to_string(),
        );
        for worker in 2..=32 {
            engine.request(
                format!("r{worker}"),
                10,
                format!("worker-{worker}"),
                cache_key.clone(),
                true,
                "slow".to_string(),
            );
        }
        assert_eq!(engine.active_lease_count(), 1);
        assert_eq!(engine.metrics.revalidation_starts, 1);
        assert_eq!(engine.metrics.revalidation_waits, 31);
    }

    #[test]
    fn purge_is_tagged_sequence_aware_and_reordered_events_converge() {
        let mut engine = seed_engine(1_000_000);
        let cache_key = key("tenant-a", "en");
        engine.purge(10, "tenant-a".to_string(), 2, "article".to_string());
        engine.add_blob(BlobManifest {
            digest: "body-a".to_string(),
            size: 10,
            verified: true,
        });
        engine.add_initial_entry(
            cache_key.clone(),
            Entry {
                fresh_until: 100,
                stale_until: 200,
                validator: "etag-new".to_string(),
                body_digest: "body-a".to_string(),
                body_size: 10,
                tags: vec!["article".to_string()],
                last_access: 10,
            },
            10,
        );
        engine.purge(11, "tenant-a".to_string(), 1, "article".to_string());
        engine.purge(12, "tenant-a".to_string(), 2, "article".to_string());
        assert_eq!(engine.final_index().len(), 1);
        assert_eq!(engine.metrics.purges_ignored, 2);
        assert_eq!(engine.metrics.purges_applied, 1);
        assert!(
            engine
                .final_index()
                .iter()
                .any(|entry| entry.key_id == cache_key.id())
        );
    }

    #[test]
    fn purge_rejects_an_old_revalidation_result() {
        let mut engine = seed_engine(1_000_000);
        engine.add_origin(
            "modified".to_string(),
            OriginOutcome::Modified {
                digest: "body-b".to_string(),
                size: 11,
                fresh_for: 100,
                stale_for: 200,
                tags: vec!["article".to_string()],
                validator: "etag-b".to_string(),
                verified: true,
            },
        );
        let cache_key = key("tenant-a", "en");
        engine.request(
            "r".to_string(),
            10,
            "worker".to_string(),
            cache_key.clone(),
            true,
            "modified".to_string(),
        );
        engine.purge(11, "tenant-a".to_string(), 1, "article".to_string());
        engine.complete("r", 12, CrashPoint::None).unwrap();
        assert!(engine.final_index().is_empty());
        assert!(
            engine
                .drain_decisions()
                .iter()
                .any(|decision| decision.reason == "REVALIDATION_REJECTED_PURGE")
        );
    }

    #[test]
    fn stale_if_error_is_only_served_inside_window_and_policy() {
        let mut engine = seed_engine(1_000_000);
        engine.add_origin(
            "error".to_string(),
            OriginOutcome::Error {
                code: "origin-down".to_string(),
            },
        );
        let cache_key = key("tenant-a", "en");
        engine.request(
            "r1".to_string(),
            20,
            "worker".to_string(),
            cache_key.clone(),
            true,
            "error".to_string(),
        );
        engine.complete("r1", 21, CrashPoint::None).unwrap();
        engine.request(
            "r2".to_string(),
            31,
            "worker".to_string(),
            cache_key,
            true,
            "error".to_string(),
        );
        engine.complete("r2", 32, CrashPoint::None).unwrap();
        assert_eq!(engine.metrics.stale_responses, 1);
        assert_eq!(engine.metrics.misses, 1);
    }

    #[test]
    fn every_crash_point_recovers_without_an_unverified_body() {
        for crash in [
            CrashPoint::AfterPrepare,
            CrashPoint::AfterBodyCommit,
            CrashPoint::AfterMetadataCommit,
        ] {
            let mut engine = seed_engine(1_000_000);
            engine.add_origin(
                "modified".to_string(),
                OriginOutcome::Modified {
                    digest: format!("body-{crash:?}"),
                    size: 11,
                    fresh_for: 100,
                    stale_for: 200,
                    tags: vec!["article".to_string()],
                    validator: "etag-new".to_string(),
                    verified: true,
                },
            );
            let cache_key = key("tenant-a", "en");
            engine.request(
                "r".to_string(),
                10,
                "worker".to_string(),
                cache_key.clone(),
                true,
                "modified".to_string(),
            );
            engine.complete("r", 11, crash).unwrap();
            assert!(
                engine.modeled_reference_integrity_holds(),
                "crash point {crash:?}"
            );
            assert_eq!(engine.final_index().len(), 1);
        }
    }

    #[test]
    fn quota_eviction_keeps_usage_bounded() {
        let mut engine = seed_engine(300);
        engine.add_origin(
            "modified".to_string(),
            OriginOutcome::Modified {
                digest: "body-b".to_string(),
                size: 200,
                fresh_for: 100,
                stale_for: 200,
                tags: Vec::new(),
                validator: "etag-b".to_string(),
                verified: true,
            },
        );
        let cache_key = key("tenant-a", "en");
        engine.request(
            "r".to_string(),
            10,
            "worker".to_string(),
            cache_key,
            true,
            "modified".to_string(),
        );
        engine.complete("r", 11, CrashPoint::None).unwrap();
        assert!(engine.committed_usage_bytes() <= engine.quota_bytes());
        assert!(engine.metrics.quota_evictions > 0);
    }

    #[test]
    fn baseline_reproduction_exposes_expected_failures() {
        let report = baseline_reproduction();
        assert!(report.wrong_variant_served);
        assert!(report.cross_tenant_collision);
        assert_eq!(report.duplicate_revalidations, 2);
        assert!(report.purge_left_entry_servable);
        assert!(report.unverified_body_served_after_crash);
        assert!(report.missing_body_served_after_crash);
    }

    #[test]
    fn trace_parser_rejects_trailing_fields_and_excess_tags() {
        assert!(parse_trace("quota 100 extra\n").is_err());
        let tags = (0..17)
            .map(|index| format!("tag{index}"))
            .collect::<Vec<_>>()
            .join(",");
        let trace = format!(
            "blob body 10 verified\nentry tenant-a GET https://x.test/a en 10 20 body 10 0 {tags} etag\n"
        );
        assert!(parse_trace(&trace).is_err());
    }

    #[test]
    fn trace_parser_accepts_the_small_format() {
        let input = "\
quota 1000000\n\
blob body-a 10 verified\n\
entry tenant-a GET https://cache.test/item en 10 30 body-a 10 0 article etag-a\n\
origin ok not_modified 100 200 etag-b\n\
request r1 10 worker-1 tenant-a GET https://cache.test/item EN allow ok\n\
complete r1 11 none\n";
        let (quota, events) = parse_trace(input).unwrap();
        assert_eq!(quota, 1_000_000);
        assert_eq!(events.len(), 5);
    }

    #[test]
    fn shape_workload_reports_logical_usage() {
        let report = run_shape_workload(1_000, 10_000, 100);
        assert_eq!(report.objects, 1_000);
        assert_eq!(report.requests, 10_000);
        assert!(report.committed_usage_bytes < report.quota_bytes);
    }
}
