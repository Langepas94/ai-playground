# Исходники: карта для разработчика

Этот каталог делит приложение на четыре рабочих слоя:

```text
CLI / web UI
  -> config + secrets
  -> chat runtime
  -> provider client
  -> external provider API
```

## С чего начинать расследование

- CLI команда ведет себя странно: `src/cli/README.md`, затем `src/cli/mod.rs` для dispatch или конкретный файл в `src/cli/commands/`.
- Web UI или JSON API сломаны: `src/web/README.md`, затем handler в `src/web/mod.rs`, DTO рядом в этом же файле, token/error/parameter helpers в соседних модулях.
- Ответ модели, payload, usage, cost или список моделей неверные: `src/providers/README.md`, затем `src/providers/openai_compatible.rs`.
- История, память, goal mode или интерактивный чат работают не так: `src/chat/README.md`.
- Профиль не находится, active profile неверный, TOML не читается: `src/config.rs`.
- Токен не находится, миграция keychain странная, секрет напечатался: `src/secrets.rs`.
- Ошибка плохо выглядит пользователю: `src/errors.rs`.
- Интеграция с provider сломалась после HTTP-изменения: `tests/http_mock.rs`.

## Границы модулей

- `cli/` только собирает пользовательский ввод и вызывает доменную логику.
- `web/` держит Axum routes, request/response DTO, token/error/parameter helpers и вшитый HTML.
- `chat/` управляет локальной историей, memory и сборкой контекста для модели.
- `providers/` знает про HTTP provider API и нормализацию OpenAI-compatible payload.
- `config.rs` пишет только TOML config, без секретов и provider HTTP.
- `secrets.rs` пишет только keychain/storage секретов, без CLI/web ввода.

## Инварианты

- Токены не попадают в config, history, memory, debug output и panic text.
- CLI и web идут через один `ProviderClient`.
- `AppError` остается единым пользовательским типом ошибок.
- Provider-specific поведение живет в `providers/`, а не размазывается по CLI/web.
- Полная история чата является источником правды; memory summary только оптимизация контекста.

## Документы рядом

- `AGENTS.md` в корне: правила работы агента в репозитории.
- `src/*/AGENTS.md`: быстрые правила для конкретного модуля.
- `src/*/README.md`: объяснение структуры и маршрутов изменения кода.
- `docs/debugging.md`: практические сценарии расследования багов.
