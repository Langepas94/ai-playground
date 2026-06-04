# ai playground

Локальный Rust CLI и web UI для общения с LLM-провайдерами через OpenAI-compatible Chat Completions. Команда бинарника: `ai-playground`.

Поддерживаются OpenRouter, DeepSeek, GigaChat, Kimi и произвольный OpenAI-compatible endpoint.

## Стек

- Rust stable, edition 2024 - основной язык и сборка через Cargo.
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
ai-playground setup
ai-playground ask "Объясни ownership в Rust"
ai-playground chat
```

`setup` интерактивно спросит provider, base_url, модель, имя профиля и токен. Enter принимает значение по умолчанию. Токен сохраняется в системный keychain, а не в config.

## Профили

Профиль хранит provider, model и base_url. Активный профиль используется командами без `--profile`.

```bash
ai-playground profile add
ai-playground profile list
ai-playground profile use
ai-playground use
ai-playground use work
ai-playground profile remove work
```

`ai-playground profile use` и `ai-playground use` без имени показывают список уже созданных профилей и позволяют выбрать номером.

Профиль можно указать явно:

```bash
ai-playground ask --profile work "Коротко сравни Rust и Go"
ai-playground models list --profile work
ai-playground doctor --profile work
```

## Токены

Токены хранятся в keychain на уровне provider. Если у вас несколько профилей OpenRouter с разными моделями, токен будет общий.

```bash
ai-playground token set --profile work
ai-playground token delete --profile work
```

Старые profile-scoped токены подхватываются как fallback и мигрируют в provider-scoped хранение при первом использовании.

## Web UI

```bash
ai-playground web
```

Адрес по умолчанию:

```text
http://127.0.0.1:8787
```

Другой адрес:

```bash
ai-playground web --listen 127.0.0.1:9000
```

Web UI использует те же provider-клиенты и тот же keychain, что CLI. Поле `API token override` можно оставить пустым: backend возьмет сохраненный токен выбранного provider. Если вставить токен в web-форму, он сохранится в keychain так же, как через CLI.

В форме доступны provider, base_url, модель, prompt, загрузка списка моделей и параметры ответа.

## Параметры ответа

Основные опции:

```bash
ai-playground ask "Верни краткое резюме" --max-tokens 120
ai-playground ask "Ответь списком" --answer-format bullets
ai-playground ask "Верни JSON объект" --response-format json-object
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
ai-playground compare "Сравни Rust и Go" --max-tokens 120 --stop "END"
```

Для проверки диалоговой цели:

```bash
ai-playground compare-goal \
  "Собери требования к статье" \
  --required-field topic \
  --required-field audience \
  --required-field format
```

## Chat

```bash
ai-playground chat --max-tokens 180
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
ai-playground config path
ai-playground doctor
ai-playground models list
```

`doctor` проверяет активный профиль, base_url и наличие токена, но не печатает сам токен.

## Разработка

```bash
rtk cargo fmt --check
rtk cargo test
```

Короткие заметки для будущих правок лежат в `AGENTS.md` и `docs/`.
