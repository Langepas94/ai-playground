# Модули проекта

## chat:: — Логика диалогов и агентов

### agent.rs — ChatAgent (основной агент)
**Ответственность:** управление состоянием диалога, история сообщений, вычисление контекста, запросы к провайдеру

- **ChatAgent** — состояние одной сессии (профиль, токен, история, память)
  - `history: Vec<ChatMessage>` — все сообщения в сессии
  - `memory: AgentMemory` — сжатые старые сообщения (summarization)
  - `control: ResponseControl` — параметры ответа (temperature, max_tokens, etc)
  - `pricing` + `billing` — информация о стоимости
  - `context_limit: Option<u32>` — максимум токенов в контексте (из модели)

- **Публичный API:**
  - `respond()` — синхронный запрос, добавить в историю
  - `build_stream_request()` — подготовить запрос для стриминга
  - `record_stream_response()` — записать ответ стрима в историю
  - `estimate_next_exchange()` — предсчитать стоимость следующего запроса
  - `set_context_limit()` — установить лимит контекста модели

- **Бизнес-правила:**
  - История = все сообщения пользователя + агента, в порядке появления
  - Старые сообщения → summarization (AgentMemory) когда история растёт
  - Контекст для запроса = summary старых + свежие сообщения
  - Без дефолтного system prompt (удален)

### memory.rs — AgentMemory (управление контекстом)
**Ответственность:** сжатие старой истории (summarization), выбор актуальных сообщений для контекста

- **AgentMemory** — хранит сводку старых сообщений
  - `session_summary: Option<String>` — текстовая сжатая сводка старых диалогов
  - `summarized_message_count: usize` — сколько старых сообщений уже в сводке

- **MemoryConfig** — параметры сжатия
  - `strategy: MemoryStrategy` — как сжимать (Sliding, Full, Summary)
  - `window_size: usize` — сколько свежих сообщений всегда отправлять полностью
  - `min_history_tokens: u32` — минимум токенов истории перед сжатием

- **Публичный API:**
  - `build_context()` — выбрать актуальные сообщения для текущего запроса
    - Возвращает: [summary] + [свежие сообщения] в формате ChatMessage
  - `next_summary_range()` — когда и какую часть истории сжимать

- **Бизнес-правила:**
  - Контекст ≈ (summary старого) + (window последних сообщений)
  - Summary генерируется только если история > min_history_tokens
  - Старое → summary только раз в ~20+ новых сообщений

### session.rs — интерактивный чат (CLI)
**Ответственность:** REPL цикл для `ai chat` команды, управление `/` командами

- Поддерживает команды:
  - `/attach <file>` — приложить файл к следующему сообщению
  - `/clear` — новая сессия
  - `/models` — список доступных моделей
  - и др.

### store.rs — хранилище сессий (web)
**Ответственность:** сохранить/загрузить сессии на диск (для web UI)

- **LocalSessionStore** — JSON файлы в `~/.ai/sessions/`
  - Ключ = session_id (UUID)
  - Содержимое = сохранённая ChatAgent с историей и памятью

- **Публичный API:**
  - `save_session(key, agent)` — сохранить состояние
  - `load_session(key)` → ChatAgent
  - `list_sessions()` → Vec<(id, timestamp)>`

- **Бизнес-правило:** сессия сохраняется только в web UI (для перезагрузок страницы)

### goal.rs — ConversationGoal (для compare)
**Ответственность:** спецификация для `ai compare` (сравнение ответов от разных моделей)

### token_accounting.rs — подсчёт стоимости
**Ответственность:** оценка токенов, оценка стоимости (в API-валюте)

- **TokenEstimate** — результат оценки
  - `current_request_tokens` — токены в текущем запросе
  - `response_tokens` — оценка ответа
  - `cost: Option<Cost>` — денежная стоимость

- **Бизнес-правило:** `estimate_text_tokens` используется как fallback **только** когда провайдер не вернул `usage`. Не-ASCII буквы (кириллица, CJK) считаются ~2 символа/токен (как cl100k), а не 2 токена/символ — иначе стоимость завышается ~4x.

---

## providers:: — подключение к LLM провайдерам

### mod.rs — trait ProviderClient + enum ProviderKind
**Ответственность:** единый интерфейс для всех провайдеров (OpenAI, Anthropic, Deepseek и т.д.)

- **ProviderClient trait** — что может делать провайдер
  - `chat_completion()` → ChatResponse (single request)
  - `chat_completion_with_debug()` → ChatResponse + HTTP debug info
  - `list_models()` → Vec<ModelInfo> (с context_length)
  - `list_model_info()` → модели с ценовой информацией

- **ProviderKind enum** — какой провайдер использовать
  - `OpenAiCompatible`, `Deepseek`, `Kimi`, `Gigachat`, `OpenRouter` и др.
  - Каждый вариант → `spec()` с default URL и моделью

- **ReqwestProviderClient** — реализация через reqwest
  - `stream_chat_completion()` — SSE стриминг ответов (не в trait, добавлен прямо на impl)

- **Бизнес-правила:**
  - Все запросы идут через OpenAI-compatible Chat Completions API
  - Каждый провайдер может иметь свой default URL и model
  - Стриминг = `stream: true` в payload + SSE парсинг ответа

### openai_compatible.rs — реализация OpenAI API
**Ответственность:** формирование запроса, парсинг ответа, SSE парсинг для стрима

- **OpenAiChatPayload** — JSON body для /v1/chat/completions
  - `model`, `messages`, `temperature`, `max_tokens` и т.д.
  - `stream: Option<bool>` — включить стриминг?

- **stream_chat_completion()** — реализация стрима
  - Устанавливает `stream: true`
  - Читает `response.bytes_stream()`
  - Парсит SSE линии: `data: {"choices": [{"delta": {"content": "..."}}]}`
  - Вызывает callback `on_token(&text)` на каждый чанк

- **Бизнес-правило:** контекстное окно (`context_length`) загружается из `GET /v1/models/{id}`

### deepseek.rs, kimi.rs, gigachat.rs, openrouter.rs
**Ответственность:** spec (default URL, model, auth header) для каждого провайдера

---

## config:: — конфигурация

### ProfileConfig — профиль подключения
- `provider: ProviderKind` — какой провайдер
- `model: String` — конкретная модель
- `base_url: String` — URL API (может быть переопределён)
- `token_ref: String` — ключ в keyring для токена

---

## secrets:: — управление токенами

### KeyringSecretStore
**Ответственность:** безопасное хранение API токенов в системном keyring

- **Бизнес-правило:** токены **никогда** не в config файле, только в keyring
- Токены **никогда** не печатаются полностью (только `sk-...` первые 3 символа)

---

## web:: — HTTP API + веб интерфейс

### mod.rs — Axum маршруты
**Ответственность:** HTTP API endpoints, SSE стриминг, управление сессиями

**Маршруты:**
- `POST /api/agent/chat` — синхронный запрос (старый, используется падение)
- `POST /api/agent/chat/stream` — стриминг SSE
  - Читает `ChatWebRequest { profile, prompt, attachments }`
  - Вызывает `client.stream_chat_completion()`
  - Отправляет SSE события:
    - `event: token` `data: "текст"` — новый токен
    - `event: done` `data: {...}` — конец, в том числе `session_id`, `messages`, `session_metrics`
    - `event: error` `data: "текст ошибки"`
  - Сохраняет session в store

- `POST /api/models` — список моделей с контекстным окном
- `POST /api/token/save` — сохранить токен в keyring
- `GET /api/agent/session` + `POST /api/chat/session` — управление сессией

**Бизнес-правила:**
- Body limit = 50MB (для больших файлов)
- Стриминг = mpsc канал + futures stream → Axum Sse<>
- Сессия сохраняется в LocalSessionStore после успешного ответа

### ui.html — веб интерфейс
**Ответственность:** фронтенд, управление UI состоянием, отправка запросов

---

## cli:: — командная строка

### mod.rs — dispatcher
**Ответственность:** распределение команд (ask, chat, compare, profile, token, pricing)

- `run()` → обрабатывает Cli { command: Command::Ask/Chat/Compare/... }
- `run_with_store()` → с сохранением в LocalSessionStore

---

## errors:: — обработка ошибок

### AppError enum
**Ответственность:** единый тип ошибок для всего приложения

- `InvalidInput(String)`
- `ProviderHttpError { status, body, provider }`
- `InvalidProfile(String)`
- и т.д.

- **Бизнес-правило:** `anyhow` только в `main()`, везде else `AppError`
