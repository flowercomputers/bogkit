//! Multi-file serve (spec §14): each mirrored Figma file is a [`FileSession`]
//! — its own store, opened at its own `open_store!` call site, with
//! everything that touches it captured in boxed closures (`dispatch`,
//! `pull`, `watermark`). This generalizes the single-store logic
//! `serve.rs` used to inline directly: the store's pipeline type contains
//! fn items and can't be named (see the doc comment on `store.rs`'s
//! `open_store!` macro and the identical note in `cli::dispatch`), so a
//! session's three closures share the one open handle via `Rc<RefCell<_>>`
//! — the only way three independently-boxed `FnMut`s can all reach the
//! same unnameable, non-`Copy` value.
//!
//! [`SessionManager`] owns every open session, in open order (first =
//! default — spec §14's resolution rule for an omitted `file` argument),
//! and implements the `file`-argument routing rule: explicit `file` →
//! that mirror, auto-opening (and spending exactly one Tier-1 pull) if
//! it's new to this manager; omitted → the first-opened session, or an
//! error naming `figmog_open`/`figmog_files` if none exists yet.
//!
//! **Proxied (upstream) tools stay outside this module entirely** — spec
//! §14's documented caveat is that the desktop server has no concept of
//! "which file", so the `file` argument only ever routes figmog's own
//! local tools. `serve.rs` still owns the single, global upstream
//! connection and decides for itself which session's `proxy_cache` a
//! proxied call reads/writes through (see its own doc comment for that
//! choice).

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use serde_json::{Value, json};

use crate::api::{FigmaApi, UreqApi};
use crate::cli::{now_ms, open_store_checked};
use crate::dispatch;
use crate::flatten::flatten_file;
use crate::ident::parse_file_ref;
use crate::mcp::ToolOutput;
use crate::model::Id;
use crate::store::{self, Churn, collect_sweepable, collect_variable_ids};
use crate::watch::{BACKOFF_START, Watcher};

/// What one [`FileSession::pull`] call did: the sync churn plus the file's
/// name/version as of that pull — `figmog_open`'s own result and
/// `figmog_sync`'s churn value are both built from this.
pub(crate) struct PullOutcome {
    pub(crate) churn: Churn,
    pub(crate) name: String,
    pub(crate) version: String,
}

// Named aliases for `FileSession`'s boxed-closure fields (clippy's
// `type_complexity` lint, and plain readability): every one of these
// exists only because the store's pipeline type contains fn items and
// can't be named (this module's doc comment), so it can never appear in a
// named struct field directly — only behind one of these closures.
type DispatchFn = Box<dyn FnMut(&str, &Value) -> Result<ToolOutput, String>>;
type PullFn = Box<dyn FnMut() -> Result<PullOutcome, String>>;
type WatermarkFn = Box<dyn FnMut() -> Option<String>>;
type ProxyCacheFn = Box<
    dyn FnMut(&mut crate::upstream::HttpUpstream, &str, &Value) -> Result<(Value, bool), String>,
>;

/// One mirrored Figma file. `dispatch` answers the 16 read-only
/// `figmog_*` tools (everything [`dispatch::dispatch_read_tool`] knows);
/// `pull` runs one Tier-1 fetch-flatten-sync-evict cycle (used by
/// startup, `figmog_sync`, `figmog_open`, and a watch tick's `Changed`
/// branch); `watermark` reads the stored `FileMeta.last_modified`, used to
/// (re)seed `watcher` after every successful pull. `watcher`/`backoff` are
/// plain per-session state (not closures) so `serve.rs`'s round-robin tick
/// loop can drive them directly, exactly as the old single-session loop
/// drove its own local variables.
///
/// `proxy_cache` is a fourth closure beyond spec §14's literal list,
/// needed for the same structural reason as the other three: a proxied
/// (upstream, non-`figmog_*`) call's version-keyed cache lives in *this*
/// session's own `proxy_cache` table (spec §12/§14: "each session's store
/// carries its own `proxy_cache`"), and the store can only ever be touched
/// from behind one of these closures. `serve.rs` calls it only for the
/// *default* session — see its own doc comment for why proxied calls
/// route through the default rather than any per-call `file` argument.
pub(crate) struct FileSession {
    pub(crate) key: String,
    pub(crate) name: String,
    pub(crate) dispatch: DispatchFn,
    pub(crate) pull: PullFn,
    pub(crate) watermark: WatermarkFn,
    pub(crate) proxy_cache: ProxyCacheFn,
    pub(crate) watcher: Watcher,
    pub(crate) backoff: Duration,
}

impl FileSession {
    /// Reseed `watcher`/`backoff` after a successful pull from any call
    /// site (startup, `figmog_sync`, `figmog_open`, a watch tick) — the
    /// same bookkeeping every one of those paths used to repeat inline.
    pub(crate) fn note_pull_success(&mut self, outcome: &PullOutcome) {
        self.name = outcome.name.clone();
        let seen = (self.watermark)();
        self.watcher = Watcher::new(seen);
        self.backoff = BACKOFF_START;
    }
}

/// Build a [`FileSession`] mirroring `key` under `root` (`root/<key>/db` —
/// the per-key store layout every mirror has used since v1, see
/// `cli::db_path_for`). This owns the concrete `open_store!` call site;
/// `pull_now`: perform one Tier-1 pull-and-sync cycle before returning,
/// unconditionally (callers that only want a pull when the store happens
/// to be empty check that themselves — via the returned session's own
/// `watermark()` — since deciding requires opening the store anyway, and
/// this function is the only thing that does).
pub(crate) fn open_session(
    root: &Path,
    key: &str,
    api_token: Option<&str>,
    pull_now: bool,
) -> Result<FileSession, String> {
    let path = root.join(key).join("db");
    open_session_at(path, key.to_string(), api_token, pull_now)
}

/// Like [`open_session`], but at an explicit store path rather than one
/// derived from `root`/`key` — the CLI's legacy `--db <path>` escape hatch
/// (`serve.rs`'s `run_serve`) needs this: it predates multi-file serve and
/// its existing tests pin an explicit, arbitrary store directory (no
/// `--figmog-root` layout involved), so `figmog serve --db <path>` keeps
/// opening exactly that path as a single session, unchanged.
pub(crate) fn open_session_at(
    path: PathBuf,
    key: String,
    api_token: Option<&str>,
    pull_now: bool,
) -> Result<FileSession, String> {
    let token = api_token.map(str::to_string);
    let st = Rc::new(RefCell::new(open_store_checked(|| {
        crate::open_store!(&path)
    })?));

    // The pull-and-sync cycle (build design §12's do_pull-equivalent
    // sequence): fetch, flatten, sync, evict stale cache rows on a version
    // change. Defined once and reused for the immediate `pull_now` call
    // below and — moved as-is — for the `pull` closure every other call
    // site (`figmog_sync`, `figmog_open`, watch) drives.
    let pull_closure = {
        let st = st.clone();
        let key = key.clone();
        let token = token.clone();
        move || -> Result<PullOutcome, String> {
            let token = token
                .clone()
                .ok_or_else(|| "FIGMA_TOKEN not set — required for network pulls".to_string())?;
            let api = UreqApi::new(token);
            let resp = api.file(&key).map_err(|e| e.to_string())?;
            // Opportunistic Enterprise variables sync (spec §12): `Ok(None)`
            // on non-Enterprise plans is not an error — v1 behavior
            // (import/inference, sweep-exempt) holds unchanged below.
            let vars_resp = api.variables_local(&key).map_err(|e| e.to_string())?;
            let mut flattened = flatten_file(&resp).map_err(|e| e.to_string())?;

            let mut st = st.borrow_mut();
            let mut prior: BTreeSet<Id> =
                st.rtx(|((nodes, ..), components, component_sets, styles, ..)| {
                    collect_sweepable(&nodes, &components, &component_sets, &styles)
                });
            if let Some(v) = &vars_resp {
                let var_recs = crate::vars::parse_variables_export(v).map_err(|e| e.to_string())?;
                flattened.recs.extend(var_recs);
                let stored_var_ids =
                    st.rtx(|(_, _, _, _, variables, variable_collections, _, _)| {
                        collect_variable_ids(&variables, &variable_collections)
                    });
                prior.extend(stored_var_ids);
            }
            let churn = store::sync(&mut st, &prior, &flattened, now_ms());

            // Cache eviction lives here rather than folded into `sync`
            // (store.rs's own note): a version-changing pull sweeps stale
            // `proxy_cache` rows; computing `stale` against the version
            // just synced makes this a no-op whenever the version didn't
            // actually move, with no separate "did it change" check needed.
            let version = flattened.file.version.clone();
            let stale =
                st.rtx(|(_, _, _, _, _, _, _, cache)| store::stale_cache_ids(&cache, &version));
            if !stale.is_empty() {
                store::evict_stale_cache(&mut st, &stale);
            }

            Ok(PullOutcome {
                churn,
                name: flattened.file.name.clone(),
                version,
            })
        }
    };

    if pull_now {
        pull_closure()?;
    }

    let name = st
        .borrow()
        .rtx(|(_, _, _, _, _, _, meta, _)| meta.get(&0).map(|m| m.name.clone()))
        .unwrap_or_else(|| key.clone());
    let seen = st
        .borrow()
        .rtx(|(_, _, _, _, _, _, meta, _)| meta.get(&0).map(|m| m.last_modified.clone()));

    let dispatch: DispatchFn = {
        let st = st.clone();
        Box::new(
            move |name: &str, args: &Value| -> Result<ToolOutput, String> {
                let st = st.borrow();
                // `upstream_status` is a purely global concern (proxy routing
                // never varies per file — see this module's doc comment), so
                // it's spliced into a client-requested `figmog_status` result
                // at the call site that actually knows it (`serve.rs`), not
                // here. `""` is never observed by a real caller.
                match st.rtx(|r| dispatch::dispatch_read_tool(name, args, "", r)) {
                    Some(result) => result.map(ToolOutput::Json),
                    None => Err(format!("unknown tool: {name}")),
                }
            },
        )
    };

    let pull: PullFn = Box::new(pull_closure);

    let watermark: WatermarkFn = {
        let st = st.clone();
        Box::new(move || {
            st.borrow()
                .rtx(|(_, _, _, _, _, _, meta, _)| meta.get(&0).map(|m| m.last_modified.clone()))
        })
    };

    let proxy_cache: ProxyCacheFn = {
        let st = st.clone();
        Box::new(move |upstream, name, args| {
            let args_canonical = crate::proxy::canonical_args(args);
            let version_and_hit = if crate::proxy::is_cacheable(name, args) {
                st.borrow().rtx(|(_, _, _, _, _, _, meta, cache)| {
                    let version = meta.get(&0).map(|m| m.version.clone());
                    let hit = version
                        .as_ref()
                        .and_then(|v| crate::cache::lookup(&cache, name, &args_canonical, v));
                    (version, hit)
                })
            } else {
                (None, None)
            };
            let mut st = st.borrow_mut();
            crate::proxy::proxy_call(&mut st, upstream, name, args, version_and_hit)
        })
    };

    Ok(FileSession {
        key,
        name,
        dispatch,
        pull,
        watermark,
        proxy_cache,
        watcher: Watcher::new(seen),
        backoff: BACKOFF_START,
    })
}

/// Every mirrored file for one `figmog serve` process, in open order
/// (index 0 = default — spec §14). `root`/`token` are what every
/// auto-opened session is built with ([`open_session`]).
pub(crate) struct SessionManager {
    pub(crate) sessions: Vec<FileSession>,
    pub(crate) root: PathBuf,
    pub(crate) token: Option<String>,
}

/// The `file`-argument resolution error's shared text (spec §14: must name
/// `figmog_open`/`figmog_files`).
const NO_DEFAULT_FILE_MSG: &str = "no file specified and no default mirrored file — pass a `file` argument, mirror one with figmog_open, or see figmog_files for the current list";

impl SessionManager {
    /// Get-or-create the session for `file_ref` (URL or bare key),
    /// deduped by key — never pulls: a freshly-created session is left
    /// exactly as [`open_session`] built it (empty unless the caller asked
    /// for `pull_now`). [`Self::resolve`]/`figmog_open` are what decide
    /// whether — and how many times — to actually pull.
    pub(crate) fn open(&mut self, file_ref: &str) -> Result<&mut FileSession, String> {
        let key = parse_file_ref(file_ref)
            .ok_or_else(|| format!("not a Figma file key or URL: {file_ref}"))?;
        if let Some(pos) = self.sessions.iter().position(|s| s.key == key) {
            return Ok(&mut self.sessions[pos]);
        }
        let session = open_session(&self.root, &key, self.token.as_deref(), false)?;
        self.sessions.push(session);
        Ok(self.sessions.last_mut().expect("just pushed"))
    }

    /// Spec §14's `file`-argument resolution rule. Explicit `file`:
    /// that mirror, auto-opening it (and spending exactly one Tier-1 pull,
    /// only for a session that's genuinely new to this manager) if
    /// unknown. Omitted: the first-opened session (the first startup
    /// FILE if any were given, else whichever file got mirrored first),
    /// or [`NO_DEFAULT_FILE_MSG`] if none exists yet.
    pub(crate) fn resolve(&mut self, file_arg: Option<&str>) -> Result<&mut FileSession, String> {
        match file_arg {
            Some(f) => {
                let key =
                    parse_file_ref(f).ok_or_else(|| format!("not a Figma file key or URL: {f}"))?;
                let existed = self.sessions.iter().any(|s| s.key == key);
                let session = self.open(f)?;
                if !existed {
                    let outcome = (session.pull)()?;
                    session.note_pull_success(&outcome);
                }
                Ok(session)
            }
            None => {
                if self.sessions.is_empty() {
                    Err(NO_DEFAULT_FILE_MSG.to_string())
                } else {
                    Ok(&mut self.sessions[0])
                }
            }
        }
    }

    /// `figmog_files`: every mirrored file, in open order (index 0 =
    /// default), as `{key, name, version, nodes, last_synced, default}`.
    /// Deterministic — plain `Vec` order, no `HashMap` involved.
    pub(crate) fn list(&mut self) -> Value {
        let rows: Vec<Value> = self
            .sessions
            .iter_mut()
            .enumerate()
            .map(|(i, s)| {
                let status = (s.dispatch)("figmog_status", &json!({})).ok();
                let (name, version, nodes, last_synced) = match status {
                    Some(ToolOutput::Json(v)) => (
                        v["name"].clone(),
                        v["version"].clone(),
                        v["nodes"].clone(),
                        v["synced_at_unix_ms"].clone(),
                    ),
                    _ => (Value::Null, Value::Null, Value::Null, Value::Null),
                };
                json!({
                    "key": s.key,
                    "name": name,
                    "version": version,
                    "nodes": nodes,
                    "last_synced": last_synced,
                    "default": i == 0,
                })
            })
            .collect();
        json!(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scripted stand-in for a [`FileSession`] built without ever
    /// touching a real store — proves [`SessionManager`]'s routing/dedupe
    /// logic in isolation, per the brief's Step 1.
    fn scripted_session(key: &str, pull_calls: Rc<RefCell<u32>>) -> FileSession {
        let watermark: WatermarkFn = Box::new(|| Some("t".to_string()));
        let pull: PullFn = {
            let pull_calls = pull_calls.clone();
            Box::new(move || {
                *pull_calls.borrow_mut() += 1;
                Ok(PullOutcome {
                    churn: Churn::default(),
                    name: "Scripted".to_string(),
                    version: "1".to_string(),
                })
            })
        };
        FileSession {
            key: key.to_string(),
            name: "Scripted".to_string(),
            dispatch: Box::new(|_name, _args| Ok(ToolOutput::Json(json!({})))),
            pull,
            watermark,
            proxy_cache: Box::new(|_upstream, name, _args| {
                Err(format!(
                    "scripted session has no store to proxy through: {name}"
                ))
            }),
            watcher: Watcher::new(None),
            backoff: BACKOFF_START,
        }
    }

    fn empty_manager() -> SessionManager {
        SessionManager {
            sessions: Vec::new(),
            root: PathBuf::from("/nonexistent"),
            token: None,
        }
    }

    #[test]
    fn resolve_omitted_with_no_sessions_names_figmog_open_and_figmog_files() {
        let mut mgr = empty_manager();
        let err = mgr.resolve(None).map(|_| ()).unwrap_err();
        assert!(err.contains("figmog_open"), "{err}");
        assert!(err.contains("figmog_files"), "{err}");
    }

    #[test]
    fn resolve_omitted_returns_first_opened_session() {
        let mut mgr = empty_manager();
        let calls = Rc::new(RefCell::new(0));
        mgr.sessions
            .push(scripted_session("keyA1234567890", calls.clone()));
        mgr.sessions
            .push(scripted_session("keyB1234567890", calls.clone()));
        let session = mgr.resolve(None).unwrap();
        assert_eq!(session.key, "keyA1234567890");
    }

    #[test]
    fn resolve_explicit_known_key_never_pulls() {
        let mut mgr = empty_manager();
        let calls = Rc::new(RefCell::new(0));
        mgr.sessions
            .push(scripted_session("flAtUnMfzvA5daBSTFQK35", calls.clone()));
        let session = mgr.resolve(Some("flAtUnMfzvA5daBSTFQK35")).unwrap();
        assert_eq!(session.key, "flAtUnMfzvA5daBSTFQK35");
        assert_eq!(
            *calls.borrow(),
            0,
            "an already-known session must not be re-pulled"
        );
    }

    #[test]
    fn resolve_explicit_unknown_key_dedupes_by_key_from_a_url() {
        let mut mgr = empty_manager();
        let calls = Rc::new(RefCell::new(0));
        mgr.sessions
            .push(scripted_session("flAtUnMfzvA5daBSTFQK35", calls.clone()));
        // A full figma.com URL for the same key resolves to the existing
        // session rather than creating a second one (dedupe by parsed key).
        let session = mgr
            .resolve(Some(
                "https://www.figma.com/design/flAtUnMfzvA5daBSTFQK35/whatever",
            ))
            .unwrap();
        assert_eq!(session.key, "flAtUnMfzvA5daBSTFQK35");
        assert_eq!(mgr.sessions.len(), 1);
        assert_eq!(*calls.borrow(), 0);
    }

    #[test]
    fn resolve_rejects_garbage_file_ref() {
        let mut mgr = empty_manager();
        let err = mgr.resolve(Some("not a key!")).map(|_| ()).unwrap_err();
        assert!(err.contains("not a Figma file key or URL"), "{err}");
    }

    #[test]
    fn open_dedupes_repeated_calls_for_the_same_key() {
        let mut mgr = empty_manager();
        let calls = Rc::new(RefCell::new(0));
        mgr.sessions
            .push(scripted_session("keyA1234567890", calls.clone()));
        mgr.open("keyA1234567890").unwrap();
        mgr.open("keyA1234567890").unwrap();
        assert_eq!(mgr.sessions.len(), 1);
    }

    #[test]
    fn list_marks_only_the_first_session_default_and_is_ordered() {
        let mut mgr = empty_manager();
        let calls = Rc::new(RefCell::new(0));
        mgr.sessions
            .push(scripted_session("keyA1234567890", calls.clone()));
        mgr.sessions
            .push(scripted_session("keyB1234567890", calls.clone()));
        let list = mgr.list();
        let rows = list.as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["key"], json!("keyA1234567890"));
        assert_eq!(rows[0]["default"], json!(true));
        assert_eq!(rows[1]["key"], json!("keyB1234567890"));
        assert_eq!(rows[1]["default"], json!(false));
    }
}
