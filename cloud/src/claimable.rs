//! Anonymous one-hour Bogs. This lifecycle never grants workspace membership.
use crate::registry::{db_error, hash, now};
use crate::{BogId, CloudError, CloudService, Principal, Registry, WorkspaceId};
use axum::{
    Json,
    body::to_bytes,
    extract::{ConnectInfo, Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::{net::SocketAddr, sync::Arc};
use subtle::ConstantTimeEq;
use uuid::Uuid;

pub const HOLDING: &str = "00000000-0000-0000-0000-000000000002";
const LIFETIME: i64 = 3600;

fn invalid(message: &str) -> CloudError {
    CloudError::new("invalid_request", message)
}
fn denied() -> CloudError {
    CloudError::new("unauthorized", "invalid temporary credential")
}
fn holding() -> WorkspaceId {
    WorkspaceId(Uuid::parse_str(HOLDING).unwrap())
}
fn secret_proof(raw: &str) -> Result<Vec<u8>, CloudError> {
    if raw.len() != 64 || !raw.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid(
            "recovery_secret must be 32 random bytes encoded as hex",
        ));
    }
    Ok(hash(raw.as_bytes()))
}
fn claim_code(raw: &str) -> Result<&str, CloudError> {
    if raw.len() != 64 || !raw.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(CloudError::new("not_found", "claim link not found"));
    }
    Ok(raw)
}

impl Registry {
    pub fn is_live_claimable_credential(
        &self,
        p: &Principal,
        target: Option<BogId>,
    ) -> Result<bool, CloudError> {
        let (Some(bog), Some(token)) = (target, p.token_id.as_deref()) else {
            return Ok(false);
        };
        if p.bog_id != Some(bog) || p.workspace_id != Some(holding()) || p.account_id.is_some() {
            return Ok(false);
        }
        let db = self.connection()?;
        db.query_row("SELECT EXISTS(SELECT 1 FROM claimable_bogs c JOIN tokens t ON t.bog_id=c.bog_id WHERE c.bog_id=?1 AND t.id=?2 AND c.state='active' AND c.expires_at>?3 AND t.revoked_at IS NULL AND t.expires_at>?3)", params![bog.to_string(),token,now()], |r| r.get(0)).map_err(db_error)
    }
    pub fn expire_claimable(&self, at: i64) -> Result<usize, CloudError> {
        let mut db = self.connection()?;
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(db_error)?;
        tx.execute("UPDATE tokens SET revoked_at=?1 WHERE revoked_at IS NULL AND bog_id IN (SELECT bog_id FROM claimable_bogs WHERE state='active' AND expires_at<=?1)",[at]).map_err(db_error)?;
        tx.execute(
            "UPDATE claimable_bogs SET state='expired' WHERE state='active' AND expires_at<=?1",
            [at],
        )
        .map_err(db_error)?;
        // A crash between Bog allocation and metadata insertion leaves an orphan in the holding workspace.
        let n=tx.execute("UPDATE bogs SET deleted_at=?1,desired_state='stopped' WHERE deleted_at IS NULL AND workspace_id=?2 AND id IN (SELECT b.id FROM bogs b LEFT JOIN claimable_bogs c ON c.bog_id=b.id WHERE b.workspace_id=?2 AND ((c.state='expired' AND c.expires_at<=?1) OR (c.bog_id IS NULL AND b.created_at<=?3)))",params![at,HOLDING,at-LIFETIME]).map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        Ok(n)
    }
}

async fn body(request: Request) -> Result<Value, CloudError> {
    if request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_none_or(|v| v.split(';').next() != Some("application/json"))
    {
        return Err(invalid("application/json required"));
    }
    let bytes = to_bytes(request.into_body(), 1024 * 1024)
        .await
        .map_err(|_| CloudError::new("payload_too_large", "request exceeds 1 MiB"))?;
    serde_json::from_slice(&bytes).map_err(|_| invalid("invalid JSON body"))
}
fn reply(result: Result<Response, CloudError>) -> Response {
    let mut r =
        result.unwrap_or_else(|e| crate::http::error_response(e, &Uuid::new_v4().to_string()));
    r.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    r
}
fn limited(wait_seconds: i64) -> Response {
    let wait = wait_seconds.max(1) as u64;
    let mut response=(StatusCode::TOO_MANY_REQUESTS,Json(json!({"error":{"code":"capacity","message":"Temporary Bog capacity reached; retry later"},"retry_after_ms":wait*1000}))).into_response();
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, wait.to_string().parse().unwrap());
    response
}
fn bearer(request: &Request) -> Result<&str, CloudError> {
    request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .ok_or_else(denied)
}
fn id(path: &str) -> Result<BogId, CloudError> {
    let raw = path
        .trim_end_matches("/claim")
        .trim_start_matches("/v1/claimable-bogs/");
    Uuid::parse_str(raw)
        .map(BogId)
        .map_err(|_| invalid("invalid Bog ID"))
}

pub async fn create(State(service): State<Arc<CloudService>>, request: Request) -> Response {
    reply(create_inner(&service, request).await)
}
async fn create_inner(service: &CloudService, request: Request) -> Result<Response, CloudError> {
    if !service.supervisor.config.claimable_enabled || service.native_auth.is_none() {
        return Err(CloudError::new(
            "feature_disabled",
            "claimable Bogs are not enabled",
        ));
    }
    let key = request
        .headers()
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| invalid("Idempotency-Key required"))?
        .to_owned();
    if key.is_empty() || key.len() > 128 || key.chars().any(char::is_control) {
        return Err(invalid("invalid Idempotency-Key"));
    }
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|v| v.0.ip().to_string())
        .unwrap_or_else(|| "unknown-peer".into());
    let value = body(request).await?;
    if value.as_object().is_none_or(|o| {
        o.keys()
            .any(|k| !["name", "definition", "recovery_secret"].contains(&k.as_str()))
    }) {
        return Err(invalid(
            "supported fields: name, definition, recovery_secret",
        ));
    }
    let name = value["name"]
        .as_str()
        .ok_or_else(|| invalid("name required"))?;
    if name.trim().is_empty()
        || name.len() > 100
        || name
            .chars()
            .any(|c| c.is_control() || c == '/' || c == '\\')
    {
        return Err(invalid("invalid name"));
    }
    let recovery = secret_proof(
        value["recovery_secret"]
            .as_str()
            .ok_or_else(|| invalid("recovery_secret required"))?,
    )?;
    let definition = match value.get("definition") {
        Some(v) => crate::definitions::parse(v.clone())?,
        None => bog_definition::Definition::records_v1(),
    };
    if !service.supervisor.config.composable_enabled && value.get("definition").is_some() {
        return Err(CloudError::new(
            "feature_disabled",
            "composable hosted Bogs are disabled",
        ));
    }
    service
        .supervisor
        .config
        .composable_limits
        .validate_definition(&definition)
        .map_err(crate::definitions::invalid)?;
    let digest = hash(
        json!({"name":name,"definition":definition.digest().map_err(crate::definitions::invalid)?})
            .to_string()
            .as_bytes(),
    );
    let source_hash = hash(peer.as_bytes());
    let _guard = service.claimable_create.lock().await;
    crate::config::require_free_space(
        &service.supervisor.config.root,
        service.supervisor.config.min_free_bytes,
    )?;
    let at = now();
    let existing:Option<(String,Vec<u8>,Vec<u8>,i64,String)>=service.registry.connection()?.query_row("SELECT bog_id,request_hash,recovery_hash,expires_at,state FROM claimable_bogs WHERE request_key=?1",[&key],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional().map_err(db_error)?;
    let bog = if let Some((id, old_hash, old_recovery, expires, state)) = existing {
        if old_hash != digest || !bool::from(recovery.ct_eq(&old_recovery)) {
            return Err(CloudError::new(
                "conflict",
                "idempotency key belongs to a different request",
            ));
        }
        if state != "active" || expires <= at {
            return Err(CloudError::new(
                "not_found",
                "temporary Bog has expired or been claimed",
            ));
        }
        BogId(
            Uuid::parse_str(&id)
                .map_err(|_| CloudError::new("unavailable", "invalid stored Bog ID"))?,
        )
    } else {
        let db = service.registry.connection()?;
        let (active,active_reset):(i64,Option<i64>)=db.query_row("SELECT COUNT(*),MIN(expires_at) FROM claimable_bogs WHERE state='active' AND expires_at>?1",[at],|r|Ok((r.get(0)?,r.get(1)?))).map_err(db_error)?;
        let (recent,recent_reset):(i64,Option<i64>)=db.query_row("SELECT COUNT(*),MIN(created_at) FROM claimable_bogs WHERE source_hash=?1 AND created_at>?2",params![source_hash,at-3600],|r|Ok((r.get(0)?,r.get(1)?))).map_err(db_error)?;
        let (daily, daily_reset): (i64, Option<i64>) = db
            .query_row(
                "SELECT COUNT(*),MIN(created_at) FROM claimable_bogs WHERE created_at>?1",
                [at - 86400],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(db_error)?;
        let mut wait = 0;
        if active >= service.supervisor.config.claimable_limit as i64 {
            wait = wait.max(active_reset.unwrap_or(at + 60) - at);
        }
        if recent >= service.supervisor.config.claimable_per_source_hour as i64 {
            wait = wait.max(recent_reset.unwrap_or(at) + 3600 - at);
        }
        if daily >= service.supervisor.config.claimable_daily_limit as i64 {
            wait = wait.max(daily_reset.unwrap_or(at) + 86400 - at);
        }
        if wait > 0 {
            return Ok(limited(wait));
        }
        drop(db);
        let suffix = hash(key.as_bytes())[..4]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let temporary_name = format!("{}-{}", name.trim().to_lowercase(), suffix);
        let bog = service.registry.create_in_workspace(
            holding(),
            &temporary_name,
            "records-v1",
            &key,
            service.supervisor.config.claimable_limit,
            service.supervisor.config.claimable_limit,
            crate::ObservedState::Creating,
            None,
            32,
            Some(&definition),
            false,
        )?;
        let mut db = service.registry.connection()?;
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(db_error)?;
        tx.execute("INSERT INTO claimable_bogs(bog_id,desired_name,request_key,request_hash,recovery_hash,source_hash,created_at,expires_at,state) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'active')",params![bog.id.to_string(),name.trim().to_lowercase(),key,digest,recovery,source_hash,at,at+LIFETIME]).map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        let supervisor = service.supervisor.clone();
        let id = bog.id;
        tokio::spawn(async move {
            let _ = supervisor.ensure_running(id).await;
        });
        bog.id
    };
    let token_id = Uuid::new_v4().to_string();
    let secret = format!("{token_id}.{}", crate::auth::random_secret()?);
    let mut db = service.registry.connection()?;
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(db_error)?;
    let (expiry, state): (i64, String) = tx
        .query_row(
            "SELECT expires_at,state FROM claimable_bogs WHERE bog_id=?1",
            [bog.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(db_error)?;
    if state != "active" || expiry <= now() {
        return Err(CloudError::new(
            "not_found",
            "temporary Bog expired or claimed",
        ));
    }
    tx.execute(
        "UPDATE tokens SET revoked_at=?1 WHERE bog_id=?2 AND revoked_at IS NULL",
        params![at, bog.to_string()],
    )
    .map_err(db_error)?;
    tx.execute("INSERT INTO tokens(id,bog_id,secret_hash,scope,created_at,workspace_id,expires_at) VALUES(?1,?2,?3,'write',?4,?5,?6)",params![token_id,bog.to_string(),hash(secret.as_bytes()),at,HOLDING,expiry]).map_err(db_error)?;
    tx.execute(
        "UPDATE claimable_bogs SET creator_token_id=?1 WHERE bog_id=?2",
        params![token_id, bog.to_string()],
    )
    .map_err(db_error)?;
    tx.commit().map_err(db_error)?;
    drop(db);
    let routes = service
        .registry
        .definition(bog)
        .map(|d| crate::definitions::compact_routes(&d, &bog.to_string()))
        .unwrap_or(Value::Null);
    Ok((StatusCode::CREATED,Json(json!({"id":bog,"status_url":format!("/v1/bogs/{bog}"),"routes":routes,"expires_at":expiry,"credential":{"access_token":secret,"token_type":"Bearer"},"claim_endpoint":format!("/v1/claimable-bogs/{bog}/claim")}))).into_response())
}

pub async fn issue_claim(State(service): State<Arc<CloudService>>, request: Request) -> Response {
    reply(issue_claim_inner(&service, request).await)
}
async fn issue_claim_inner(
    service: &CloudService,
    request: Request,
) -> Result<Response, CloudError> {
    let bog = id(request.uri().path())?;
    let principal = service
        .authenticate_rest_bearer(bearer(&request)?, None)
        .await?;
    if !service
        .registry
        .is_live_claimable_credential(&principal, Some(bog))?
    {
        return Err(denied());
    }
    service.rate_limit(&principal)?;
    let code = crate::auth::random_secret()?;
    let at = now();
    let mut db = service.registry.connection()?;
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(db_error)?;
    let expires: i64 = tx
        .query_row(
            "SELECT expires_at FROM claimable_bogs WHERE bog_id=?1 AND state='active'",
            [bog.to_string()],
            |r| r.get(0),
        )
        .map_err(db_error)?;
    tx.execute(
        "DELETE FROM claimable_claims WHERE bog_id=?1 AND consumed_at IS NULL",
        [bog.to_string()],
    )
    .map_err(db_error)?;
    let until = (at + 900).min(expires);
    tx.execute("INSERT INTO claimable_claims(id,bog_id,code_hash,created_at,expires_at) VALUES(?1,?2,?3,?4,?5)",params![Uuid::new_v4().to_string(),bog.to_string(),hash(code.as_bytes()),at,until]).map_err(db_error)?;
    tx.commit().map_err(db_error)?;
    let origin = service
        .supervisor
        .config
        .public_origin
        .clone()
        .unwrap_or_else(|| crate::public_discovery::origin(service));
    Ok(
        Json(json!({"claim_url":format!("{origin}/claim/{code}"),"expires_at":until,"bog_id":bog}))
            .into_response(),
    )
}

pub async fn claim_page(State(service): State<Arc<CloudService>>, request: Request) -> Response {
    reply(claim_page_inner(&service, &request))
}
pub async fn claim_status(State(service): State<Arc<CloudService>>, request: Request) -> Response {
    reply(claim_status_inner(&service, &request))
}
fn claim_status_inner(service: &CloudService, request: &Request) -> Result<Response, CloudError> {
    let code = claim_code(
        request
            .uri()
            .path()
            .trim_start_matches("/claim/")
            .trim_end_matches("/status"),
    )?;
    let db = service.registry.connection()?;
    let row:Option<(String,i64)>=db.query_row("SELECT b.state,c.expires_at FROM claimable_claims c JOIN claimable_bogs b ON b.bog_id=c.bog_id WHERE c.code_hash=?1",[hash(code.as_bytes())],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(db_error)?;
    let (state, expiry) =
        row.ok_or_else(|| CloudError::new("not_found", "claim link not found"))?;
    let state = if state == "claimed" {
        "claimed"
    } else if expiry <= now() || state == "expired" {
        "expired"
    } else {
        "active"
    };
    Ok(Json(json!({"state":state,"expires_at":expiry})).into_response())
}
fn claim_page_inner(service: &CloudService, request: &Request) -> Result<Response, CloudError> {
    let code = claim_code(request.uri().path().trim_start_matches("/claim/"))?;
    let db = service.registry.connection()?;
    let info:Option<(String,String,i64,String)>=db.query_row("SELECT c.bog_id,b.desired_name,c.expires_at,b.state FROM claimable_bogs b JOIN claimable_claims c ON c.bog_id=b.bog_id WHERE c.code_hash=?1",[hash(code.as_bytes())],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(db_error)?;
    let (bog, name, expiry, state) =
        info.ok_or_else(|| CloudError::new("not_found", "claim link not found"))?;
    let safe = name
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;");
    let active = state == "active" && expiry > now();
    let body = if active {
        format!(
            "<h1>Claim {safe}</h1><p>This Bog expires soon. Sign in, choose your workspace, and keep its data. Temporary credentials will stop working when you claim it.</p><a href=\"/auth/login?claim={code}\">Sign in with GitHub</a><div id=\"claim-details\" data-bog=\"{bog}\" data-name=\"{safe}\"></div><script src=\"/claim.js\" defer></script>"
        )
    } else {
        "<h1>This claim link is no longer available</h1><p>The temporary Bog has expired or has already been claimed.</p>".into()
    };
    let mut response = crate::http::guide_asset(
        "text/html; charset=utf-8",
        format!(
            "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>Claim a Bog</title></head><body><main>{body}</main></body></html>"
        ),
    );
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        header::HeaderValue::from_static("no-referrer"),
    );
    Ok(response)
}

pub async fn claim(State(service): State<Arc<CloudService>>, request: Request) -> Response {
    reply(claim_inner(&service, request).await)
}
async fn claim_inner(service: &CloudService, request: Request) -> Result<Response, CloudError> {
    let code = claim_code(request.uri().path().trim_start_matches("/claim/"))?.to_owned();
    let native = service
        .native_auth
        .as_ref()
        .ok_or_else(|| CloudError::new("unavailable", "GitHub sign-in required"))?;
    let session = crate::http::cookie_value(request.headers(), crate::browser_auth::SESSION_COOKIE)
        .ok_or_else(|| CloudError::new("unauthorized", "sign in required"))?;
    native.check_csrf(
        &session,
        crate::http::header_text(request.headers(), "origin"),
        crate::http::header_text(request.headers(), "x-csrf-token"),
    )?;
    let identity = native.authenticate(&session)?;
    let (_, personal) = service.auth.provision_identity(&identity)?;
    let data = body(request).await?;
    if data.as_object().is_none_or(|o| {
        o.keys()
            .any(|k| !["workspace_id", "name"].contains(&k.as_str()))
    }) {
        return Err(invalid("supported fields: workspace_id, name"));
    }
    let requested_name = data
        .get("name")
        .map(|v| v.as_str().ok_or_else(|| invalid("name must be a string")))
        .transpose()?;
    if let Some(name) = requested_name {
        if name.trim().is_empty()
            || name.len() > 100
            || name
                .chars()
                .any(|c| c.is_control() || c == '/' || c == '\\')
        {
            return Err(invalid("invalid name"));
        }
    }
    let workspace = data["workspace_id"]
        .as_str()
        .map(|v| {
            Uuid::parse_str(v)
                .map(WorkspaceId)
                .map_err(|_| invalid("invalid workspace_id"))
        })
        .transpose()?
        .unwrap_or(personal.id);
    let principal = service.auth.principal_from_verified(&identity, workspace)?;
    let _selected = service.auth.select_workspace(&principal, workspace)?;
    let code_hash = hash(code.as_bytes());
    let at = now();
    let preview:Option<(String,i64,String)>=service.registry.connection()?.query_row("SELECT c.bog_id,c.expires_at,b.state FROM claimable_claims c JOIN claimable_bogs b ON b.bog_id=c.bog_id WHERE c.code_hash=?1",[&code_hash],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(db_error)?;
    let (raw, expiry, state) =
        preview.ok_or_else(|| CloudError::new("not_found", "claim link not found"))?;
    service
        .registry
        .connection()?
        .execute(
            "UPDATE claimable_bogs SET claim_attempts=claim_attempts+1 WHERE bog_id=?1",
            [&raw],
        )
        .map_err(db_error)?;
    let bog = BogId(
        Uuid::parse_str(&raw)
            .map_err(|_| CloudError::new("unavailable", "invalid stored Bog ID"))?,
    );
    if state == "claimed" {
        let db = service.registry.connection()?;
        let prior:Option<String>=db.query_row("SELECT workspace_id FROM claimable_claims WHERE code_hash=?1 AND consumed_at IS NOT NULL",[&code_hash],|r|r.get(0)).optional().map_err(db_error)?;
        if prior.as_deref() == Some(&workspace.to_string()) {
            return Ok(
                Json(json!({"id":bog,"workspace_id":workspace,"claimed":true})).into_response(),
            );
        }
    }
    if expiry <= at || state != "active" {
        return Err(CloudError::new(
            "not_found",
            "claim link expired or Bog unavailable",
        ));
    }
    // An exclusive gate drains in-flight worker requests and blocks new ones until token revocation.
    let _gate = service.supervisor.gate(bog)?.write_owned().await;
    let mut db = service.registry.connection()?;
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(db_error)?;
    let row:Option<(String,i64,String,String)>=tx.query_row("SELECT b.desired_name,b.expires_at,b.state,c.id FROM claimable_bogs b JOIN claimable_claims c ON c.bog_id=b.bog_id WHERE c.code_hash=?1 AND c.consumed_at IS NULL",[&code_hash],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(db_error)?;
    let (original_name, expires, state, claim_id) =
        row.ok_or_else(|| CloudError::new("not_found", "claim no longer available"))?;
    let name = requested_name
        .map(|s| s.trim().to_lowercase())
        .unwrap_or(original_name);
    if state != "active" || expires <= now() {
        return Err(CloudError::new("not_found", "temporary Bog expired"));
    }
    crate::workspace::member(
        &tx,
        principal.account_id().ok_or_else(denied)?,
        workspace,
        false,
    )?;
    let uncapped:bool=tx.query_row("SELECT w.uncapped_bogs OR COALESCE(a.uncapped_bogs,0) FROM workspaces w LEFT JOIN accounts a ON a.id=w.personal_account_id WHERE w.id=?1",[workspace.to_string()],|r|r.get(0)).map_err(db_error)?;
    let count:i64=tx.query_row("SELECT COUNT(*) FROM bogs WHERE workspace_id=?1 AND (deleted_at IS NULL OR cleanup_completed_at IS NULL) AND id NOT IN (SELECT bog_id FROM sandboxes)",[workspace.to_string()],|r|r.get(0)).map_err(db_error)?;
    if !uncapped && count >= 3 {
        return Err(CloudError::new("capacity", "destination workspace is full"));
    }
    let collision: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM bogs WHERE workspace_id=?1 AND name=?2)",
            params![workspace.to_string(), name],
            |r| r.get(0),
        )
        .map_err(db_error)?;
    if collision {
        return Err(CloudError::new(
            "conflict",
            "destination already has a Bog with this name",
        ));
    }
    tx.execute(
        "UPDATE bogs SET workspace_id=?1,name=?2 WHERE id=?3 AND workspace_id=?4",
        params![workspace.to_string(), name, bog.to_string(), HOLDING],
    )
    .map_err(db_error)?;
    tx.execute(
        "UPDATE tokens SET revoked_at=?1 WHERE bog_id=?2 AND revoked_at IS NULL",
        params![at, bog.to_string()],
    )
    .map_err(db_error)?;
    tx.execute("UPDATE claimable_bogs SET state='claimed',claimed_workspace_id=?1,claimed_at=?2 WHERE bog_id=?3",params![workspace.to_string(),at,bog.to_string()]).map_err(db_error)?;
    tx.execute(
        "UPDATE claimable_claims SET consumed_at=?1,workspace_id=?2 WHERE id=?3",
        params![at, workspace.to_string(), claim_id],
    )
    .map_err(db_error)?;
    tx.execute(
        "DELETE FROM claimable_claims WHERE bog_id=?1 AND consumed_at IS NULL",
        [bog.to_string()],
    )
    .map_err(db_error)?;
    tx.commit().map_err(db_error)?;
    Ok(Json(json!({"id":bog,"workspace_id":workspace,"claimed":true,"console_url":format!("/console#resources")})).into_response())
}
