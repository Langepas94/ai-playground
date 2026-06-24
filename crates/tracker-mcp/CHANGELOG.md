# Changelog

All notable changes to `tracker-mcp` are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/), and the project adheres to
[Semantic Versioning](https://semver.org/).

## [0.2.0] — 2026-06-23

### Added

- `tracker-mcp-http` binary: a Streamable-HTTP server transport for remote
  deployment, mounting the shared `tracker_mcp::mcp::TrackerServer` handler.
- Bearer auth gate (`MCP_AUTH_TOKEN`) with constant-time comparison; the binary
  refuses to start without a gate unless `MCP_ALLOW_NO_AUTH=1`. Open `/health`
  probe, optional `MCP_ALLOWED_HOSTS` Host-header allowlist.
- Deploy artifacts: `Dockerfile`, `.dockerignore`, `render.yaml` (root),
  `fly.toml`. README "Remote deploy (HTTP)" section.
- Shared MCP handler extracted into the public `tracker_mcp::mcp` module, reused
  by both the stdio and HTTP binaries.

### Changed

- This crate's `reqwest` now uses `rustls-tls` (was `native-tls`) so the
  container image needs no system OpenSSL.

## [0.1.0] — 2026-06-23

### Added

- Library-first, transport-agnostic core for a Yandex Tracker MCP server.
- Five tools: `issue_get`, `issue_search`, `issue_create`, `issue_update`,
  `comment_add`, each with a validated JSON Schema input.
- Configurable auth: OAuth or IAM token scheme; `X-Org-ID` or `X-Cloud-Org-ID`
  org header. Token wrapped in a masking `Secret` type — never logged.
- Single HTTP helper: 10s timeout, retry ≤2 on `5xx` with backoff, no retry on
  `4xx`, overridable base URL.
- Error mapping for `401/403/404/422` and token-free messages everywhere.
- Thin `tracker-mcp-server` stdio binary (rmcp `ServerHandler`).
- Offline test suite (unit + integration via mock HTTP), the 4-combo auth
  matrix, doctests, and two runnable examples.
