---
name: run-ai-playground
description: Run the Rust CLI/web UI for LLM providers — start the web server, test API, interact via browser
---

# ai playground — Run Skill

Rust CLI & web UI for LLM providers (OpenAI-compatible, OpenRouter, DeepSeek, GigaChat, Kimi). Web UI at `http://127.0.0.1:8787` supports chat, profiles, token management, parameter controls, and streaming responses in Russian.

## Prerequisites

- Rust 1.xx stable (via `rustup`)
- macOS/Linux (tested on macOS)
- Optional: `jq` for pretty-printing JSON responses

No Node.js, Python, or external services required.

## Build

```bash
cd /Users/artem/Documents/Ai\ tech\ rust
rtk cargo build 2>&1 | tail -10
```

Binary: `target/debug/ai`

## Setup (one-time)

Web UI requires a profile with provider + model. Create minimal config:

```bash
mkdir -p "$HOME/Library/Application Support/dev.ai-playground.ai-playground"

cat > "$HOME/Library/Application Support/dev.ai-playground.ai-playground/config.toml" << 'EOF'
active_profile = "test"

[profiles.test]
provider = "OpenRouter"
model = "openrouter/auto"
base_url = "https://openrouter.ai/api/v1"
token_ref = "provider:OpenRouter"
EOF
```

For real requests, set token via CLI:
```bash
./target/debug/ai token set --profile test
```

For demo/testing without a real token, skip this step — UI will load but requests will fail gracefully.

## Run: Agent Path

Use the driver script to launch and control the server:

```bash
./.claude/skills/run-ai-playground/driver.sh start
```

Server starts at `http://127.0.0.1:8787`. In another terminal:

```bash
# Test API endpoints
./.claude/skills/run-ai-playground/driver.sh test-api

# Interactive REPL (curl, screenshot, etc.)
./.claude/skills/run-ai-playground/driver.sh interact

# Stop server
./.claude/skills/run-ai-playground/driver.sh stop
```

Driver commands:
- `start` — Launch server, wait for readiness
- `stop` — Gracefully stop
- `test-api` — GET /api/providers, POST /api/token/status, etc.
- `screenshot [file]` — Capture UI with browser driver (requires chromium-cli)
- `interact` — REPL mode: `screenshot`, `test-api`, `curl /path`, `open /path`, `exit`

## Run: Human Path

Direct CLI commands without UI:

```bash
./target/debug/ai ask "What is Rust?"
./target/debug/ai chat                    # Interactive REPL
./target/debug/ai web --listen 127.0.0.1:8787
```

Open browser to `http://127.0.0.1:8787` manually.

## Test

Unit + integration tests:

```bash
rtk cargo test 2>&1 | tail -30
```

Covers HTTP mocking, config load/save, token deserialization.

## Gotchas

- **No token = graceful failure.** UI loads, but `/api/chat` returns 401 if keyring token missing or invalid.
- **Models list loads async.** "Загружаю модели..." persists until provider pricing catalog syncs (~2–5s).
- **Config location.** Must be exact path shown by `ai config path`. If moved, app falls back to empty config (new profiles mode).
- **Port collision.** If 8787 in use, driver will hang; specify `--listen 127.0.0.1:9000` to use a different port.
- **macOS keyring required.** `Keyring::Entry::password()` will panic if Keychain service unavailable (rare; restart Keychain or logout/in).
- **Browser driver (screenshot).** Requires `chromium-cli` binary in PATH. Falls back to curl-only if missing (no visual capture).

## Troubleshooting

| Symptom | Fix |
|---------|-----|
| `Address already in use (os error 48)` | Kill existing process: `lsof -i :8787 \| grep -v COMMAND \| awk '{print $2}' \| xargs kill -9` |
| Models list never loads ("Загружаю модели...") | Network issue or provider pricing endpoint down. Check logs: `cat /tmp/ai-web.log` |
| `curl /api/token/status` returns empty or parse error | Endpoint may return non-JSON on error. Retry with valid provider: `curl -X POST http://127.0.0.1:8787/api/token/status -H "Content-Type: application/json" -d '{"provider":"OpenRouter"}'` |
| Web UI blank or only shows spinner | Check server running: `curl http://127.0.0.1:8787/ \| head -20`. If empty, rebuild: `cargo build` |
| `chromium-cli: command not found` | Install: `npm install -g chromium-cli` or use curl-only mode (no screenshots). Driver falls back gracefully. |

## API Endpoints

All return JSON. Base: `http://127.0.0.1:8787/api/`.

- `GET /providers` — List LLM providers + constraints
- `GET /agents` — List available agents (local-session-agent, etc.)
- `POST /token/status {provider}` — Check token presence for provider
- `POST /token/save {provider, token}` — Save token to keyring
- `POST /models {provider, base_url}` — Fetch available models
- `POST /chat {agent_id, messages, provider, model, ...}` — Single chat request
- `POST /chat/stream {agent_id, messages, provider, model, ...}` — Streaming response (SSE)
- `POST /agent/session {agent_id}` — Create session
- `GET /pricing/status` — Price catalog sync status
- `POST /pricing/sync` — Force re-fetch provider pricing

## Direct Invocation (Unit Testing)

If you need to test the library without the full server:

```bash
# Import and call provider clients directly
cat > test_direct.rs << 'EOF'
use ai_playground::providers::{ProviderKind, ReqwestProviderClient};

#[tokio::main]
async fn main() {
    let client = ReqwestProviderClient::new().expect("Client init failed");
    println!("Providers available.");
}
EOF

rustc --edition 2024 -L target/debug/deps --extern ai_playground=target/debug/libai_playground.rlib test_direct.rs
./test_direct
```

For most PRs, test via `cargo test` (mocked HTTP) or `driver.sh test-api` (live server).
