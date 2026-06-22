//! Live MCP integration tests against real, publicly available servers.
//!
//! These are `#[ignore]`d by default because they need network access (and
//! `npx` for the stdio case), so CI stays deterministic. Run them on demand:
//!
//! ```sh
//! cargo test --test mcp_live -- --ignored
//! ```

use ai_playground::config::McpServerConfig;
use ai_playground::mcp::connect_and_list;

/// Public reference filesystem server, launched over stdio via `npx`.
#[tokio::test]
#[ignore = "requires network + npx; hits the public @modelcontextprotocol/server-filesystem"]
async fn lists_tools_from_public_filesystem_server() {
    let server = McpServerConfig::Stdio {
        command: "npx".to_string(),
        args: vec![
            "-y".to_string(),
            "@modelcontextprotocol/server-filesystem".to_string(),
            ".".to_string(),
        ],
    };

    let connection = connect_and_list(&server)
        .await
        .expect("connect to public filesystem MCP server");

    assert!(connection.tool_count > 0);
    assert!(connection.tools.iter().any(|tool| tool.name == "read_file"));
}

/// Public DeepWiki server, reached over remote streamable HTTP (no auth).
#[tokio::test]
#[ignore = "requires network; hits the public DeepWiki MCP server over streamable HTTP"]
async fn lists_tools_from_public_deepwiki_server() {
    let server = McpServerConfig::Http {
        url: "https://mcp.deepwiki.com/mcp".to_string(),
    };

    let connection = connect_and_list(&server)
        .await
        .expect("connect to public DeepWiki MCP server");

    assert!(connection.tool_count > 0);
    assert!(
        connection
            .tools
            .iter()
            .any(|tool| tool.name == "ask_question")
    );
}
