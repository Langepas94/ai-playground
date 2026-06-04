# Архитектура

`ai playground` - локальный Rust CLI и web UI для запросов к LLM-провайдерам.

## Основные модули

- `src/cli.rs` - команды CLI, интерактивные промпты, выбор профилей.
- `src/config.rs` - TOML config, active profile, список профилей.
- `src/secrets.rs` - keyring abstraction, provider-scoped токены и legacy fallback.
- `src/providers/` - OpenAI-compatible request/response слой и provider specs.
- `src/chat.rs` - ask/chat/compare сценарии.
- `src/web.rs` - локальный Axum web UI и JSON API.

## Поток запроса

1. CLI или web собирает `ProfileConfig`.
2. Токен берется из `SecretStore` или из явного override.
3. `ReqwestProviderClient` нормализует provider auth.
4. `openai_compatible` собирает Chat Completions payload.
5. Ответ возвращается как plain text плюс optional finish reason.

## Где чаще всего менять

- Новый CLI сценарий: `src/cli.rs`.
- Новая ручка ответа: `src/providers/mod.rs`, `src/providers/openai_compatible.rs`, `src/cli.rs`, `src/web.rs`.
- Изменение хранения профилей: `src/config.rs` плюс тесты рядом.
- Изменение токенов: `src/secrets.rs`, затем проверить web и CLI.

