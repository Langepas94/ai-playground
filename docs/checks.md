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

Открыть `http://127.0.0.1:8787`, выбрать provider/model и отправить короткий prompt. Токен должен браться из keyring, если поле токена пустое и token уже сохранен через CLI.

## Перед релизной правкой

- `README.md` синхронизирован с текущими командами.
- В тексте нет старого бренда `aiteach`, кроме совместимости/migration, если она специально нужна.
- Ошибки не раскрывают секреты.
