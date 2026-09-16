use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudError {
    pub code: String,
    pub message: String,
}
impl CloudError {
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
