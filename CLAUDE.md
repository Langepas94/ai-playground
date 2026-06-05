# ai-playground — контекст для Claude

Локальный Rust CLI (`ai`) и web UI для LLM-провайдеров через OpenAI-compatible Chat Completions.
Текущая версия: см. `Cargo.toml` → `version`.

## Быстрый старт для агента

```bash
rtk cargo fmt --check   # проверить форматирование
rtk cargo test          # запустить тесты
rtk cargo build         # убедиться, что всё компилируется
```

## Структура

```
src/
  cli.rs          — все команды (setup, profile, token, ask, chat, web, compare, doctor, config)
  config.rs       — AppConfig / ProfileConfig (TOML)
  secrets.rs      — keyring, provider-scoped токены
  errors.rs       — AppError, ProviderHttpError
  chat.rs         — логика ask/chat/compare
  web.rs          — Axum web UI + JSON API
  providers/
    mod.rs              — ProviderKind, trait ProviderClient, общие типы
    openai_compatible.rs — Chat Completions impl (используется всеми)
    openrouter.rs       — spec + billing headers
    deepseek.rs         — spec
    gigachat.rs         — spec + custom CA bundle
    kimi.rs             — spec
  bin/ai.rs       — точка входа бинарника `ai`
crates/aiteach-compat/  — legacy бинарник `aiteach`
tests/http_mock.rs      — интеграционные тесты через wiremock
docs/
  architecture.md — обзор стека и потока запроса
  checks.md       — чеклист перед релизом
```

## Рабочий процесс

1. Новая задача → новая ветка.
2. Изменил код → `rtk cargo fmt` + `rtk cargo test`.
3. Перед коммитом → поднять версию в `Cargo.toml` + `crates/aiteach-compat/Cargo.toml` + `Cargo.lock`.
4. После завершения → смержить в `dev` и `main`.

Версии: `patch` — фикс/внутренние правки, `minor` — новая CLI/API фича, `major` — breaking change.

## Ключевые инварианты

- Токены **никогда** не попадают в config и не печатаются полностью.
- Config хранит профили + `active_profile`; секреты — только в keyring.
- CLI и web используют **один и тот же** слой provider-клиентов.
- `AppError` — единственный тип ошибок; `anyhow` только в `main`.
- Версия одинакова в обоих `Cargo.toml`.

## Добавить провайдера

1. `src/providers/<name>.rs` → `pub fn spec() -> ProviderSpec`.
2. `src/providers/mod.rs` → вариант в `ProviderKind`, `all()`, `Display`, `FromStr`, `spec()`.
3. Тест в `tests/http_mock.rs`.

## Добавить параметр ответа

1. Поле в `ResponseControl` / `ChatRequest` (`providers/mod.rs`).
2. Включить в payload (`providers/openai_compatible.rs`).
3. Флаг в `AskArgs`/`ChatArgs` (`cli.rs`).
4. Поле в JSON API и HTML (`web.rs`).
