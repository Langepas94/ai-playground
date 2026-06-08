# CLI

CLI отвечает за пользовательский ввод, dispatch команд и перевод флагов в доменные структуры.

## Файлы

- `mod.rs` - `Cli`, `run()`, `run_with_store()`, dispatch, shared helpers для профилей, моделей, pricing и stdin.
- `args.rs` - только clap structs/enums. Здесь не должно быть бизнес-логики.
- `commands/ask.rs` - одноразовый prompt через `ask_once`.
- `commands/chat.rs` - запуск интерактивной REPL-сессии.
- `commands/compare.rs` - сравнение обычного/управляемого ответа и goal stop modes.
- `commands/setup.rs` - интерактивное создание профиля и optional token setup.
- `commands/profile.rs` - add/list/use/remove профилей.
- `commands/token.rs` - set/delete токенов через `SecretStore`.
- `commands/models.rs` - загрузка моделей и цен у provider.
- `commands/doctor.rs` - локальная диагностика профиля/base_url/token.
- `commands/config.rs` - служебные config-команды.

## Поток команды

```text
Cli::parse()
  -> run_with_store()
  -> commands::<command>::run_*
  -> AppConfig + SecretStore
  -> chat/provider layer
```

## Как добавить CLI-флаг ответа

1. Добавить поле в нужный `*Args` в `args.rs`.
2. Если флаг общий для ask/chat/compare, добавить его в `ResponseControlArgs`.
3. Преобразовать args в `ResponseControl` рядом с существующим handler.
4. Проверить web API отдельно, если параметр должен быть доступен и там.

## Частые баги

- Команда не видит активный профиль: смотри `AppConfig::selected_profile` в `src/config.rs`.
- Модель не подставляется: смотри `collect_profile_input_inner()` и `prompt_model()` в `mod.rs`.
- Токен "пропал": смотри `get_config_profile_token()` в `src/secrets.rs`.
- В CLI напечатался секрет: это bug; искать `println!`/`eprintln!` в command handler и использовать `mask_token`.

## Тестирование

После Rust-правок:

```bash
rtk cargo build 2>&1 | head -30
rtk cargo fmt
rtk cargo test
```
