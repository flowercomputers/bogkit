use crate::{
    Auth, BogId, CloudError, Principal, Registry, Scope, config::Config, supervisor::Supervisor,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    CreateBog {
        name: String,
        template: String,
        idempotency_key: String,
    },
    ListBogs,
    WaitForChange {
        bog_id: BogId,
        cursor: Option<String>,
        timeout_seconds: u64,
    },
    ListWorkspaces,
    ListTokens {
        bog_id: BogId,
    },
    DescribeBog {
        bog_id: BogId,
    },
    Usage {
        bog_id: BogId,
    },
    Schema {
        bog_id: BogId,
    },
    GetRecord {
        bog_id: BogId,
        key: String,
    },
    UpsertRecord {
        bog_id: BogId,
        key: String,
        data: Value,
    },
    DeleteRecord {
        bog_id: BogId,
        key: String,
    },
    ReadView {
        bog_id: BogId,
        view: String,
        limit: Option<usize>,
        offset: Option<usize>,
    },
    Batch {
        bog_id: BogId,
        operations: Value,
    },
    IssueToken {
        bog_id: BogId,
        scope: Scope,
    },
    RevokeToken {
        bog_id: BogId,
        token_id: String,
    },
}
pub struct OperationResult {
    pub status: u16,
    pub body: Value,
}
pub struct CloudService {
    pub changes: crate::changes::ChangeWaiter,
    pub native_auth: Option<Arc<crate::native_auth::NativeAuth>>,
    /// Temporary preview compatibility; never permits cross-workspace operator access.
    pub preview_legacy_operator: bool,
    pub public_auth: Option<crate::gateway::PublicAuth>,
    pub registry: Arc<Registry>,
    pub auth: Auth,
    pub supervisor: Arc<Supervisor>,
    requests: tokio::sync::Semaphore,
    rates: std::sync::Mutex<std::collections::HashMap<String, (std::time::Instant, u32)>>,
}
impl CloudService {
    pub fn authentication_configured(&self) -> bool {
        self.native_auth.is_some() || self.public_auth.is_some()
    }
    pub fn open(config: Config, owner_token: &str) -> Result<Arc<Self>, CloudError> {
        if !config.root.is_absolute() {
            return Err(CloudError::new(
                "invalid_config",
                "absolute service root required",
            ));
        }
        crate::supervisor::private_directory(&config.root)?;
        let registry_path = config.root.join("registry.sqlite");
        if std::fs::symlink_metadata(&registry_path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(CloudError::new(
                "unavailable",
                "registry must not be a symlink",
            ));
        }
        let native_auth = crate::native_auth::NativeAuth::from_env(&config.root)?;
        let registry = Arc::new(Registry::open(&config.root.join("registry.sqlite"))?);
        let auth = Auth::new(registry.clone(), owner_token)?;
        let public_auth = crate::gateway::PublicAuth::from_env(&config.root)?;
        let supervisor = Arc::new(Supervisor::open(config, registry.clone())?);
        Ok(Arc::new(Self {
            registry,
            public_auth,
            native_auth,
            preview_legacy_operator: std::env::var("BOG_PREVIEW_LEGACY_OPERATOR")
                .is_ok_and(|value| value == "true"),
            changes: crate::changes::ChangeWaiter::new(),
            auth,
            supervisor,
            requests: tokio::sync::Semaphore::new(64),
            rates: Default::default(),
        }))
    }
    pub async fn execute(
        &self,
        principal: &Principal,
        operation: Operation,
    ) -> Result<OperationResult, CloudError> {
        if let Operation::WaitForChange {
            bog_id,
            cursor,
            timeout_seconds,
        } = &operation
        {
            self.rate_limit(principal)?;
            let result = if let Some(cursor) = cursor {
                self.changes
                    .wait(
                        self,
                        principal,
                        *bog_id,
                        cursor,
                        std::time::Duration::from_secs(*timeout_seconds),
                    )
                    .await?
            } else {
                self.changes
                    .initialize(
                        self,
                        principal,
                        *bog_id,
                        std::time::Duration::from_secs(*timeout_seconds),
                    )
                    .await?
            };
            return Ok(OperationResult {
                status: 200,
                body: serde_json::to_value(result)
                    .map_err(|_| CloudError::new("unavailable", "cannot describe changes"))?,
            });
        }
        let _permit = self
            .requests
            .try_acquire()
            .map_err(|_| CloudError::new("capacity", "concurrent request limit reached"))?;
        self.rate_limit(principal)?;
        use Operation::*;
        let (target, write) = match &operation {
            WaitForChange { .. } => unreachable!(),
            CreateBog { .. }
            | ListBogs
            | ListWorkspaces
            | ListTokens { .. }
            | IssueToken { .. }
            | RevokeToken { .. } => (None, true),
            DescribeBog { bog_id }
            | Usage { bog_id }
            | Schema { bog_id }
            | GetRecord { bog_id, .. }
            | ReadView { bog_id, .. } => (Some(*bog_id), false),
            UpsertRecord { bog_id, .. } | DeleteRecord { bog_id, .. } | Batch { bog_id, .. } => {
                (Some(*bog_id), true)
            }
        };
        self.auth.authorize(principal, target, write)?;
        let result = match operation {
            WaitForChange { .. } => unreachable!(),
            CreateBog {
                name,
                template,
                idempotency_key,
            } => {
                crate::config::require_free_space(
                    &self.supervisor.config.root,
                    self.supervisor.config.min_free_bytes,
                )?;
                let bog = if principal.kind() != crate::PrincipalKind::Operator
                    && principal.workspace_id().is_some()
                {
                    self.registry.create_for_principal(
                        principal,
                        &name,
                        &template,
                        &idempotency_key,
                        32,
                    )?
                } else {
                    self.registry.create_limited(
                        &name,
                        &template,
                        &idempotency_key,
                        self.supervisor.max_active(),
                    )?
                };
                let supervisor = self.supervisor.clone();
                let registry = self.registry.clone();
                let id = bog.id;
                tokio::spawn(async move {
                    if let Err(e) = supervisor.ensure_running(id).await {
                        let _ =
                            registry.set_status(id, crate::ObservedState::Failed, Some(&e.code));
                    }
                });
                let mut value = serde_json::to_value(&bog)
                    .map_err(|_| CloudError::new("unavailable", "cannot describe database"))?;
                value["api_url"] = Value::String(format!("/v1/bogs/{}", bog.id));
                return Ok(OperationResult {
                    status: 202,
                    body: value,
                });
            }
            ListBogs => {
                serde_json::json!({"bogs":match principal.workspace_id() { Some(w)=>self.registry.list_scoped(w)?,None=>self.registry.list()? }})
            }
            ListWorkspaces => {
                serde_json::json!({"workspaces":self.auth.workspaces_for_principal(principal)?})
            }
            ListTokens { bog_id } => {
                self.auth.authorize(principal, Some(bog_id), false)?;
                serde_json::json!({"tokens":self.auth.list_tokens(principal)?.into_iter().filter(|t|t.bog_id==bog_id).collect::<Vec<_>>()})
            }
            DescribeBog { bog_id } => serde_json::to_value(self.registry.get(bog_id)?)
                .map_err(|_| CloudError::new("unavailable", "cannot describe database"))?,
            IssueToken { bog_id, scope } => {
                let token = self.auth.issue(principal, bog_id, scope)?;
                serde_json::json!({"id":token.id,"token":token.secret,"scope":scope})
            }
            RevokeToken { bog_id, token_id } => {
                if principal.kind() != crate::PrincipalKind::Operator
                    && !self
                        .auth
                        .list_tokens(principal)?
                        .iter()
                        .any(|t| t.bog_id == bog_id && t.id == token_id)
                {
                    return Err(CloudError::new("not_found", "token not found for database"));
                }

                self.auth.revoke(principal, bog_id, &token_id)?;
                return Ok(OperationResult {
                    status: 204,
                    body: Value::Null,
                });
            }
            Usage { bog_id } => {
                self.worker(bog_id, reqwest::Method::GET, "/_cloud/usage".into(), None)
                    .await?
            }
            Schema { bog_id } => {
                let mut response = self
                    .worker(bog_id, reqwest::Method::GET, "/schema".into(), None)
                    .await?;
                response["template_version"] = Value::String("records-v1".into());
                response
            }
            GetRecord { bog_id, key } => {
                self.worker(
                    bog_id,
                    reqwest::Method::GET,
                    format!("/docs/{}", encode_key(&key)?),
                    None,
                )
                .await?
            }
            UpsertRecord { bog_id, key, data } => {
                bog_cloud_records::JsonDocument::try_from_value(data.clone())
                    .map_err(|e| CloudError::new("invalid_request", &e.to_string()))?;
                self.worker(
                    bog_id,
                    reqwest::Method::PUT,
                    format!("/docs/{}", encode_key(&key)?),
                    Some(data),
                )
                .await?
            }
            DeleteRecord { bog_id, key } => {
                self.worker(
                    bog_id,
                    reqwest::Method::DELETE,
                    format!("/docs/{}", encode_key(&key)?),
                    None,
                )
                .await?
            }
            ReadView {
                bog_id,
                view,
                limit,
                offset,
            } => {
                if !["docs", "total"].contains(&view.as_str()) {
                    return Err(CloudError::new("invalid_request", "unknown view"));
                }
                let limit = limit.unwrap_or(100);
                let offset = offset.unwrap_or(0);
                if limit > 1000 || offset > 10_000 {
                    return Err(CloudError::new("invalid_request", "page exceeds limits"));
                }
                self.worker(
                    bog_id,
                    reqwest::Method::GET,
                    format!("/views/{view}?limit={limit}&offset={offset}"),
                    None,
                )
                .await?
            }
            Batch { bog_id, operations } => {
                bog_cloud_records::validate_batch(&operations)
                    .map_err(|e| CloudError::new("invalid_request", &e))?;
                self.worker(
                    bog_id,
                    reqwest::Method::POST,
                    "/batch".into(),
                    Some(operations),
                )
                .await?
            }
        };
        Ok(OperationResult {
            status: 200,
            body: result,
        })
    }
    pub fn rate_limit(&self, principal: &Principal) -> Result<(), CloudError> {
        let key = principal
            .account_id()
            .or(principal.token_id.as_deref())
            .unwrap_or("operator")
            .to_owned();
        let mut rates = self
            .rates
            .lock()
            .map_err(|_| CloudError::new("unavailable", "rate limiter unavailable"))?;
        let now = std::time::Instant::now();
        rates.retain(|_, (start, _)| now.duration_since(*start).as_secs() < 60);
        if !rates.contains_key(&key) && rates.len() >= 10000 {
            return Err(CloudError::new("capacity", "rate limit capacity reached"));
        }
        let entry = rates.entry(key).or_insert((now, 0));
        if entry.1 >= 600 {
            return Err(CloudError::new(
                "capacity",
                "request rate limit reached; retry in one minute",
            ));
        }
        entry.1 += 1;
        Ok(())
    }
    async fn worker(
        &self,
        id: BogId,
        method: reqwest::Method,
        path: String,
        body: Option<Value>,
    ) -> Result<Value, CloudError> {
        if body
            .as_ref()
            .is_some_and(|v| v.to_string().len() > 1024 * 1024)
        {
            return Err(CloudError::new(
                "payload_too_large",
                "request exceeds 1 MiB",
            ));
        }
        if method != reqwest::Method::GET {
            crate::config::require_free_space(
                &self.supervisor.config.root,
                self.supervisor.config.min_free_bytes,
            )?;
        }
        let lease = self.supervisor.lease(id).await?;
        let (status, mut value) = lease.client.request(method, &path, body).await?;
        if let Some(seq) = value.get("seq").and_then(Value::as_u64) {
            let generation = lease.generation;
            value["cursor"] =
                serde_json::json!(crate::changes::cursor_for_response(id, generation, seq));
        }
        if (200..300).contains(&status) {
            return Ok(value);
        }
        let code = match status {
            400 | 422 => "invalid_request",
            404 => "not_found",
            409 => "conflict",
            413 => "payload_too_large",
            _ => "unavailable",
        };
        Err(CloudError::new(
            code,
            value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("database operation failed"),
        ))
    }
}
fn encode_key(key: &str) -> Result<String, CloudError> {
    bog_cloud_records::validate_key(key).map_err(|e| CloudError::new("invalid_request", &e))?;
    Ok(key
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect())
}
