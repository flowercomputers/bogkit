//! Version-keyed proxy response cache (spec §12).
//!
//! An upstream `get_*`/`list_*` call whose args carry an explicit node id
//! is cacheable: key it by `hash(tool, canonical args)`, tag the row with
//! the file version it was fetched at, and only serve it back while that
//! version is still current. `store::stale_cache_ids` /
//! `store::evict_stale_cache` handle eviction when the version moves on;
//! this module owns key hashing and the read/write helpers.

use fold::pipeline::terminal::TableReader;
use fold::pipeline::{Keyed, Push};
use fold::stream::{KeyedStream, Readable};
use serde_json::Value;

use crate::model::{Id, ProxyCacheRec, Rec};

/// Deterministic hex key for a `(tool, args_canonical)` pair: FNV-1a 64
/// over `tool`'s bytes, a NUL separator, then `args_canonical`'s bytes.
/// The separator prevents boundary collisions (`tool="ab", args="c"` vs
/// `tool="a", args="bc"` would otherwise hash the same concatenation).
pub fn cache_key(tool: &str, args_canonical: &str) -> String {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = OFFSET_BASIS;
    for byte in tool
        .as_bytes()
        .iter()
        .chain(std::iter::once(&0u8))
        .chain(args_canonical.as_bytes())
    {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    format!("{hash:016x}")
}

/// Look up a cached response. A hit requires the stored row's
/// `file_version` to equal `current_version`; a miss (absent or stale)
/// returns `None` without evicting anything — eviction is a separate,
/// explicit step (see `store::evict_stale_cache`).
pub fn lookup<R: Readable>(
    cache: &TableReader<'_, R, String, ProxyCacheRec>,
    tool: &str,
    args_canonical: &str,
    current_version: &str,
) -> Option<Value> {
    let rec = cache.get(&cache_key(tool, args_canonical))?;
    if rec.file_version != current_version {
        return None;
    }
    serde_json::from_str(&rec.content).ok()
}

/// Store (upsert) a response under its cache key, tagged with the file
/// version it was fetched at.
pub fn store<P: Push<Keyed<Id, Rec>>>(
    st: &mut KeyedStream<Id, Rec, P>,
    tool: &str,
    args_canonical: &str,
    file_version: &str,
    content: &Value,
) {
    let key = cache_key(tool, args_canonical);
    let rec = ProxyCacheRec {
        key_hash: key.clone(),
        tool: tool.to_string(),
        args_canonical: args_canonical.to_string(),
        file_version: file_version.to_string(),
        content: serde_json::to_string(content).unwrap_or_default(),
    };
    st.wtx(|tx| {
        tx.upsert(&Id::ProxyCache(key.clone()), &Rec::ProxyCache(rec.clone()));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_is_deterministic() {
        assert_eq!(
            cache_key("get_code", "{\"nodeId\":\"1:2\"}"),
            cache_key("get_code", "{\"nodeId\":\"1:2\"}")
        );
    }

    #[test]
    fn cache_key_distinguishes_boundary_shift() {
        assert_ne!(cache_key("ab", "c"), cache_key("a", "bc"));
    }

    #[test]
    fn cache_key_distinguishes_tool_and_args() {
        assert_ne!(
            cache_key("get_code", "{\"nodeId\":\"1:2\"}"),
            cache_key("get_variable_defs", "{\"nodeId\":\"1:2\"}")
        );
    }
}
