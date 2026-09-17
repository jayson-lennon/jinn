//! Execution vocabulary shared by the orchestrator and every tool impl.

use std::future::Future;
use std::pin::Pin;

use jinn_core_types::tool_types::ToolResult;

/// The boxed future every built-in tool returns from its execute function.
pub type BoxedToolFuture = Pin<Box<dyn Future<Output = ToolResult> + Send>>;

/// The unique name of the `task` subagent tool.
pub const TASK_TOOL_NAME: &str = "task";
