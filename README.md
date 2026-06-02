# aiteach

## Русский

`aiteach` - Rust CLI для общения с LLM-провайдерами из терминала.

### Стек

- Rust stable
- `clap` для CLI
- `tokio` для async runtime
- `reqwest` для HTTP
- `serde` / `serde_json` для JSON
- `thiserror` для ошибок домена
- `anyhow` только на верхнем CLI-уровне
- `directories` + `toml` для config
- `keyring` для хранения токенов в системном хранилище

### Команды

```bash
aiteach profile add work --provider openrouter --model openai/gpt-4.1-mini
aiteach profile list
aiteach profile use work
aiteach profile remove work

aiteach token set --profile work
aiteach token delete --profile work

aiteach models list --profile work
aiteach ask --profile work "Объясни ownership в Rust"
aiteach chat --profile work

aiteach config path
aiteach doctor --profile work
```

### Провайдеры

Поддерживаются профили:

- `openai-compatible`
- `openrouter`
- `deepseek`
- `gigachat`
- `kimi`

Архитектура provider-слоя разделена на две части:

- `ProviderSpec` описывает конкретного провайдера: `kind`, имя, `base_url`, модель по умолчанию, auth-схему и дополнительные HTTP headers.
- `providers/openai_compatible` содержит общий транспорт для OpenAI-compatible `/models` и `/chat/completions`.

Чтобы добавить нового провайдера:

1. Создайте модуль в `src/providers/<provider>.rs`.
2. Верните из него `ProviderSpec`.
3. Добавьте вариант в `ProviderKind`.
4. Подключите модуль в provider registry в `src/providers/mod.rs`.

CLI при этом менять не нужно.

### Config И Secrets

Config хранит только несекретные поля:

- `provider`
- `model`
- `base_url`
- `token_ref`

Токен хранится только в OS keychain через `keyring`. `token_ref` стабилен и строится как `provider:profile_name`, например `openrouter:work`.

Токены нельзя писать в:

- config
- git
- logs
- stdout/stderr
- debug output

Команды показывают только факт наличия токена. Полное значение токена никогда не печатается.

### Chat

`ask` отправляет один prompt и печатает один ответ.

`chat` запускает интерактивную сессию. Внутренние команды:

- `/exit`
- `/profile`
- `/model`
- `/clear`
- `/save`

История сохраняется локально и не содержит токены. Prompt/response не логируются в debug output без явного будущего режима `--log-conversation`.

### Ошибки

По умолчанию CLI показывает user-facing ошибки. `--verbose` включает внутренние детали.

HTTP ошибки содержат:

- provider
- категорию endpoint: `models`, `chat` или `other`
- HTTP status code, если он доступен
- понятное описание auth-проблем
- `Retry-After` для rate limit, если provider его вернул
- подсказки для DNS/TLS/proxy/timeout проблем
- сообщение о неожиданном JSON-формате ответа provider

Config ошибки содержат путь к config и конкретную причину: чтение, запись или TOML parsing.

### Структура

```text
src/
  chat.rs
  cli.rs
  config.rs
  errors.rs
  secrets.rs
  providers/
    mod.rs
    openai_compatible.rs
    openrouter.rs
    deepseek.rs
    gigachat.rs
    kimi.rs
```

### Проверки

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

## English

`aiteach` is a Rust CLI for chatting with LLM providers from the terminal.

### Stack

- Rust stable
- `clap` for CLI parsing
- `tokio` async runtime
- `reqwest` for HTTP
- `serde` / `serde_json` for JSON
- `thiserror` for domain errors
- `anyhow` only at the top CLI boundary
- `directories` + `toml` for config
- `keyring` for secure OS keychain token storage

### Commands

```bash
aiteach profile add work --provider openrouter --model openai/gpt-4.1-mini
aiteach profile list
aiteach profile use work
aiteach profile remove work

aiteach token set --profile work
aiteach token delete --profile work

aiteach models list --profile work
aiteach ask --profile work "Explain Rust ownership"
aiteach chat --profile work

aiteach config path
aiteach doctor --profile work
```

### Providers

Supported profile kinds:

- `openai-compatible`
- `openrouter`
- `deepseek`
- `gigachat`
- `kimi`

The provider layer is split into two responsibilities:

- `ProviderSpec` describes one provider: `kind`, display name, `base_url`, default model, auth scheme, and extra HTTP headers.
- `providers/openai_compatible` implements the shared transport for OpenAI-compatible `/models` and `/chat/completions`.

To add a provider:

1. Create `src/providers/<provider>.rs`.
2. Return a `ProviderSpec` from that module.
3. Add a variant to `ProviderKind`.
4. Register it in `src/providers/mod.rs`.

The CLI does not need to change.

### Config And Secrets

Config stores only non-secret fields:

- `provider`
- `model`
- `base_url`
- `token_ref`

The token is stored only in the OS keychain through `keyring`. `token_ref` is stable and uses `provider:profile_name`, for example `openrouter:work`.

Tokens must never be written to:

- config
- git
- logs
- stdout/stderr
- debug output

Commands only show whether a token exists. The full token value is never printed.

### Chat

`ask` sends one prompt and prints one answer.

`chat` starts an interactive session. In-chat commands:

- `/exit`
- `/profile`
- `/model`
- `/clear`
- `/save`

History is stored locally and does not contain tokens. Prompts and responses are not logged in debug output unless an explicit future `--log-conversation` mode is implemented.

### Errors

By default the CLI prints user-facing errors. `--verbose` enables internal details.

HTTP errors include:

- provider
- endpoint category: `models`, `chat`, or `other`
- HTTP status code when available
- clear auth guidance
- `Retry-After` for rate limits when returned by the provider
- DNS/TLS/proxy/timeout guidance for network failures
- unexpected JSON format messages when provider responses do not match the expected schema

Config errors include the config path and a concrete reason: read, write, or TOML parsing.

### Structure

```text
src/
  chat.rs
  cli.rs
  config.rs
  errors.rs
  secrets.rs
  providers/
    mod.rs
    openai_compatible.rs
    openrouter.rs
    deepseek.rs
    gigachat.rs
    kimi.rs
```

### Checks

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```
