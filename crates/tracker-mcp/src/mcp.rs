//! MCP protocol binding: an [`rmcp`] [`ServerHandler`] that delegates to the
//! transport-agnostic core ([`tool_defs`] / [`call_tool`]).
//!
//! This is the one place the crate maps its own [`ToolDef`](crate::ToolDef) /
//! [`CallToolOutput`](crate::CallToolOutput) onto `rmcp` model types. It is
//! deliberately transport-free: the same handler is mounted on stdio by the
//! `tracker-mcp-server` binary and on Streamable HTTP by the
//! `tracker-mcp-http` binary. The core API (config, client, tools, auth,
//! errors) stays free of any protocol/transport types.

use std::sync::Arc;

use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, Content, Implementation, ListToolsResult,
    PaginatedRequestParams, ProtocolVersion, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer};
use serde_json::Value;

use crate::{call_tool, tool_defs, TrackerClient};

/// MCP server handler delegating every call to the library core.
///
/// Construct one with [`TrackerServer::new`] and hand it to any `rmcp`
/// transport (stdio, Streamable HTTP, …). It is cheap to [`Clone`] — the
/// underlying [`TrackerClient`] shares one HTTP client.
#[derive(Clone)]
pub struct TrackerServer {
    client: TrackerClient,
}

impl TrackerServer {
    /// Wrap a configured [`TrackerClient`] as an MCP handler.
    pub fn new(client: TrackerClient) -> Self {
        Self { client }
    }
}

impl ServerHandler for TrackerServer {
    // rmcp's ServerInfo / ListToolsResult are #[non_exhaustive], so they can
    // only be built via Default + field assignment (no struct literal).
    #[allow(clippy::field_reassign_with_default)]
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.protocol_version = ProtocolVersion::LATEST;
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.server_info = Implementation::from_build_env();
        info.instructions = Some(
            "Yandex Tracker tools: issue_get, issue_search, issue_create, \
             issue_update, comment_add."
                .to_string(),
        );
        info
    }

    #[allow(clippy::field_reassign_with_default)]
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let tools = tool_defs()
            .into_iter()
            .map(|d| {
                let schema = match d.input_schema {
                    Value::Object(map) => map,
                    _ => serde_json::Map::new(),
                };
                Tool::new(d.name, d.description, Arc::new(schema))
            })
            .collect();
        let mut result = ListToolsResult::default();
        result.tools = tools;
        Ok(result)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let args = Value::Object(request.arguments.unwrap_or_default());
        let out = call_tool(&self.client, request.name.as_ref(), &args).await;
        let content = vec![Content::text(out.text)];
        Ok(if out.is_error {
            CallToolResult::error(content)
        } else {
            CallToolResult::success(content)
        })
    }
}
