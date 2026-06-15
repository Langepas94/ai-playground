# Project Agent Notes

@/Users/artem/.codex/RTK.md

## Быстрый режим

- Shell-команды запускай через `rtk`.
- Сначала читай `docs/agent-index.md` или `src/README.md`; затем только README/файлы нужного модуля.
- Для поиска по коду сначала используй `rtk ast-index search|symbol|outline`; `rg` - для строк/regex.
- Не читай большие файлы целиком. Для файлов >500 строк сначала `rtk ast-index outline <file>`, потом точный срез.
- Ручные правки делай через `apply_patch`.
- Не трогай пользовательские незакоммиченные файлы.

## Workflow

- Каждая задача - новая ветка.
- Перед коммитом пользовательских изменений подними версию:
  - patch - фиксы/малые внутренние правки;
  - minor - совместимые CLI/API-фичи;
  - major - breaking changes.
- Обычно синхронизируй версии в `Cargo.toml`, `crates/aiteach-compat/Cargo.toml`, `Cargo.lock`.
- После каждого изменения Rust-кода обязательно:

```bash
rtk cargo build 2>&1 | head -30
rtk cargo fmt
rtk cargo test
```

Не коммитить и не мержить, пока `cargo build` не прошел без ошибок.

## Куда идти

- Общая карта: `src/README.md`.
- Компактный индекс для экономии контекста: `docs/agent-index.md`.
- CLI: `src/cli/README.md`, затем `src/cli/mod.rs` или нужный `src/cli/commands/*`.
- Web/API/UI: `src/web/README.md`, затем `src/web/mod.rs`, `src/web/ui.html` или соседние helper-файлы.
- Chat/context/facts/goal/session: `src/chat/README.md`.
- Provider payload/debug/models/billing: `src/providers/README.md`, затем `src/providers/openai_compatible.rs`.
- Config/secrets/errors: `src/config.rs`, `src/secrets.rs`, `src/errors.rs`.
- Интеграционные HTTP-тесты: `tests/http_mock.rs`.

## Инварианты

- Токены не пишутся в config/history/memory/debug и не печатаются полностью.
- Config хранит профили/active profile/ссылки на токены; secrets хранит provider-scoped токены.
- CLI и web используют один provider-client слой.
- `AppError` - единый пользовательский тип ошибок; `anyhow` только наверху.
- Context strategy явная: summary, sliding window, sticky facts, branching, scoped branches.
- `src/lib.rs` намеренно использует `#[path = "cli/mod.rs"]` и `#[path = "web/mod.rs"]`.
