# aiteach

Rust CLI for chatting with LLM providers.

## Stack

- Rust stable
- `clap` for CLI parsing
- `tokio` async runtime
- `reqwest` HTTP client
- `serde` JSON mapping
- `thiserror` for domain errors and `anyhow` at the binary boundary
- `directories` and `toml` for config
- `keyring` for OS keychain token storage

## Providers

Supported provider profiles:

- `openai-compatible`
- `openrouter`
- `deepseek`
- `gigachat`
- `kimi`

All providers use the OpenAI-compatible `/models` and `/chat/completions` request shape. Add a provider by adding a module under `src/providers`, extending `ProviderKind`, and setting default `base_url` and model values.

## Commands

```bash
aiteach profile add work --provider openrouter --model openai/gpt-4.1-mini
aiteach profile list
aiteach profile use work
aiteach profile remove work

aiteach token set --profile work
aiteach token delete --profile work

aiteach models list --profile work
aiteach ask --profile work "Explain Rust ownership briefly"
aiteach chat --profile work

aiteach config path
aiteach doctor --profile work
```

## Config And Secrets

Config stores only profile metadata:

- `provider`
- `model`
- `base_url`
- `token_ref`

Tokens are stored in the OS keychain through the `keyring` crate. The token reference is stable and uses `provider:profile_name`, for example `openrouter:work`.

Tokens are not written to config, logs, stdout, stderr, or git. Error messages report whether a token is missing or likely invalid without revealing the token value.

## Chat

`ask` sends one prompt and prints one answer.

`chat` starts an interactive session with local history. In-chat commands:

- `/exit`
- `/profile`
- `/model`
- `/clear`
- `/save`

Saved history contains only roles and message content. It never stores tokens.

Prompts and responses are not logged by debug output. `--log-conversation` is reserved for explicit future logging behavior.

## Error Handling

The CLI shows user-facing errors by default. Use `--verbose` for internal details.

HTTP errors include:

- provider
- endpoint category
- status code when available
- auth guidance for missing, expired, or invalid tokens
- rate-limit retry hints when `Retry-After` is returned
- network guidance for DNS, TLS, proxy, and timeout issues
- unexpected JSON format messages when provider responses do not match the expected schema

Config errors include the config path and a concrete read, write, or TOML parsing reason.
