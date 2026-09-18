use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudError {
    pub code: String,
    pub message: String,
}
impl CloudError {
    /// Safe repair metadata for known validation failures; never copies submitted values.
    pub fn fix(&self) -> Option<serde_json::Value> {
        if self.code != "invalid_request" {
            return None;
        }
        let (field, expected) = if self.message.contains("exactly one of key or keys") {
            ("/", serde_json::json!({"oneOf":["key","keys"]}))
        } else if self.message.contains("bounds cannot")
            || self.message.contains("offset") && self.message.contains("bound")
        {
            (
                "/offset",
                serde_json::json!("omit offset with after or before"),
            )
        } else if self.message.contains("name required") {
            ("/name", serde_json::json!("nonempty string"))
        } else if self.message.contains("unsupported creation field") {
            (
                "/",
                serde_json::json!([
                    "name",
                    "template",
                    "definition",
                    "wait",
                    "sandbox",
                    "app_access"
                ]),
            )
        } else {
            return None;
        };
        Some(serde_json::json!({"field":field,"expected":expected}))
    }
    pub fn next_action(&self) -> &'static str {
        match self.code.as_str() {
            "not_found" if self.message.contains("resource operation") => {
                "Call list_resources and choose an exposed action from that resource request_schema. Use the resource name, not the operation name."
            }
            "writes_paused" => {
                "Poll definition_update_status using the update job ID; retry writes after activation or failure. describe_bog includes active_definition_job."
            }
            "revision_conflict" => {
                "Read describe_definition, then plan the update again using its current revision."
            }
            "forbidden" => {
                "Check get_current_context and your workspace membership; supply workspace_id for shared Bogs. Account administration requires an owner in the console."
            }
            "not_found" => {
                "Check the Bog or credential ID and select its workspace explicitly. Other workspaces remain hidden."
            }
            "capacity" => {
                "Check get_current_context for allowances; retry later if the host is busy. No infrastructure expands automatically."
            }
            _ => {
                "Correct the reported argument or operation requirement and retry; reuse a creation key only with the identical body."
            }
        }
    }

    pub fn new(code: &str, message: &str) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}
impl std::fmt::Display for CloudError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for CloudError {}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BogId(pub Uuid);
impl std::fmt::Display for BogId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Read,
    Write,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bog {
    pub id: BogId,
    pub name: String,
    pub template: TemplateId,
    pub template_version: String,
    pub status: ObservedState,
    pub desired_state: DesiredState,
    pub generation: i64,
    pub failure_code: Option<String>,
    pub created_at: i64,
}

macro_rules! string_enum {
 ($name:ident { $($variant:ident => $value:literal),+ $(,)? }) => {
  #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
  pub enum $name { $(#[serde(rename=$value)] $variant),+ }
  impl $name { pub fn as_str(self) -> &'static str { match self {$(Self::$variant=>$value),+} } }
  impl std::str::FromStr for $name {
   type Err=CloudError;
   fn from_str(s:&str)->Result<Self,Self::Err>{match s{$($value=>Ok(Self::$variant)),+, _=>Err(CloudError::new("invalid_request","unsupported value"))}}
  }
 };
}
string_enum!(TemplateId { RecordsV1=>"records-v1" });
string_enum!(DesiredState { Running=>"running", Stopped=>"stopped" });
string_enum!(ObservedState { Creating=>"creating", Ready=>"ready", Stopped=>"stopped", Failed=>"failed", Restoring=>"restoring", Maintenance=>"maintenance" });

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkspaceId(pub Uuid);
impl WorkspaceId {
    pub fn legacy() -> Self {
        Self(Uuid::from_u128(1))
    }
}
impl std::fmt::Display for WorkspaceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct Account {
    pub id: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct Workspace {
    pub uncapped_bogs: bool,
    pub effective_uncapped_bogs: bool,
    pub bog_limit_source: String,
    pub bog_limit: Option<usize>,
    pub personal: bool,
    pub id: WorkspaceId,
    pub name: String,
    pub role: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct Member {
    pub account_id: String,
    pub role: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct TokenInfo {
    pub id: String,
    pub bog_id: BogId,
    pub account_id: Option<String>,
    pub scope: String,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub revoked_at: Option<i64>,
}
