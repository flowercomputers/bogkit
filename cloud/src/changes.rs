//! Bounded change notification, not a retained event feed. Polls only the tiny
//! total view; callers fetch their desired views after changed/reset responses.
use crate::{BogId, CloudError, CloudService, Principal};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Mutex, time::Duration};

#[derive(Debug, Clone, Serialize)]
pub struct ChangeResult {
    pub cursor: String,
    pub seq: u64,
    pub changed: bool,
    pub reset: bool,
}
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    v: u8,
    bog: BogId,
    generation: i64,
    seq: u64,
}
impl Cursor {
    fn encode(&self) -> String {
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(self).expect("cursor serialization"))
    }
    fn decode(input: &str) -> Result<Self, CloudError> {
        let invalid = || CloudError::new("invalid_request", "invalid change cursor");
        if input.len() > 256 {
            return Err(invalid());
        }
        let bytes = URL_SAFE_NO_PAD.decode(input).map_err(|_| invalid())?;
        let cursor: Self = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        if cursor.v != 1 || cursor.generation < 0 {
            return Err(invalid());
        }
        Ok(cursor)
    }
    fn result(&self, previous: Option<&Self>) -> ChangeResult {
        let reset = previous.is_some_and(|p| {
            p.bog != self.bog || p.generation != self.generation || p.seq > self.seq
        });
        ChangeResult {
            cursor: self.encode(),
            seq: self.seq,
            changed: previous.is_some_and(|p| p != self),
            reset,
        }
    }
}
/// Call only while holding the worker lease that produced this sequence.
pub fn cursor_for_response(id: BogId, generation: i64, seq: u64) -> String {
    Cursor {
        v: 1,
        bog: id,
        generation,
        seq,
    }
    .encode()
}
#[derive(Default)]
pub struct ChangeWaiter {
    active: Mutex<HashMap<BogId, usize>>,
}
struct Permit<'a> {
    waiter: &'a ChangeWaiter,
    id: BogId,
}
impl Drop for Permit<'_> {
    fn drop(&mut self) {
        if let Ok(mut active) = self.waiter.active.lock()
            && let Some(count) = active.get_mut(&self.id)
        {
            *count -= 1;
            if *count == 0 {
                active.remove(&self.id);
            }
        }
    }
}
impl ChangeWaiter {
    pub fn new() -> Self {
        Self::default()
    }
    fn acquire(&self, id: BogId) -> Result<Permit<'_>, CloudError> {
        let mut active = self
            .active
            .lock()
            .map_err(|_| CloudError::new("unavailable", "change wait capacity unavailable"))?;
        if active.values().sum::<usize>() >= 64 || active.get(&id).copied().unwrap_or(0) >= 8 {
            return Err(CloudError::new("capacity", "change wait limit reached"));
        }
        *active.entry(id).or_default() += 1;
        Ok(Permit { waiter: self, id })
    }
    async fn snapshot(
        service: &CloudService,
        principal: &Principal,
        id: BogId,
    ) -> Result<Cursor, CloudError> {
        service.auth.authorize(principal, Some(id), false)?;
        let lease = service.supervisor.lease(id).await?;
        let before = service.registry.get(id)?.generation;
        let (status, body) = lease
            .client
            .request(reqwest::Method::GET, "/views/total", None)
            .await?;
        let after = service.registry.get(id)?.generation;
        service.auth.authorize(principal, Some(id), false)?;
        if status != 200 || before != after {
            return Err(CloudError::new("unavailable", "change state unavailable"));
        }
        let seq = body["seq"]
            .as_u64()
            .ok_or_else(|| CloudError::new("unavailable", "invalid change state"))?;
        Ok(Cursor {
            v: 1,
            bog: id,
            generation: after,
            seq,
        })
    }
    pub async fn read_cursor(
        &self,
        service: &CloudService,
        principal: &Principal,
        id: BogId,
    ) -> Result<ChangeResult, CloudError> {
        Ok(Self::snapshot(service, principal, id).await?.result(None))
    }
    pub async fn wait(
        &self,
        service: &CloudService,
        principal: &Principal,
        id: BogId,
        cursor: &str,
        timeout: Duration,
    ) -> Result<ChangeResult, CloudError> {
        service.auth.authorize(principal, Some(id), false)?;
        let previous = Cursor::decode(cursor)?;
        if timeout > Duration::from_secs(25) {
            return Err(CloudError::new(
                "invalid_request",
                "change wait timeout must not exceed 25 seconds",
            ));
        }
        let _permit = self.acquire(id)?;
        // Initial snapshot is required even for timeout=0. Each subsequent
        // probe has an absolute deadline so a stalled worker cannot extend wait.
        let deadline = tokio::time::Instant::now() + timeout;
        let mut current = tokio::time::timeout(
            Duration::from_secs(25),
            Self::snapshot(service, principal, id),
        )
        .await
        .map_err(|_| CloudError::new("unavailable", "change state deadline exceeded"))??;
        loop {
            service.auth.authorize(principal, Some(id), false)?;
            let result = current.result(Some(&previous));
            if result.changed || tokio::time::Instant::now() >= deadline {
                return Ok(result);
            }
            // No worker lease is held during this sleep: shutdown/restore and
            // idle eviction can acquire their exclusive gate.
            tokio::time::sleep_until(
                (tokio::time::Instant::now() + Duration::from_millis(250)).min(deadline),
            )
            .await;
            service.auth.authorize(principal, Some(id), false)?;
            if tokio::time::Instant::now() >= deadline {
                return Ok(current.result(Some(&previous)));
            }
            current = tokio::time::timeout_at(deadline, Self::snapshot(service, principal, id))
                .await
                .map_err(|_| CloudError::new("unavailable", "change state deadline exceeded"))??;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cursors_and_capacity_are_bounded_and_scoped() {
        let id = BogId(uuid::Uuid::new_v4());
        let cursor = Cursor {
            v: 1,
            bog: id,
            generation: 1,
            seq: 4,
        };
        assert_eq!(Cursor::decode(&cursor.encode()).unwrap(), cursor);
        assert!(Cursor::decode(&"a".repeat(257)).is_err());
        assert!(
            Cursor {
                v: 1,
                bog: id,
                generation: 2,
                seq: 4
            }
            .result(Some(&cursor))
            .reset
        );
        assert!(
            Cursor {
                v: 1,
                bog: BogId(uuid::Uuid::new_v4()),
                generation: 1,
                seq: 4
            }
            .result(Some(&cursor))
            .reset
        );
        let waiter = ChangeWaiter::new();
        let permits: Vec<_> = (0..8).map(|_| waiter.acquire(id).unwrap()).collect();
        assert!(waiter.acquire(id).is_err());
        drop(permits);
        assert!(waiter.acquire(id).is_ok());
        let permits: Vec<_> = (0..64)
            .map(|_| waiter.acquire(BogId(uuid::Uuid::new_v4())).unwrap())
            .collect();
        assert!(waiter.acquire(id).is_err());
        drop(permits);
        assert!(waiter.active.lock().unwrap().is_empty());
    }
}
