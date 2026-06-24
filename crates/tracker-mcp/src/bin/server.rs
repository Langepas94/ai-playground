//! Thin **stdio** wrapper around the `tracker-mcp` library.
//!
//! Transport lives here and *only* here: build an MCP stdio server, mount the
//! shared [`TrackerServer`] handler, and run.
//! Config comes from the environment (see the crate README). For a remote
//! deployment use the `tracker-mcp-http` binary instead.

use rmcp::ServiceExt;
use tracker_mcp::mcp::TrackerServer;
use tracker_mcp::{Config, TrackerClient};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::from_env()?;
    let server = TrackerServer::new(TrackerClient::new(config));

    // Bind to stdio and serve until the peer disconnects.
    let service = server.serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}
