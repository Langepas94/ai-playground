# CLI модуль

## Структура

```
cli/
  mod.rs        — run(), dispatch всех команд, shared helpers
  args.rs       — все *Args structs (clap), без логики
  commands/
    ask.rs      — ai ask "..."
    chat.rs     — ai chat
    compare.rs  — ai compare, ai compare-goal
    config.rs   — ai config path
    doctor.rs   — ai doctor
    models.rs   — ai models list
    profile.rs  — ai profile add/list/use/remove
    setup.rs    — ai setup
    token.rs    — ai token set/delete
```

## Как добавить команду

1. Добавить вариант в `Command` enum в `args.rs`.
2. Добавить `*Args` struct в `args.rs`.
3. Создать `commands/<name>.rs` с `pub async fn run_<name>(...) -> Result<(), AppError>`.
4. Экспортировать из `commands/mod.rs`.
5. Добавить match-ветку в `run_with_store()` в `mod.rs`.

## Ключевые хелперы в mod.rs

- `request_pricing(args, client, secrets, config, name, profile)` — пробует взять цену из args, иначе из /models API
- `collect_profile_input(...)` — интерактивный ввод профиля
- `prompt_model(provider, base_url, token)` — выбор модели из списка или ввод вручную
- `select_profile_name(config, parts)` — интерактивный выбор профиля
- `read_stdin_line()` — читает строку из stdin (используется token.rs)

## Инварианты

- `args.rs` не содержит логики — только структуры clap
- Каждая команда создаёт свой `ReqwestProviderClient` (клиент дешёвый)
- Токен никогда не печатается, только `token: present / missing`
