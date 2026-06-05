# ai playground

Локальный Rust CLI и web UI для общения с LLM-провайдерами через OpenAI-compatible Chat Completions. Основная команда: `ai`.

Поддерживаются OpenRouter, DeepSeek, GigaChat, Kimi и произвольный OpenAI-compatible endpoint.

## Стек

- Rust stable с `edition = "2024"` в `Cargo.toml` - основной язык и сборка через Cargo.
- `clap` - парсинг CLI-команд и аргументов.
- `tokio` - async runtime для HTTP и локального web-сервера.
- `reqwest` - HTTP-клиент для LLM-провайдеров и загрузки списка моделей.
- `axum` - локальный web UI backend и JSON API.
- `serde`, `serde_json`, `toml` - сериализация request/response, config и provider-specific параметров.
- `directories` - OS-specific пути для config и локальной истории.
- `keyring` - хранение токенов в системном keychain.
- `thiserror` - доменные ошибки, `anyhow` - верхний CLI-уровень.
- `wiremock`, `tempfile` - тесты HTTP-клиента и config-сценариев.

## Быстрый старт

```bash
cargo run -- setup
cargo run -- ask "Сколько будет 3 + 2?"
cargo run -- chat
```

После установки бинарника команды выглядят так:

```bash
ai setup
ai ask "Объясни ownership в Rust"
ai chat
```

Для совместимости оставлены длинный бинарник и старый Cargo package:

```bash
cargo run --bin ai-playground -- --help
cargo run -p aiteach -- --help
```

Package/repo slug остается `ai-playground`; ежедневная команда - короткая `ai`.

`setup` интерактивно спросит provider, base_url, модель, имя профиля и токен. Enter принимает значение по умолчанию. Токен сохраняется в системный keychain, а не в config.

## Профили

Профиль хранит provider, model и base_url. Активный профиль используется командами без `--profile`.

```bash
ai profile add
ai profile list
ai profile use
ai use
ai use work
ai profile remove work
```

`ai profile use` и `ai use` без имени показывают список уже созданных профилей и позволяют выбрать номером.

Профиль можно указать явно:

```bash
ai ask --profile work "Коротко сравни Rust и Go"
ai models list --profile work
ai doctor --profile work
```

## Токены

Токены хранятся в keychain на уровне provider. Если у вас несколько профилей OpenRouter с разными моделями, токен будет общий.

```bash
ai token set --profile work
ai token delete --profile work
```

Старые profile-scoped токены подхватываются как fallback и мигрируют в provider-scoped хранение при первом использовании.

## Web UI

```bash
ai web
```

Адрес по умолчанию:

```text
http://127.0.0.1:8787
```

Другой адрес:

```bash
ai web --listen 127.0.0.1:9000
```

Web UI использует те же provider-клиенты и тот же keychain, что CLI. Поле `API token override` можно оставить пустым: backend возьмет сохраненный токен выбранного provider. Если вставить токен в web-форму, он сохранится в keychain так же, как через CLI.

В форме доступны provider, base_url, модель, prompt, загрузка списка моделей и параметры ответа.
Для системной инструкции используйте поле `system_prompt`. Поля `format_instruction` и `completion_instruction` тоже уходят как system messages, но предназначены для формата ответа и правил завершения.
Под ответом есть скрываемый блок `JSON отладка`: там видны запрос в backend, запрос к provider API, сырой ответ provider API и итоговый ответ backend. Токен в отладке редактируется.
При переключении provider web UI применяет диапазоны параметров из документации provider и показывает предупреждения для неподдерживаемых или выходящих за диапазон значений.

## Параметры ответа

Основные опции:

```bash
ai ask "Верни краткое резюме" --max-tokens 120
ai ask "Ответь списком" --answer-format bullets
ai ask "Верни JSON объект" --response-format json-object
```

Полезные ручки:

- `--max-tokens`
- `--max-completion-tokens`
- `--temperature`
- `--top-p`
- `--stop`
- `--answer-format`
- `--response-format`
- `--format-instruction`
- `--completion-instruction`

Для сравнения обычного и управляемого ответа:

```bash
ai compare "Сравни Rust и Go" --max-tokens 120 --stop "END"
```

Для проверки диалоговой цели:

```bash
ai compare-goal \
  "Собери требования к статье" \
  --required-field topic \
  --required-field audience \
  --required-field format
```

## Chat

```bash
ai chat --max-tokens 180
```

Команды внутри чата:

- `/exit`
- `/profile`
- `/model`
- `/clear`
- `/save`
- `/control`
- `/format`
- `/answer-format`
- `/max-tokens <number>`
- `/temperature <number>`
- `/top-p <number>`
- `/stop <text>`
- `/goal`

История сохраняется локально и не содержит токены.

## Config и диагностика

```bash
ai config path
ai doctor
ai models list
```

`doctor` проверяет активный профиль, base_url и наличие токена, но не печатает сам токен.

## Разработка

```bash
rtk cargo fmt --check
rtk cargo test
```

Короткие заметки для будущих правок лежат в `AGENTS.md` и `docs/`.
