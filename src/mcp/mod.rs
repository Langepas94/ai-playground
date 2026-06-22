//! Minimal MCP (Model Context Protocol) client.
//!
//! Connects to an already-running MCP server (stdio child process or remote
//! streamable-HTTP URL), performs the `initialize` handshake, and lists the
//! tools the server exposes. Shared by both the CLI (`ai mcp tools`) and the
//! web API (`/api/mcp/tools`). No tool execution — connect + list only.

use std::time::Duration;

use rmcp::{
    ServiceExt,
    transport::{ConfigureCommandExt, StreamableHttpClientTransport, TokioChildProcess},
};
use serde::Serialize;
use tokio::process::Command;

use crate::config::McpServerConfig;
use crate::errors::AppError;

/// How long to wait for the `initialize` handshake before giving up. A missing
/// or slow-to-download stdio server would otherwise hang the request forever.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(60);

/// A single tool advertised by an MCP server.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct McpToolInfo {
    pub name: String,
    pub description: Option<String>,
}

/// Result of a successful connect + list-tools round trip.
#[derive(Debug, Clone, Serialize)]
pub struct McpConnection {
    /// Server name reported by the `initialize` handshake, when available.
    pub server_name: String,
    /// Server version reported by the `initialize` handshake, when available.
    pub server_version: Option<String>,
    pub tool_count: usize,
    pub tools: Vec<McpToolInfo>,
}

/// Connect to an MCP server described by `config`, run the `initialize`
/// handshake, then list its tools.
///
/// A handshake failure and a list-tools failure are surfaced as distinct
/// [`AppError::Mcp`] messages so callers can tell "could not connect" apart
/// from "connected but listing failed".
pub async fn connect_and_list(config: &McpServerConfig) -> Result<McpConnection, AppError> {
    match config {
        McpServerConfig::Stdio { command, args } => connect_stdio(command, args).await,
        McpServerConfig::Http { url } => connect_http(url).await,
    }
}

async fn connect_stdio(command: &str, args: &[String]) -> Result<McpConnection, AppError> {
    let owned_args: Vec<String> = args.to_vec();
    let transport = TokioChildProcess::new(Command::new(command).configure(|cmd| {
        cmd.args(&owned_args);
    }))
    .map_err(|error| AppError::Mcp(format!("could not launch MCP server `{command}`: {error}")))?;

    let label = format!("{command} {}", owned_args.join(" "));
    serve_and_list(transport, &label).await
}

async fn connect_http(url: &str) -> Result<McpConnection, AppError> {
    let transport = StreamableHttpClientTransport::from_uri(url);
    serve_and_list(transport, url).await.map_err(|error| match error {
        // The remote transport only speaks Streamable HTTP. Make connect
        // failures actionable instead of leaking the raw reqwest/transport chain.
        AppError::Mcp(message) if message.contains("could not connect") => AppError::Mcp(format!(
            "{message}. Check the URL is reachable and serves a Streamable-HTTP MCP endpoint \
             (legacy SSE-only servers are not supported)."
        )),
        other => other,
    })
}

/// Drive the handshake and tool listing over any rmcp client transport.
async fn serve_and_list<T, E, A>(transport: T, label: &str) -> Result<McpConnection, AppError>
where
    T: rmcp::transport::IntoTransport<rmcp::service::RoleClient, E, A>,
    E: std::error::Error + Send + Sync + 'static,
{
    // `serve()` performs the `initialize` handshake. A returned error here means
    // the connection / handshake never succeeded.
    let service = tokio::time::timeout(HANDSHAKE_TIMEOUT, ().serve(transport))
        .await
        .map_err(|_| {
            AppError::Mcp(format!(
                "timed out after {}s waiting for MCP server `{label}` to initialize",
                HANDSHAKE_TIMEOUT.as_secs()
            ))
        })?
        .map_err(|error| {
            AppError::Mcp(format!(
                "could not connect to MCP server `{label}`: {error}"
            ))
        })?;

    let (server_name, server_version) = match service.peer_info() {
        Some(info) => (
            info.server_info.name.clone(),
            Some(info.server_info.version.clone()),
        ),
        None => (label.to_string(), None),
    };

    // Distinct from the handshake error above: we are connected, but the
    // server's tools/list call failed.
    let tools = service.list_all_tools().await.map_err(|error| {
        AppError::Mcp(format!(
            "connected to MCP server `{server_name}` but listing tools failed: {error}"
        ))
    })?;

    let tools: Vec<McpToolInfo> = tools
        .into_iter()
        .map(|tool| McpToolInfo {
            name: tool.name.to_string(),
            description: tool.description.map(|value| value.to_string()),
        })
        .collect();

    // Best-effort graceful shutdown of the child / connection.
    let _ = service.cancel().await;

    Ok(McpConnection {
        server_name,
        server_version,
        tool_count: tools.len(),
        tools,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::ServiceExt;
    use rmcp::handler::server::ServerHandler;
    use rmcp::model::{
        Implementation, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerInfo,
        Tool,
    };
    use rmcp::service::{RequestContext, RoleServer};
    use std::sync::Arc;

    /// In-process MCP server fixture. Advertises a fixed name/version and two
    /// tools — one with a description, one without — to exercise the mapping.
    #[derive(Clone)]
    struct FixtureServer {
        tools: Vec<Tool>,
    }

    impl ServerHandler for FixtureServer {
        fn get_info(&self) -> ServerInfo {
            ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
                .with_server_info(Implementation::new("fixture-mcp-server", "9.9.9"))
        }

        async fn list_tools(
            &self,
            _request: Option<PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListToolsResult, rmcp::ErrorData> {
            Ok(ListToolsResult::with_all_items(self.tools.clone()))
        }
    }

    /// Spawn a fixture server on one end of an in-memory duplex pipe and run
    /// `serve_and_list` (the real handshake + list path) on the other end.
    async fn connect_fixture(tools: Vec<Tool>) -> Result<McpConnection, AppError> {
        let (server_io, client_io) = tokio::io::duplex(4096);
        let server = tokio::spawn(async move {
            let running = FixtureServer { tools }
                .serve(server_io)
                .await
                .expect("server initialize");
            let _ = running.waiting().await;
        });
        let result = serve_and_list(client_io, "fixture").await;
        let _ = server.await;
        result
    }

    #[tokio::test]
    async fn handshake_reports_server_identity_and_maps_tools() {
        let described = Tool::new("alpha", "Alpha tool", serde_json::Map::new());
        let bare = Tool::new_with_raw("beta", None, Arc::new(serde_json::Map::new()));
        let connection = connect_fixture(vec![described, bare])
            .await
            .expect("connect to fixture");

        assert_eq!(connection.server_name, "fixture-mcp-server");
        assert_eq!(connection.server_version.as_deref(), Some("9.9.9"));
        assert_eq!(connection.tool_count, 2);
        assert_eq!(
            connection.tools[0],
            McpToolInfo {
                name: "alpha".to_string(),
                description: Some("Alpha tool".to_string()),
            }
        );
        assert_eq!(
            connection.tools[1],
            McpToolInfo {
                name: "beta".to_string(),
                description: None,
            }
        );
    }

    #[tokio::test]
    async fn handshake_succeeds_with_zero_tools() {
        let connection = connect_fixture(Vec::new())
            .await
            .expect("connect to empty fixture");
        assert_eq!(connection.tool_count, 0);
        assert!(connection.tools.is_empty());
        assert_eq!(connection.server_name, "fixture-mcp-server");
    }

    #[tokio::test]
    async fn stdio_launch_failure_is_mcp_error() {
        let config = McpServerConfig::Stdio {
            command: "definitely-not-a-real-binary-xyz".to_string(),
            args: Vec::new(),
        };
        let error = connect_and_list(&config).await.expect_err("should fail");
        assert!(matches!(error, AppError::Mcp(_)));
        assert!(error.to_string().contains("could not launch MCP server"));
    }
}
