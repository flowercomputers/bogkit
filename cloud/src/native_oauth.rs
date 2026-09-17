//! Self-hosted OAuth for native GitHub accounts. GitHub proves identity;
//! existing device approval supplies explicit, CSRF-protected human consent.
use crate::{
    CloudError, CloudService, Principal, WorkspaceId,
    auth::random_secret,
    native_auth::NativeAuth,
    registry::{db_error, hash, now},
};
use axum::{
    Json,
    body::to_bytes,
    extract::{ConnectInfo, Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::Arc,
};
use subtle::ConstantTimeEq;

const READ: &str = "bog:read";
const WRITE: &str = "bog:write";
#[derive(Default)]
pub(crate) struct OauthState {
    pending: HashMap<String, Grant>,
    codes: HashMap<Vec<u8>, Grant>,
    registrations: HashMap<IpAddr, (i64, u32)>,
}
struct Grant {
    client: String,
    redirect: String,
    state: String,
    challenge: String,
    scope: String,
    device: String,
    expires: i64,
}
fn invalid(message: &str) -> CloudError {
    CloudError::new("invalid_request", message)
}
fn unavailable() -> CloudError {
    CloudError::new(
        "temporarily_unavailable",
        "authorization temporarily unavailable",
    )
}
pub(crate) fn initialize(db: &Connection) -> Result<(), CloudError> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS oauth_clients(id TEXT PRIMARY KEY,name TEXT NOT NULL,redirects TEXT NOT NULL,expires INTEGER NOT NULL); CREATE TABLE IF NOT EXISTS oauth_access(secret_hash BLOB PRIMARY KEY,agent_id TEXT NOT NULL,scope TEXT NOT NULL,resource TEXT NOT NULL,client_id TEXT NOT NULL,expires INTEGER NOT NULL);").map_err(db_error)
}
fn redirect_valid(value: &str) -> bool {
    reqwest::Url::parse(value).is_ok_and(|u| {
        u.fragment().is_none()
            && u.username().is_empty()
            && u.password().is_none()
            && u.host_str().is_some()
            && (u.scheme() == "https"
                || (u.scheme() == "http" && matches!(u.host_str(), Some("127.0.0.1" | "[::1]"))))
    })
}
fn parameters(bytes: &[u8]) -> Result<HashMap<String, String>, CloudError> {
    let mut p = HashMap::new();
    for (k, v) in reqwest::Url::parse(&format!(
        "https://unused.invalid/?{}",
        std::str::from_utf8(bytes).map_err(|_| invalid("UTF-8 required"))?
    ))
    .map_err(|_| invalid("invalid form"))?
    .query_pairs()
    {
        if p.insert(k.into_owned(), v.into_owned()).is_some() {
            return Err(invalid("duplicate parameter"));
        }
    }
    Ok(p)
}
fn field<'a>(p: &'a HashMap<String, String>, k: &str) -> Result<&'a str, CloudError> {
    p.get(k)
        .filter(|v| !v.is_empty())
        .map(String::as_str)
        .ok_or_else(|| invalid(&format!("{k} required")))
}
impl NativeAuth {
    fn resource(&self) -> String {
        format!("{}/mcp", self.config.origin())
    }
    pub(crate) fn oauth_metadata(&self) -> Value {
        let b = self.config.origin();
        json!({"issuer":b,"authorization_endpoint":format!("{b}/oauth/authorize"),"token_endpoint":format!("{b}/oauth/token"),"registration_endpoint":format!("{b}/oauth/register"),"revocation_endpoint":format!("{b}/oauth/revoke"),"response_types_supported":["code"],"grant_types_supported":["authorization_code"],"token_endpoint_auth_methods_supported":["none"],"revocation_endpoint_auth_methods_supported":["none"],"code_challenge_methods_supported":["S256"],"scopes_supported":[READ,WRITE],"authorization_response_iss_parameter_supported":true,"client_id_metadata_document_supported":false,"service_documentation":format!("{b}/auth.md")})
    }
    pub(crate) fn resource_metadata(&self) -> Value {
        json!({"resource":self.resource(),"resource_name":"Bog Cloud HTTP and MCP API","authorization_servers":[self.config.origin()],"scopes_supported":[READ,WRITE],"bearer_methods_supported":["header"],"resource_documentation":format!("{}/docs",self.config.origin())})
    }
    fn register_client(&self, peer: IpAddr, body: Value) -> Result<Value, CloudError> {
        let name = body["client_name"].as_str().unwrap_or("MCP client");
        crate::agent_tokens::validate_name(name)?;
        if body
            .get("token_endpoint_auth_method")
            .is_some_and(|v| v != "none")
        {
            return Err(invalid(
                "public clients use token_endpoint_auth_method none",
            ));
        }
        for (key, allowed) in [
            ("grant_types", "authorization_code"),
            ("response_types", "code"),
        ] {
            if body
                .get(key)
                .is_some_and(|v| v.as_array().is_none_or(|a| a.len() != 1 || a[0] != allowed))
            {
                return Err(invalid(
                    "only authorization_code and response_type code are supported",
                ));
            }
        }
        let redirects = body["redirect_uris"]
            .as_array()
            .ok_or_else(|| invalid("redirect_uris required"))?;
        if redirects.is_empty()
            || redirects.len() > 8
            || redirects.iter().any(|v| {
                v.as_str()
                    .is_none_or(|s| s.len() > 2048 || !redirect_valid(s))
            })
        {
            return Err(invalid(
                "register 1 to 8 exact HTTPS redirects or HTTP loopback IP redirects; fragments and userinfo are forbidden",
            ));
        }
        let mut state = self.oauth.lock().map_err(|_| unavailable())?;
        state
            .registrations
            .retain(|_, (time, _)| *time + 600 > now());
        if state.registrations.len() >= 10000 {
            return Err(CloudError::new("slow_down", "retry registration later"));
        }
        let entry = state.registrations.entry(peer).or_insert((now(), 0));
        entry.1 += 1;
        if entry.1 > 20 {
            return Err(CloudError::new(
                "slow_down",
                "wait ten minutes before registering another client",
            ));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let db = self.db.lock().map_err(|_| unavailable())?;
        db.execute("DELETE FROM oauth_clients WHERE expires<=?1", [now()])
            .map_err(db_error)?;
        let count: i64 = db
            .query_row("SELECT COUNT(*) FROM oauth_clients", [], |r| r.get(0))
            .map_err(db_error)?;
        if count >= 10000 {
            return Err(unavailable());
        }
        db.execute(
            "INSERT INTO oauth_clients VALUES(?1,?2,?3,?4)",
            params![
                id,
                name,
                serde_json::to_string(redirects).map_err(|_| invalid("invalid redirects"))?,
                now() + 30 * 86400
            ],
        )
        .map_err(db_error)?;
        Ok(
            json!({"client_id":id,"client_name":name,"redirect_uris":redirects,"token_endpoint_auth_method":"none","grant_types":["authorization_code"],"response_types":["code"],"scope":format!("{READ} {WRITE}"),"client_id_issued_at":now()}),
        )
    }
    fn authorize_client(
        &self,
        peer: IpAddr,
        p: HashMap<String, String>,
    ) -> Result<String, CloudError> {
        let client = field(&p, "client_id")?;
        let redirect = field(&p, "redirect_uri")?;
        let row: Option<(String, String)> = self
            .db
            .lock()
            .map_err(|_| unavailable())?
            .query_row(
                "SELECT name,redirects FROM oauth_clients WHERE id=?1 AND expires>?2",
                params![client, now()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(db_error)?;
        let (name, redirects) =
            row.ok_or_else(|| invalid("unknown or expired client; register again"))?;
        let redirects: Vec<String> = serde_json::from_str(&redirects).map_err(|_| unavailable())?;
        if !redirects.iter().any(|r| r == redirect) {
            return Err(invalid("redirect_uri must exactly match registration"));
        }
        if field(&p, "response_type")? != "code" || field(&p, "code_challenge_method")? != "S256" {
            return Err(invalid("response_type code and PKCE S256 required"));
        }
        if field(&p, "resource")? != self.resource() {
            return Err(CloudError::new(
                "invalid_target",
                "resource must be this service's /mcp URL",
            ));
        }
        let scope = p.get("scope").map(String::as_str).unwrap_or(READ);
        // Write includes read. Reject unknown/duplicate scopes; normalize order.
        let scopes: Vec<_> = scope.split_whitespace().collect();
        if scopes.is_empty()
            || scopes.len() > 2
            || scopes.iter().any(|s| !matches!(*s, READ | WRITE))
            || (scopes.len() == 2 && scopes[0] == scopes[1])
        {
            return Err(CloudError::new(
                "invalid_scope",
                "request bog:read or bog:write (write includes read)",
            ));
        }
        let scope = if scopes.contains(&WRITE) { WRITE } else { READ };
        let challenge = field(&p, "code_challenge")?;
        if URL_SAFE_NO_PAD.decode(challenge).is_err() || challenge.len() != 43 {
            return Err(invalid("code_challenge must be a base64url SHA-256 digest"));
        }
        let state = p.get("state").cloned().unwrap_or_default();
        if state.len() > 1024 || state.chars().any(char::is_control) {
            return Err(invalid("invalid state"));
        }
        let device = self.start_device(peer, &name)?;
        let public = device["user_code"].as_str().ok_or_else(unavailable)?;
        let mut grants = self.oauth.lock().map_err(|_| unavailable())?;
        grants.pending.retain(|_, g| g.expires > now());
        grants.codes.retain(|_, g| g.expires > now());
        if grants.pending.len() + grants.codes.len() >= 10000 {
            return Err(unavailable());
        }
        grants.pending.insert(
            public.into(),
            Grant {
                client: client.into(),
                redirect: redirect.into(),
                state,
                challenge: challenge.into(),
                scope: scope.into(),
                device: device["device_code"]
                    .as_str()
                    .ok_or_else(unavailable)?
                    .into(),
                expires: now() + 600,
            },
        );
        Ok(format!("/auth/device/approve?user_code={public}"))
    }
    pub(crate) fn oauth_access_description(
        &self,
        public: &str,
    ) -> Result<Option<String>, CloudError> {
        let state = self.oauth.lock().map_err(|_| unavailable())?;
        Ok(state.pending.get(public).filter(|g|g.expires>now()).map(|g|format!("{} for 30 days in your current workspaces. Cannot manage members, delete Bogs, or create account credentials. Client registration is self-service, not a verification of the app's identity. Return to: {}",if g.scope==READ {"Read-only access to Bogs and records (bog:read)"}else{"Create Bogs, read/write records, and issue app credentials (bog:write)"},g.redirect)))
    }
    pub(crate) fn oauth_approved(
        &self,
        public: &str,
        approve: bool,
    ) -> Result<Option<String>, CloudError> {
        let mut state = self.oauth.lock().map_err(|_| unavailable())?;
        let Some(mut grant) = state.pending.remove(public) else {
            return Ok(None);
        };
        if grant.expires <= now() {
            return Err(invalid("authorization expired; start again"));
        }
        let mut redirect = reqwest::Url::parse(&grant.redirect).map_err(|_| unavailable())?;
        redirect
            .query_pairs_mut()
            .append_pair("iss", &self.config.origin());
        if !grant.state.is_empty() {
            redirect
                .query_pairs_mut()
                .append_pair("state", &grant.state);
        }
        if approve {
            let code = random_secret()?;
            redirect.query_pairs_mut().append_pair("code", &code);
            grant.expires = now() + 60;
            state.codes.insert(hash(code.as_bytes()), grant);
        } else {
            redirect
                .query_pairs_mut()
                .append_pair("error", "access_denied");
        }
        Ok(Some(redirect.into()))
    }
    fn exchange(
        &self,
        service: &CloudService,
        p: HashMap<String, String>,
    ) -> Result<Value, CloudError> {
        if field(&p, "grant_type")? != "authorization_code" {
            return Err(CloudError::new(
                "unsupported_grant_type",
                "authorization_code required; refresh tokens are not issued",
            ));
        }
        let mut state = self.oauth.lock().map_err(|_| unavailable())?;
        let key = hash(field(&p, "code")?.as_bytes());
        let grant = state
            .codes
            .get(&key)
            .filter(|g| g.expires > now())
            .ok_or_else(|| {
                CloudError::new(
                    "invalid_grant",
                    "authorization code expired or already used",
                )
            })?;
        let verifier = field(&p, "code_verifier")?;
        if !(43..=128).contains(&verifier.len())
            || !verifier
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b))
        {
            return Err(CloudError::new("invalid_grant", "invalid PKCE verifier"));
        }
        let challenge = URL_SAFE_NO_PAD.encode(hash(verifier.as_bytes()));
        if field(&p, "client_id")? != grant.client
            || field(&p, "redirect_uri")? != grant.redirect
            || field(&p, "resource")? != self.resource()
            || !bool::from(challenge.as_bytes().ct_eq(grant.challenge.as_bytes()))
        {
            return Err(CloudError::new(
                "invalid_grant",
                "client, redirect, resource or PKCE binding mismatch",
            ));
        }
        let grant = state.codes.remove(&key).ok_or_else(unavailable)?;
        drop(state);
        let agent = self.poll_device(&grant.device, &service.auth)?;
        // The underlying account credential is never disclosed. OAuth has a
        // different opaque token, audience, and scope; no prefix bypass exists.
        let secret = format!("bog_oauth_{}", random_secret()?);
        let db = self.db.lock().map_err(|_| unavailable())?;
        db.execute("DELETE FROM oauth_access WHERE expires<=?1", [now()])
            .map_err(db_error)?;
        db.execute(
            "INSERT INTO oauth_access VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                hash(secret.as_bytes()),
                agent.id,
                grant.scope,
                self.resource(),
                grant.client,
                now() + 30 * 86400
            ],
        )
        .map_err(db_error)?;
        Ok(
            json!({"access_token":secret,"token_type":"Bearer","expires_in":2592000,"scope":grant.scope}),
        )
    }
    pub(crate) fn authenticate_oauth(
        &self,
        service: &CloudService,
        secret: &str,
        workspace: Option<WorkspaceId>,
    ) -> Result<Principal, CloudError> {
        if secret.len() != 74 {
            return Err(CloudError::new("unauthorized", "invalid OAuth credential"));
        }
        let row:Option<(String,String)>=self.db.lock().map_err(|_|unavailable())?.query_row("SELECT agent_id,scope FROM oauth_access WHERE secret_hash=?1 AND resource=?2 AND expires>?3",params![hash(secret.as_bytes()),self.resource(),now()],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(db_error)?;
        let (id, scope) =
            row.ok_or_else(|| CloudError::new("unauthorized", "invalid OAuth credential"))?;
        let mut principal = service.auth.principal_for_agent_id(&id, workspace)?;
        principal.read_only = match scope.as_str() {
            READ => true,
            WRITE => false,
            _ => return Err(unavailable()),
        };
        Ok(principal)
    }
    fn revoke_oauth(
        &self,
        service: &CloudService,
        p: HashMap<String, String>,
    ) -> Result<(), CloudError> {
        let secret = field(&p, "token")?;
        let client = field(&p, "client_id")?;
        let id: Option<String> = self
            .db
            .lock()
            .map_err(|_| unavailable())?
            .query_row(
                "SELECT agent_id FROM oauth_access WHERE secret_hash=?1 AND client_id=?2",
                params![hash(secret.as_bytes()), client],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?;
        if let Some(id) = id {
            service.auth.revoke_oauth_agent(&id)?;
        }
        Ok(())
    }
}
pub(crate) async fn endpoint(
    State(service): State<Arc<CloudService>>,
    request: Request,
) -> Response {
    let id = uuid::Uuid::new_v4().to_string();
    let mut r = match inner(&service, request).await {
        Ok(r) => r,
        Err(e) => {
            let status = if e.code == "slow_down" {
                429
            } else if e.code == "temporarily_unavailable" || e.code == "unavailable" {
                503
            } else {
                400
            };
            (
                StatusCode::from_u16(status).unwrap(),
                Json(json!({"error":e.code,"error_description":e.message,"request_id":id})),
            )
                .into_response()
        }
    };
    for (k, v) in [
        ("cache-control", "no-store"),
        ("pragma", "no-cache"),
        ("referrer-policy", "no-referrer"),
        ("access-control-allow-origin", "*"),
        ("access-control-allow-methods", "GET, POST, OPTIONS"),
        ("access-control-allow-headers", "Content-Type"),
    ] {
        r.headers_mut()
            .insert(k, header::HeaderValue::from_static(v));
    }
    r.headers_mut()
        .insert("x-request-id", header::HeaderValue::from_str(&id).unwrap());
    eprintln!(
        "{}",
        json!({"event":"oauth_request","request_id":id,"status":r.status().as_u16()})
    );
    r
}
async fn inner(service: &CloudService, request: Request) -> Result<Response, CloudError> {
    let Some(native) = service.native_auth.as_ref() else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };
    if request.method() == "OPTIONS" {
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    let path = request.uri().path().to_owned();
    if path == "/.well-known/oauth-authorization-server" {
        return Ok(Json(native.oauth_metadata()).into_response());
    }
    if path == "/.well-known/oauth-protected-resource/mcp" {
        return Ok(Json(native.resource_metadata()).into_response());
    }
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|p| p.0.ip());
    if path == "/oauth/authorize" {
        let target = native.authorize_client(
            peer.ok_or_else(unavailable)?,
            parameters(request.uri().query().unwrap_or("").as_bytes())?,
        )?;
        return Ok((StatusCode::SEE_OTHER, [(header::LOCATION, target)]).into_response());
    }
    let content_type = crate::http::header_text(request.headers(), "content-type")
        .split(';')
        .next()
        .unwrap_or("")
        .to_owned();
    let body = to_bytes(request.into_body(), 8192)
        .await
        .map_err(|_| invalid("request too large"))?;
    if path == "/oauth/register" {
        if content_type != "application/json" {
            return Err(invalid("application/json required"));
        }
        return Ok((
            StatusCode::CREATED,
            Json(native.register_client(
                peer.ok_or_else(unavailable)?,
                serde_json::from_slice(&body).map_err(|_| invalid("invalid JSON"))?,
            )?),
        )
            .into_response());
    }
    if content_type != "application/x-www-form-urlencoded" {
        return Err(invalid("application/x-www-form-urlencoded required"));
    }
    let p = parameters(&body)?;
    match path.as_str() {
        "/oauth/token" => Ok(Json(native.exchange(service, p)?).into_response()),
        "/oauth/revoke" => {
            native.revoke_oauth(service, p)?;
            Ok(StatusCode::OK.into_response())
        }
        _ => Ok(StatusCode::NOT_FOUND.into_response()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn persisted_scope_audience_expiry_and_account_suspension_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let config =
            crate::native_auth::GithubConfig::for_loopback_testing("http://127.0.0.1:9998")
                .unwrap();
        let native = NativeAuth::new(config.clone(), &dir.path().join("sessions")).unwrap();
        let service = CloudService::open(
            crate::config::Config::new(dir.path().join("cloud"), "/tmp/unused-worker".into()),
            "test-owner-secret-at-least-thirty-two-bytes",
        )
        .unwrap();
        let identity = crate::oauth::VerifiedIdentity::native("42", (now() + 3600) as u64);
        let (account, workspace) = service.auth.provision_identity(&identity).unwrap();
        let human = service
            .auth
            .principal_from_verified(&identity, workspace.id)
            .unwrap();
        let agent = service
            .auth
            .issue_agent_token(&human, "OAuth fixture")
            .unwrap();
        let secret = format!("bog_oauth_{}", random_secret().unwrap());
        native
            .db
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO oauth_access VALUES(?1,?2,?3,?4,?5,?6)",
                params![
                    hash(secret.as_bytes()),
                    agent.id,
                    READ,
                    native.resource(),
                    "test-client",
                    now() + 3600
                ],
            )
            .unwrap();
        drop(native);
        let reopened = NativeAuth::new(config, &dir.path().join("sessions")).unwrap();
        let principal = reopened
            .authenticate_oauth(&service, &secret, None)
            .unwrap();
        assert!(principal.read_only);
        assert!(service.auth.authorize(&principal, None, false).is_ok());
        assert!(service.auth.authorize(&principal, None, true).is_err());
        assert!(
            service
                .auth
                .issue_agent_token(&principal, "escalation")
                .is_err()
        );
        reopened
            .db
            .lock()
            .unwrap()
            .execute(
                "UPDATE oauth_access SET resource='https://other.example/mcp'",
                [],
            )
            .unwrap();
        assert!(
            reopened
                .authenticate_oauth(&service, &secret, None)
                .is_err()
        );
        reopened
            .db
            .lock()
            .unwrap()
            .execute(
                "UPDATE oauth_access SET resource=?1,expires=0",
                [reopened.resource()],
            )
            .unwrap();
        assert!(
            reopened
                .authenticate_oauth(&service, &secret, None)
                .is_err()
        );
        reopened
            .db
            .lock()
            .unwrap()
            .execute("UPDATE oauth_access SET expires=?1", [now() + 3600])
            .unwrap();
        service
            .registry
            .connection()
            .unwrap()
            .execute(
                "UPDATE accounts SET suspended_at=?1 WHERE id=?2",
                params![now(), account.id],
            )
            .unwrap();
        assert!(
            reopened
                .authenticate_oauth(&service, &secret, None)
                .is_err()
        );
    }
    #[test]
    fn reject_duplicate_parameters_and_unsafe_redirects() {
        assert!(parameters(b"client_id=a&client_id=b").is_err());
        for uri in [
            "javascript:alert(1)",
            "http://example.com/cb",
            "https://example.com/cb#x",
            "https://user@example.com/cb",
            "http://localhost/cb",
        ] {
            assert!(!redirect_valid(uri));
        }
        for uri in [
            "https://example.com/cb",
            "http://127.0.0.1:8123/cb",
            "http://[::1]:8123/cb",
        ] {
            assert!(redirect_valid(uri));
        }
    }
}
