//! `tracker-mcp` — a transport-agnostic core for a Model Context Protocol
//! server over the [Yandex Tracker REST API v3].
//!
//! # Design
//!
//! This crate is **library-first**. All logic — tool schemas, input
//! validation, the HTTP layer, auth, and the error contract — lives here and
//! depends on *no* transport. The companion binary (`tracker-mcp-server`) is a
//! thin stdio wrapper that mounts this library onto an MCP transport. The same
//! library can instead be embedded as a dependency in a larger host (e.g.
//! ai-playground) which supplies its own transport — call [`tool_defs`] to
//! register and [`call_tool`] to invoke, no rewrite required.
//!
//! Because nothing here touches stdio or a global runtime, the crate is
//! import-safe and unit-testable against a mock HTTP server.
//!
//! # Auth model
//!
//! Auth is fully configurable, never hardcoded ([`Config`]):
//!
//! - token scheme — [`TokenKind::OAuth`] (`Authorization: OAuth …`) or
//!   [`TokenKind::Iam`] (`Authorization: Bearer …`);
//! - org header — [`OrgKind::XOrgId`] (`X-Org-ID`) or
//!   [`OrgKind::XCloudOrgId`] (`X-Cloud-Org-ID`).
//!
//! The token is wrapped in [`Secret`] and never logged or printed.
//!
//! # Quick start
//!
//! ```no_run
//! # async fn run() {
//! use serde_json::json;
//! use tracker_mcp::{Config, TokenKind, OrgKind, TrackerClient, call_tool, tool_defs};
//!
//! // Register tools (for MCP `list_tools`).
//! let defs = tool_defs();
//! assert_eq!(defs.len(), 5);
//!
//! // Invoke a tool (for MCP `call_tool`).
//! let cfg = Config::from_env().expect("config from env");
//! let client = TrackerClient::new(cfg);
//! let out = call_tool(&client, "issue_get", &json!({ "key": "TASK-1" })).await;
//! println!("{}", out.text);
//! # }
//! ```
//!
//! [Yandex Tracker REST API v3]: https://yandex.cloud/docs/tracker/about-api

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod auth;
mod client;
mod config;
mod error;
pub mod mcp;
mod tools;

pub use client::{Method, TrackerClient};
pub use config::{Config, OrgKind, Secret, TokenKind, DEFAULT_BASE_URL};
pub use error::TrackerError;
pub use tools::{call_tool, tool_def, tool_defs, CallToolOutput, ToolDef};
