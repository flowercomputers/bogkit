//! GitHub proves identity; opaque sessions and device approval belong to Bog.
use crate::registry::{db_error, hash, now};
use crate::{
    CloudError,
    auth::random_secret,
    browser_auth::{BrowserSession, LOGIN_COOKIE, LoginStart, SESSION_COOKIE},
    oauth::{VerifiedIdentity, bounded_response, denied},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    net::IpAddr,
    path::Path,
    sync::{Arc, Mutex},
};
use subtle::ConstantTimeEq;

pub struct GithubConfig {
    client_id: String,
    secret: String,
    pub redirect_uri: String,
    authorize: String,
    token: String,
    user: String,
}
impl GithubConfig {
    pub fn new(id: &str, secret: &str, redirect: &str) -> Result<Self, CloudError> {
        if id.is_empty()
            || secret.is_empty()
            || redirect != "https://flower-bog-cloud.fly.dev/auth/callback"
        {
            return Err(CloudError::new(
                "invalid_config",
                "GitHub requires complete credentials and the exact production callback",
            ));
        }
        Ok(Self {
            client_id: id.into(),
            secret: secret.into(),
            redirect_uri: redirect.into(),
            authorize: "https://github.com/login/oauth/authorize".into(),
            token: "https://github.com/login/oauth/access_token".into(),
            user: "https://api.github.com/user".into(),
        })
    }
    /// Isolated test transport only; no environment variable can change provider endpoints.
    pub fn for_loopback_testing(origin: &str) -> Result<Self, CloudError> {
        let url = reqwest::Url::parse(origin).map_err(|_| denied())?;
        if url.scheme() != "http"
            || !matches!(url.host_str(), Some("127.0.0.1" | "[::1]"))
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(denied());
        }
        Ok(Self {
            client_id: "test".into(),
            secret: "test-secret".into(),
            redirect_uri: format!("{origin}/auth/callback"),
            authorize: format!("{origin}/authorize"),
            token: format!("{origin}/token"),
            user: format!("{origin}/user"),
        })
    }
    pub fn origin(&self) -> String {
        reqwest::Url::parse(&self.redirect_uri)
            .expect("validated callback")
            .origin()
            .ascii_serialization()
    }
}
pub struct NativeAuth {
    pub config: GithubConfig,
    client: reqwest::Client,
    db: Mutex<Connection>,
    grants: Mutex<DeviceState>,
}
struct Grant {
    public: String,
    name: String,
    expires: i64,
    last_poll: i64,
    decision: Decision,
}
enum Decision {
    Pending,
    Approved(crate::Principal),
    Denied,
}
#[derive(Default)]
struct DeviceState {
    grants: HashMap<Vec<u8>, Grant>,
    sources: HashMap<IpAddr, (i64, u32)>,
    approvals: HashMap<Vec<u8>, (i64, u32)>,
}
fn cookie(name: &str, value: &str, age: i64) -> String {
    format!("{name}={value}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age={age}")
}
fn unavailable() -> CloudError {
    CloudError::new("unavailable", "authentication storage unavailable")
}
fn lock<T>(m: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>, CloudError> {
    m.lock().map_err(|_| unavailable())
}
impl NativeAuth {
    pub fn from_env(root: &Path) -> Result<Option<Arc<Self>>, CloudError> {
        let names = [
            "BOG_GITHUB_CLIENT_ID",
            "BOG_GITHUB_CLIENT_SECRET",
            "BOG_GITHUB_REDIRECT_URI",
        ];
        if names.iter().all(|n| std::env::var_os(n).is_none()) {
            return Ok(None);
        }
        if [
            "BOG_WORKOS_ISSUER",
            "BOG_WORKOS_RESOURCE",
            "BOG_WORKOS_AUDIENCE",
            "BOG_WORKOS_CLIENT_ID",
            "BOG_WORKOS_CLIENT_SECRET",
            "BOG_WORKOS_REDIRECT_URI",
            "BOG_WORKOS_DEVICE_CLIENT_ID",
        ]
        .iter()
        .any(|n| std::env::var_os(n).is_some())
        {
            return Err(CloudError::new(
                "invalid_config",
                "native and WorkOS authentication are mutually exclusive",
            ));
        }
        let values = names
            .iter()
            .map(|n| {
                std::env::var(n).map_err(|_| {
                    CloudError::new("invalid_config", "complete GitHub configuration required")
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Self::new(
            GithubConfig::new(&values[0], &values[1], &values[2])?,
            &root.join("auth-sessions"),
        )
        .map(Some)
    }
    pub fn new(config: GithubConfig, directory: &Path) -> Result<Arc<Self>, CloudError> {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
        if !directory.exists() {
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(directory)
                .map_err(|_| unavailable())?;
        }
        let m = std::fs::symlink_metadata(directory).map_err(|_| unavailable())?;
        if !m.is_dir()
            || m.permissions().mode() & 0o077 != 0
            || m.uid() != unsafe { libc::geteuid() }
        {
            return Err(unavailable());
        }
        let path = directory.join("native-sessions.sqlite3");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|_| unavailable())?;
        let m = file.metadata().map_err(|_| unavailable())?;
        if !m.is_file()
            || m.permissions().mode() & 0o077 != 0
            || m.uid() != unsafe { libc::geteuid() }
        {
            return Err(unavailable());
        }
        let db = Connection::open(path).map_err(db_error)?;
        db.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA secure_delete=ON; CREATE TABLE IF NOT EXISTS native_logins(state BLOB PRIMARY KEY,binding BLOB NOT NULL,verifier TEXT NOT NULL,target TEXT,expires INTEGER NOT NULL); CREATE TABLE IF NOT EXISTS native_sessions(id BLOB PRIMARY KEY,csrf TEXT NOT NULL,subject TEXT NOT NULL,expires INTEGER NOT NULL);").map_err(db_error)?;
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("Bog-Cloud")
            .build()
            .map_err(|_| unavailable())?;
        Ok(Arc::new(Self {
            config,
            client,
            db: Mutex::new(db),
            grants: Mutex::new(DeviceState::default()),
        }))
    }
    pub fn begin_login(&self, public_code: Option<&str>) -> Result<LoginStart, CloudError> {
        if public_code.is_some_and(|c| !valid_public(c)) {
            return Err(denied());
        }
        let state = random_secret()?;
        let binding = random_secret()?;
        let verifier = random_secret()?;
        let mut url = reqwest::Url::parse(&self.config.authorize).map_err(|_| denied())?;
        url.query_pairs_mut().extend_pairs([
            ("client_id", self.config.client_id.as_str()),
            ("redirect_uri", &self.config.redirect_uri),
            ("scope", ""),
            ("state", &state),
            ("code_challenge_method", "S256"),
            (
                "code_challenge",
                &URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
            ),
        ]);
        let db = lock(&self.db)?;
        db.execute("DELETE FROM native_logins WHERE expires<=?1", [now()])
            .map_err(db_error)?;
        let count: i64 = db
            .query_row("SELECT COUNT(*) FROM native_logins", [], |r| r.get(0))
            .map_err(db_error)?;
        if count >= 10000 {
            return Err(CloudError::new("capacity", "login capacity reached"));
        }
        db.execute(
            "INSERT INTO native_logins VALUES(?1,?2,?3,?4,?5)",
            params![
                hash(state.as_bytes()),
                hash(binding.as_bytes()),
                verifier,
                public_code,
                now() + 600
            ],
        )
        .map_err(db_error)?;
        Ok(LoginStart {
            authorization_url: url.into(),
            set_cookie: cookie(LOGIN_COOKIE, &binding, 600),
        })
    }
    pub fn cancel_login(&self, state: &str, binding: &str) -> Result<(), CloudError> {
        if state.len() != 64 || binding.len() != 64 {
            return Err(denied());
        }
        let changed = lock(&self.db)?
            .execute(
                "DELETE FROM native_logins WHERE state=?1 AND binding=?2",
                params![hash(state.as_bytes()), hash(binding.as_bytes())],
            )
            .map_err(db_error)?;
        if changed != 1 {
            return Err(denied());
        }
        Ok(())
    }
    pub async fn complete_login(
        &self,
        state: &str,
        code: &str,
        binding: &str,
    ) -> Result<NativeSession, CloudError> {
        if state.len() != 64 || binding.len() != 64 || code.is_empty() || code.len() > 4096 {
            return Err(denied());
        }
        let (verifier, target) = {
            let db = lock(&self.db)?;
            let row: Option<(Vec<u8>, String, Option<String>, i64)> = db
                .query_row(
                    "SELECT binding,verifier,target,expires FROM native_logins WHERE state=?1",
                    [hash(state.as_bytes())],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()
                .map_err(db_error)?;
            let (stored, verifier, target, expires) = row.ok_or_else(denied)?;
            if expires <= now() || !bool::from(stored.ct_eq(&hash(binding.as_bytes()))) {
                return Err(denied());
            }
            db.execute(
                "DELETE FROM native_logins WHERE state=?1",
                [hash(state.as_bytes())],
            )
            .map_err(db_error)?;
            (verifier, target)
        };
        let response = self
            .client
            .post(&self.config.token)
            .header("accept", "application/json")
            .form(&[
                ("client_id", self.config.client_id.as_str()),
                ("client_secret", &self.config.secret),
                ("code", code),
                ("redirect_uri", &self.config.redirect_uri),
                ("code_verifier", &verifier),
            ])
            .send()
            .await
            .map_err(|_| denied())?;
        let tokens: serde_json::Value =
            serde_json::from_slice(&bounded_response(response, 65536).await?)
                .map_err(|_| denied())?;
        if tokens.get("error").is_some()
            || tokens["token_type"].as_str() != Some("bearer")
            || tokens.get("scope").is_some_and(|v| v.as_str() != Some(""))
        {
            return Err(denied());
        }
        let access = tokens["access_token"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 4096)
            .ok_or_else(denied)?;
        let response = self
            .client
            .get(&self.config.user)
            .bearer_auth(access)
            .header("accept", "application/vnd.github+json")
            .send()
            .await
            .map_err(|_| denied())?;
        let user: serde_json::Value =
            serde_json::from_slice(&bounded_response(response, 65536).await?)
                .map_err(|_| denied())?;
        let id = user["id"]
            .as_u64()
            .filter(|id| *id > 0)
            .ok_or_else(denied)?;
        let secret = random_secret()?;
        let csrf = random_secret()?;
        let expiry = now() + 43200;
        let db = lock(&self.db)?;
        db.execute("DELETE FROM native_sessions WHERE expires<=?1", [now()])
            .map_err(db_error)?;
        db.execute(
            "INSERT INTO native_sessions VALUES(?1,?2,?3,?4)",
            params![hash(secret.as_bytes()), csrf, id.to_string(), expiry],
        )
        .map_err(db_error)?;
        Ok(NativeSession {
            identity: VerifiedIdentity::native(&id.to_string(), expiry as u64),
            set_cookie: cookie(SESSION_COOKIE, &secret, 43200),
            csrf_token: csrf,
            public_code: target,
        })
    }
    pub fn authenticate(&self, id: &str) -> Result<VerifiedIdentity, CloudError> {
        if id.len() != 64 {
            return Err(denied());
        }
        let db = lock(&self.db)?;
        let row: Option<(String, i64)> = db
            .query_row(
                "SELECT subject,expires FROM native_sessions WHERE id=?1 AND expires>?2",
                params![hash(id.as_bytes()), now()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(db_error)?;
        let (subject, expires) = row.ok_or_else(denied)?;
        Ok(VerifiedIdentity::native(&subject, expires as u64))
    }
    pub fn csrf_token(&self, id: &str) -> Result<String, CloudError> {
        self.authenticate(id)?;
        lock(&self.db)?
            .query_row(
                "SELECT csrf FROM native_sessions WHERE id=?1",
                [hash(id.as_bytes())],
                |r| r.get(0),
            )
            .map_err(db_error)
    }
    pub fn check_csrf(&self, id: &str, origin: &str, csrf: &str) -> Result<(), CloudError> {
        if origin != self.config.origin()
            || csrf.len() != 64
            || !bool::from(self.csrf_token(id)?.as_bytes().ct_eq(csrf.as_bytes()))
        {
            return Err(denied());
        }
        Ok(())
    }
    pub fn logout(&self, id: &str, origin: &str, csrf: &str) -> Result<String, CloudError> {
        self.check_csrf(id, origin, csrf)?;
        lock(&self.db)?
            .execute(
                "DELETE FROM native_sessions WHERE id=?1",
                [hash(id.as_bytes())],
            )
            .map_err(db_error)?;
        Ok(cookie(SESSION_COOKIE, "", 0))
    }
    pub fn refresh(
        &self,
        id: &str,
        origin: &str,
        csrf: &str,
    ) -> Result<BrowserSession, CloudError> {
        self.check_csrf(id, origin, csrf)?;
        let identity = self.authenticate(id)?;
        let secret = random_secret()?;
        let csrf = random_secret()?;
        let changed = lock(&self.db)?
            .execute(
                "UPDATE native_sessions SET id=?1,csrf=?2 WHERE id=?3 AND expires>?4",
                params![hash(secret.as_bytes()), csrf, hash(id.as_bytes()), now()],
            )
            .map_err(db_error)?;
        if changed != 1 {
            return Err(denied());
        }
        Ok(BrowserSession {
            set_cookie: cookie(
                SESSION_COOKIE,
                &secret,
                identity.expires_at() as i64 - now(),
            ),
            identity,
            csrf_token: csrf,
        })
    }
    pub fn start_device(
        &self,
        source: IpAddr,
        name: &str,
    ) -> Result<serde_json::Value, CloudError> {
        crate::agent_tokens::validate_name(name)?;
        let mut state = lock(&self.grants)?;
        let time = now();
        state.grants.retain(|_, g| g.expires > time);
        state.sources.retain(|_, (start, _)| *start + 600 > time);
        state.approvals.retain(|_, (start, _)| *start + 600 > time);
        if state.sources.len() >= 10000 && !state.sources.contains_key(&source) {
            return Err(device_error("slow_down"));
        }
        let entry = state.sources.entry(source).or_insert((time, 0));
        entry.1 += 1;
        if entry.1 > 20 || state.grants.len() >= 10000 {
            return Err(device_error("slow_down"));
        }
        let private = random_secret()?;
        let public = random_secret()?[..8].to_uppercase();
        if state.grants.values().any(|g| g.public == public) {
            return Err(device_error("slow_down"));
        }
        state.grants.insert(
            hash(private.as_bytes()),
            Grant {
                public: public.clone(),
                name: name.into(),
                expires: time + 600,
                last_poll: 0,
                decision: Decision::Pending,
            },
        );
        Ok(
            serde_json::json!({"device_code":private,"user_code":public,"verification_uri":format!("{}/auth/device/approve",self.config.origin()),"expires_in":600,"interval":5}),
        )
    }
    pub fn device_details(&self, session: &str, public: &str) -> Result<String, CloudError> {
        self.authenticate(session)?;
        let mut state = lock(&self.grants)?;
        approval_limit(&mut state, session)?;
        let grant = state
            .grants
            .values()
            .find(|g| g.public == public && g.expires > now())
            .ok_or_else(|| device_error("expired_token"))?;
        if !matches!(grant.decision, Decision::Pending) {
            return Err(device_error("access_denied"));
        }
        Ok(grant.name.clone())
    }
    pub fn approve_device(
        &self,
        session: &str,
        public: &str,
        principal: crate::Principal,
        approve: bool,
        auth: &crate::Auth,
    ) -> Result<(), CloudError> {
        self.authenticate(session)?;
        auth.authorize(&principal, None, false)?;
        if principal.kind() != crate::PrincipalKind::Human {
            return Err(denied());
        }
        let mut state = lock(&self.grants)?;
        approval_limit(&mut state, session)?;
        let grant = state
            .grants
            .values_mut()
            .find(|g| g.public == public && g.expires > now())
            .ok_or_else(|| device_error("expired_token"))?;
        if !matches!(grant.decision, Decision::Pending) {
            return Err(device_error("access_denied"));
        }
        auth.audit_agent_approval(&principal, approve)?;
        grant.decision = if approve {
            Decision::Approved(principal)
        } else {
            Decision::Denied
        };
        Ok(())
    }
    pub fn poll_device(
        &self,
        private: &str,
        auth: &crate::Auth,
    ) -> Result<crate::auth::IssuedToken, CloudError> {
        if private.len() != 64 {
            return Err(device_error("expired_token"));
        }
        let mut state = lock(&self.grants)?;
        let key = hash(private.as_bytes());
        let grant = state
            .grants
            .get_mut(&key)
            .ok_or_else(|| device_error("expired_token"))?;
        if grant.expires <= now() {
            state.grants.remove(&key);
            return Err(device_error("expired_token"));
        }
        if grant.last_poll + 5 > now() {
            return Err(device_error("slow_down"));
        }
        grant.last_poll = now();
        if matches!(grant.decision, Decision::Pending) {
            return Err(device_error("authorization_pending"));
        }
        // Consume first. A crash can lose approval, but can never issue twice.
        let grant = state
            .grants
            .remove(&key)
            .ok_or_else(|| device_error("expired_token"))?;
        match grant.decision {
            Decision::Approved(p) => auth.issue_agent_token(&p, &grant.name),
            _ => Err(device_error("access_denied")),
        }
    }
}
fn approval_limit(state: &mut DeviceState, session: &str) -> Result<(), CloudError> {
    let time = now();
    state.approvals.retain(|_, (start, _)| *start + 600 > time);
    if state.approvals.len() >= 10000 {
        return Err(device_error("slow_down"));
    }
    let entry = state
        .approvals
        .entry(hash(session.as_bytes()))
        .or_insert((time, 0));
    entry.1 += 1;
    if entry.1 > 30 {
        return Err(device_error("slow_down"));
    }
    Ok(())
}
pub struct NativeSession {
    pub identity: VerifiedIdentity,
    pub set_cookie: String,
    pub csrf_token: String,
    pub public_code: Option<String>,
}
pub fn valid_public(s: &str) -> bool {
    s.len() == 8
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_lowercase())
}
fn device_error(code: &str) -> CloudError {
    CloudError::new(code, "device authorization is not available")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (
        tempfile::TempDir,
        Arc<NativeAuth>,
        crate::Auth,
        crate::Principal,
        String,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let native = NativeAuth::new(
            GithubConfig::for_loopback_testing("http://127.0.0.1").unwrap(),
            &dir.path().join("sessions"),
        )
        .unwrap();
        let registry =
            Arc::new(crate::Registry::open(&dir.path().join("registry.sqlite")).unwrap());
        let auth = crate::Auth::new(registry, "owner-credential-with-at-least-32-bytes").unwrap();
        let identity = VerifiedIdentity::native("42", (now() + 43200) as u64);
        let (_, w) = auth.provision_identity(&identity).unwrap();
        let p = auth.principal_from_verified(&identity, w.id).unwrap();
        let session = random_secret().unwrap();
        native
            .db
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO native_sessions VALUES(?1,?2,'42',?3)",
                params![
                    hash(session.as_bytes()),
                    random_secret().unwrap(),
                    now() + 43200
                ],
            )
            .unwrap();
        (dir, native, auth, p, session)
    }
    #[test]
    fn device_denial_expiry_slowdown_suspension_and_single_issuance() {
        let (_dir, native, auth, p, session) = fixture();
        let start = native
            .start_device("127.0.0.1".parse().unwrap(), "agent")
            .unwrap();
        let private = start["device_code"].as_str().unwrap();
        let public = start["user_code"].as_str().unwrap();
        assert_eq!(
            native.poll_device(private, &auth).err().unwrap().code,
            "authorization_pending"
        );
        assert_eq!(
            native.poll_device(private, &auth).err().unwrap().code,
            "slow_down"
        );
        native
            .approve_device(&session, public, p.clone(), false, &auth)
            .unwrap();
        native
            .grants
            .lock()
            .unwrap()
            .grants
            .get_mut(&hash(private.as_bytes()))
            .unwrap()
            .last_poll = 0;
        assert_eq!(
            native.poll_device(private, &auth).err().unwrap().code,
            "access_denied"
        );
        assert_eq!(
            native.poll_device(private, &auth).err().unwrap().code,
            "expired_token"
        );
        let start = native
            .start_device("127.0.0.1".parse().unwrap(), "agent")
            .unwrap();
        let private = start["device_code"].as_str().unwrap();
        native
            .grants
            .lock()
            .unwrap()
            .grants
            .get_mut(&hash(private.as_bytes()))
            .unwrap()
            .expires = now() - 1;
        assert_eq!(
            native.poll_device(private, &auth).err().unwrap().code,
            "expired_token"
        );
        let start = native
            .start_device("127.0.0.1".parse().unwrap(), "agent")
            .unwrap();
        let private = start["device_code"].as_str().unwrap();
        let public = start["user_code"].as_str().unwrap();
        native
            .approve_device(&session, public, p.clone(), true, &auth)
            .unwrap();
        assert!(
            native
                .approve_device(&session, public, p.clone(), true, &auth)
                .is_err()
        );
        std::thread::scope(|scope| {
            let a = scope.spawn(|| native.poll_device(private, &auth).is_ok());
            let b = scope.spawn(|| native.poll_device(private, &auth).is_ok());
            assert_ne!(a.join().unwrap(), b.join().unwrap());
        });
        let start = native
            .start_device("127.0.0.1".parse().unwrap(), "agent")
            .unwrap();
        let private = start["device_code"].as_str().unwrap();
        let public = start["user_code"].as_str().unwrap();
        native
            .approve_device(&session, public, p.clone(), true, &auth)
            .unwrap();
        auth.registry
            .connection()
            .unwrap()
            .execute("UPDATE accounts SET suspended_at=?1", [now()])
            .unwrap();
        assert!(native.poll_device(private, &auth).is_err());
        let count: i64 = auth
            .registry
            .connection()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM agent_tokens", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }
    #[test]
    fn refresh_rotates_without_extension_and_expired_sessions_fail() {
        let (_dir, native, _auth, _p, session) = fixture();
        let csrf = native.csrf_token(&session).unwrap();
        let identity = native.authenticate(&session).unwrap();
        let rotated = native.refresh(&session, "http://127.0.0.1", &csrf).unwrap();
        let new = rotated
            .set_cookie
            .split(';')
            .next()
            .unwrap()
            .split_once('=')
            .unwrap()
            .1;
        assert!(native.authenticate(&session).is_err());
        assert_eq!(
            native.authenticate(new).unwrap().expires_at(),
            identity.expires_at()
        );
        native
            .db
            .lock()
            .unwrap()
            .execute("UPDATE native_sessions SET expires=?1", [now() - 1])
            .unwrap();
        assert!(native.authenticate(new).is_err());
        assert!(native.csrf_token(new).is_err());
    }
    #[test]
    fn bounded_device_sources_and_approval_attempts() {
        let (_dir, native, _auth, _p, session) = fixture();
        for _ in 0..20 {
            native
                .start_device("127.0.0.1".parse().unwrap(), "agent")
                .unwrap();
        }
        assert_eq!(
            native
                .start_device("127.0.0.1".parse().unwrap(), "agent")
                .unwrap_err()
                .code,
            "slow_down"
        );
        for _ in 0..30 {
            assert!(native.device_details(&session, "BADCODE0").is_err());
        }
        assert_eq!(
            native
                .device_details(&session, "BADCODE0")
                .unwrap_err()
                .code,
            "slow_down"
        );
    }
}
#[cfg(test)]
mod login_failure_tests {
    use super::*;
    #[tokio::test]
    async fn expired_and_cancelled_logins_fail_and_preserve_only_public_targets() {
        let dir = tempfile::tempdir().unwrap();
        let native = NativeAuth::new(
            GithubConfig::for_loopback_testing("http://127.0.0.1").unwrap(),
            &dir.path().join("sessions"),
        )
        .unwrap();
        for target in ["https://evil.test", "/console", "ABCDEF01/private-secret"] {
            assert!(native.begin_login(Some(target)).is_err());
        }
        let l = native.begin_login(Some("ABCDEF01")).unwrap();
        let url = reqwest::Url::parse(&l.authorization_url).unwrap();
        let state = url
            .query_pairs()
            .find(|(k, _)| k == "state")
            .unwrap()
            .1
            .into_owned();
        let binding = l
            .set_cookie
            .split(';')
            .next()
            .unwrap()
            .split_once('=')
            .unwrap()
            .1;
        native.cancel_login(&state, binding).unwrap();
        assert!(
            native
                .complete_login(&state, "code", binding)
                .await
                .is_err()
        );
        let l = native.begin_login(None).unwrap();
        let url = reqwest::Url::parse(&l.authorization_url).unwrap();
        let state = url
            .query_pairs()
            .find(|(k, _)| k == "state")
            .unwrap()
            .1
            .into_owned();
        let binding = l
            .set_cookie
            .split(';')
            .next()
            .unwrap()
            .split_once('=')
            .unwrap()
            .1;
        native
            .db
            .lock()
            .unwrap()
            .execute("UPDATE native_logins SET expires=?1", [now() - 1])
            .unwrap();
        assert!(
            native
                .complete_login(&state, "code", binding)
                .await
                .is_err()
        );
    }
}
