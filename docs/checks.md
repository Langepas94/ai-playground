# Проверки

## Быстрая локальная проверка

```bash
rtk cargo fmt --check
rtk cargo test
```

## CLI smoke

```bash
rtk cargo run -- profile list
rtk cargo run -- config path
rtk cargo run -- doctor
rtk cargo run --bin ai -- token-demo --context-limit 4096
```

## Web smoke

```bash
rtk cargo run -- web
```

Открыть `http://127.0.0.1:8787`.

- Вкладка `Профиль`: выбрать provider/model, проверить token status, при необходимости сохранить token override.
- Основная область: отправить короткий prompt и убедиться, что диалог занимает большую левую часть экрана.
- Вкладка `Параметры`: оставить `max_tokens` и `max_completion_tokens` пустыми и проверить в debug/provider payload, что лимиты не отправлены.
- Вкладка `Метрики`: проверить, что отдельно видны `Токены запроса` и `Токены диалога`.
- Вкладка `Debug`: проверить, что provider JSON доступен, а authorization отредактирован.

## Перед релизной правкой

- Версия поднята до коммита: `Cargo.toml`, `crates/aiteach-compat/Cargo.toml`, `Cargo.lock`.
- `README.md` синхронизирован с текущими командами.
- `src/README.md`, модульные README и `docs/debugging.md` синхронизированы, если менялись границы модулей.
- В тексте нет старого бренда `aiteach`, кроме совместимости/migration, если она специально нужна.
- Ошибки не раскрывают секреты.
