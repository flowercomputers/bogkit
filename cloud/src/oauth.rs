//! WorkOS Connect verification. All URLs derive from administrator configuration.
use crate::CloudError;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header, jwk::JwkSet};
use serde::Deserialize;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

pub(crate) fn denied() -> CloudError {
    CloudError::new("unauthorized", "authentication failed")
}
fn config_error() -> CloudError {
    CloudError::new("invalid_config", "invalid WorkOS configuration")
}
#[derive(Clone)]
pub struct WorkOsConfig {
    pub(crate) issuer: String,
    pub(crate) resource: String,
    pub(crate) audience: String,
    pub(crate) client_id: String,
    device_client_id: Option<String>,
    pub(crate) secret: String,
    pub(crate) redirect_uri: String,
}
impl std::fmt::Debug for WorkOsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkOsConfig")
            .field("issuer", &self.issuer)
            .field("client_id", &self.client_id)
            .field("secret", &"[REDACTED]")
            .finish()
    }
}
impl WorkOsConfig {
    pub fn new(
        issuer: &str,
        resource: &str,
        audience: &str,
        client_id: &str,
        secret: &str,
        redirect_uri: &str,
    ) -> Result<Self, CloudError> {
        Self::validate(
            issuer,
            resource,
            audience,
            client_id,
            secret,
            redirect_uri,
            false,
        )
    }
    /// Explicitly permits HTTP only on numeric loopback addresses for local fixtures.
    pub fn for_loopback_testing(
        issuer: &str,
        resource: &str,
        audience: &str,
        client_id: &str,
        secret: &str,
        redirect_uri: &str,
    ) -> Result<Self, CloudError> {
        Self::validate(
            issuer,
            resource,
            audience,
            client_id,
            secret,
            redirect_uri,
            true,
        )
    }
    fn validate(
        issuer: &str,
        resource: &str,
        audience: &str,
        client_id: &str,
        secret: &str,
        redirect_uri: &str,
        testing: bool,
    ) -> Result<Self, CloudError> {
        for value in [issuer, resource, redirect_uri] {
            let u = reqwest::Url::parse(value).map_err(|_| config_error())?;
            let loopback = u
                .host_str()
                .and_then(|h| h.trim_matches(['[', ']']).parse::<std::net::IpAddr>().ok())
                .is_some_and(|ip| ip.is_loopback());
            if !(u.scheme() == "https" || testing && u.scheme() == "http" && loopback)
                || u.host().is_none()
                || !u.username().is_empty()
                || u.password().is_some()
                || u.fragment().is_some()
                || u.query().is_some()
            {
                return Err(config_error());
            }
        }
        let issuer_url = reqwest::Url::parse(issuer).map_err(|_| config_error())?;
        if issuer_url.path() != "/"
            || [audience, client_id, secret]
                .iter()
                .any(|s| s.is_empty() || s.len() > 4096 || s.chars().any(char::is_control))
        {
            return Err(config_error());
        }
        Ok(Self {
            issuer: issuer.trim_end_matches('/').into(),
            resource: resource.into(),
            audience: audience.into(),
            client_id: client_id.into(),
            device_client_id: None,
            secret: secret.into(),
            redirect_uri: redirect_uri.into(),
        })
    }
    pub fn issuer(&self) -> &str {
        &self.issuer
    }
    pub fn resource_metadata(&self) -> serde_json::Value {
        serde_json::json!({"resource":self.resource,"authorization_servers":[self.issuer],"bearer_methods_supported":["header"]})
    }
    pub fn with_device_client_id(mut self, client_id: &str) -> Result<Self, CloudError> {
        if client_id.is_empty()
            || client_id.len() > 256
            || !client_id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
        {
            return Err(config_error());
        }
        self.device_client_id = Some(client_id.into());
        Ok(self)
    }
    pub fn auth_markdown(&self) -> String {
        let mut doc = format!(
            "# Bog Cloud authentication\n\nIssuer: `{}`. Resource: `{}`. Access-token audience: `{}`. Discovery: `{}/.well-known/openid-configuration`. Browser users sign in through WorkOS AuthKit using authorization code with PKCE; GitHub must be enabled in WorkOS. Send access tokens only in the Authorization bearer header.\n",
            self.issuer, self.resource, self.audience, self.issuer
        );
        if let Some(client) = &self.device_client_id {
            doc.push_str(&format!("\n## CLI device authorization\n\nPublic client ID: `{client}`. This client must be registered as a public WorkOS Connect application with CLI Auth enabled. No client secret is distributed.\n\n1. POST form data to `{issuer}/oauth2/device_authorization` with `client_id={client}` and `scope=openid profile email offline_access`.\n2. Show the returned `verification_uri` and `user_code` to the user (or open `verification_uri_complete`). Keep `device_code` private.\n3. Poll `{issuer}/oauth2/token` with form fields `grant_type=urn:ietf:params:oauth:grant-type:device_code`, `device_code` from step 1, and `client_id={client}`. Wait at least the returned `interval` seconds (default 5). Continue on `authorization_pending`; add 5 seconds on `slow_down`. Stop on `access_denied`, `expired_token`, or the returned `expires_in` deadline.\n4. Send the returned `access_token` as `Authorization: Bearer <access_token>` to this resource. Keep access/refresh tokens in private local storage, never query strings. Refresh at the same token endpoint with `grant_type=refresh_token`, `client_id={client}`, and `refresh_token`; save any replacement refresh token.\n\n[WorkOS CLI Auth reference](https://workos.com/docs/reference/workos-connect/cli-auth). Bog does not issue OAuth tokens.\n",issuer=self.issuer));
        } else {
            doc.push_str("\nCLI device authentication is not configured. The operator must register a public WorkOS Connect CLI client before publishing device-login instructions.\n");
        }
        doc
    }
}
#[derive(Clone, Debug)]
pub struct VerifiedIdentity {
    issuer: String,
    subject: String,
    client_id: Option<String>,
    expires_at: u64,
}
impl VerifiedIdentity {
    pub(crate) fn native(subject: &str, expires_at: u64) -> Self {
        Self {
            issuer: "https://github.com".into(),
            subject: subject.into(),
            client_id: None,
            expires_at,
        }
    }
    pub fn issuer(&self) -> &str {
        &self.issuer
    }
    pub fn subject(&self) -> &str {
        &self.subject
    }
    pub fn client_id(&self) -> Option<&str> {
        self.client_id.as_deref()
    }
    pub fn expires_at(&self) -> u64 {
        self.expires_at
    }
}
#[derive(Deserialize, Clone)]
pub(crate) struct Claims {
    pub iss: String,
    pub sub: String,
    pub exp: u64,
    pub aud: serde_json::Value,
    pub nonce: Option<String>,
    pub client_id: Option<String>,
    pub azp: Option<String>,
}
struct Cache {
    keys: Option<JwkSet>,
    fetched: Option<Instant>,
    attempted: Option<Instant>,
}
pub struct WorkOsVerifier {
    pub(crate) config: WorkOsConfig,
    pub(crate) client: reqwest::Client,
    cache: Mutex<Cache>,
}
impl WorkOsVerifier {
    pub fn new(config: WorkOsConfig) -> Result<Arc<Self>, CloudError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(8))
            .build()
            .map_err(|_| config_error())?;
        Ok(Arc::new(Self {
            config,
            client,
            cache: Mutex::new(Cache {
                keys: None,
                fetched: None,
                attempted: None,
            }),
        }))
    }
    pub async fn verify_access_token(&self, token: &str) -> Result<VerifiedIdentity, CloudError> {
        let c = self.verify(token, &self.config.audience).await?;
        if c.nonce.is_some()
            || c.client_id
                .as_deref()
                .is_none_or(|id| id.is_empty() || id.len() > 256)
        {
            return Err(denied());
        }
        Ok(identity(c))
    }
    pub(crate) async fn verify_id_token(
        &self,
        token: &str,
        nonce: Option<&str>,
    ) -> Result<VerifiedIdentity, CloudError> {
        let c = self.verify(token, &self.config.client_id).await?;
        if (c.aud.as_array().is_some_and(|a| a.len() > 1)
            && c.azp.as_deref() != Some(self.config.client_id.as_str()))
            || nonce.is_some_and(|n| c.nonce.as_deref() != Some(n))
            || c.azp.as_deref().is_some_and(|a| a != self.config.client_id)
        {
            return Err(denied());
        }
        Ok(identity(c))
    }
    async fn verify(&self, token: &str, audience: &str) -> Result<Claims, CloudError> {
        if token.len() > 16384 {
            return Err(denied());
        }
        let header = decode_header(token).map_err(|_| denied())?;
        if header.alg != Algorithm::RS256 {
            return Err(denied());
        }
        let kid = header
            .kid
            .filter(|k| !k.is_empty() && k.len() <= 256)
            .ok_or_else(denied)?;
        let mut cache = self.cache.lock().await;
        let stale = cache
            .fetched
            .is_none_or(|t| t.elapsed() > Duration::from_secs(300));
        let missing = cache.keys.as_ref().is_none_or(|k| k.find(&kid).is_none());
        if stale || missing {
            if cache
                .attempted
                .is_some_and(|t| t.elapsed() < Duration::from_secs(5))
            {
                return Err(denied());
            }
            cache.attempted = Some(Instant::now());
            let response = self
                .client
                .get(format!("{}/oauth2/jwks", self.config.issuer))
                .send()
                .await
                .map_err(|_| denied())?;
            let bytes = bounded_response(response, 65536).await?;
            let keys: JwkSet = serde_json::from_slice(&bytes).map_err(|_| denied())?;
            if keys.keys.is_empty() || keys.keys.len() > 32 {
                return Err(denied());
            }
            cache.keys = Some(keys);
            cache.fetched = Some(Instant::now());
        }
        let jwk = cache
            .keys
            .as_ref()
            .and_then(|k| k.find(&kid))
            .ok_or_else(denied)?;
        if jwk
            .common
            .key_algorithm
            .is_some_and(|a| a != jsonwebtoken::jwk::KeyAlgorithm::RS256)
            || jwk
                .common
                .public_key_use
                .as_ref()
                .is_some_and(|u| *u != jsonwebtoken::jwk::PublicKeyUse::Signature)
            || jwk
                .common
                .key_operations
                .as_ref()
                .is_some_and(|ops| !ops.contains(&jsonwebtoken::jwk::KeyOperations::Verify))
        {
            return Err(denied());
        }
        let key = DecodingKey::from_jwk(jwk).map_err(|_| denied())?;
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_issuer(&[&self.config.issuer]);
        validation.set_audience(&[audience]);
        validation.set_required_spec_claims(&["iss", "sub", "aud", "exp"]);
        validation.leeway = 0;
        validation.validate_nbf = true;
        let claims = decode::<Claims>(token, &key, &validation)
            .map_err(|_| denied())?
            .claims;
        if claims.sub.is_empty()
            || claims.sub.len() > 256
            || claims.sub.chars().any(char::is_control)
        {
            return Err(denied());
        }
        Ok(claims)
    }
}
fn identity(c: Claims) -> VerifiedIdentity {
    VerifiedIdentity {
        issuer: c.iss,
        subject: c.sub,
        client_id: c.client_id,
        expires_at: c.exp,
    }
}
pub(crate) async fn bounded_response(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, CloudError> {
    if !response.status().is_success()
        || response.content_length().is_some_and(|n| n > limit as u64)
    {
        return Err(denied());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| denied())? {
        if bytes.len() + chunk.len() > limit {
            return Err(denied());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
