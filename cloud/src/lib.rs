//! Private Bog resource management.
pub mod agent_guide;
pub mod claimable;
mod sandboxes;
pub use sandboxes::{CleanupPreview, CleanupResult};
mod registry;
pub mod types;
pub use registry::Registry;
pub use types::*;
pub mod auth;
pub use auth::{Auth, IssuedToken, Principal};
pub mod config;
pub mod service;
pub mod supervisor;
pub mod worker_client;
pub use service::{CloudService, Operation, OperationResult};
pub mod http;
pub use http::build_rest_router;

pub mod backup;

pub mod admin;

pub mod workspace;
pub use auth::PrincipalKind;
pub use workspace::IssuedInvitation;

pub mod browser_auth;
pub mod changes;
pub mod contract;
pub mod gateway;
pub mod oauth;

pub mod agent_tokens;
pub mod native_auth;

mod native_http;

mod public_discovery;

pub mod agent_discovery;

mod site;

mod client_metadata;
mod native_oauth;

pub mod app_access;

pub mod observability;

pub mod definitions;
pub mod domains;
