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

Первый запуск:

```bash
aiteach setup
```

`setup` показывает список провайдеров, предлагает дефолтную модель, `base_url`, имя профиля и позволяет сразу сохранить токен в OS keychain. Токен не пишется в config.

`profile add` тоже можно запускать без аргументов:

```bash
aiteach profile add
```

Тогда CLI спросит provider/model/base_url/profile name интерактивно.

```bash
aiteach setup

aiteach profile add work --provider openrouter --model openai/gpt-4.1-mini
aiteach profile add
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
aiteach compare-goal --profile work \
  "Собери требования к статье" \
  --required-field topic \
  --required-field audience \
  --required-field format
aiteach chat --profile work

aiteach config path
aiteach doctor --profile work
```

### Реальные Сценарии

#### 1. Первый запуск и проверка профиля

```bash
aiteach setup
aiteach profile list
aiteach doctor
```

Что происходит:

- `setup` помогает выбрать provider и модель.
- Токен сохраняется в OS keychain, а не в config.
- `doctor` проверяет config, активный профиль, `base_url` и наличие токена без вывода секрета.

#### 2. Один короткий ответ

```bash
aiteach ask \
  "Объясни ownership в Rust простыми словами" \
  --max-tokens 140 \
  --completion-instruction "Ответь максимум в 3 коротких пунктах и закончи мысль полностью."
```

Используйте это для быстрых терминальных вопросов, где нужен лаконичный ответ. `max-tokens` задает жесткий верхний предел, а инструкция просит модель не обрывать мысль.

#### 3. JSON для скрипта или пайплайна

```bash
aiteach ask \
  "Разбери задачу: сделать CLI чат на Rust" \
  --response-format json-object \
  --format-instruction "Верни JSON object с полями title, risks, next_steps." \
  --max-tokens 220
```

Такой режим удобен, когда результат дальше читает другой скрипт. CLI отправляет `response_format: {"type":"json_object"}` и добавляет system-инструкцию про формат.

#### 4. Остановить ответ по маркеру

```bash
aiteach ask \
  "Сделай краткий план миграции проекта" \
  --stop "END_OF_ANSWER" \
  --completion-instruction "Дай 5 пунктов максимум. После последнего пункта напиши END_OF_ANSWER."
```

Модель сама завершает мысль и пишет маркер, а API останавливает генерацию на `stop` sequence. Это мягче, чем просто упереться в лимит токенов.

#### 5. Интерактивный чат с управлением ответа

```bash
aiteach chat --max-tokens 180
```

Внутри чата:

```text
/format json-object
/max-tokens 120
/completion-instruction Отвечай только списком из 3 пунктов.
/control
/clear
/exit
```

Это удобно, когда вы настраиваете стиль ответа по ходу разговора.

#### 6. Сравнить один prompt без контроля и с контролем

```bash
aiteach compare \
  "Объясни async Rust" \
  --max-tokens 160 \
  --completion-instruction "Ответь в 3 коротких пунктах без вступления."
```

Результат покажет два блока:

- `Without constraints` - обычный ответ.
- `With constraints` - тот же prompt с ограничениями.

#### 7. Собрать сущность и остановить диалог

```bash
aiteach chat \
  --required-field topic \
  --required-field audience \
  --required-field format \
  --goal-stop-mode combined
```

Пользователь отвечает на уточняющие вопросы, а CLI останавливает диалог только когда:

- все required fields заполнены;
- модель вернула `done: true`.

#### 8. Сравнить способы завершения диалога

```bash
aiteach compare-goal \
  "Собери требования к статье про Rust ownership" \
  --required-field topic \
  --required-field audience \
  --required-field format
```

CLI сравнит три стратегии:

- `state` - остановка по заполненности полей в коде.
- `instruction` - остановка по сигналу модели `done: true`.
- `combined` - оба условия одновременно.

#### 9. Несколько профилей

```bash
aiteach profile add
aiteach profile list
aiteach profile use openrouter
aiteach ask "Проверь, какой профиль сейчас активен"
```

Профиль можно выбрать явно:

```bash
aiteach ask --profile deepseek "Сделай краткое резюме"
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
- `/goal`
- `/goal clear`
- `/goal field <name>`
- `/goal mode manual`
- `/goal mode state`
- `/goal mode instruction`
- `/goal mode combined`

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

### Завершение Диалога

Есть два разных уровня остановки:

- Остановка ответа: `stop`, `max_tokens`, `completion_instruction`. Это завершает одну генерацию provider.
- Остановка диалога: приложение решает, продолжать ли агентный цикл сбора данных.

Для остановки диалога `aiteach` поддерживает сущность с required fields. Модель должна возвращать JSON:

```json
{
  "fields": {
    "topic": "Rust ownership",
    "audience": "junior developers",
    "format": null
  },
  "next_question": "Какой формат нужен?",
  "done": false
}
```

Режимы:

- `manual` - CLI не останавливает диалог автоматически.
- `state` - deterministic code path: CLI останавливается, когда все `--required-field` заполнены не-null значениями.
- `instruction` - agent-instruction path: CLI доверяет сигналу модели `done: true`.
- `combined` - оба условия обязательны: все поля заполнены и `done: true`.

Интерактивный пример:

```bash
aiteach chat --profile work \
  --required-field topic \
  --required-field audience \
  --required-field format \
  --goal-stop-mode combined
```

Внутри chat можно менять цель:

```text
/goal
/goal field deadline
/goal mode state
/goal mode instruction
/goal mode combined
/goal clear
```

Сравнение способов остановки:

```bash
aiteach compare-goal --profile work \
  "Собери требования к статье" \
  --required-field topic \
  --required-field audience \
  --required-field format
```

`compare-goal` отправляет один и тот же prompt в трех вариантах:

1. `state` - приложение проверяет заполненность полей.
2. `instruction` - приложение доверяет `done: true`.
3. `combined` - приложение требует и заполненные поля, и `done: true`.

### Как Это Реализуется В API

Все текущие provider-профили используют `POST /chat/completions`.

#### OpenAI-compatible

- Endpoint: `POST {base_url}/chat/completions`.
- `response_format` отправляется как `{"type":"json_object"}` для JSON mode.
- `max_tokens` отправляется как верхний лимит output tokens.
- `stop` отправляется как массив stop sequences.
- Явное описание формата и условие завершения добавляются как `system` messages перед user prompt.
- Для dialogue stop CLI добавляет system-инструкцию вернуть JSON с `fields`, `next_question` и `done`, а затем локально проверяет `GoalState`.

Примечание: для некоторых новых OpenAI reasoning-моделей может использоваться `max_completion_tokens`; этот CLI сейчас ориентирован на OpenAI-compatible Chat Completions и отправляет `max_tokens`.

#### OpenRouter

- Endpoint: `POST https://openrouter.ai/api/v1/chat/completions`.
- OpenRouter принимает OpenAI-compatible body и документирует `response_format`, `max_tokens` и `stop`.
- CLI добавляет обязательные provider headers `HTTP-Referer` и `X-Title`, затем отправляет те же control-поля.
- Provider нормализует `finish_reason`, включая `stop` и `length`.
- Dialogue stop реализован на стороне CLI: OpenRouter получает обычный OpenAI-compatible JSON-mode request, а `aiteach` проверяет required fields и `done`.

Docs: [OpenRouter parameters](https://openrouter.ai/docs/api/reference/parameters), [OpenRouter API overview](https://openrouter.ai/docs/api-reference/overview).

#### DeepSeek

- Endpoint: `POST https://api.deepseek.com/v1/chat/completions`.
- DeepSeek JSON Output реализуется через `response_format: {"type":"json_object"}`.
- DeepSeek рекомендует задавать разумный `max_tokens`, чтобы JSON не обрезался посередине.
- `stop` поддерживается как строка или список stop sequences.
- Для dialogue stop используется DeepSeek JSON Output: CLI просит JSON object и локально сравнивает `fields`/`done` с выбранным stop mode.

Docs: [DeepSeek JSON Output](https://api-docs.deepseek.com/guides/json_mode/), [DeepSeek chat completion](https://api-docs.deepseek.com/zh-cn/api/create-chat-completion/).

#### GigaChat

- Endpoint: `POST https://gigachat.devices.sberbank.ru/api/v1/chat/completions`.
- GigaChat Chat Completions использует OpenAI-like request shape с `messages`, `model` и `max_tokens`.
- CLI отправляет `max_tokens`, `stop` и `response_format` в том же body. Если конкретная GigaChat-модель или тариф не поддерживает поле, provider вернет HTTP/API error, который CLI покажет без раскрытия токена.
- Явное описание формата и условие завершения всегда доступны через `system` messages.
- Для dialogue stop CLI не полагается на provider-specific session state: состояние required fields хранится локально.

Docs: [GigaChat model selection example](https://developers.sber.ru/docs/ru/gigachat/guides/selecting-a-model), [GigaChat streaming example](https://developers.sber.ru/docs/ru/gigachat/guides/response-token-streaming).

#### Kimi / Moonshot

- Endpoint: `POST https://api.moonshot.ai/v1/chat/completions`.
- Kimi/Moonshot работает через OpenAI-compatible Chat Completions.
- CLI отправляет `response_format`, `max_tokens` и `stop` в request body.
- Для thinking-моделей reasoning tokens тоже входят в token budget, поэтому маленький `max_tokens` может оставить мало места для финального ответа.
- Для dialogue stop CLI использует тот же JSON object contract и локальную проверку заполненности сущности.

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

First run:

```bash
aiteach setup
```

`setup` shows a provider menu, suggests the default model, `base_url`, profile name, and lets the user save the token directly into the OS keychain. The token is not written to config.

`profile add` can also run without arguments:

```bash
aiteach profile add
```

In that mode, the CLI asks for provider/model/base_url/profile name interactively.

```bash
aiteach setup

aiteach profile add work --provider openrouter --model openai/gpt-4.1-mini
aiteach profile add
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
aiteach compare-goal --profile work \
  "Collect article requirements" \
  --required-field topic \
  --required-field audience \
  --required-field format
aiteach chat --profile work

aiteach config path
aiteach doctor --profile work
```

### Real Usage Examples

#### 1. First run and profile check

```bash
aiteach setup
aiteach profile list
aiteach doctor
```

What happens:

- `setup` helps choose a provider and model.
- The token is stored in the OS keychain, not in config.
- `doctor` checks config, active profile, `base_url`, and token presence without printing the secret.

#### 2. One short answer

```bash
aiteach ask \
  "Explain Rust ownership in simple words" \
  --max-tokens 140 \
  --completion-instruction "Answer in at most 3 short bullets and complete the thought."
```

Use this for quick terminal questions where you want a concise answer. `max-tokens` sets the hard cap, while the instruction asks the model to finish cleanly.

#### 3. JSON for a script or pipeline

```bash
aiteach ask \
  "Analyze this task: build a Rust CLI chat" \
  --response-format json-object \
  --format-instruction "Return a JSON object with title, risks, next_steps." \
  --max-tokens 220
```

This is useful when another script reads the result. The CLI sends `response_format: {"type":"json_object"}` and adds a system instruction for the format.

#### 4. Stop an answer with a marker

```bash
aiteach ask \
  "Create a short project migration plan" \
  --stop "END_OF_ANSWER" \
  --completion-instruction "Give at most 5 bullets. After the last bullet, write END_OF_ANSWER."
```

The model finishes the thought and writes the marker, while the API stops generation at the `stop` sequence. This is cleaner than only hitting a token limit.

#### 5. Interactive chat with response control

```bash
aiteach chat --max-tokens 180
```

Inside chat:

```text
/format json-object
/max-tokens 120
/completion-instruction Answer only as a 3-item list.
/control
/clear
/exit
```

Use this when you want to adjust answer style during the conversation.

#### 6. Compare one prompt without and with controls

```bash
aiteach compare \
  "Explain async Rust" \
  --max-tokens 160 \
  --completion-instruction "Answer in 3 short bullets without an intro."
```

The result shows two blocks:

- `Without constraints` - the regular answer.
- `With constraints` - the same prompt with controls.

#### 7. Collect an entity and stop the dialogue

```bash
aiteach chat \
  --required-field topic \
  --required-field audience \
  --required-field format \
  --goal-stop-mode combined
```

The user answers follow-up questions, and the CLI stops the dialogue only when:

- all required fields are filled;
- the model returned `done: true`.

#### 8. Compare dialogue completion strategies

```bash
aiteach compare-goal \
  "Collect requirements for an article about Rust ownership" \
  --required-field topic \
  --required-field audience \
  --required-field format
```

The CLI compares three strategies:

- `state` - stop by field completeness in code.
- `instruction` - stop by the model signal `done: true`.
- `combined` - require both conditions.

#### 9. Multiple profiles

```bash
aiteach profile add
aiteach profile list
aiteach profile use openrouter
aiteach ask "Check which profile is active"
```

You can also select a profile explicitly:

```bash
aiteach ask --profile deepseek "Create a short summary"
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
- `/goal`
- `/goal clear`
- `/goal field <name>`
- `/goal mode manual`
- `/goal mode state`
- `/goal mode instruction`
- `/goal mode combined`

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

### Dialogue Completion

There are two different stopping levels:

- Response stop: `stop`, `max_tokens`, `completion_instruction`. This ends one provider generation.
- Dialogue stop: the application decides whether to continue the agentic data-collection loop.

For dialogue stop, `aiteach` supports an entity with required fields. The model is asked to return JSON:

```json
{
  "fields": {
    "topic": "Rust ownership",
    "audience": "junior developers",
    "format": null
  },
  "next_question": "Which format do you need?",
  "done": false
}
```

Modes:

- `manual` - the CLI does not stop the dialogue automatically.
- `state` - deterministic code path: the CLI stops when every `--required-field` has a non-null value.
- `instruction` - agent-instruction path: the CLI trusts the model signal `done: true`.
- `combined` - both conditions are required: all fields are filled and `done: true`.

Interactive example:

```bash
aiteach chat --profile work \
  --required-field topic \
  --required-field audience \
  --required-field format \
  --goal-stop-mode combined
```

Inside chat, the goal can be changed:

```text
/goal
/goal field deadline
/goal mode state
/goal mode instruction
/goal mode combined
/goal clear
```

Compare stopping strategies:

```bash
aiteach compare-goal --profile work \
  "Collect article requirements" \
  --required-field topic \
  --required-field audience \
  --required-field format
```

`compare-goal` sends the same prompt in three variants:

1. `state` - the application checks field completeness.
2. `instruction` - the application trusts `done: true`.
3. `combined` - the application requires both complete fields and `done: true`.

### API Implementation Details

All current provider profiles use `POST /chat/completions`.

#### OpenAI-compatible

- Endpoint: `POST {base_url}/chat/completions`.
- `response_format` is sent as `{"type":"json_object"}` for JSON mode.
- `max_tokens` is sent as the output token cap.
- `stop` is sent as an array of stop sequences.
- Explicit format and completion conditions are added as `system` messages before the user prompt.
- For dialogue stop, the CLI adds a system instruction to return JSON with `fields`, `next_question`, and `done`, then checks local `GoalState`.

Note: some newer OpenAI reasoning models use `max_completion_tokens`; this CLI currently targets OpenAI-compatible Chat Completions and sends `max_tokens`.

#### OpenRouter

- Endpoint: `POST https://openrouter.ai/api/v1/chat/completions`.
- OpenRouter accepts an OpenAI-compatible body and documents `response_format`, `max_tokens`, and `stop`.
- The CLI adds provider headers `HTTP-Referer` and `X-Title`, then sends the same control fields.
- The provider normalizes `finish_reason`, including `stop` and `length`.
- Dialogue stop is implemented by the CLI: OpenRouter receives a regular OpenAI-compatible JSON-mode request, while `aiteach` checks required fields and `done`.

Docs: [OpenRouter parameters](https://openrouter.ai/docs/api/reference/parameters), [OpenRouter API overview](https://openrouter.ai/docs/api-reference/overview).

#### DeepSeek

- Endpoint: `POST https://api.deepseek.com/v1/chat/completions`.
- DeepSeek JSON Output is implemented with `response_format: {"type":"json_object"}`.
- DeepSeek recommends setting a reasonable `max_tokens` value so JSON is not truncated midway.
- `stop` is supported as a string or a list of stop sequences.
- Dialogue stop uses DeepSeek JSON Output: the CLI requests a JSON object and locally compares `fields`/`done` against the selected stop mode.

Docs: [DeepSeek JSON Output](https://api-docs.deepseek.com/guides/json_mode/), [DeepSeek chat completion](https://api-docs.deepseek.com/zh-cn/api/create-chat-completion/).

#### GigaChat

- Endpoint: `POST https://gigachat.devices.sberbank.ru/api/v1/chat/completions`.
- GigaChat Chat Completions uses an OpenAI-like request shape with `messages`, `model`, and `max_tokens`.
- The CLI sends `max_tokens`, `stop`, and `response_format` in the same body. If a concrete GigaChat model or plan does not support a field, the provider returns an HTTP/API error and the CLI reports it without exposing the token.
- Explicit format and completion conditions remain available through `system` messages.
- For dialogue stop, the CLI does not rely on provider-specific session state: required field state is stored locally.

Docs: [GigaChat model selection example](https://developers.sber.ru/docs/ru/gigachat/guides/selecting-a-model), [GigaChat streaming example](https://developers.sber.ru/docs/ru/gigachat/guides/response-token-streaming).

#### Kimi / Moonshot

- Endpoint: `POST https://api.moonshot.ai/v1/chat/completions`.
- Kimi/Moonshot works through OpenAI-compatible Chat Completions.
- The CLI sends `response_format`, `max_tokens`, and `stop` in the request body.
- For thinking models, reasoning tokens also count toward the token budget, so a small `max_tokens` value can leave little room for the final answer.
- Dialogue stop uses the same JSON object contract and local entity completeness check.

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
