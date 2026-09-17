//! Server-side sessions: no provider tokens are returned to the browser.
use crate::{
    CloudError,
    auth::random_secret,
    oauth::{VerifiedIdentity, WorkOsVerifier, bounded_response, denied},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use subtle::ConstantTimeEq;
pub const SESSION_COOKIE: &str = "__Host-bog_session";
pub const LOGIN_COOKIE: &str = "__Host-bog_login";
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
fn hash(s: &str) -> Vec<u8> {
    Sha256::digest(s.as_bytes()).to_vec()
}
fn storage_error(_: impl std::fmt::Display) -> CloudError {
    CloudError::new("unavailable", "authentication storage unavailable")
}
fn cookie(name: &str, value: &str, age: i64) -> String {
    format!("{name}={value}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age={age}")
}
pub struct LoginStart {
    pub authorization_url: String,
    pub set_cookie: String,
}
pub struct BrowserSession {
    pub identity: VerifiedIdentity,
    pub set_cookie: String,
    pub csrf_token: String,
}
pub struct LogoutResult {
    pub set_cookie: String,
    pub provider_revoked: bool,
}
pub struct BrowserAuth {
    verifier: Arc<WorkOsVerifier>,
    db: Mutex<Connection>,
    operations: tokio::sync::Mutex<()>,
}
#[derive(Deserialize)]
struct Tokens {
    access_token: String,
    id_token: Option<String>,
    refresh_token: Option<String>,
    token_type: String,
}
impl BrowserAuth {
    /// `directory` must be dedicated to secrets and owned by the service user.
    pub fn new(verifier: Arc<WorkOsVerifier>, directory: &Path) -> Result<Arc<Self>, CloudError> {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
        if !directory.exists() {
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(directory)
                .map_err(storage_error)?;
        }
        let meta = std::fs::symlink_metadata(directory).map_err(storage_error)?;
        if !meta.is_dir()
            || meta.permissions().mode() & 0o077 != 0
            || meta.uid() != unsafe { libc::geteuid() }
        {
            return Err(storage_error("private directory required"));
        }
        let path = directory.join("sessions.sqlite3");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .map_err(storage_error)?;
        let meta = file.metadata().map_err(storage_error)?;
        if !meta.is_file()
            || meta.permissions().mode() & 0o077 != 0
            || meta.uid() != unsafe { libc::geteuid() }
        {
            return Err(storage_error("private file required"));
        }
        let db = Connection::open(&path).map_err(storage_error)?;
        db.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(storage_error)?;
        db.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA secure_delete=ON; CREATE TABLE IF NOT EXISTS logins(state BLOB PRIMARY KEY,binding BLOB NOT NULL,verifier TEXT NOT NULL,nonce TEXT NOT NULL,expires INTEGER NOT NULL); CREATE TABLE IF NOT EXISTS sessions(id BLOB PRIMARY KEY,csrf TEXT NOT NULL,access TEXT NOT NULL,refresh TEXT,issuer TEXT NOT NULL,subject TEXT NOT NULL,expires INTEGER NOT NULL);").map_err(storage_error)?;
        Ok(Arc::new(Self {
            verifier,
            db: Mutex::new(db),
            operations: tokio::sync::Mutex::new(()),
        }))
    }
    pub fn begin_login(&self) -> Result<LoginStart, CloudError> {
        let state = random_secret()?;
        let binding = random_secret()?;
        let verifier = random_secret()?;
        let nonce = random_secret()?;
        let mut url =
            reqwest::Url::parse(&format!("{}/oauth2/authorize", self.verifier.config.issuer))
                .map_err(storage_error)?;
        url.query_pairs_mut().extend_pairs([
            ("response_type", "code"),
            ("client_id", &self.verifier.config.client_id),
            ("redirect_uri", &self.verifier.config.redirect_uri),
            ("scope", "openid profile email offline_access"),
            ("state", &state),
            ("nonce", &nonce),
            ("code_challenge_method", "S256"),
            (
                "code_challenge",
                &URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
            ),
            ("resource", &self.verifier.config.resource),
        ]);
        let db = self.db.lock().map_err(storage_error)?;
        db.execute("DELETE FROM logins WHERE expires<=?1", [now()])
            .map_err(storage_error)?;
        let count: i64 = db
            .query_row("SELECT COUNT(*) FROM logins", [], |r| r.get(0))
            .map_err(storage_error)?;
        if count >= 10000 {
            return Err(storage_error("login capacity"));
        }
        db.execute(
            "INSERT INTO logins VALUES(?1,?2,?3,?4,?5)",
            params![hash(&state), hash(&binding), verifier, nonce, now() + 600],
        )
        .map_err(storage_error)?;
        Ok(LoginStart {
            authorization_url: url.into(),
            set_cookie: cookie(LOGIN_COOKIE, &binding, 600),
        })
    }
    pub async fn complete_login(
        &self,
        state: &str,
        code: &str,
        binding: &str,
    ) -> Result<BrowserSession, CloudError> {
        if state.len() != 64 || binding.len() != 64 || code.is_empty() || code.len() > 4096 {
            return Err(denied());
        }
        let (verifier, nonce) = {
            let db = self.db.lock().map_err(storage_error)?;
            let row: Option<(Vec<u8>, String, String, i64)> = db
                .query_row(
                    "SELECT binding,verifier,nonce,expires FROM logins WHERE state=?1",
                    [hash(state)],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()
                .map_err(storage_error)?;
            let (stored, verifier, nonce, expires) = row.ok_or_else(denied)?;
            if expires <= now() || !bool::from(stored.ct_eq(&hash(binding))) {
                return Err(denied());
            }
            db.execute("DELETE FROM logins WHERE state=?1", [hash(state)])
                .map_err(storage_error)?;
            (verifier, nonce)
        };
        let tokens = self
            .exchange(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("code_verifier", &verifier),
                ("redirect_uri", &self.verifier.config.redirect_uri),
            ])
            .await?;
        let identity = self
            .verifier
            .verify_id_token(tokens.id_token.as_deref().ok_or_else(denied)?, Some(&nonce))
            .await?;
        let access = self
            .verifier
            .verify_access_token(&tokens.access_token)
            .await?;
        if identity.subject() != access.subject() {
            return Err(denied());
        }
        let id = random_secret()?;
        let csrf = random_secret()?;
        let db = self.db.lock().map_err(storage_error)?;
        db.execute("DELETE FROM sessions WHERE expires<=?1", [now()])
            .map_err(storage_error)?;
        db.execute(
            "INSERT INTO sessions VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                hash(&id),
                csrf,
                tokens.access_token,
                tokens.refresh_token,
                identity.issuer(),
                identity.subject(),
                now() + 43200
            ],
        )
        .map_err(storage_error)?;
        Ok(BrowserSession {
            identity,
            set_cookie: cookie(SESSION_COOKIE, &id, 43200),
            csrf_token: csrf,
        })
    }
    pub async fn authenticate(&self, id: &str) -> Result<VerifiedIdentity, CloudError> {
        let (access, _, issuer, subject) = self.session(id)?;
        let identity = self.verifier.verify_access_token(&access).await?;
        if identity.issuer() != issuer || identity.subject() != subject {
            return Err(denied());
        }
        Ok(identity)
    }
    fn session(&self, id: &str) -> Result<(String, Option<String>, String, String), CloudError> {
        if id.len() != 64 {
            return Err(denied());
        }
        self.db
            .lock()
            .map_err(storage_error)?
            .query_row(
                "SELECT access,refresh,issuer,subject FROM sessions WHERE id=?1 AND expires>?2",
                params![hash(id), now()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .map_err(storage_error)?
            .ok_or_else(denied)
    }
    pub fn csrf_token(&self, id: &str) -> Result<String, CloudError> {
        if id.len() != 64 {
            return Err(denied());
        }
        self.db
            .lock()
            .map_err(storage_error)?
            .query_row(
                "SELECT csrf FROM sessions WHERE id=?1 AND expires>?2",
                params![hash(id), now()],
                |r| r.get(0),
            )
            .optional()
            .map_err(storage_error)?
            .ok_or_else(denied)
    }
    /// Require both exact Origin and the session CSRF token on every cookie-authenticated mutation.
    pub fn check_csrf(&self, id: &str, origin: &str, token: &str) -> Result<(), CloudError> {
        let expected = reqwest::Url::parse(&self.verifier.config.redirect_uri)
            .map_err(storage_error)?
            .origin()
            .ascii_serialization();
        if origin != expected
            || token.len() != 64
            || !bool::from(self.csrf_token(id)?.as_bytes().ct_eq(token.as_bytes()))
        {
            return Err(denied());
        }
        Ok(())
    }
    pub async fn refresh(
        &self,
        id: &str,
        origin: &str,
        csrf: &str,
    ) -> Result<VerifiedIdentity, CloudError> {
        self.check_csrf(id, origin, csrf)?;
        let _guard = self.operations.lock().await;
        let (_, refresh, issuer, subject) = self.session(id)?;
        let tokens = self
            .exchange(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh.as_deref().ok_or_else(denied)?),
            ])
            .await?;
        let identity = self
            .verifier
            .verify_access_token(&tokens.access_token)
            .await?;
        if identity.issuer() != issuer || identity.subject() != subject {
            return Err(denied());
        }
        let db = self.db.lock().map_err(storage_error)?;
        let changed = db
            .execute(
                "UPDATE sessions SET access=?1,refresh=?2 WHERE id=?3 AND expires>?4",
                params![
                    tokens.access_token,
                    tokens.refresh_token.or(refresh),
                    hash(id),
                    now()
                ],
            )
            .map_err(storage_error)?;
        if changed != 1 {
            return Err(denied());
        }
        Ok(identity)
    }
    /// Local logout always succeeds before attempting provider revocation. A false
    /// result means discovery did not advertise revocation or the provider failed.
    pub async fn logout(
        &self,
        id: &str,
        origin: &str,
        csrf: &str,
    ) -> Result<LogoutResult, CloudError> {
        self.check_csrf(id, origin, csrf)?;
        let _guard = self.operations.lock().await;
        let (_, refresh, _, _) = self.session(id)?;
        self.db
            .lock()
            .map_err(storage_error)?
            .execute("DELETE FROM sessions WHERE id=?1", [hash(id)])
            .map_err(storage_error)?;
        let provider_revoked = if let Some(token) = refresh {
            self.revoke(&token).await.is_ok()
        } else {
            false
        };
        Ok(LogoutResult {
            set_cookie: cookie(SESSION_COOKIE, "", 0),
            provider_revoked,
        })
    }
    async fn exchange(&self, fields: &[(&str, &str)]) -> Result<Tokens, CloudError> {
        let mut fields = fields.to_vec();
        fields.push(("client_id", &self.verifier.config.client_id));
        fields.push(("client_secret", &self.verifier.config.secret));
        let response = self
            .verifier
            .client
            .post(format!("{}/oauth2/token", self.verifier.config.issuer))
            .form(&fields)
            .send()
            .await
            .map_err(|_| denied())?;
        let tokens: Tokens = serde_json::from_slice(&bounded_response(response, 65536).await?)
            .map_err(|_| denied())?;
        if !tokens.token_type.eq_ignore_ascii_case("bearer") {
            return Err(denied());
        }
        Ok(tokens)
    }
    async fn revoke(&self, token: &str) -> Result<(), CloudError> {
        let response = self
            .verifier
            .client
            .get(format!(
                "{}/.well-known/openid-configuration",
                self.verifier.config.issuer
            ))
            .send()
            .await
            .map_err(|_| denied())?;
        let metadata: serde_json::Value =
            serde_json::from_slice(&bounded_response(response, 65536).await?)
                .map_err(|_| denied())?;
        if metadata["issuer"].as_str() != Some(self.verifier.config.issuer.as_str()) {
            return Err(denied());
        }
        let endpoint = reqwest::Url::parse(
            metadata["revocation_endpoint"]
                .as_str()
                .ok_or_else(denied)?,
        )
        .map_err(|_| denied())?;
        let issuer = reqwest::Url::parse(&self.verifier.config.issuer).map_err(|_| denied())?;
        if endpoint.origin() != issuer.origin()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(denied());
        }
        let response = self
            .verifier
            .client
            .post(endpoint)
            .form(&[
                ("client_id", self.verifier.config.client_id.as_str()),
                ("client_secret", self.verifier.config.secret.as_str()),
                ("token", token),
                ("token_type_hint", "refresh_token"),
            ])
            .send()
            .await
            .map_err(|_| denied())?;
        bounded_response(response, 65536).await?;
        Ok(())
    }
}
