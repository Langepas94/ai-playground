# tracker-mcp

A **library-first** [Model Context Protocol](https://modelcontextprotocol.io)
server for the [Yandex Tracker REST API v3](https://yandex.cloud/docs/tracker/about-api).

All logic lives in a transport-agnostic library crate (`tracker_mcp`). A thin
binary (`tracker-mcp-server`) wraps it for stdio. The same library can be
embedded as a dependency in a larger host that supplies its own transport — no
rewrite.

- **Library** (`src/lib.rs`): tool schemas, validation, HTTP, auth, errors. No
  stdio, no `main`, no global state.
- **Binary** (`src/bin/server.rs`): builds the stdio transport, mounts the lib,
  runs. ~80 lines.

## Tools

| Tool | Params (type, required, default) | Example call | Example result |
|------|----------------------------------|--------------|----------------|
| `issue_get` | `key` (string, **required**) | `{"key":"TASK-42"}` | issue object |
| `issue_search` | `query` (string, **required**); `per_page` (int 1–100, =50); `page` (int ≥1, =1) | `{"query":"Queue: TASK","per_page":50,"page":1}` | array of issues |
| `issue_create` | `queue` (string, **required**); `summary` (string, **required**); `description` (string); `assignee` (string); `priority` (enum trivial\|minor\|normal\|critical\|blocker, =normal) | `{"queue":"TASK","summary":"Fix bug"}` | created issue |
| `issue_update` | `key` (string, **required**); `fields` (object, **required**) | `{"key":"TASK-42","fields":{"summary":"New"}}` | updated issue |
| `comment_add` | `key` (string, **required**); `text` (string, **required**) | `{"key":"TASK-42","text":"done"}` | comment object |

Schemas are the single source of truth (see `tool_defs()` in `src/tools.rs`).
Every input is validated against its JSON Schema **before** any HTTP call. List
them live:

```bash
cargo run -p tracker-mcp --example list_tools
```

## Result contract

- Success: `{ "content": [{ "type": "text", "text": <pretty JSON> }] }`
- Failure: `{ "content": [{ "type": "text", "text": <message> }], "isError": true }`

Error messages are token-free. Status mapping: `401` → auth, `403` → forbidden,
`404` → not found, `422` → Tracker field errors.

## Auth / environment

| Variable | Required | Default | Notes |
|----------|----------|---------|-------|
| `TRACKER_TOKEN` | yes | — | secret; never logged or printed |
| `TRACKER_TOKEN_KIND` | no | `oauth` | `oauth` → `Authorization: OAuth …`; `iam` → `Authorization: Bearer …` |
| `TRACKER_ORG_ID` | yes | — | organization id value |
| `TRACKER_ORG_KIND` | no | `x_org_id` | `x_org_id` → `X-Org-ID` (Yandex 360); `x_cloud_org_id` → `X-Cloud-Org-ID` (Yandex Cloud) |
| `TRACKER_BASE_URL` | no | `https://api.tracker.yandex.net/v3` | override for tests/self-hosted |

## Standalone: build + register

```bash
cargo build -p tracker-mcp --release
# binary at target/release/tracker-mcp-server
```

Register in an MCP host (e.g. ai-playground `config.mcp_servers`) as a stdio
server:

```json
{
  "tracker": {
    "command": "/path/to/target/release/tracker-mcp-server",
    "args": [],
    "env": {
      "TRACKER_TOKEN": "…",
      "TRACKER_ORG_ID": "…",
      "TRACKER_TOKEN_KIND": "oauth",
      "TRACKER_ORG_KIND": "x_org_id"
    }
  }
}
```

## Embedded: as a dependency

```toml
[dependencies]
tracker-mcp = { path = "crates/tracker-mcp" } # or a git/version dep
```

```rust
use serde_json::json;
use tracker_mcp::{Config, TrackerClient, call_tool, tool_defs};

async fn demo() -> anyhow::Result<()> {
    let _defs = tool_defs();                 // -> MCP list_tools
    let client = TrackerClient::new(Config::from_env()?);
    let out = call_tool(&client, "issue_get", &json!({ "key": "TASK-1" })).await;
    println!("{}", out.text);                // -> MCP call_tool result
    Ok(())
}
```

The host owns the transport; the library stays import-safe.

## Use in any MCP agent

The standalone binary is a plain stdio MCP server, so it drops into **any**
host that speaks MCP — the protocol is the contract, not the language or app.
Build once, then point the host at the binary + env:

```bash
cargo build -p tracker-mcp --release   # -> target/release/tracker-mcp-server
```

The config shape is the same everywhere — `command`, optional `args`, and
`env` with the four auth variables. Per-host config file:

| Host | Config location | Key |
|------|-----------------|-----|
| Claude Desktop | `claude_desktop_config.json` | `mcpServers` |
| Claude Code | `.mcp.json` (project) or `claude mcp add` | `mcpServers` |
| Cursor | `.cursor/mcp.json` | `mcpServers` |
| Cline / Roo (VS Code) | `cline_mcp_settings.json` | `mcpServers` |
| ai-playground | app config | `mcp_servers` |

Example (`mcpServers` form — Claude Desktop / Code / Cursor / Cline):

```json
{
  "mcpServers": {
    "tracker": {
      "command": "/abs/path/target/release/tracker-mcp-server",
      "args": [],
      "env": {
        "TRACKER_TOKEN": "…",
        "TRACKER_ORG_ID": "…",
        "TRACKER_TOKEN_KIND": "oauth",
        "TRACKER_ORG_KIND": "x_org_id"
      }
    }
  }
}
```

Claude Code one-liner:

```bash
claude mcp add tracker /abs/path/target/release/tracker-mcp-server \
  -e TRACKER_TOKEN=… -e TRACKER_ORG_ID=… \
  -e TRACKER_TOKEN_KIND=oauth -e TRACKER_ORG_KIND=x_org_id
```

After registering, the host's `list_tools` shows the five tools and the agent
can call them.

**Two ways to consume, recap:**

- **Standalone binary** — any MCP host (above). Separate process over stdio.
- **Embedded library** — a Rust host adds the crate as a dependency and calls
  `tool_defs()` / `call_tool()` directly, reusing its own transport (see
  [Embedded](#embedded-as-a-dependency)).

### Transport scope

Two thin binaries mount the *same* library:

- `tracker-mcp-server` — **stdio**, for local/desktop MCP hosts.
- `tracker-mcp-http` — **Streamable HTTP**, for remote deployment (below).

The shared MCP handler lives in `tracker_mcp::mcp::TrackerServer`; the library
core stays transport-agnostic.

## Remote deploy (HTTP)

Use this when the MCP host runs elsewhere (e.g. a hosted ai-playground) and
can't spawn a local process. The host connects via the **remote URL** field
(`https://<your-host>/mcp`).

```bash
# Run locally first:
TRACKER_TOKEN=… TRACKER_ORG_ID=… \
TRACKER_TOKEN_KIND=oauth TRACKER_ORG_KIND=x_org_id \
MCP_AUTH_TOKEN=$(openssl rand -hex 32) PORT=8080 \
  cargo run -p tracker-mcp --bin tracker-mcp-http
# GET  /health  -> "ok"  (open, for platform probes)
# POST /mcp     -> MCP Streamable HTTP (requires `Authorization: Bearer <MCP_AUTH_TOKEN>`)
```

### Security (read before deploying)

`issue_create` / `issue_update` are write tools, so an open endpoint is a
public write path into your Tracker. The binary **refuses to start without an
auth gate**:

- `MCP_AUTH_TOKEN=<secret>` — clients must send `Authorization: Bearer <secret>`; **or**
- `MCP_ALLOW_NO_AUTH=1` — opt out, for a trusted private network only.

The Tracker token lives only in the container env, never in the image or logs.
`MCP_ALLOWED_HOSTS=<domain>` optionally pins the `Host` header (DNS-rebinding
hardening); unset disables that check and relies on the Bearer gate.

### Container

```bash
docker build -t tracker-mcp crates/tracker-mcp
docker run -p 8080:8080 \
  -e TRACKER_TOKEN=… -e TRACKER_ORG_ID=… \
  -e TRACKER_TOKEN_KIND=oauth -e TRACKER_ORG_KIND=x_org_id \
  -e MCP_AUTH_TOKEN=$(openssl rand -hex 32) \
  tracker-mcp
```

### Yandex Serverless Containers (Russia, free tier)

**Fastest to production:** free HTTPS endpoint, RU payment, no cold-start tuning needed.

```bash
# 1. Register at cloud.yandex.com, enable Compute Cloud
# 2. Install yc CLI: https://cloud.yandex.com/en/docs/cli/operations/install-cli
# 3. Run the automated deploy script:

bash crates/tracker-mcp/deploy-yandex.sh

# Prompts for:
#   - Yandex folder ID (from console)
#   - Registry name (e.g., tracker-mcp)
#   - Container name (e.g., tracker-mcp-prod)
#   - TRACKER_TOKEN, TRACKER_ORG_ID

# Output: public HTTPS URL (https://…serverless.yandexcloud.net/mcp)
```

**Billing:** ~0₽/мес for typical MCP usage (free tier covers 1M invocations/mo, 10 GB·hour/mo).
**Stateless mode:** `MCP_STATELESS=1` (set by the script) → each request is independent,
container shuts down between calls, saves on long-lived connections.

### Render / Fly.io (alternatives)

- **Render** — `render.yaml` (repo root) is a ready blueprint; free plan,
  auto-TLS, `MCP_AUTH_TOKEN` auto-generated. Free instances sleep when idle, so
  the first request cold-starts (~30-50s) and the handshake may need one retry.
- **Fly.io** — `crates/tracker-mcp/fly.toml`; scales to zero when idle. Set
  secrets with `fly secrets set …` (see the file header).

GitHub itself does **not** host a persistent server (Actions/Pages/Codespaces
are CI/static/ephemeral) — use a container host.

## Tests (offline, no real creds)

```bash
cargo test -p tracker-mcp            # unit + integration (mock HTTP)
cargo test -p tracker-mcp --doc      # doctests
cargo build -p tracker-mcp --examples
cargo clippy -p tracker-mcp --all-targets
RUSTDOCFLAGS="-D warnings" cargo doc -p tracker-mcp --no-deps
```

The whole suite mocks the network boundary (`wiremock`) and injects a fake
token at a fake base URL — no real network, no real credentials. The auth
matrix asserts exact headers for all four `{oauth,iam} × {x_org_id,x_cloud_org_id}`
combinations, and a test asserts the token never appears in any output.

## Manual smoke (optional, human-run)

```bash
export TRACKER_TOKEN=…  TRACKER_ORG_ID=…
export TRACKER_TOKEN_KIND=oauth  TRACKER_ORG_KIND=x_org_id

# Then drive the binary from your MCP host, or embed and call:
#   issue_get   on a known key   -> expect real data
#   issue_search simple query    -> expect non-empty page
#   issue_create in a test queue -> verify in UI, then delete
```

Not part of the automated suite.

## License

MIT OR Apache-2.0.
