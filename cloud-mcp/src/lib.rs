//! Stateless, authenticated MCP adapter for the shared Bog Cloud operation service.
pub mod tools;
pub mod transport;
pub use transport::{McpOptions, build_mcp_router, build_mcp_router_with_options};

mod output;
mod resources;
