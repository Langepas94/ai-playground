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
```

## Web smoke

```bash
rtk cargo run -- web
```

Открыть `http://127.0.0.1:8787`.

- Вкладка `Профиль`: выбрать provider/model, проверить token status, при необходимости сохранить token override.
- Основная область: отправить короткий prompt и убедиться, что диалог занимает большую левую часть экрана.
- Вкладка `Метрики`: проверить, что отдельно видны `Токены запроса` и `Токены диалога`.
- Вкладка `Debug`: проверить, что provider JSON доступен, а authorization отредактирован.

## Перед релизной правкой

- Версия поднята до коммита: `Cargo.toml`, `crates/aiteach-compat/Cargo.toml`, `Cargo.lock`.
- `README.md` синхронизирован с текущими командами.
- `src/README.md`, модульные README и `docs/debugging.md` синхронизированы, если менялись границы модулей.
- В тексте нет старого бренда `aiteach`, кроме совместимости/migration, если она специально нужна.
- Ошибки не раскрывают секреты.
