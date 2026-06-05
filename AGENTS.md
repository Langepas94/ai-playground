# Project Agent Notes

@/Users/artem/.codex/RTK.md

## Как работать в этом репозитории

- Проект маленький: сначала читай `src/cli.rs`, `src/config.rs`, `src/secrets.rs`, `src/web.rs`, потом уже меняй код.
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

| Файл | Ответственность |
|------|----------------|
| `src/cli.rs` | Все команды CLI (`setup`, `profile`, `token`, `ask`, `chat`, `web`, `compare`, `doctor`, `config`). Интерактивные промпты через stdin. |
| `src/config.rs` | `AppConfig` (TOML): профили, активный профиль. Пути — `ai-playground` с legacy fallback на `aiteach`. |
| `src/secrets.rs` | `KeyringSecretStore`: provider-scoped токены через keyring. Fallback-миграция из profile-scoped. |
| `src/providers/mod.rs` | `ProviderKind`, `ProviderSpec`, `ProviderClient` trait, `ReqwestProviderClient`. Общие типы: `ChatRequest`, `ChatResponse`, `ResponseControl`, `AnswerFormat`. |
| `src/providers/openai_compatible.rs` | Реализация Chat Completions, list_models, billing, debug. Используется всеми провайдерами. |
| `src/providers/openrouter.rs` | Spec + overrides для OpenRouter (extra headers, billing). |
| `src/providers/deepseek.rs` | Spec для DeepSeek. |
| `src/providers/gigachat.rs` | Spec + custom CA bundle (`russian_trusted_root_ca_pem.crt`). |
| `src/providers/kimi.rs` | Spec для Kimi/Moonshot. |
| `src/chat.rs` | Логика `ask`, `chat` (REPL с `/`-командами), `compare`, `compare_goal`. |
| `src/web.rs` | Axum server: `/api/providers`, `/api/models`, `/api/chat`. Встроенный HTML/JS. |
| `src/errors.rs` | `AppError` + `ProviderHttpError`. HTTP → `AppError` через `map_http_status`. |
| `src/bin/ai.rs` | Точка входа бинарника `ai`. |
| `src/main.rs` | Точка входа бинарника `ai-playground` (alias). |
| `crates/aiteach-compat/` | Бинарник `aiteach` для обратной совместимости — просто вызывает тот же lib. |
| `tests/http_mock.rs` | Интеграционные тесты provider-клиента через wiremock. |

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
3. `src/web.rs` — провайдер появится автоматически через `ProviderKind::all()`.
4. Тесты в `tests/http_mock.rs` по образцу существующих.

## Как добавить новый параметр ответа

1. `src/providers/mod.rs` — добавить поле в `ResponseControl` или `ChatRequest`.
2. `src/providers/openai_compatible.rs` — включить поле в payload.
3. `src/cli.rs` — добавить флаг в `AskArgs`/`ChatArgs`, пробросить в `ResponseControl`.
4. `src/web.rs` — добавить поле в JSON API и HTML-форму.

## Инварианты

- Токены не пишутся в config и не печатаются полностью.
- Config хранит профили, активный профиль и ссылки на токены.
- Secret store хранит токены на уровне provider, со старым profile-scoped fallback для миграции.
- CLI и web должны пользоваться одним слоем provider-клиентов.
- README держим на русском и не раздуваем учебником на тысячу строк.
- `AppError` — единственный путь для ошибок; `anyhow` только в `main`/верхнем CLI.
- Версия синхронна в `Cargo.toml` и `crates/aiteach-compat/Cargo.toml`.
