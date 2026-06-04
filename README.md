# aiteach

Локальный Rust CLI и web UI для общения с LLM-провайдерами через OpenAI-compatible Chat Completions.

Поддерживаются OpenRouter, DeepSeek, GigaChat, Kimi и произвольный OpenAI-compatible endpoint.

## Быстрый старт

```bash
cargo run -- setup
cargo run -- ask "Сколько будет 3 + 2?"
cargo run -- chat
```

После установки бинарника команды выглядят так:

```bash
aiteach setup
aiteach ask "Объясни ownership в Rust"
aiteach chat
```

`setup` интерактивно спросит provider, base_url, модель, имя профиля и токен. Enter принимает значение по умолчанию. Токен сохраняется в системный keychain, а не в config.

## Профили

Профиль хранит provider, model и base_url. Активный профиль используется командами без `--profile`.

```bash
aiteach profile add
aiteach profile list
aiteach profile use
aiteach use
aiteach use work
aiteach profile remove work
```

`aiteach profile use` и `aiteach use` без имени показывают список уже созданных профилей и позволяют выбрать номером.

Профиль можно указать явно:

```bash
aiteach ask --profile work "Коротко сравни Rust и Go"
aiteach models list --profile work
aiteach doctor --profile work
```

## Токены

Токены хранятся в keychain на уровне provider. Если у вас несколько профилей OpenRouter с разными моделями, токен будет общий.

```bash
aiteach token set --profile work
aiteach token delete --profile work
```

Старые profile-scoped токены подхватываются как fallback и мигрируют в provider-scoped хранение при первом использовании.

## Web UI

```bash
aiteach web
```

Адрес по умолчанию:

```text
http://127.0.0.1:8787
```

Другой адрес:

```bash
aiteach web --listen 127.0.0.1:9000
```

Web UI использует те же provider-клиенты и тот же keychain, что CLI. Поле `API token override` можно оставить пустым: backend возьмет сохраненный токен выбранного provider. Если вставить токен в web-форму, он сохранится в keychain так же, как через CLI.

В форме доступны provider, base_url, модель, prompt, загрузка списка моделей и параметры ответа.

## Параметры ответа

Основные опции:

```bash
aiteach ask "Верни краткое резюме" --max-tokens 120
aiteach ask "Ответь списком" --answer-format bullets
aiteach ask "Верни JSON объект" --response-format json-object
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
aiteach compare "Сравни Rust и Go" --max-tokens 120 --stop "END"
```

Для проверки диалоговой цели:

```bash
aiteach compare-goal \
  "Собери требования к статье" \
  --required-field topic \
  --required-field audience \
  --required-field format
```

## Chat

```bash
aiteach chat --max-tokens 180
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
aiteach config path
aiteach doctor
aiteach models list
```

`doctor` проверяет активный профиль, base_url и наличие токена, но не печатает сам токен.

## Разработка

```bash
rtk cargo fmt --check
rtk cargo test
```

Короткие заметки для будущих правок лежат в `AGENTS.md` и `docs/`.

