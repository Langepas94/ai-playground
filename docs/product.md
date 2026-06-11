# Продуктовая документация

## Web UI элементы и поведение

### Левая колонка — Диалог

#### История сообщений (conversation)
- Все сообщения пользователя (голубые, справа) и модели (белые, слева)
- Сообщения отрендерены из `session.messages` (из LocalSessionStore или от сервера)
- При переполнении контекста — не обрезаются на фронте, обрезаются на бэке в AgentMemory

**Поведение при стриминге:**
- Сообщение юзера добавлено оптимистично (сразу после отправки)
- Пузыр ассистента создан пустой, слова появляются по одному
- После `event: done` → финальный рендер с метаданными

#### Форма ввода (composer)
- Textarea для промпта
- 📎 кнопка для прикрепления файлов (в textarea-wrap, абсолютное позиционирование)
- Чипсы прикреплённых файлов (если `pendingAttachments.length > 0`)
- Метабар выше textarea:
  - Контекстное окно модели (если `modelContextById[selectedModel()]`)
  - Статус прикреплённых файлов

#### Кнопки
- "Отправить" (`id="send"`) — POST /api/agent/chat/stream
- "Новая сессия" — очистить историю, создать новый session_id

### Правая колонка — Управление

#### Профиль (profile-panel)
- Выбор провайдера (OpenAI, Deepseek, Kimi и т.д.)
- Выбор модели (загружается при смене провайдера)
- URL API (default или custom)
- Ввод токена (сохраняется в keyring через POST /api/token/save)
- Показывает "Контекст: 128k" если model.context_length загружен

#### Debug tab (4 таба)
1. **Provider Request** — JSON payload отправленный на /v1/chat/completions
2. **Provider Response** — сырой JSON ответ от провайдера (или error при ошибке)
3. Служат для диагностики (в т.ч. при ошибке "Failed to buffer the request body")
4. В debug показывается ссылка на документацию провайдера (PROVIDER_DOCS map)

**Бизнес-правило:** debug должен показывать payload и ответ **даже при ошибке**, в т.ч. "Failed to buffer the request body: length limit exceeded"

#### Metrics
- `Запрос: 1234 токен (input), 567 токен (output)`
- `Диалог: 5678 токен всего`
- Вычисляется по usage из response

---

## Сессии

### Создание сессии
1. На загрузке UI → `POST /api/agent/session` → новый session_id (UUID)
2. Сохраняется в localStorage под ключом `{provider}_{model}` (session cache)

### Загрузка сессии
1. При смене провайдера/модели → проверить localStorage есть ли session_id
2. Если есть → загрузить историю и метрики из server store
3. Если нет → новая сессия

### Сохранение сессии
1. После успешного ответа → server сохраняет в LocalSessionStore (JSON файл)
2. При перезагрузке страницы → загружается из server по session_id из localStorage

---

## Прикрепление файлов

### CLI (`ai ask --file path/to/file.txt`)
- Содержимое файла → в prompt как `--- filename ---\n{content}\n\n`

### Web UI (📎 кнопка)
1. Юзер выбирает файл(ы) → прочитать через FileReader API (на фронте, не отправляем сразу)
2. Сохранить в `pendingAttachments: Array<{name, content}>`
3. Отрендеритьься чипсы (можно удалить по клику на ×)
4. При отправке → добавить в JSON payload: `{attachments: [{name, content}]}`
5. На бэке → merge attachments в prompt (функция `build_web_prompt()`)

**Бизнес-правило:** файлы читаются на фронте (не отправляются отдельно), максимальный размер тела = 50MB

---

## Контекстное окно (context_length)

### Загрузка
1. При смене модели → `POST /api/models { provider, model }`
2. Ответ содержит `models[].context_length` (из /v1/models/{id} OpenAI API)
3. Сохранить в `modelContextById` Map

### Отображение
- В профиль-панели: "Контекст: {context_length}" (или "—" если не загружен)
- В метабаре composer: тоже самое или "— загрузите модели"

**Бизнес-правило:** модели загружаются автоматически при старте и при смене провайдера

---

## Стриминг ответов

### Основное поведение
1. Юзер отправляет prompt
2. Оптимистично добавить сообщение юзера в историю (сразу покажется)
3. POST /api/agent/chat/stream + fetch ReadableStream
4. Парсить SSE события:
   - `event: token` → добавить текст к пузыру ассистента (обновить DOM)
   - `event: done` → получить session_id, messages, session_metrics
5. Финальный рендер истории с метаданными

### Откат при ошибке
1. Если до получения первого токена произошла ошибка
2. Откатить оптимистичное сообщение юзера (удалить из истории, вернуть в textarea)
3. Показать error сообщение в conversation

**Бизнес-правило:** стриминг показывает токены в реальном времени, не ждёт полный ответ. Это решает проблему "18 секунд черный экран" (GPT-4 медленный, но начинает отвечать сразу).

---

## Метрики и затраты

### Request metrics
- `elapsed_ms: u128` — сколько миллисекунд заняла обработка
- `usage: Option<{input_tokens, output_tokens}>`
- `cost: Option<{amount, currency}}`

Вычисляются на основе response usage + pricing информации.

### Session metrics
- Накопление всех metrics за сессию
- Вычисляются функцией `add_request_metrics()` (в chat::mod.rs)
- Показываются в "Диалог: X токен всего"

**Бизнес-правило:** при загрузке существующей сессии → metrics не обнуляются, продолжают считаться только новые запросы

---

## Ошибки и исключительные ситуации

### Переполнение контекста
- Эндпоинт возвращает 400 Bad Request: "Failed to buffer the request body: length limit exceeded"
- На фронте → показать в debug, не обнулять историю
- На бэке → AgentMemory автоматически создаст summarization старых сообщений на следующий запрос

### Контекст переполнен + нет summarization
- Если history + новый запрос > context_limit и нет суммари
- На бэке → AgentMemory.build_context() применит Sliding window (только последние N сообщений)
- Поведение: модель забудет старый контекст, но запрос пройдёт

**Бизнес-правило:** переполнение лучше, чем падение с ошибкой. Graceful degradation.

---

## Команды CLI

### `ai ask <prompt>`
- Одноразовый запрос, нет истории
- Поддерживает `--file` флаг (можно повторять)

### `ai chat`
- Интерактивный REPL с историей
- Поддерживает команды:
  - `/attach <file>` — прикрепить файл
  - `/clear` — новая сессия
  - `/models` — список моделей
  - и т.д.

### `ai compare <prompt>`
- Запросить ответ от разных моделей (ConversationGoal)
- Вывести side-by-side для сравнения

### `ai profile`
- Управление профилями: `use`, `remove`, интерактивный выбор

### `ai token`
- Сохранить токен в keyring: `ai token --provider openai --value sk-...`

---

## Система сохранения и загрузки

### Что сохраняется в LocalSessionStore
- session_id (UUID)
- messages: Vec<ChatMessage> (вся история)
- session_summary (из AgentMemory)
- session_metrics (накопленные затраты)
- control (параметры ответа: temperature и т.д.)

### Что сохраняется в localStorage (браузер)
- `{provider}_{model}` → session_id (для быстрого восстановления при перезагрузке)

### Что НЕ сохраняется
- API токены (только в keyring)
- Pending attachments (теряются при перезагрузке)
- Unsent prompt (теряется при перезагрузке)

---

## API контракты

### POST /api/agent/chat/stream
**Request:**
```json
{
  "profile": {"provider": "openai_compatible", "model": "gpt-4", "base_url": "...", "token_ref": "..."},
  "prompt": "user message",
  "attachments": [{"name": "file.txt", "content": "..."}]
}
```

**Response (SSE):**
```
event: token
data: "hello "

event: token
data: "world"

event: done
data: {"session_id": "...", "messages": [...], "session_metrics": {...}}

event: error
data: "error message"
```

### POST /api/models
**Request:**
```json
{
  "profile": {...}
}
```

**Response:**
```json
{
  "models": [
    {"id": "gpt-4", "context_length": 8192, "pricing": {...}}
  ]
}
```

---

## Заимствования и ограничения

### Ограничения контекста
- Web UI: максимум 50MB body (для файлов)
- Model: зависит от провайдера (gpt-4 = 8192 токенов, gpt-4-turbo = 128k и т.д.)
- Graceful degradation: если переполнение → AgentMemory сжимает старые сообщения

### Ограничения скорости
- Стриминг SSE → нет timeout на отправку ответа (может быть долго)
- Timeout на memory summarization: 250ms (не тормозит основной запрос)

### Безопасность
- Токены в keyring (OS-level security)
- Config файлы в `~/.ai/` (user-readable)
- Sessions в `~/.ai/sessions/` (user-readable, но session_id = UUID = нужно угадать)
