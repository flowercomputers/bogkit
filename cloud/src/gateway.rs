//! Authentication at the public transport boundary.
use crate::{
    CloudError, CloudService, Principal, PrincipalKind, WorkspaceId,
    browser_auth::BrowserAuth,
    oauth::{WorkOsConfig, WorkOsVerifier},
};
use std::sync::Arc;
pub struct PublicAuth {
    pub verifier: Arc<WorkOsVerifier>,
    pub browser: Arc<BrowserAuth>,
}
impl PublicAuth {
    pub fn from_env(root: &std::path::Path) -> Result<Option<Self>, CloudError> {
        let names = [
            "BOG_WORKOS_ISSUER",
            "BOG_WORKOS_RESOURCE",
            "BOG_WORKOS_AUDIENCE",
            "BOG_WORKOS_CLIENT_ID",
            "BOG_WORKOS_CLIENT_SECRET",
            "BOG_WORKOS_REDIRECT_URI",
        ];
        if names.iter().all(|n| std::env::var_os(n).is_none()) {
            return Ok(None);
        }
        let values = names
            .iter()
            .map(|n| {
                std::env::var(n)
                    .map_err(|_| CloudError::new("invalid_config", &format!("{n} required")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut config = WorkOsConfig::new(
            &values[0], &values[1], &values[2], &values[3], &values[4], &values[5],
        )?;
        if let Ok(client) = std::env::var("BOG_WORKOS_DEVICE_CLIENT_ID") {
            config = config.with_device_client_id(&client)?;
        }
        let verifier = WorkOsVerifier::new(config)?;
        let browser = BrowserAuth::new(verifier.clone(), &root.join("auth-sessions"))?;
        Ok(Some(Self { verifier, browser }))
    }
}
impl CloudService {
    pub fn authentication_challenge(&self) -> String {
        self.public_auth
            .as_ref()
            .and_then(|a| reqwest::Url::parse(&a.verifier.config.resource).ok())
            .map(|u| {
                format!(
                    "Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource\"",
                    u.origin().ascii_serialization()
                )
            })
            .unwrap_or_else(|| "Bearer".into())
    }
    pub async fn authenticate_bearer(
        &self,
        token: &str,
        workspace: Option<WorkspaceId>,
    ) -> Result<Principal, CloudError> {
        // JWT-shaped credentials are never retried as opaque application/operator secrets.
        let principal = if token.matches('.').count() == 2 {
            let public = self.public_auth.as_ref().ok_or_else(denied)?;
            let identity = public.verifier.verify_access_token(token).await?;
            let (_, personal) = self.auth.provision_identity(&identity)?;
            self.auth
                .agent_from_verified(&identity, workspace.unwrap_or(personal.id))?
        } else {
            let mut p = self.auth.authenticate(token)?;
            if self.public_auth.is_some() && p.kind() == PrincipalKind::Operator {
                return Err(denied());
            }
            if p.kind() == PrincipalKind::Operator {
                p.workspace_id = Some(WorkspaceId::legacy());
            }
            if workspace.is_some_and(|w| p.workspace_id() != Some(w)) {
                return Err(denied());
            }
            p
        };
        Ok(principal)
    }
}
fn denied() -> CloudError {
    CloudError::new("unauthorized", "valid bearer credentials required")
}
