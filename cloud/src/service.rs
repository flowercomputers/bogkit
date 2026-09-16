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
    DescribeBog {
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
    pub registry: Arc<Registry>,
    pub auth: Auth,
    pub supervisor: Arc<Supervisor>,
    requests: tokio::sync::Semaphore,
}
impl CloudService {
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
        let registry = Arc::new(Registry::open(&config.root.join("registry.sqlite"))?);
        let auth = Auth::new(registry.clone(), owner_token)?;
        let supervisor = Arc::new(Supervisor::open(config, registry.clone())?);
        Ok(Arc::new(Self {
            registry,
            auth,
            supervisor,
            requests: tokio::sync::Semaphore::new(64),
        }))
    }
    pub async fn execute(
        &self,
        principal: &Principal,
        operation: Operation,
    ) -> Result<OperationResult, CloudError> {
        let _permit = self
            .requests
            .try_acquire()
            .map_err(|_| CloudError::new("capacity", "concurrent request limit reached"))?;
        use Operation::*;
        let (target, write) = match &operation {
            CreateBog { .. } | ListBogs | IssueToken { .. } | RevokeToken { .. } => (None, true),
            DescribeBog { bog_id }
            | Schema { bog_id }
            | GetRecord { bog_id, .. }
            | ReadView { bog_id, .. } => (Some(*bog_id), false),
            UpsertRecord { bog_id, .. } | DeleteRecord { bog_id, .. } | Batch { bog_id, .. } => {
                (Some(*bog_id), true)
            }
        };
        self.auth.authorize(principal, target, write)?;
        let result = match operation {
            CreateBog {
                name,
                template,
                idempotency_key,
            } => {
                crate::config::require_free_space(
                    &self.supervisor.config.root,
                    self.supervisor.config.min_free_bytes,
                )?;
                let bog = self.registry.create_limited(
                    &name,
                    &template,
                    &idempotency_key,
                    self.supervisor.max_active(),
                )?;
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
            ListBogs => serde_json::json!({"bogs":self.registry.list()?}),
            DescribeBog { bog_id } => serde_json::to_value(self.registry.get(bog_id)?)
                .map_err(|_| CloudError::new("unavailable", "cannot describe database"))?,
            IssueToken { bog_id, scope } => {
                let token = self.auth.issue(principal, bog_id, scope)?;
                serde_json::json!({"id":token.id,"token":token.secret,"scope":scope})
            }
            RevokeToken { bog_id, token_id } => {
                self.auth.revoke(principal, bog_id, &token_id)?;
                return Ok(OperationResult {
                    status: 204,
                    body: Value::Null,
                });
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
        let (status, value) = lease.client.request(method, &path, body).await?;
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
