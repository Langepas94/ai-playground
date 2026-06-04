# Project Agent Notes

@/Users/artem/.codex/RTK.md

## Как работать в этом репозитории

- Проект маленький: сначала читай `src/cli.rs`, `src/config.rs`, `src/secrets.rs`, `src/web.rs`, потом уже меняй код.
- Все shell-команды запускай через `rtk`.
- Не трогай пользовательские незакоммиченные файлы вроде `.DS_Store`.
- Для ручных правок используй `apply_patch`.
- Перед коммитом любого пользовательского изменения поднимай версию. Обычно patch:
  `Cargo.toml`, `crates/aiteach-compat/Cargo.toml`, `Cargo.lock`. Minor - только если меняется заметная CLI/API совместимость.
- После изменения Rust-кода запускай:

```bash
rtk cargo fmt
rtk cargo test
```

## Инварианты

- Токены не пишутся в config и не печатаются полностью.
- Config хранит профили, активный профиль и ссылки на токены.
- Secret store хранит токены на уровне provider, со старым profile-scoped fallback для миграции.
- CLI и web должны пользоваться одним слоем provider-клиентов.
- README держим на русском и не раздуваем учебником на тысячу строк.
