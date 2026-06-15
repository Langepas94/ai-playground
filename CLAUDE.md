# ai-playground - Claude context

@/Users/artem/.codex/RTK.md

Rust CLI (`ai`) и web UI для OpenAI-compatible LLM providers.

## Cheap context rules

- Run shell through `rtk`.
- Start with `docs/agent-index.md` or `src/README.md`, then only the relevant module README/file.
- Prefer `rtk ast-index search|symbol|outline` for code navigation.
- Do not bulk-read large files. For >500 lines, run `rtk ast-index outline <file>` and read the target slice.
- `src/web/ui.html`, `src/web/mod.rs`, `src/chat/agent.rs`, `src/chat/memory.rs` are large; search by function/id first.

## Required workflow

```bash
rtk cargo build 2>&1 | head -30
rtk cargo fmt
rtk cargo test
```

- New task -> new branch.
- Do not commit/merge without successful build.
- Before committing user-visible changes, bump versions in both `Cargo.toml` files and `Cargo.lock`.
- Version rule: patch for fixes/small internals, minor for compatible CLI/API features, major for breaking changes.

## Map

- CLI: `src/cli/README.md`, `src/cli/mod.rs`, `src/cli/commands/*`.
- Cheap navigation index: `docs/agent-index.md`.
- Web/API/UI: `src/web/README.md`, `src/web/mod.rs`, `src/web/ui.html`.
- Chat/session/context/facts/goal: `src/chat/README.md`.
- Providers/payload/debug/billing: `src/providers/README.md`, `src/providers/openai_compatible.rs`.
- Config/secrets/errors: `src/config.rs`, `src/secrets.rs`, `src/errors.rs`.
- HTTP tests: `tests/http_mock.rs`.

## Invariants

- Never persist or print full tokens.
- Config and secrets stay separate.
- CLI and web share provider clients.
- `AppError` is the user-facing error path.
- Keep versions synchronized.
