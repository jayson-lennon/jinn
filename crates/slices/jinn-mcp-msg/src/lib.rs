pub mod config;
pub mod mcp_contracts;

pub use config::{
    HeaderExpandError, McpServerConfig, TransportKind, expand_header_value, expand_mcp_headers,
    referenced_header_variables,
};
pub use mcp_contracts::*;
