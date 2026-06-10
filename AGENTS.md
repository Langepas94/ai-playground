# Project Agent Notes

@/Users/artem/.codex/RTK.md

## Как работать в этом репозитории

- Сначала читай `src/README.md`. Для CLI смотри `src/cli/README.md`, `src/cli/mod.rs`, затем нужный `src/cli/commands/*`. Для web смотри `src/web/README.md`, `src/web/mod.rs`, затем соседние `error.rs`/`parameters.rs`/`tokens.rs`/`util.rs`.
- Все shell-команды запускай через `rtk`.
- Не трогай пользовательские незакоммиченные файлы вроде `.DS_Store`.
- Для ручных правок используй `apply_patch`.
- Для каждого исправления или задачи обязательно создавай новую ветку. После завершения мержи ее в `dev` и `main`.
- Перед коммитом любого пользовательского изменения поднимай версию в зависимости от серьезности изменений:
  patch - для исправлений и небольших внутренних правок, minor - для заметных CLI/API-изменений или новой совместимой функциональности, major - для несовместимых изменений.
  Обычно обновляй `Cargo.toml`, `crates/aiteach-compat/Cargo.toml`, `Cargo.lock`.
- После **каждого** изменения Rust-кода — обязательно:

```bash
rtk cargo build 2>&1 | head -30   # убедиться что компилируется
rtk cargo fmt
rtk cargo test
```

Не коммитить и не мержить, пока `cargo build` не прошёл без ошибок.

## Карта модулей

| Файл/Директория | Ответственность |
|-----------------|----------------|
| `src/README.md` | Карта исходников: куда идти при баге, где границы модулей, какие инварианты нельзя ломать. |
| `src/cli/mod.rs` | `run()` — точка входа CLI и dispatch команд. |
| `src/cli/args.rs` | Все `*Args` structs (clap), `ResponseControlArgs`, `PricingArgs`, `ConversationGoalArgs`. |
| `src/cli/commands/` | По одному файлу на команду: `ask`, `chat`, `compare`, `setup`, `profile`, `token`, `models`, `doctor`, `config`. |
| `src/cli/input.rs` | Низкоуровневый stdin/prompt без config/provider знаний. |
| `src/cli/profile_input.rs` | Интерактивный сбор профиля, provider, base_url и модели. |
| `src/cli/pricing.rs` | Выбор/загрузка pricing для CLI-команд. |
| `src/web/mod.rs` | Axum server: `/api/providers`, `/api/models`, `/api/chat`. Handlers, DTO, тесты. |
| `src/web/error.rs` | Mapping `AppError` → HTTP status + JSON. |
| `src/web/parameters.rs` | Ограничения response parameters для web формы. |
| `src/web/tokens.rs` | Web token override/status/lookup. |
| `src/web/util.rs` | Маленькие parse/blank helpers для web DTO. |
| `src/web/ui.html` | Web UI — HTML/CSS/JS. Подключается через `include_str!("ui.html")`. Редактируется как обычный файл с подсветкой. |
| `src/chat/` | Логика `ask_once`, `interactive_chat` (REPL), `compare_*`, `ConversationGoal`, `GoalState`, local agent runtime. |
| `src/config.rs` | `AppConfig` (TOML): профили, активный профиль. Пути — `ai-playground` с legacy fallback на `aiteach`. |
| `src/secrets.rs` | `KeyringSecretStore`: provider-scoped токены через keyring. Fallback-миграция из profile-scoped. |
| `src/providers/mod.rs` | `ProviderKind`, `ProviderSpec`, `ProviderClient` trait, `ReqwestProviderClient`. Общие типы. |
| `src/providers/openai_compatible.rs` | Chat Completions impl, list_models, billing, debug. Используется всеми провайдерами. |
| `src/providers/openrouter.rs` | Spec + overrides для OpenRouter. |
| `src/providers/deepseek.rs` | Spec для DeepSeek. |
| `src/providers/gigachat.rs` | Spec + custom CA bundle. |
| `src/providers/kimi.rs` | Spec для Kimi/Moonshot. |
| `src/errors.rs` | `AppError` + `ProviderHttpError`. |
| `src/bin/ai.rs` | Точка входа бинарника `ai`. |
| `crates/aiteach-compat/` | Legacy бинарник `aiteach`. |
| `tests/http_mock.rs` | Интеграционные тесты через wiremock. |
| `docs/debugging.md` | Практические маршруты расследования багов CLI/web/provider/config/token/session. |

> `src/lib.rs` использует `#[path = "cli/mod.rs"]` и `#[path = "web/mod.rs"]` — это намеренно, так как рядом остались пустые `cli.rs` / `web.rs` (нельзя удалить через агента).

## Поток запроса (коротко)

```
CLI/web  →  ProfileConfig + token  →  ReqwestProviderClient
         →  openai_compatible::chat_completion()
         →  HTTP POST /chat/completions
         →  ChatResponse { text, finish_reason, debug }
```

## Как добавить новый провайдер

1. `src/providers/<name>.rs` — реализовать `pub fn spec() -> ProviderSpec` (и overrides если нужны).
2. `src/providers/mod.rs` — добавить вариант в `ProviderKind`, `all()`, `Display`, `FromStr`, `spec()`.
3. `src/web/mod.rs` — провайдер появится автоматически через `ProviderKind::all()`.
4. Тесты в `tests/http_mock.rs` по образцу существующих.

## Как добавить новый параметр ответа

1. `src/providers/mod.rs` — добавить поле в `ResponseControl` или `ChatRequest`.
2. `src/providers/openai_compatible.rs` — включить поле в payload.
3. `src/cli/args.rs` — добавить флаг в `AskArgs`/`ChatArgs`.
4. `src/cli/commands/` или `src/cli/mod.rs` — пробросить флаг в `ResponseControl`.
5. `src/web/mod.rs` — добавить поле в JSON API, а provider-specific ограничения при необходимости в `src/web/parameters.rs`.
6. `src/web/ui.html` — добавить поле в HTML-форму.

## Инварианты

- Токены не пишутся в config и не печатаются полностью.
- Config хранит профили, активный профиль и ссылки на токены.
- Secret store хранит токены на уровне provider, со старым profile-scoped fallback для миграции.
- CLI и web должны пользоваться одним слоем provider-клиентов.
- README держим на русском и не раздуваем учебником на тысячу строк.
- `AppError` — единственный путь для ошибок; `anyhow` только в `main`/верхнем CLI.
- Версия синхронна в `Cargo.toml` и `crates/aiteach-compat/Cargo.toml`.
