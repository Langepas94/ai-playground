# aiteach

## Русский

`aiteach` - Rust CLI для общения с LLM-провайдерами из терминала.

### Стек

- Rust stable
- `clap` для CLI
- `tokio` для async runtime
- `reqwest` для HTTP
- `serde` / `serde_json` для JSON
- `thiserror` для ошибок домена
- `anyhow` только на верхнем CLI-уровне
- `directories` + `toml` для config
- `keyring` для хранения токенов в системном хранилище

### Команды

Первый запуск:

```bash
aiteach setup
```

`setup` показывает список провайдеров, предлагает дефолтную модель, `base_url`, имя профиля и позволяет сразу сохранить токен в OS keychain. Токен не пишется в config.

В `setup` можно нажимать Enter, чтобы принять значение в квадратных скобках:

```text
Provider number or name [2]:      # Enter выберет OpenRouter
Profile name [openrouter]:        # Enter оставит имя openrouter
Choose model for OpenRouter.
  1. openai/gpt-4.1-mini recommended
  2. openai/gpt-4.1
  3. deepseek/deepseek-chat
  4. google/gemini-2.0-flash-001
  custom. Type another model id manually
Model number or custom id [1]:    # Enter выберет рекомендованную модель
Base URL [https://...]:           # Enter оставит адрес по умолчанию
API token (...):                  # вставьте токен или Enter, чтобы добавить позже
```

В текущей версии `setup` показывает список моделей для выбранного провайдера. Если не знаете, какую модель выбрать, нажимайте Enter. После сохранения токена можно получить живой список моделей от провайдера:

```bash
aiteach models list
```

Если не знаете, что вводить в `Base URL`, нажимайте Enter.

`profile add` тоже можно запускать без аргументов:

```bash
aiteach profile add
```

Тогда CLI спросит provider/model/base_url/profile name интерактивно.

```bash
aiteach setup

aiteach profile add work --provider openrouter --model openai/gpt-4.1-mini
aiteach profile add
aiteach profile list
aiteach profile use work
aiteach profile remove work

aiteach token set --profile work
aiteach token delete --profile work

aiteach models list --profile work
aiteach ask --profile work "Объясни ownership в Rust"
aiteach ask --profile work "Верни краткое резюме" --max-tokens 120
aiteach ask --profile work "Объясни ownership" --answer-format bullets --address-as Artem
aiteach ask --profile work "Верни объект с полями title и bullets" --response-format json-object
aiteach compare --profile work "Сравни Rust и Go" --max-tokens 120 --stop "END"
aiteach compare-goal --profile work \
  "Собери требования к статье" \
  --required-field topic \
  --required-field audience \
  --required-field format
aiteach chat --profile work
aiteach web

aiteach config path
aiteach doctor --profile work
```

### Веб-интерфейс

Локальный сайт запускается из того же бинарника и использует тот же слой провайдеров, что и CLI:

```bash
aiteach web
```

По умолчанию он открывает сервер на:

```text
http://127.0.0.1:8787
```

Пока открыта вкладка, процесс `aiteach web` должен продолжать работать. Если сервер остановлен, браузер покажет сетевую ошибку вроде `Failed to fetch`; запустите `aiteach web` снова и обновите страницу.

Можно выбрать другой адрес:

```bash
aiteach web --listen 127.0.0.1:9000
```

В форме доступны provider, token, base_url, live-загрузка списка моделей, model, custom model id, prompt и параметры запроса.

Порядок работы:

1. Вставьте токен провайдера.
2. Выберите provider. `base_url` и дефолтная model подставятся автоматически.
3. Нажмите `Загрузить модели`.
4. Выберите модель в выпадающем списке `Model`.
5. Если нужной модели нет в списке, впишите ее вручную в `Custom model id`; при отправке запроса это поле имеет приоритет над выпадающим списком.
6. Введите prompt, настройте параметры и нажмите `Отправить`.

Веб-форма показывает дефолтные значения для основных ручек:

- `max_tokens`: `1024`
- `temperature`: `1`
- `top_p`: `1`
- `presence_penalty`: `0`
- `frequency_penalty`: `0`
- `reasoning_effort`: `medium`
- `verbosity`: `medium`
- `n`: `1`
- `parallel_tool_calls`: `true`

Параметры запроса:

- `response_format`
- `answer_format`
- `max_tokens`
- `max_completion_tokens`
- `temperature`
- `top_p`
- `top_k`
- `min_p`
- `top_a`
- `presence_penalty`
- `frequency_penalty`
- `repetition_penalty`
- `seed`
- `reasoning_effort`
- `include_reasoning`
- `verbosity`
- `logprobs`
- `top_logprobs`
- `n`
- `store`
- `parallel_tool_calls`
- `user`
- `service_tier`
- `stop`
- `answer_prefix`
- `answer_suffix`
- `address_as`
- `quote_question`
- `format_instruction`
- `completion_instruction`

Для редких или новых provider-specific параметров используйте поле `extra API parameters`. Оно принимает JSON object и добавляет его поля в тело запроса к провайдеру:

```json
{
  "web_search_options": {},
  "metadata": {
    "source": "aiteach"
  }
}
```

Токен в веб-форме отправляется только в локальный backend для конкретного запроса и не сохраняется в config/keychain.

### Учебные Примеры Использования

Этот раздел показывает не только команды, но и зачем они нужны. Думайте о настройках ответа как о ручках управления учителем: можно попросить “реши задачу”, а можно сказать “реши кратко, в 3 шага, верни JSON, остановись после слова END”.

#### 1. Первый запуск: не придумывать профиль вручную

Задача: пользователь впервые открыл CLI и не знает, что такое profile/provider/base_url.

```bash
aiteach setup
```

Что делает CLI:

1. Показывает список провайдеров: OpenRouter, DeepSeek, GigaChat, Kimi, OpenAI-compatible.
2. Показывает список моделей и предлагает модель по умолчанию.
3. Предлагает `base_url` по умолчанию.
4. Предлагает имя профиля, например `openrouter`.
5. Спрашивает токен и сохраняет его в OS keychain.

Потом можно проверить:

```bash
aiteach profile list
aiteach doctor
```

Зачем это нужно: пользователь не должен помнить внутреннюю структуру config. Он выбирает из списка, как в мастере установки.

#### 2. Простая задача “про яблоки” без контроля

Задача: спросить обычный вопрос.

```bash
aiteach ask "У Маши было 3 яблока, она купила еще 2. Сколько яблок стало?"
```

Возможный ответ:

```text
У Маши стало 5 яблок.
```

Зачем это нужно: обычный режим подходит, когда формат не важен и ответ можно просто прочитать глазами.

#### 3. Та же задача, но нужен очень короткий ответ

Проблема: модель может начать объяснять слишком подробно.

```bash
aiteach ask \
  "У Маши было 3 яблока, она купила еще 2. Сколько яблок стало?" \
  --max-tokens 40 \
  --completion-instruction "Ответь одним коротким предложением. Не добавляй объяснение."
```

Ожидаемый ответ:

```text
У Маши стало 5 яблок.
```

Что здесь происходит:

- `--max-tokens 40` ставит жесткий потолок длины.
- `--completion-instruction` объясняет модели, какой стиль нужен.

Зачем это нужно: токены защищают от слишком длинного ответа, а инструкция помогает не получить грубый обрыв посередине мысли.

#### 4. Ответ в строго заданном формате

Задача: другой скрипт должен прочитать ответ. Текст вроде “У Маши стало 5 яблок” неудобен, нужен JSON. Это API-level формат: provider получает специальное поле `response_format`.

```bash
aiteach ask \
  "У Маши было 3 яблока, она купила еще 2. Верни результат." \
  --response-format json-object \
  --format-instruction "Верни JSON object с полями initial, added, total." \
  --max-tokens 120
```

Ожидаемый ответ:

```json
{
  "initial": 3,
  "added": 2,
  "total": 5
}
```

Зачем это нужно: если ответ читает программа, ей нужен предсказуемый формат, а не свободный текст.

#### 5. Формат ответа как шаблон, а не как файл

Задача: ответ должен начинаться с имени, процитировать вопрос и быть списком. Это не JSON/YAML/CSV, а “форма речи”.

```bash
aiteach ask \
  "Почему 3 + 2 = 5?" \
  --answer-format bullets \
  --address-as "Артем" \
  --quote-question \
  --answer-prefix "Коротко:" \
  --max-tokens 120
```

Ожидаемый стиль ответа:

```text
Коротко: Артем, вопрос: "Почему 3 + 2 = 5?"
- 3 + 2 означает добавить 2 к 3.
- После 3 идут 4 и 5.
- Значит, получается 5.
```

Что здесь происходит:

- `--answer-format bullets` просит отвечать пунктами.
- `--address-as "Артем"` просит обратиться по имени.
- `--quote-question` просит сначала процитировать вопрос.
- `--answer-prefix "Коротко:"` просит начать ответ с конкретного текста.

Зачем это нужно: во многих продуктах важен не “формат файла”, а привычный шаблон ответа: приветствие, цитата вопроса, список, финальная строка, обращение по имени.

#### 6. Stop sequence: закончить мысль и остановиться по маркеру

Проблема: если просто поставить маленький `max-tokens`, ответ может оборваться:

```text
У Маши стало 5 яб...
```

Лучше дать модели маркер завершения:

```bash
aiteach ask \
  "У Маши было 3 яблока, она купила еще 2. Объясни решение." \
  --stop "END_OF_ANSWER" \
  --completion-instruction "Дай короткое решение. Когда закончишь мысль, напиши END_OF_ANSWER."
```

Ожидаемый ответ:

```text
3 + 2 = 5, значит у Маши стало 5 яблок.
```

Маркер `END_OF_ANSWER` не печатается, потому что API останавливает генерацию на stop sequence.

Зачем это нужно: модель сама завершает мысль, а API технически останавливает поток в нужном месте.

#### 7. Сравнить: без ограничений и с ограничениями

Задача: увидеть разницу на одном и том же вопросе.

```bash
aiteach compare \
  "У Маши было 3 яблока, она купила еще 2. Объясни решение." \
  --max-tokens 60 \
  --completion-instruction "Ответь одним предложением, без вступления."
```

CLI отправит два запроса:

1. Без ограничений.
2. С `max-tokens` и инструкцией.

Зачем это нужно: вы видите, как один и тот же prompt меняется от уровня контроля. Это полезно перед тем, как встроить prompt в скрипт или продукт.

#### 8. Интерактивный чат: менять правила по ходу разговора

```bash
aiteach chat --max-tokens 180
```

Внутри чата:

```text
> Объясни задачу про яблоки
/max-tokens 60
/answer-format bullets
/address-as Артем
/completion-instruction Отвечай как учитель в начальной школе, максимум 2 предложения.
> Теперь объясни задачу про 7 груш и 4 груши
/control
/exit
```

Зачем это нужно: иногда сначала нужен подробный ответ, потом краткий, потом JSON. Не надо перезапускать программу.

#### 9. Сбор сущности: анкета про яблоки

Задача: не просто ответить, а собрать все нужные поля для задачи. Например, нам нужны:

- `item` - что считаем;
- `initial_count` - сколько было;
- `added_count` - сколько добавили.

Команда:

```bash
aiteach chat \
  --required-field item \
  --required-field initial_count \
  --required-field added_count \
  --goal-stop-mode combined
```

Пример диалога:

```text
> Составь задачу
assistant: Какой предмет считаем?
> яблоки
assistant: Сколько яблок было сначала?
> 3
assistant: Сколько яблок добавили?
> 2
assistant: {"fields":{"item":"яблоки","initial_count":3,"added_count":2},"next_question":null,"done":true}
```

После этого CLI сам останавливает диалог, потому что:

- все required fields заполнены;
- модель вернула `done: true`;
- выбран режим `combined`.

Зачем это нужно: это уже не просто “один ответ”, а маленький агент, который собирает данные до готовности.

#### 10. Сравнить способы остановки диалога

```bash
aiteach compare-goal \
  "Собери данные для задачи: у Маши было 3 яблока, она купила еще 2" \
  --required-field item \
  --required-field initial_count \
  --required-field added_count
```

CLI сравнит:

- `state` - код смотрит: все поля заполнены? Тогда стоп.
- `instruction` - модель сказала `done: true`? Тогда стоп.
- `combined` - и поля заполнены, и модель сказала `done: true`.

Простой смысл:

- `state` надежнее как проверка формы.
- `instruction` гибче, но модель может ошибиться.
- `combined` строже всего и лучше для важных сценариев.

#### 11. Несколько профилей для разных провайдеров

Задача: один профиль для OpenRouter, другой для DeepSeek.

```bash
aiteach profile add
aiteach profile add
aiteach profile list
aiteach profile use openrouter
aiteach ask "Сколько будет 3 + 2?"
aiteach ask --profile deepseek "Сколько будет 3 + 2?"
```

Зачем это нужно: можно быстро сравнивать провайдеров и модели, не переписывая config и не перенося токены вручную.

### Провайдеры

Поддерживаются профили:

- `openai-compatible`
- `openrouter`
- `deepseek`
- `gigachat`
- `kimi`

Архитектура provider-слоя разделена на две части:

- `ProviderSpec` описывает конкретного провайдера: `kind`, имя, `base_url`, модель по умолчанию, auth-схему и дополнительные HTTP headers.
- `providers/openai_compatible` содержит общий транспорт для OpenAI-compatible `/models` и `/chat/completions`.

Чтобы добавить нового провайдера:

1. Создайте модуль в `src/providers/<provider>.rs`.
2. Верните из него `ProviderSpec`.
3. Добавьте вариант в `ProviderKind`.
4. Подключите модуль в provider registry в `src/providers/mod.rs`.

CLI при этом менять не нужно.

### Config И Secrets

Config хранит только несекретные поля:

- `provider`
- `model`
- `base_url`
- `token_ref`

Токен хранится только в OS keychain через `keyring`. `token_ref` стабилен и строится как `provider:profile_name`, например `openrouter:work`.

Токены нельзя писать в:

- config
- git
- logs
- stdout/stderr
- debug output

Команды показывают только факт наличия токена. Полное значение токена никогда не печатается.

### Chat

`ask` отправляет один prompt и печатает один ответ.

`chat` запускает интерактивную сессию. Внутренние команды:

- `/exit`
- `/profile`
- `/model`
- `/clear`
- `/save`
- `/control`
- `/control clear`
- `/format text`
- `/format json-object`
- `/answer-format natural`
- `/answer-format bullets`
- `/answer-format numbered`
- `/answer-format short`
- `/answer-format steps`
- `/answer-format table`
- `/answer-prefix <text>`
- `/answer-suffix <text>`
- `/address-as <name>`
- `/quote-question`
- `/quote-question clear`
- `/max-tokens <number>`
- `/stop <sequence>`
- `/stop clear`
- `/format-instruction <text>`
- `/completion-instruction <text>`
- `/goal`
- `/goal clear`
- `/goal field <name>`
- `/goal mode manual`
- `/goal mode state`
- `/goal mode instruction`
- `/goal mode combined`

История сохраняется локально и не содержит токены. Prompt/response не логируются в debug output без явного будущего режима `--log-conversation`.

### Управление Ответом

Пользователь может управлять формой и завершением ответа через `ask`, `chat` и `compare`.

Есть два разных понятия:

- API-level format: то, что provider может технически усилить, например JSON object через `response_format`.
- Answer format: человеческий шаблон ответа, например “начни с имени”, “процитируй вопрос”, “ответь списком”.

CLI-флаги:

- `--response-format text` - обычный текст, API-поле `response_format` не отправляется.
- `--response-format json-object` - отправляет `response_format: {"type":"json_object"}` и добавляет system-инструкцию вернуть только JSON object.
- `--answer-format natural|bullets|numbered|short|steps|table` - задает человеческий шаблон ответа через system-инструкцию.
- `--answer-prefix <text>` - просит начать ответ с конкретного текста.
- `--answer-suffix <text>` - просит закончить ответ конкретным текстом.
- `--address-as <name>` - просит обратиться к пользователю по имени или роли.
- `--quote-question` - просит сначала процитировать вопрос пользователя.
- `--max-tokens <number>` - отправляет `max_tokens` и ограничивает длину генерации на стороне provider.
- `--stop <sequence>` - можно указать несколько раз; отправляет массив `stop`.
- `--format-instruction <text>` - добавляет system-инструкцию с явным описанием формата ответа.
- `--completion-instruction <text>` - добавляет system-инструкцию с условием завершения ответа, например `Finish after exactly 3 bullets.`

`compare` отправляет один и тот же prompt два раза:

1. Без ограничений: только `model` и `messages`.
2. С ограничениями: тот же prompt плюс выбранные `response_format`, `max_tokens`, `stop` и инструкции.

Пример:

```bash
aiteach compare --profile work \
  "Объясни ownership в Rust для junior developer" \
  --response-format json-object \
  --max-tokens 160 \
  --stop "END" \
  --completion-instruction "Finish after exactly 3 bullet points and then write END."
```

Результат показывает два блока: `Without constraints` и `With constraints`.

### Завершение Диалога

Есть два разных уровня остановки:

- Остановка ответа: `stop`, `max_tokens`, `completion_instruction`. Это завершает одну генерацию provider.
- Остановка диалога: приложение решает, продолжать ли агентный цикл сбора данных.

Для остановки диалога `aiteach` поддерживает сущность с required fields. Модель должна возвращать JSON:

```json
{
  "fields": {
    "topic": "Rust ownership",
    "audience": "junior developers",
    "format": null
  },
  "next_question": "Какой формат нужен?",
  "done": false
}
```

Режимы:

- `manual` - CLI не останавливает диалог автоматически.
- `state` - deterministic code path: CLI останавливается, когда все `--required-field` заполнены не-null значениями.
- `instruction` - agent-instruction path: CLI доверяет сигналу модели `done: true`.
- `combined` - оба условия обязательны: все поля заполнены и `done: true`.

Интерактивный пример:

```bash
aiteach chat --profile work \
  --required-field topic \
  --required-field audience \
  --required-field format \
  --goal-stop-mode combined
```

Внутри chat можно менять цель:

```text
/goal
/goal field deadline
/goal mode state
/goal mode instruction
/goal mode combined
/goal clear
```

Сравнение способов остановки:

```bash
aiteach compare-goal --profile work \
  "Собери требования к статье" \
  --required-field topic \
  --required-field audience \
  --required-field format
```

`compare-goal` отправляет один и тот же prompt в трех вариантах:

1. `state` - приложение проверяет заполненность полей.
2. `instruction` - приложение доверяет `done: true`.
3. `combined` - приложение требует и заполненные поля, и `done: true`.

### Как Это Реализуется В API

Все текущие provider-профили используют `POST /chat/completions`.

#### OpenAI-compatible

- Endpoint: `POST {base_url}/chat/completions`.
- `response_format` отправляется как `{"type":"json_object"}` для JSON mode.
- `max_tokens` отправляется как верхний лимит output tokens.
- `stop` отправляется как массив stop sequences.
- Явное описание формата и условие завершения добавляются как `system` messages перед user prompt.
- Для dialogue stop CLI добавляет system-инструкцию вернуть JSON с `fields`, `next_question` и `done`, а затем локально проверяет `GoalState`.

Примечание: для некоторых новых OpenAI reasoning-моделей может использоваться `max_completion_tokens`; этот CLI сейчас ориентирован на OpenAI-compatible Chat Completions и отправляет `max_tokens`.

#### OpenRouter

- Endpoint: `POST https://openrouter.ai/api/v1/chat/completions`.
- OpenRouter принимает OpenAI-compatible body и документирует `response_format`, `max_tokens` и `stop`.
- CLI добавляет обязательные provider headers `HTTP-Referer` и `X-Title`, затем отправляет те же control-поля.
- Provider нормализует `finish_reason`, включая `stop` и `length`.
- Dialogue stop реализован на стороне CLI: OpenRouter получает обычный OpenAI-compatible JSON-mode request, а `aiteach` проверяет required fields и `done`.

Docs: [OpenRouter parameters](https://openrouter.ai/docs/api/reference/parameters), [OpenRouter API overview](https://openrouter.ai/docs/api-reference/overview).

#### DeepSeek

- Endpoint: `POST https://api.deepseek.com/v1/chat/completions`.
- DeepSeek JSON Output реализуется через `response_format: {"type":"json_object"}`.
- DeepSeek рекомендует задавать разумный `max_tokens`, чтобы JSON не обрезался посередине.
- `stop` поддерживается как строка или список stop sequences.
- Для dialogue stop используется DeepSeek JSON Output: CLI просит JSON object и локально сравнивает `fields`/`done` с выбранным stop mode.

Docs: [DeepSeek JSON Output](https://api-docs.deepseek.com/guides/json_mode/), [DeepSeek chat completion](https://api-docs.deepseek.com/zh-cn/api/create-chat-completion/).

#### GigaChat

- Endpoint: `POST https://gigachat.devices.sberbank.ru/api/v1/chat/completions`.
- GigaChat Chat Completions использует OpenAI-like request shape с `messages`, `model` и `max_tokens`.
- Токен в keychain может быть authorization key из кабинета GigaChat: CLI обменяет его на access token через OAuth. Для B2B/CORP scope задайте `AITEACH_GIGACHAT_SCOPE`, по умолчанию используется `GIGACHAT_API_PERS`.
- CLI отправляет `max_tokens`, `stop` и `response_format` в том же body. Если конкретная GigaChat-модель или тариф не поддерживает поле, provider вернет HTTP/API error, который CLI покажет без раскрытия токена.
- Явное описание формата и условие завершения всегда доступны через `system` messages.
- Для dialogue stop CLI не полагается на provider-specific session state: состояние required fields хранится локально.
- Если GigaChat падает с TLS ошибкой `self signed certificate in certificate chain`, скачайте сертификаты из документации Sber, объедините root/sub CA в PEM bundle и запустите CLI с `AITEACH_CA_BUNDLE=/path/to/chain_pem.txt`.

Docs: [GigaChat model selection example](https://developers.sber.ru/docs/ru/gigachat/guides/selecting-a-model), [GigaChat streaming example](https://developers.sber.ru/docs/ru/gigachat/guides/response-token-streaming).

#### Kimi / Moonshot

- Endpoint: `POST https://api.moonshot.ai/v1/chat/completions`.
- Kimi/Moonshot работает через OpenAI-compatible Chat Completions.
- CLI отправляет `response_format`, `max_tokens` и `stop` в request body.
- Для thinking-моделей reasoning tokens тоже входят в token budget, поэтому маленький `max_tokens` может оставить мало места для финального ответа.
- Для dialogue stop CLI использует тот же JSON object contract и локальную проверку заполненности сущности.

Docs: [Kimi FAQ](https://platform.kimi.ai/docs/guide/faq), [Kimi thinking models](https://platform.moonshot.ai/docs/guide/use-kimi-k2-thinking-model.en-US).

### Ошибки

По умолчанию CLI показывает user-facing ошибки. `--verbose` включает внутренние детали.

HTTP ошибки содержат:

- provider
- категорию endpoint: `models`, `chat` или `other`
- HTTP status code, если он доступен
- понятное описание auth-проблем
- `Retry-After` для rate limit, если provider его вернул
- подсказки для DNS/TLS/proxy/timeout проблем
- сообщение о неожиданном JSON-формате ответа provider

Config ошибки содержат путь к config и конкретную причину: чтение, запись или TOML parsing.

### Структура

```text
src/
  chat.rs
  cli.rs
  config.rs
  errors.rs
  secrets.rs
  providers/
    mod.rs
    openai_compatible.rs
    openrouter.rs
    deepseek.rs
    gigachat.rs
    kimi.rs
```

### Проверки

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

## English

`aiteach` is a Rust CLI for chatting with LLM providers from the terminal.

### Stack

- Rust stable
- `clap` for CLI parsing
- `tokio` async runtime
- `reqwest` for HTTP
- `serde` / `serde_json` for JSON
- `thiserror` for domain errors
- `anyhow` only at the top CLI boundary
- `directories` + `toml` for config
- `keyring` for secure OS keychain token storage

### Commands

First run:

```bash
aiteach setup
```

`setup` shows a provider menu, suggests the default model, `base_url`, profile name, and lets the user save the token directly into the OS keychain. The token is not written to config.

In `setup`, press Enter to accept the value shown in brackets:

```text
Provider number or name [2]:      # Enter selects OpenRouter
Profile name [openrouter]:        # Enter keeps openrouter
Choose model for OpenRouter.
  1. openai/gpt-4.1-mini recommended
  2. openai/gpt-4.1
  3. deepseek/deepseek-chat
  4. google/gemini-2.0-flash-001
  custom. Type another model id manually
Model number or custom id [1]:    # Enter selects the recommended model
Base URL [https://...]:           # Enter keeps the default URL
API token (...):                  # paste token or press Enter to add it later
```

In the current version, `setup` shows a model list for the selected provider. If you do not know which model to choose, press Enter. After saving the token, you can fetch the live provider model list:

```bash
aiteach models list
```

If you do not know what to enter for `Base URL`, press Enter.

`profile add` can also run without arguments:

```bash
aiteach profile add
```

In that mode, the CLI asks for provider/model/base_url/profile name interactively.

```bash
aiteach setup

aiteach profile add work --provider openrouter --model openai/gpt-4.1-mini
aiteach profile add
aiteach profile list
aiteach profile use work
aiteach profile remove work

aiteach token set --profile work
aiteach token delete --profile work

aiteach models list --profile work
aiteach ask --profile work "Explain Rust ownership"
aiteach ask --profile work "Return a short summary" --max-tokens 120
aiteach ask --profile work "Explain ownership" --answer-format bullets --address-as Artem
aiteach ask --profile work "Return an object with title and bullets" --response-format json-object
aiteach compare --profile work "Compare Rust and Go" --max-tokens 120 --stop "END"
aiteach compare-goal --profile work \
  "Collect article requirements" \
  --required-field topic \
  --required-field audience \
  --required-field format
aiteach chat --profile work

aiteach config path
aiteach doctor --profile work
```

### Tutorial-Style Usage Examples

This section explains not only which command to run, but why each control exists. Think of response controls like classroom instructions: you can ask “solve the problem”, or you can ask “solve it briefly, in 3 steps, return JSON, and stop after END”.

#### 1. First run: no manual profile guessing

Task: the user opens the CLI for the first time and does not know what profile/provider/base_url means.

```bash
aiteach setup
```

What the CLI does:

1. Shows a provider list: OpenRouter, DeepSeek, GigaChat, Kimi, OpenAI-compatible.
2. Shows a model list and suggests the default model.
3. Suggests the default `base_url`.
4. Suggests a profile name, for example `openrouter`.
5. Asks for the token and stores it in the OS keychain.

Then check it:

```bash
aiteach profile list
aiteach doctor
```

Why this matters: the user does not need to remember config internals. They choose from a menu, like in an installer.

#### 2. Simple apple problem without controls

Task: ask a normal question.

```bash
aiteach ask "Masha had 3 apples and bought 2 more. How many apples does she have?"
```

Possible answer:

```text
Masha has 5 apples.
```

Why this matters: normal mode is fine when the format does not matter and a human will read the answer.

#### 3. Same problem, but very short

Problem: the model may explain too much.

```bash
aiteach ask \
  "Masha had 3 apples and bought 2 more. How many apples does she have?" \
  --max-tokens 40 \
  --completion-instruction "Answer in one short sentence. Do not add an explanation."
```

Expected answer:

```text
Masha has 5 apples.
```

What happens:

- `--max-tokens 40` sets a hard length cap.
- `--completion-instruction` tells the model what style to use.

Why this matters: tokens protect against a long answer, while the instruction helps avoid cutting the thought in the middle.

#### 4. Strict response format

Task: another script needs to read the answer. Free text like “Masha has 5 apples” is inconvenient; JSON is better. This is an API-level format: the provider receives the special `response_format` field.

```bash
aiteach ask \
  "Masha had 3 apples and bought 2 more. Return the result." \
  --response-format json-object \
  --format-instruction "Return a JSON object with initial, added, total." \
  --max-tokens 120
```

Expected answer:

```json
{
  "initial": 3,
  "added": 2,
  "total": 5
}
```

Why this matters: if a program reads the answer, it needs predictable structure, not natural language.

#### 5. Answer format as a template, not a file type

Task: the answer should start with a name, quote the question, and use a list. This is not JSON/YAML/CSV; it is the shape of the response.

```bash
aiteach ask \
  "Why is 3 + 2 equal to 5?" \
  --answer-format bullets \
  --address-as "Artem" \
  --quote-question \
  --answer-prefix "Short answer:" \
  --max-tokens 120
```

Expected style:

```text
Short answer: Artem, question: "Why is 3 + 2 equal to 5?"
- 3 + 2 means adding 2 to 3.
- After 3 come 4 and 5.
- So the result is 5.
```

What happens:

- `--answer-format bullets` asks for bullet points.
- `--address-as "Artem"` asks the model to address the user by name.
- `--quote-question` asks the model to quote the question first.
- `--answer-prefix "Short answer:"` asks the model to start with exact text.

Why this matters: many products care less about file formats and more about a familiar answer template: greeting, quoted question, list, final line, or addressing the user by name.

#### 6. Stop sequence: finish the thought and stop at a marker

Problem: if you only set a tiny `max-tokens`, the answer may be cut off:

```text
Masha has 5 app...
```

Better: ask the model to write a completion marker.

```bash
aiteach ask \
  "Masha had 3 apples and bought 2 more. Explain the solution." \
  --stop "END_OF_ANSWER" \
  --completion-instruction "Give a short solution. When the thought is complete, write END_OF_ANSWER."
```

Expected answer:

```text
3 + 2 = 5, so Masha has 5 apples.
```

The marker `END_OF_ANSWER` is not printed because the API stops generation at the stop sequence.

Why this matters: the model finishes the sentence, and the API stops the stream at the right place.

#### 7. Compare one prompt without and with controls

Task: see the difference on the same question.

```bash
aiteach compare \
  "Masha had 3 apples and bought 2 more. Explain the solution." \
  --max-tokens 60 \
  --completion-instruction "Answer in one sentence, without an intro."
```

The CLI sends two requests:

1. Without constraints.
2. With `max-tokens` and the instruction.

Why this matters: you can see how the same prompt changes when response control is added. This is useful before putting the prompt into a script or product.

#### 8. Interactive chat: change rules during the conversation

```bash
aiteach chat --max-tokens 180
```

Inside chat:

```text
> Explain the apple problem
/max-tokens 60
/answer-format bullets
/address-as Artem
/completion-instruction Answer like an elementary school teacher, max 2 sentences.
> Now explain a problem about 7 pears and 4 more pears
/control
/exit
```

Why this matters: sometimes you first need detail, then brevity, then JSON. You do not need to restart the program.

#### 9. Entity collection: an apple-problem form

Task: do not just answer; collect every field needed for a problem. For example:

- `item` - what is being counted;
- `initial_count` - how many there were;
- `added_count` - how many were added.

Command:

```bash
aiteach chat \
  --required-field item \
  --required-field initial_count \
  --required-field added_count \
  --goal-stop-mode combined
```

Example dialogue:

```text
> Create a word problem
assistant: What item are we counting?
> apples
assistant: How many apples were there at first?
> 3
assistant: How many apples were added?
> 2
assistant: {"fields":{"item":"apples","initial_count":3,"added_count":2},"next_question":null,"done":true}
```

After that, the CLI stops the dialogue because:

- all required fields are filled;
- the model returned `done: true`;
- the selected mode is `combined`.

Why this matters: this is no longer just one answer. It is a small agent that collects data until the entity is complete.

#### 10. Compare dialogue completion strategies

```bash
aiteach compare-goal \
  "Collect data for a problem: Masha had 3 apples and bought 2 more" \
  --required-field item \
  --required-field initial_count \
  --required-field added_count
```

The CLI compares:

- `state` - code checks: are all fields filled? Then stop.
- `instruction` - did the model say `done: true`? Then stop.
- `combined` - fields are filled and the model said `done: true`.

Simple meaning:

- `state` is more reliable as a shape check.
- `instruction` is more flexible, but the model can be wrong.
- `combined` is the strictest and best for important flows.

#### 11. Multiple profiles for different providers

Task: one profile for OpenRouter, another for DeepSeek.

```bash
aiteach profile add
aiteach profile add
aiteach profile list
aiteach profile use openrouter
aiteach ask "What is 3 + 2?"
aiteach ask --profile deepseek "What is 3 + 2?"
```

Why this matters: you can compare providers and models quickly without rewriting config or moving tokens manually.

### Providers

Supported profile kinds:

- `openai-compatible`
- `openrouter`
- `deepseek`
- `gigachat`
- `kimi`

The provider layer is split into two responsibilities:

- `ProviderSpec` describes one provider: `kind`, display name, `base_url`, default model, auth scheme, and extra HTTP headers.
- `providers/openai_compatible` implements the shared transport for OpenAI-compatible `/models` and `/chat/completions`.

To add a provider:

1. Create `src/providers/<provider>.rs`.
2. Return a `ProviderSpec` from that module.
3. Add a variant to `ProviderKind`.
4. Register it in `src/providers/mod.rs`.

The CLI does not need to change.

### Config And Secrets

Config stores only non-secret fields:

- `provider`
- `model`
- `base_url`
- `token_ref`

The token is stored only in the OS keychain through `keyring`. `token_ref` is stable and uses `provider:profile_name`, for example `openrouter:work`.

Tokens must never be written to:

- config
- git
- logs
- stdout/stderr
- debug output

Commands only show whether a token exists. The full token value is never printed.

### Chat

`ask` sends one prompt and prints one answer.

`chat` starts an interactive session. In-chat commands:

- `/exit`
- `/profile`
- `/model`
- `/clear`
- `/save`
- `/control`
- `/control clear`
- `/format text`
- `/format json-object`
- `/answer-format natural`
- `/answer-format bullets`
- `/answer-format numbered`
- `/answer-format short`
- `/answer-format steps`
- `/answer-format table`
- `/answer-prefix <text>`
- `/answer-suffix <text>`
- `/address-as <name>`
- `/quote-question`
- `/quote-question clear`
- `/max-tokens <number>`
- `/stop <sequence>`
- `/stop clear`
- `/format-instruction <text>`
- `/completion-instruction <text>`
- `/goal`
- `/goal clear`
- `/goal field <name>`
- `/goal mode manual`
- `/goal mode state`
- `/goal mode instruction`
- `/goal mode combined`

History is stored locally and does not contain tokens. Prompts and responses are not logged in debug output unless an explicit future `--log-conversation` mode is implemented.

### Response Control

Users can control response shape and completion behavior through `ask`, `chat`, and `compare`.

There are two different concepts:

- API-level format: something the provider can technically enforce, for example JSON object through `response_format`.
- Answer format: the human-facing response template, for example “start with my name”, “quote the question”, or “answer as bullets”.

CLI flags:

- `--response-format text` - regular text; the API field `response_format` is not sent.
- `--response-format json-object` - sends `response_format: {"type":"json_object"}` and adds a system instruction to return only a JSON object.
- `--answer-format natural|bullets|numbered|short|steps|table` - sets the human-facing answer template through a system instruction.
- `--answer-prefix <text>` - asks the model to start the answer with exact text.
- `--answer-suffix <text>` - asks the model to end the answer with exact text.
- `--address-as <name>` - asks the model to address the user by name or role.
- `--quote-question` - asks the model to quote the user's question first.
- `--max-tokens <number>` - sends `max_tokens` and bounds generation length on the provider side.
- `--stop <sequence>` - can be provided multiple times; sends a `stop` array.
- `--format-instruction <text>` - adds a system instruction that explicitly describes the response format.
- `--completion-instruction <text>` - adds a system instruction that describes when to finish, for example `Finish after exactly 3 bullets.`

`compare` sends the same prompt twice:

1. Without constraints: only `model` and `messages`.
2. With constraints: the same prompt plus selected `response_format`, `max_tokens`, `stop`, and instructions.

Example:

```bash
aiteach compare --profile work \
  "Explain Rust ownership to a junior developer" \
  --response-format json-object \
  --max-tokens 160 \
  --stop "END" \
  --completion-instruction "Finish after exactly 3 bullet points and then write END."
```

The result prints two blocks: `Without constraints` and `With constraints`.

### Dialogue Completion

There are two different stopping levels:

- Response stop: `stop`, `max_tokens`, `completion_instruction`. This ends one provider generation.
- Dialogue stop: the application decides whether to continue the agentic data-collection loop.

For dialogue stop, `aiteach` supports an entity with required fields. The model is asked to return JSON:

```json
{
  "fields": {
    "topic": "Rust ownership",
    "audience": "junior developers",
    "format": null
  },
  "next_question": "Which format do you need?",
  "done": false
}
```

Modes:

- `manual` - the CLI does not stop the dialogue automatically.
- `state` - deterministic code path: the CLI stops when every `--required-field` has a non-null value.
- `instruction` - agent-instruction path: the CLI trusts the model signal `done: true`.
- `combined` - both conditions are required: all fields are filled and `done: true`.

Interactive example:

```bash
aiteach chat --profile work \
  --required-field topic \
  --required-field audience \
  --required-field format \
  --goal-stop-mode combined
```

Inside chat, the goal can be changed:

```text
/goal
/goal field deadline
/goal mode state
/goal mode instruction
/goal mode combined
/goal clear
```

Compare stopping strategies:

```bash
aiteach compare-goal --profile work \
  "Collect article requirements" \
  --required-field topic \
  --required-field audience \
  --required-field format
```

`compare-goal` sends the same prompt in three variants:

1. `state` - the application checks field completeness.
2. `instruction` - the application trusts `done: true`.
3. `combined` - the application requires both complete fields and `done: true`.

### API Implementation Details

All current provider profiles use `POST /chat/completions`.

#### OpenAI-compatible

- Endpoint: `POST {base_url}/chat/completions`.
- `response_format` is sent as `{"type":"json_object"}` for JSON mode.
- `max_tokens` is sent as the output token cap.
- `stop` is sent as an array of stop sequences.
- Explicit format and completion conditions are added as `system` messages before the user prompt.
- For dialogue stop, the CLI adds a system instruction to return JSON with `fields`, `next_question`, and `done`, then checks local `GoalState`.

Note: some newer OpenAI reasoning models use `max_completion_tokens`; this CLI currently targets OpenAI-compatible Chat Completions and sends `max_tokens`.

#### OpenRouter

- Endpoint: `POST https://openrouter.ai/api/v1/chat/completions`.
- OpenRouter accepts an OpenAI-compatible body and documents `response_format`, `max_tokens`, and `stop`.
- The CLI adds provider headers `HTTP-Referer` and `X-Title`, then sends the same control fields.
- The provider normalizes `finish_reason`, including `stop` and `length`.
- Dialogue stop is implemented by the CLI: OpenRouter receives a regular OpenAI-compatible JSON-mode request, while `aiteach` checks required fields and `done`.

Docs: [OpenRouter parameters](https://openrouter.ai/docs/api/reference/parameters), [OpenRouter API overview](https://openrouter.ai/docs/api-reference/overview).

#### DeepSeek

- Endpoint: `POST https://api.deepseek.com/v1/chat/completions`.
- DeepSeek JSON Output is implemented with `response_format: {"type":"json_object"}`.
- DeepSeek recommends setting a reasonable `max_tokens` value so JSON is not truncated midway.
- `stop` is supported as a string or a list of stop sequences.
- Dialogue stop uses DeepSeek JSON Output: the CLI requests a JSON object and locally compares `fields`/`done` against the selected stop mode.

Docs: [DeepSeek JSON Output](https://api-docs.deepseek.com/guides/json_mode/), [DeepSeek chat completion](https://api-docs.deepseek.com/zh-cn/api/create-chat-completion/).

#### GigaChat

- Endpoint: `POST https://gigachat.devices.sberbank.ru/api/v1/chat/completions`.
- GigaChat Chat Completions uses an OpenAI-like request shape with `messages`, `model`, and `max_tokens`.
- The token stored in keychain can be a GigaChat authorization key: the CLI exchanges it for an access token through OAuth. For B2B/CORP scopes, set `AITEACH_GIGACHAT_SCOPE`; the default is `GIGACHAT_API_PERS`.
- The CLI sends `max_tokens`, `stop`, and `response_format` in the same body. If a concrete GigaChat model or plan does not support a field, the provider returns an HTTP/API error and the CLI reports it without exposing the token.
- Explicit format and completion conditions remain available through `system` messages.
- For dialogue stop, the CLI does not rely on provider-specific session state: required field state is stored locally.
- If GigaChat fails with TLS error `self signed certificate in certificate chain`, download certificates from Sber docs, combine root/sub CA into a PEM bundle, and run the CLI with `AITEACH_CA_BUNDLE=/path/to/chain_pem.txt`.

Docs: [GigaChat model selection example](https://developers.sber.ru/docs/ru/gigachat/guides/selecting-a-model), [GigaChat streaming example](https://developers.sber.ru/docs/ru/gigachat/guides/response-token-streaming).

#### Kimi / Moonshot

- Endpoint: `POST https://api.moonshot.ai/v1/chat/completions`.
- Kimi/Moonshot works through OpenAI-compatible Chat Completions.
- The CLI sends `response_format`, `max_tokens`, and `stop` in the request body.
- For thinking models, reasoning tokens also count toward the token budget, so a small `max_tokens` value can leave little room for the final answer.
- Dialogue stop uses the same JSON object contract and local entity completeness check.

Docs: [Kimi FAQ](https://platform.kimi.ai/docs/guide/faq), [Kimi thinking models](https://platform.moonshot.ai/docs/guide/use-kimi-k2-thinking-model.en-US).

### Errors

By default the CLI prints user-facing errors. `--verbose` enables internal details.

HTTP errors include:

- provider
- endpoint category: `models`, `chat`, or `other`
- HTTP status code when available
- clear auth guidance
- `Retry-After` for rate limits when returned by the provider
- DNS/TLS/proxy/timeout guidance for network failures
- unexpected JSON format messages when provider responses do not match the expected schema

Config errors include the config path and a concrete reason: read, write, or TOML parsing.

### Structure

```text
src/
  chat.rs
  cli.rs
  config.rs
  errors.rs
  secrets.rs
  providers/
    mod.rs
    openai_compatible.rs
    openrouter.rs
    deepseek.rs
    gigachat.rs
    kimi.rs
```

### Checks

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```
