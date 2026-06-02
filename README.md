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
aiteach ask --profile work "Верни краткое резюме" --max-tokens 120
aiteach ask --profile work "Верни объект с полями title и bullets" --response-format json-object
aiteach compare --profile work "Сравни Rust и Go" --max-tokens 120 --stop "END"
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
- `/control`
- `/control clear`
- `/format text`
- `/format json-object`
- `/max-tokens <number>`
- `/stop <sequence>`
- `/stop clear`
- `/format-instruction <text>`
- `/completion-instruction <text>`

История сохраняется локально и не содержит токены. Prompt/response не логируются в debug output без явного будущего режима `--log-conversation`.

### Управление Ответом

Пользователь может управлять формой и завершением ответа через `ask`, `chat` и `compare`.

CLI-флаги:

- `--response-format text` - обычный текст, API-поле `response_format` не отправляется.
- `--response-format json-object` - отправляет `response_format: {"type":"json_object"}` и добавляет system-инструкцию вернуть только JSON object.
- `--max-tokens <number>` - отправляет `max_tokens` и ограничивает длину генерации на стороне provider.
- `--stop <sequence>` - можно указать несколько раз; отправляет массив `stop`.
- `--format-instruction <text>` - добавляет system-инструкцию с явным описанием формата ответа.
- `--completion-instruction <text>` - добавляет system-инструкцию с условием завершения ответа, например `Finish after exactly 3 bullets.`

`compare` отправляет один и тот же prompt два раза:

1. Без ограничений: только `model` и `messages`.
2. С ограничениями: тот же prompt плюс выбранные `response_format`, `max_tokens`, `stop` и инструкции.

Пример:

```bash
aiteach compare --profile work \
  "Объясни ownership в Rust для junior developer" \
  --response-format json-object \
  --max-tokens 160 \
  --stop "END" \
  --completion-instruction "Finish after exactly 3 bullet points and then write END."
```

Результат показывает два блока: `Without constraints` и `With constraints`.

### Как Это Реализуется В API

Все текущие provider-профили используют `POST /chat/completions`.

#### OpenAI-compatible

- Endpoint: `POST {base_url}/chat/completions`.
- `response_format` отправляется как `{"type":"json_object"}` для JSON mode.
- `max_tokens` отправляется как верхний лимит output tokens.
- `stop` отправляется как массив stop sequences.
- Явное описание формата и условие завершения добавляются как `system` messages перед user prompt.

Примечание: для некоторых новых OpenAI reasoning-моделей может использоваться `max_completion_tokens`; этот CLI сейчас ориентирован на OpenAI-compatible Chat Completions и отправляет `max_tokens`.

#### OpenRouter

- Endpoint: `POST https://openrouter.ai/api/v1/chat/completions`.
- OpenRouter принимает OpenAI-compatible body и документирует `response_format`, `max_tokens` и `stop`.
- CLI добавляет обязательные provider headers `HTTP-Referer` и `X-Title`, затем отправляет те же control-поля.
- Provider нормализует `finish_reason`, включая `stop` и `length`.

Docs: [OpenRouter parameters](https://openrouter.ai/docs/api/reference/parameters), [OpenRouter API overview](https://openrouter.ai/docs/api-reference/overview).

#### DeepSeek

- Endpoint: `POST https://api.deepseek.com/v1/chat/completions`.
- DeepSeek JSON Output реализуется через `response_format: {"type":"json_object"}`.
- DeepSeek рекомендует задавать разумный `max_tokens`, чтобы JSON не обрезался посередине.
- `stop` поддерживается как строка или список stop sequences.

Docs: [DeepSeek JSON Output](https://api-docs.deepseek.com/guides/json_mode/), [DeepSeek chat completion](https://api-docs.deepseek.com/zh-cn/api/create-chat-completion/).

#### GigaChat

- Endpoint: `POST https://gigachat.devices.sberbank.ru/api/v1/chat/completions`.
- GigaChat Chat Completions использует OpenAI-like request shape с `messages`, `model` и `max_tokens`.
- CLI отправляет `max_tokens`, `stop` и `response_format` в том же body. Если конкретная GigaChat-модель или тариф не поддерживает поле, provider вернет HTTP/API error, который CLI покажет без раскрытия токена.
- Явное описание формата и условие завершения всегда доступны через `system` messages.

Docs: [GigaChat model selection example](https://developers.sber.ru/docs/ru/gigachat/guides/selecting-a-model), [GigaChat streaming example](https://developers.sber.ru/docs/ru/gigachat/guides/response-token-streaming).

#### Kimi / Moonshot

- Endpoint: `POST https://api.moonshot.ai/v1/chat/completions`.
- Kimi/Moonshot работает через OpenAI-compatible Chat Completions.
- CLI отправляет `response_format`, `max_tokens` и `stop` в request body.
- Для thinking-моделей reasoning tokens тоже входят в token budget, поэтому маленький `max_tokens` может оставить мало места для финального ответа.

Docs: [Kimi FAQ](https://platform.kimi.ai/docs/guide/faq), [Kimi thinking models](https://platform.moonshot.ai/docs/guide/use-kimi-k2-thinking-model.en-US).

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
aiteach ask --profile work "Return a short summary" --max-tokens 120
aiteach ask --profile work "Return an object with title and bullets" --response-format json-object
aiteach compare --profile work "Compare Rust and Go" --max-tokens 120 --stop "END"
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
- `/control`
- `/control clear`
- `/format text`
- `/format json-object`
- `/max-tokens <number>`
- `/stop <sequence>`
- `/stop clear`
- `/format-instruction <text>`
- `/completion-instruction <text>`

History is stored locally and does not contain tokens. Prompts and responses are not logged in debug output unless an explicit future `--log-conversation` mode is implemented.

### Response Control

Users can control response shape and completion behavior through `ask`, `chat`, and `compare`.

CLI flags:

- `--response-format text` - regular text; the API field `response_format` is not sent.
- `--response-format json-object` - sends `response_format: {"type":"json_object"}` and adds a system instruction to return only a JSON object.
- `--max-tokens <number>` - sends `max_tokens` and bounds generation length on the provider side.
- `--stop <sequence>` - can be provided multiple times; sends a `stop` array.
- `--format-instruction <text>` - adds a system instruction that explicitly describes the response format.
- `--completion-instruction <text>` - adds a system instruction that describes when to finish, for example `Finish after exactly 3 bullets.`

`compare` sends the same prompt twice:

1. Without constraints: only `model` and `messages`.
2. With constraints: the same prompt plus selected `response_format`, `max_tokens`, `stop`, and instructions.

Example:

```bash
aiteach compare --profile work \
  "Explain Rust ownership to a junior developer" \
  --response-format json-object \
  --max-tokens 160 \
  --stop "END" \
  --completion-instruction "Finish after exactly 3 bullet points and then write END."
```

The result prints two blocks: `Without constraints` and `With constraints`.

### API Implementation Details

All current provider profiles use `POST /chat/completions`.

#### OpenAI-compatible

- Endpoint: `POST {base_url}/chat/completions`.
- `response_format` is sent as `{"type":"json_object"}` for JSON mode.
- `max_tokens` is sent as the output token cap.
- `stop` is sent as an array of stop sequences.
- Explicit format and completion conditions are added as `system` messages before the user prompt.

Note: some newer OpenAI reasoning models use `max_completion_tokens`; this CLI currently targets OpenAI-compatible Chat Completions and sends `max_tokens`.

#### OpenRouter

- Endpoint: `POST https://openrouter.ai/api/v1/chat/completions`.
- OpenRouter accepts an OpenAI-compatible body and documents `response_format`, `max_tokens`, and `stop`.
- The CLI adds provider headers `HTTP-Referer` and `X-Title`, then sends the same control fields.
- The provider normalizes `finish_reason`, including `stop` and `length`.

Docs: [OpenRouter parameters](https://openrouter.ai/docs/api/reference/parameters), [OpenRouter API overview](https://openrouter.ai/docs/api-reference/overview).

#### DeepSeek

- Endpoint: `POST https://api.deepseek.com/v1/chat/completions`.
- DeepSeek JSON Output is implemented with `response_format: {"type":"json_object"}`.
- DeepSeek recommends setting a reasonable `max_tokens` value so JSON is not truncated midway.
- `stop` is supported as a string or a list of stop sequences.

Docs: [DeepSeek JSON Output](https://api-docs.deepseek.com/guides/json_mode/), [DeepSeek chat completion](https://api-docs.deepseek.com/zh-cn/api/create-chat-completion/).

#### GigaChat

- Endpoint: `POST https://gigachat.devices.sberbank.ru/api/v1/chat/completions`.
- GigaChat Chat Completions uses an OpenAI-like request shape with `messages`, `model`, and `max_tokens`.
- The CLI sends `max_tokens`, `stop`, and `response_format` in the same body. If a concrete GigaChat model or plan does not support a field, the provider returns an HTTP/API error and the CLI reports it without exposing the token.
- Explicit format and completion conditions remain available through `system` messages.

Docs: [GigaChat model selection example](https://developers.sber.ru/docs/ru/gigachat/guides/selecting-a-model), [GigaChat streaming example](https://developers.sber.ru/docs/ru/gigachat/guides/response-token-streaming).

#### Kimi / Moonshot

- Endpoint: `POST https://api.moonshot.ai/v1/chat/completions`.
- Kimi/Moonshot works through OpenAI-compatible Chat Completions.
- The CLI sends `response_format`, `max_tokens`, and `stop` in the request body.
- For thinking models, reasoning tokens also count toward the token budget, so a small `max_tokens` value can leave little room for the final answer.

Docs: [Kimi FAQ](https://platform.kimi.ai/docs/guide/faq), [Kimi thinking models](https://platform.moonshot.ai/docs/guide/use-kimi-k2-thinking-model.en-US).

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
