//! A deterministic, in-memory webhook delivery scheduler prototype.
//!
//! This is a scheduler state machine, not an HTTP client or a database
//! adapter. A caller supplies timestamped inputs and HTTP outcomes; the
//! scheduler returns durable send decisions, retry deadlines, statuses, and
//! metrics. The durable snapshot is cloned in memory so crash/restart behavior
//! can be exercised without pretending that this is PostgreSQL persistence.

use std::collections::{BTreeMap, VecDeque};

pub type EventId = u64;
pub type TenantId = u16;
pub type EndpointId = u16;
pub type AttemptId = u64;
pub type TimestampMs = u64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Event {
    pub id: EventId,
    pub tenant_id: TenantId,
    pub endpoint_id: EndpointId,
    pub created_at_ms: TimestampMs,
    pub ttl_ms: TimestampMs,
    pub payload_bytes: usize,
    pub retryable: bool,
}

impl Event {
    pub fn expires_at_ms(&self) -> TimestampMs {
        self.created_at_ms.saturating_add(self.ttl_ms)
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum DeliveryStatus {
    Pending,
    InFlight,
    RetryScheduled,
    Delivered,
    NonRetryableFailed,
    Expired,
    DeadLettered,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HttpOutcome {
    Success,
    RetryableFailure,
    NonRetryableFailure,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Delivery {
    pub event: Event,
    pub status: DeliveryStatus,
    pub attempt_count: u32,
    pub retry_at_ms: Option<TimestampMs>,
    pub delivered_at_ms: Option<TimestampMs>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SchedulerConfig {
    pub worker_limit: usize,
    pub per_endpoint_in_flight_limit: usize,
    pub per_tenant_in_flight_limit: usize,
    pub endpoint_min_interval_ms: TimestampMs,
    pub tenant_min_interval_ms: TimestampMs,
    pub retry_base_ms: TimestampMs,
    pub retry_cap_ms: TimestampMs,
    pub max_attempts: u32,
    pub endpoint_queue_budget: usize,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            worker_limit: 32,
            per_endpoint_in_flight_limit: 1,
            per_tenant_in_flight_limit: 2,
            endpoint_min_interval_ms: 100,
            tenant_min_interval_ms: 100,
            retry_base_ms: 1_000,
            retry_cap_ms: 300_000,
            max_attempts: 20,
            endpoint_queue_budget: 64,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EnqueueResult {
    Queued,
    DeadLetteredQueueBudget,
    RejectedDuplicate,
    RejectedUnknownEndpoint,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Observation {
    SendDecision {
        attempt_id: AttemptId,
        event_id: EventId,
        tenant_id: TenantId,
        endpoint_id: EndpointId,
        attempt: u32,
        at_ms: TimestampMs,
    },
    RetryScheduled {
        event_id: EventId,
        attempt: u32,
        at_ms: TimestampMs,
        retry_at_ms: TimestampMs,
    },
    StatusChanged {
        event_id: EventId,
        status: DeliveryStatus,
        at_ms: TimestampMs,
    },
    IgnoredOutcome {
        attempt_id: AttemptId,
        at_ms: TimestampMs,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TraceEvent {
    Enqueue {
        at_ms: TimestampMs,
        event: Event,
    },
    Advance {
        at_ms: TimestampMs,
    },
    EndpointAvailability {
        at_ms: TimestampMs,
        endpoint_id: EndpointId,
        available: bool,
    },
    HttpOutcome {
        at_ms: TimestampMs,
        attempt_id: AttemptId,
        outcome: HttpOutcome,
    },
    WorkerCrash {
        at_ms: TimestampMs,
    },
    WorkerRestart {
        at_ms: TimestampMs,
    },
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Metrics {
    pub sends: usize,
    pub delivered: usize,
    pub requeued_after_crash: usize,
    pub ignored_outcomes: usize,
    pub max_endpoint_occupancy: BTreeMap<EndpointId, usize>,
    pub delivery_latency_ms: BTreeMap<TenantId, Vec<TimestampMs>>,
    pub current_status_counts: BTreeMap<DeliveryStatus, usize>,
}

impl Metrics {
    pub fn p99_latency_ms(&self, tenant_id: TenantId) -> Option<TimestampMs> {
        let values = self.delivery_latency_ms.get(&tenant_id)?;
        if values.is_empty() {
            return None;
        }
        let mut sorted = values.clone();
        sorted.sort_unstable();
        let index = (sorted.len() * 99).div_ceil(100).saturating_sub(1);
        sorted.get(index).copied()
    }
}

#[derive(Clone, Debug)]
struct EndpointState {
    available: bool,
    next_send_at_ms: TimestampMs,
    in_flight: usize,
    queue: VecDeque<EventId>,
}

impl Default for EndpointState {
    fn default() -> Self {
        Self {
            available: true,
            next_send_at_ms: 0,
            in_flight: 0,
            queue: VecDeque::new(),
        }
    }
}

#[derive(Clone, Debug, Default)]
struct TenantState {
    next_send_at_ms: TimestampMs,
    in_flight: usize,
}

#[derive(Clone, Debug)]
struct InFlight {
    event_id: EventId,
    tenant_id: TenantId,
    endpoint_id: EndpointId,
}

#[derive(Clone, Debug)]
struct DurableState {
    now_ms: TimestampMs,
    deliveries: BTreeMap<EventId, Delivery>,
    endpoints: BTreeMap<EndpointId, EndpointState>,
    tenants: BTreeMap<TenantId, TenantState>,
    in_flight: BTreeMap<AttemptId, InFlight>,
    next_attempt_id: AttemptId,
    rr_cursor: usize,
}

#[derive(Clone, Debug)]
pub struct Scheduler {
    config: SchedulerConfig,
    now_ms: TimestampMs,
    worker_running: bool,
    deliveries: BTreeMap<EventId, Delivery>,
    endpoints: BTreeMap<EndpointId, EndpointState>,
    tenants: BTreeMap<TenantId, TenantState>,
    in_flight: BTreeMap<AttemptId, InFlight>,
    next_attempt_id: AttemptId,
    rr_cursor: usize,
    durable: DurableState,
    observations: Vec<Observation>,
    metrics: Metrics,
}

impl Scheduler {
    pub fn new(
        config: SchedulerConfig,
        tenant_ids: impl IntoIterator<Item = TenantId>,
        endpoint_ids: impl IntoIterator<Item = EndpointId>,
    ) -> Self {
        let tenants = tenant_ids
            .into_iter()
            .map(|id| (id, TenantState::default()))
            .collect();
        let endpoints = endpoint_ids
            .into_iter()
            .map(|id| (id, EndpointState::default()))
            .collect();
        let mut scheduler = Self {
            config,
            now_ms: 0,
            worker_running: true,
            deliveries: BTreeMap::new(),
            endpoints,
            tenants,
            in_flight: BTreeMap::new(),
            next_attempt_id: 1,
            rr_cursor: 0,
            durable: DurableState {
                now_ms: 0,
                deliveries: BTreeMap::new(),
                endpoints: BTreeMap::new(),
                tenants: BTreeMap::new(),
                in_flight: BTreeMap::new(),
                next_attempt_id: 1,
                rr_cursor: 0,
            },
            observations: Vec::new(),
            metrics: Metrics::default(),
        };
        scheduler.persist();
        scheduler
    }

    pub fn now_ms(&self) -> TimestampMs {
        self.now_ms
    }

    pub fn metrics(&self) -> &Metrics {
        &self.metrics
    }

    pub fn delivery(&self, event_id: EventId) -> Option<&Delivery> {
        self.deliveries.get(&event_id)
    }

    pub fn queue_depth(&self, endpoint_id: EndpointId) -> usize {
        self.endpoints
            .get(&endpoint_id)
            // The queue retains its head while that event is in flight. This
            // makes the ordering invariant directly inspectable and counts a
            // leased head exactly once for the endpoint budget.
            .map_or(0, |endpoint| endpoint.queue.len())
    }

    pub fn active_attempt_ids(&self) -> Vec<AttemptId> {
        self.in_flight.keys().copied().collect()
    }

    pub fn take_observations(&mut self) -> Vec<Observation> {
        std::mem::take(&mut self.observations)
    }

    pub fn enqueue(&mut self, event: Event) -> EnqueueResult {
        self.assert_event_time(&event);
        self.expire_queued();
        if !self.endpoints.contains_key(&event.endpoint_id) {
            return EnqueueResult::RejectedUnknownEndpoint;
        }
        if self.deliveries.contains_key(&event.id) {
            return EnqueueResult::RejectedDuplicate;
        }

        let delivery = Delivery {
            event: event.clone(),
            status: DeliveryStatus::Pending,
            attempt_count: 0,
            retry_at_ms: None,
            delivered_at_ms: None,
        };
        self.deliveries.insert(event.id, delivery);
        self.increment_status(DeliveryStatus::Pending);

        let occupancy = self.queue_depth(event.endpoint_id);
        if occupancy >= self.config.endpoint_queue_budget {
            self.transition(event.id, DeliveryStatus::DeadLettered);
            self.persist();
            return EnqueueResult::DeadLetteredQueueBudget;
        }

        self.endpoints
            .get_mut(&event.endpoint_id)
            .expect("endpoint checked above")
            .queue
            .push_back(event.id);
        self.record_occupancy(event.endpoint_id);
        self.observations.push(Observation::StatusChanged {
            event_id: event.id,
            status: DeliveryStatus::Pending,
            at_ms: self.now_ms,
        });
        self.persist();
        self.dispatch();
        EnqueueResult::Queued
    }

    pub fn advance_to(&mut self, at_ms: TimestampMs) {
        assert!(
            at_ms >= self.now_ms,
            "timestamps must be non-decreasing: {} then {}",
            self.now_ms,
            at_ms
        );
        self.now_ms = at_ms;
        self.expire_queued();
        self.dispatch();
    }

    pub fn set_endpoint_available(&mut self, endpoint_id: EndpointId, available: bool) {
        if let Some(endpoint) = self.endpoints.get_mut(&endpoint_id) {
            endpoint.available = available;
            self.persist();
            self.dispatch();
        }
    }

    pub fn on_outcome(&mut self, attempt_id: AttemptId, outcome: HttpOutcome) {
        let Some(attempt) = self.in_flight.remove(&attempt_id) else {
            self.metrics.ignored_outcomes += 1;
            self.observations.push(Observation::IgnoredOutcome {
                attempt_id,
                at_ms: self.now_ms,
            });
            return;
        };

        if let Some(endpoint) = self.endpoints.get_mut(&attempt.endpoint_id) {
            endpoint.in_flight = endpoint.in_flight.saturating_sub(1);
        }
        if let Some(tenant) = self.tenants.get_mut(&attempt.tenant_id) {
            tenant.in_flight = tenant.in_flight.saturating_sub(1);
        }

        let event = self
            .deliveries
            .get(&attempt.event_id)
            .expect("in-flight attempt has a delivery")
            .event
            .clone();
        let status = match outcome {
            HttpOutcome::Success => DeliveryStatus::Delivered,
            HttpOutcome::NonRetryableFailure => DeliveryStatus::NonRetryableFailed,
            HttpOutcome::RetryableFailure if !event.retryable => DeliveryStatus::NonRetryableFailed,
            HttpOutcome::RetryableFailure if self.now_ms >= event.expires_at_ms() => {
                DeliveryStatus::Expired
            }
            HttpOutcome::RetryableFailure => {
                let attempt_count = self
                    .deliveries
                    .get(&attempt.event_id)
                    .expect("delivery still exists")
                    .attempt_count;
                if attempt_count >= self.config.max_attempts {
                    DeliveryStatus::DeadLettered
                } else {
                    let retry_at_ms = self
                        .now_ms
                        .saturating_add(self.retry_delay_ms(event.id, attempt_count));
                    if let Some(delivery) = self.deliveries.get_mut(&event.id) {
                        delivery.retry_at_ms = Some(retry_at_ms);
                    }
                    self.transition(event.id, DeliveryStatus::RetryScheduled);
                    self.observations.push(Observation::RetryScheduled {
                        event_id: event.id,
                        attempt: attempt_count,
                        at_ms: self.now_ms,
                        retry_at_ms,
                    });
                    self.record_occupancy(event.endpoint_id);
                    self.persist();
                    self.dispatch();
                    return;
                }
            }
        };

        if let Some(endpoint) = self.endpoints.get_mut(&attempt.endpoint_id) {
            let removed = endpoint.queue.pop_front();
            assert_eq!(
                removed,
                Some(attempt.event_id),
                "endpoint order was violated"
            );
        }
        self.record_occupancy(attempt.endpoint_id);

        if status == DeliveryStatus::Delivered
            && let Some(delivery) = self.deliveries.get_mut(&event.id)
        {
            delivery.delivered_at_ms = Some(self.now_ms);
            self.metrics.delivered += 1;
            self.metrics
                .delivery_latency_ms
                .entry(event.tenant_id)
                .or_default()
                .push(self.now_ms.saturating_sub(event.created_at_ms));
        }
        self.transition(event.id, status);
        self.persist();
        self.dispatch();
    }

    /// Persisted in-flight leases are requeued on restart. An outcome that
    /// arrives for the old attempt is ignored, modeling at-least-once retry
    /// after a crash before the acknowledgement was durably recorded.
    pub fn crash(&mut self) {
        let durable = self.durable.clone();
        self.restore(durable);
        self.worker_running = false;
    }

    pub fn restart(&mut self) {
        let durable = self.durable.clone();
        self.restore(durable);
        self.worker_running = true;
        self.requeue_in_flight();
        self.persist();
        self.dispatch();
    }

    pub fn apply(&mut self, event: TraceEvent) {
        let at_ms = match &event {
            TraceEvent::Enqueue { at_ms, .. }
            | TraceEvent::Advance { at_ms }
            | TraceEvent::EndpointAvailability { at_ms, .. }
            | TraceEvent::HttpOutcome { at_ms, .. }
            | TraceEvent::WorkerCrash { at_ms }
            | TraceEvent::WorkerRestart { at_ms } => *at_ms,
        };
        self.advance_to(at_ms);
        match event {
            TraceEvent::Enqueue { event, .. } => {
                self.enqueue(event);
            }
            TraceEvent::Advance { .. } => {}
            TraceEvent::EndpointAvailability {
                endpoint_id,
                available,
                ..
            } => self.set_endpoint_available(endpoint_id, available),
            TraceEvent::HttpOutcome {
                attempt_id,
                outcome,
                ..
            } => self.on_outcome(attempt_id, outcome),
            TraceEvent::WorkerCrash { .. } => self.crash(),
            TraceEvent::WorkerRestart { .. } => self.restart(),
        }
    }

    fn dispatch(&mut self) {
        if !self.worker_running {
            return;
        }
        self.expire_queued();
        loop {
            if self.in_flight.len() >= self.config.worker_limit {
                return;
            }
            let tenant_ids: Vec<_> = self.tenants.keys().copied().collect();
            if tenant_ids.is_empty() {
                return;
            }
            let mut chosen = None;
            for offset in 0..tenant_ids.len() {
                let index = (self.rr_cursor + offset) % tenant_ids.len();
                let tenant_id = tenant_ids[index];
                let tenant = self.tenants.get(&tenant_id).expect("tenant id from map");
                if tenant.in_flight >= self.config.per_tenant_in_flight_limit
                    || self.now_ms < tenant.next_send_at_ms
                {
                    continue;
                }
                for (endpoint_id, endpoint) in &self.endpoints {
                    if !endpoint.available
                        || endpoint.in_flight >= self.config.per_endpoint_in_flight_limit
                        || self.now_ms < endpoint.next_send_at_ms
                    {
                        continue;
                    }
                    let Some(event_id) = endpoint.queue.front() else {
                        continue;
                    };
                    let delivery = self
                        .deliveries
                        .get(event_id)
                        .expect("queued delivery exists");
                    if delivery.event.tenant_id != tenant_id
                        || !matches!(
                            delivery.status,
                            DeliveryStatus::Pending | DeliveryStatus::RetryScheduled
                        )
                        || delivery
                            .retry_at_ms
                            .is_some_and(|retry_at| retry_at > self.now_ms)
                    {
                        continue;
                    }
                    chosen = Some((index, tenant_id, *endpoint_id, *event_id));
                    break;
                }
                if chosen.is_some() {
                    break;
                }
            }
            let Some((tenant_index, tenant_id, endpoint_id, event_id)) = chosen else {
                return;
            };
            self.dispatch_one(tenant_index, tenant_id, endpoint_id, event_id);
        }
    }

    fn dispatch_one(
        &mut self,
        tenant_index: usize,
        tenant_id: TenantId,
        endpoint_id: EndpointId,
        event_id: EventId,
    ) {
        assert_eq!(
            self.endpoints
                .get(&endpoint_id)
                .and_then(|endpoint| endpoint.queue.front())
                .copied(),
            Some(event_id),
            "dispatch must use the endpoint head"
        );

        let attempt = {
            let delivery = self.deliveries.get_mut(&event_id).expect("delivery exists");
            delivery.attempt_count += 1;
            delivery.retry_at_ms = None;
            delivery.attempt_count
        };
        self.transition(event_id, DeliveryStatus::InFlight);
        let attempt_id = self.next_attempt_id;
        self.next_attempt_id += 1;
        self.in_flight.insert(
            attempt_id,
            InFlight {
                event_id,
                tenant_id,
                endpoint_id,
            },
        );
        let endpoint = self
            .endpoints
            .get_mut(&endpoint_id)
            .expect("endpoint candidate exists");
        endpoint.in_flight += 1;
        endpoint.next_send_at_ms = self
            .now_ms
            .saturating_add(self.config.endpoint_min_interval_ms);
        let tenant = self.tenants.get_mut(&tenant_id).expect("tenant exists");
        tenant.in_flight += 1;
        tenant.next_send_at_ms = self
            .now_ms
            .saturating_add(self.config.tenant_min_interval_ms);
        self.rr_cursor = (tenant_index + 1) % self.tenants.len().max(1);
        self.metrics.sends += 1;
        self.record_occupancy(endpoint_id);
        self.observations.push(Observation::SendDecision {
            attempt_id,
            event_id,
            tenant_id,
            endpoint_id,
            attempt,
            at_ms: self.now_ms,
        });
        // The lease is durable before the caller can perform the external send.
        self.persist();
    }

    fn expire_queued(&mut self) {
        let mut expired = Vec::new();
        for endpoint in self.endpoints.values() {
            for event_id in &endpoint.queue {
                let delivery = self
                    .deliveries
                    .get(event_id)
                    .expect("queued delivery exists");
                if delivery.event.expires_at_ms() <= self.now_ms
                    && matches!(
                        delivery.status,
                        DeliveryStatus::Pending | DeliveryStatus::RetryScheduled
                    )
                {
                    expired.push((*event_id, delivery.event.endpoint_id));
                }
            }
        }
        let had_expired = !expired.is_empty();
        for (event_id, endpoint_id) in expired {
            if let Some(endpoint) = self.endpoints.get_mut(&endpoint_id) {
                endpoint.queue.retain(|queued| *queued != event_id);
            }
            self.record_occupancy(endpoint_id);
            if let Some(delivery) = self.deliveries.get_mut(&event_id) {
                delivery.retry_at_ms = None;
            }
            self.transition(event_id, DeliveryStatus::Expired);
        }
        if had_expired {
            self.persist();
        }
    }

    fn requeue_in_flight(&mut self) {
        let attempts: Vec<_> = self.in_flight.values().cloned().collect();
        self.in_flight.clear();
        for attempt in attempts {
            if let Some(endpoint) = self.endpoints.get_mut(&attempt.endpoint_id) {
                endpoint.in_flight = endpoint.in_flight.saturating_sub(1);
            }
            if let Some(tenant) = self.tenants.get_mut(&attempt.tenant_id) {
                tenant.in_flight = tenant.in_flight.saturating_sub(1);
            }
            if let Some(delivery) = self.deliveries.get_mut(&attempt.event_id) {
                delivery.retry_at_ms = None;
            }
            self.transition(attempt.event_id, DeliveryStatus::Pending);
            self.metrics.requeued_after_crash += 1;
            self.record_occupancy(attempt.endpoint_id);
        }
    }

    fn retry_delay_ms(&self, event_id: EventId, attempt: u32) -> TimestampMs {
        let exponent = attempt.saturating_sub(1).min(20);
        let exponential = self.config.retry_base_ms.saturating_mul(1u64 << exponent);
        let capped = exponential.min(self.config.retry_cap_ms);
        // Stable event/attempt jitter spreads tenants without using a random
        // source, making replay and tests byte-for-byte deterministic.
        let jitter_window = (capped / 10).max(1);
        let jitter = stable_mix(event_id ^ u64::from(attempt)) % jitter_window;
        capped.saturating_add(jitter).min(self.config.retry_cap_ms)
    }

    fn assert_event_time(&self, event: &Event) {
        assert!(
            event.created_at_ms <= self.now_ms,
            "event creation time cannot be in the future"
        );
    }

    fn transition(&mut self, event_id: EventId, status: DeliveryStatus) {
        let Some(old) = self
            .deliveries
            .get(&event_id)
            .map(|delivery| delivery.status)
        else {
            return;
        };
        if old == status {
            return;
        }
        self.decrement_status(old);
        self.increment_status(status);
        if let Some(delivery) = self.deliveries.get_mut(&event_id) {
            delivery.status = status;
        }
        self.observations.push(Observation::StatusChanged {
            event_id,
            status,
            at_ms: self.now_ms,
        });
    }

    fn increment_status(&mut self, status: DeliveryStatus) {
        *self
            .metrics
            .current_status_counts
            .entry(status)
            .or_default() += 1;
    }

    fn decrement_status(&mut self, status: DeliveryStatus) {
        if let Some(count) = self.metrics.current_status_counts.get_mut(&status) {
            *count = count.saturating_sub(1);
        }
    }

    fn record_occupancy(&mut self, endpoint_id: EndpointId) {
        let occupancy = self.queue_depth(endpoint_id);
        let max = self
            .metrics
            .max_endpoint_occupancy
            .entry(endpoint_id)
            .or_default();
        *max = (*max).max(occupancy);
    }

    fn persist(&mut self) {
        self.durable = DurableState {
            now_ms: self.now_ms,
            deliveries: self.deliveries.clone(),
            endpoints: self.endpoints.clone(),
            tenants: self.tenants.clone(),
            in_flight: self.in_flight.clone(),
            next_attempt_id: self.next_attempt_id,
            rr_cursor: self.rr_cursor,
        };
    }

    fn restore(&mut self, durable: DurableState) {
        self.now_ms = durable.now_ms;
        self.deliveries = durable.deliveries;
        self.endpoints = durable.endpoints;
        self.tenants = durable.tenants;
        self.in_flight = durable.in_flight;
        self.next_attempt_id = durable.next_attempt_id;
        self.rr_cursor = durable.rr_cursor;
    }
}

fn stable_mix(mut value: u64) -> u64 {
    value ^= value >> 30;
    value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

/// A deliberately small comparison model for the stated fixed-queue pain:
/// a single slow endpoint may occupy every worker while its requests wait for
/// a timeout. It is not a claim about a particular production implementation;
/// it makes the acceptance comparison explicit and repeatable.
pub fn baseline_healthy_tail_latency_ms(
    worker_limit: usize,
    noisy_endpoint_events: usize,
    slow_response_ms: TimestampMs,
) -> TimestampMs {
    if worker_limit > 0 && noisy_endpoint_events >= worker_limit {
        slow_response_ms
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> SchedulerConfig {
        SchedulerConfig {
            worker_limit: 4,
            per_endpoint_in_flight_limit: 1,
            per_tenant_in_flight_limit: 1,
            endpoint_min_interval_ms: 0,
            tenant_min_interval_ms: 0,
            retry_base_ms: 1_000,
            retry_cap_ms: 60_000,
            max_attempts: 3,
            endpoint_queue_budget: 8,
        }
    }

    fn event(
        id: EventId,
        tenant_id: TenantId,
        endpoint_id: EndpointId,
        ttl_ms: TimestampMs,
        retryable: bool,
    ) -> Event {
        Event {
            id,
            tenant_id,
            endpoint_id,
            created_at_ms: 0,
            ttl_ms,
            payload_bytes: 1024,
            retryable,
        }
    }

    fn scheduler() -> Scheduler {
        Scheduler::new(config(), [1, 2, 3], [10, 20, 30])
    }

    #[test]
    fn endpoint_order_is_preserved_across_retry() {
        let mut scheduler = scheduler();
        assert_eq!(
            scheduler.enqueue(event(1, 1, 10, 10_000, true)),
            EnqueueResult::Queued
        );
        let first = scheduler.active_attempt_ids()[0];
        assert_eq!(
            scheduler.enqueue(event(2, 1, 10, 10_000, true)),
            EnqueueResult::Queued
        );
        assert_eq!(scheduler.active_attempt_ids(), vec![first]);

        scheduler.on_outcome(first, HttpOutcome::RetryableFailure);
        assert_eq!(
            scheduler.delivery(1).unwrap().status,
            DeliveryStatus::RetryScheduled
        );
        assert!(scheduler.active_attempt_ids().is_empty());
        scheduler.advance_to(scheduler.delivery(1).unwrap().retry_at_ms.unwrap());
        let retry = scheduler.active_attempt_ids()[0];
        assert_eq!(scheduler.delivery(1).unwrap().attempt_count, 2);
        assert_eq!(
            scheduler.delivery(2).unwrap().status,
            DeliveryStatus::Pending
        );
        scheduler.on_outcome(retry, HttpOutcome::Success);
        assert_eq!(
            scheduler.delivery(1).unwrap().status,
            DeliveryStatus::Delivered
        );
        assert_eq!(scheduler.active_attempt_ids().len(), 1);
        let second = scheduler.active_attempt_ids()[0];
        assert_eq!(scheduler.delivery(2).unwrap().attempt_count, 1);
        scheduler.on_outcome(second, HttpOutcome::Success);
        assert_eq!(
            scheduler.delivery(2).unwrap().status,
            DeliveryStatus::Delivered
        );
    }

    #[test]
    fn noisy_endpoint_is_bounded_and_healthy_tenant_is_admitted() {
        let mut scheduler = scheduler();
        for id in 1..=8 {
            assert_eq!(
                scheduler.enqueue(event(id, 1, 10, 3_600_000, true)),
                EnqueueResult::Queued
            );
        }
        assert_eq!(
            scheduler.enqueue(event(100, 2, 20, 3_600_000, true)),
            EnqueueResult::Queued
        );
        assert_eq!(scheduler.active_attempt_ids().len(), 2);
        let healthy = scheduler.delivery(100).unwrap();
        assert_eq!(healthy.status, DeliveryStatus::InFlight);
        assert_eq!(scheduler.queue_depth(10), 8);
        assert_eq!(scheduler.metrics().max_endpoint_occupancy[&10], 8);
        assert_eq!(baseline_healthy_tail_latency_ms(4, 4, 60_000), 60_000);
    }

    #[test]
    fn crash_requeues_unacknowledged_lease_and_old_outcome_is_ignored() {
        let mut scheduler = scheduler();
        scheduler.enqueue(event(1, 1, 10, 10_000, true));
        let old_attempt = scheduler.active_attempt_ids()[0];
        scheduler.crash();
        scheduler.restart();
        let new_attempt = scheduler.active_attempt_ids()[0];
        assert_ne!(old_attempt, new_attempt);
        assert_eq!(scheduler.delivery(1).unwrap().attempt_count, 2);
        scheduler.on_outcome(old_attempt, HttpOutcome::Success);
        assert_eq!(
            scheduler.delivery(1).unwrap().status,
            DeliveryStatus::InFlight
        );
        scheduler.on_outcome(new_attempt, HttpOutcome::Success);
        assert_eq!(
            scheduler.delivery(1).unwrap().status,
            DeliveryStatus::Delivered
        );
        assert_eq!(scheduler.metrics().requeued_after_crash, 1);
        assert_eq!(scheduler.metrics().ignored_outcomes, 1);
    }

    #[test]
    fn failure_classes_are_distinct() {
        let mut scheduler = scheduler();
        scheduler.enqueue(event(1, 1, 10, 10_000, false));
        let attempt = scheduler.active_attempt_ids()[0];
        scheduler.on_outcome(attempt, HttpOutcome::NonRetryableFailure);
        assert_eq!(
            scheduler.delivery(1).unwrap().status,
            DeliveryStatus::NonRetryableFailed
        );

        scheduler.enqueue(event(2, 1, 20, 100, true));
        let attempt = scheduler.active_attempt_ids()[0];
        scheduler.advance_to(100);
        scheduler.on_outcome(attempt, HttpOutcome::RetryableFailure);
        assert_eq!(
            scheduler.delivery(2).unwrap().status,
            DeliveryStatus::Expired
        );

        scheduler.enqueue(event(3, 1, 30, 10_000, true));
        let first = scheduler.active_attempt_ids()[0];
        scheduler.on_outcome(first, HttpOutcome::RetryableFailure);
        scheduler.advance_to(scheduler.delivery(3).unwrap().retry_at_ms.unwrap());
        let second = scheduler.active_attempt_ids()[0];
        scheduler.on_outcome(second, HttpOutcome::RetryableFailure);
        scheduler.advance_to(scheduler.delivery(3).unwrap().retry_at_ms.unwrap());
        let third = scheduler.active_attempt_ids()[0];
        scheduler.on_outcome(third, HttpOutcome::RetryableFailure);
        assert_eq!(
            scheduler.delivery(3).unwrap().status,
            DeliveryStatus::DeadLettered
        );
    }

    #[test]
    fn tenant_rate_limit_does_not_starve_another_tenant() {
        let mut scheduler = Scheduler::new(
            SchedulerConfig {
                tenant_min_interval_ms: 1_000,
                ..config()
            },
            [1, 2],
            [10, 20, 30],
        );
        scheduler.enqueue(event(1, 1, 10, 10_000, true));
        scheduler.enqueue(event(2, 1, 20, 10_000, true));
        scheduler.enqueue(event(3, 2, 30, 10_000, true));
        let active = scheduler.active_attempt_ids();
        assert_eq!(active.len(), 2);
        assert_eq!(
            scheduler.delivery(1).unwrap().status,
            DeliveryStatus::InFlight
        );
        assert_eq!(
            scheduler.delivery(3).unwrap().status,
            DeliveryStatus::InFlight
        );
        assert_eq!(
            scheduler.delivery(2).unwrap().status,
            DeliveryStatus::Pending
        );
        for attempt in active {
            scheduler.on_outcome(attempt, HttpOutcome::Success);
        }
        scheduler.advance_to(1_000);
        assert_eq!(
            scheduler.delivery(2).unwrap().status,
            DeliveryStatus::InFlight
        );
    }

    #[test]
    fn one_hour_outage_has_no_retry_storm_and_recovers_one_head() {
        let mut scheduler = scheduler();
        scheduler.enqueue(event(1, 1, 10, 7_200_000, true));
        let first = scheduler.active_attempt_ids()[0];
        scheduler.set_endpoint_available(10, false);
        scheduler.on_outcome(first, HttpOutcome::RetryableFailure);
        let retry_at = scheduler.delivery(1).unwrap().retry_at_ms.unwrap();
        scheduler.advance_to(3_600_000);
        assert!(scheduler.active_attempt_ids().is_empty());
        assert_eq!(scheduler.delivery(1).unwrap().attempt_count, 1);
        scheduler.set_endpoint_available(10, true);
        assert_eq!(scheduler.active_attempt_ids().len(), 1);
        let recovered = scheduler.active_attempt_ids()[0];
        assert!(retry_at < scheduler.now_ms());
        scheduler.on_outcome(recovered, HttpOutcome::Success);
        assert_eq!(
            scheduler.delivery(1).unwrap().status,
            DeliveryStatus::Delivered
        );
    }

    #[test]
    fn identical_trace_is_deterministic() {
        fn run() -> Vec<Observation> {
            let mut scheduler = scheduler();
            scheduler.apply(TraceEvent::Enqueue {
                at_ms: 0,
                event: event(1, 1, 10, 10_000, true),
            });
            let attempt = scheduler.active_attempt_ids()[0];
            scheduler.apply(TraceEvent::HttpOutcome {
                at_ms: 0,
                attempt_id: attempt,
                outcome: HttpOutcome::RetryableFailure,
            });
            let retry_at = scheduler.delivery(1).unwrap().retry_at_ms.unwrap();
            scheduler.apply(TraceEvent::Advance { at_ms: retry_at });
            scheduler.take_observations()
        }
        assert_eq!(run(), run());
    }

    #[test]
    fn retry_jitter_respects_hard_cap() {
        let mut limited = config();
        limited.retry_cap_ms = 1_000;
        let scheduler = Scheduler::new(limited, [1, 2, 3], [10, 20, 30]);
        for event_id in 0..1_024 {
            assert!(scheduler.retry_delay_ms(event_id, 8) <= 1_000);
        }
    }
}
