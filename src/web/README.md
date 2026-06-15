# Web

`web/` - локальный Axum server и вшитый single-file UI. Backend использует те же config, secrets, chat и provider слои, что CLI.

## Файлы

- `mod.rs` - Axum server, routes, handlers и request/response DTO.
- `tests.rs` - unit-тесты web handlers/DTO/UI string checks.
- `error.rs` - преобразование `AppError` в HTTP status + JSON error body.
- `parameters.rs` - web-facing constraints для provider response controls.
- `tokens.rs` - web token override/status/lookup rules.
- `util.rs` - маленькие text/parse helpers для request DTO.
- `ui.html` - HTML/CSS/JS целиком; подключается через `include_str!("ui.html")`.

## Routes

| Метод | Путь | Назначение |
| --- | --- | --- |
| `GET` | `/` | Отдать UI |
| `GET` | `/api/agents` | Список локальных агентов |
| `GET` | `/api/providers` | Список providers и ограничения параметров |
| `POST` | `/api/token/status` | Проверить наличие сохраненного токена для provider |
| `POST` | `/api/token/save` | Сохранить token override для provider |
| `POST` | `/api/models` | Загрузить модели provider |
| `POST` | `/api/chat/session` | Открыть или создать web chat session |
| `POST` | `/api/chat` | Отправить prompt через `ChatAgent` |
| `POST` | `/api/memory/update` | Ручное управление слоями памяти (set/delete/clear-layer), без provider-запроса |
| `POST` | `/api/agents/manage` | CRUD именованных агентов (list/load/save/delete/save-task/save-knowledge) |

`/api/agent/*` сохранены как совместимые aliases для chat session/chat.

## Именованные агенты (вкладка «Агент»)

Отдельная inspector-вкладка `Агент` (`data-tab="agent"`). Агент — это персистентная
сущность: создаёшь, заходишь («Войти»), и он помнит свои настройки и 3 слоя памяти.

- **краткосрочная** — диалог чата (session store), `session_key = "agent:<id>"`.
- **рабочая** — `TaskContext` (title/goal/status/steps/notes), редактор в UI.
- **долговременная** — `KnowledgeDoc` (текст знаний/профиль/решения), хранится в
  проектном TOON-store (не `.md`).

`POST /api/agents/manage` (`agents_manage` handler, action-dispatch как
`memory_update`): `list` / `load` / `save` (upsert, id генерится при создании) /
`delete` / `save-task` / `save-knowledge`. Хранение — `LocalSessionStore`:
`agent-<id>.agent.toon`, `agent-<id>.task.toon`, `agent-<id>.knowledge.toon`,
индекс `agents-index.toon`. Токен НЕ хранится в агенте (остаётся в keyring —
инвариант config/secrets раздельно).

При чате с активным агентом запрос несёт `saved_agent_id`; сервер биндит сессию
к `agent:<id>`, грузит TaskContext + KnowledgeDoc и инъектит их в provider request
блоками `[memory:working]` / `[memory:long-term]` (`ChatAgent::set_working_context`
/ `set_knowledge`). «Войти» восстанавливает provider/model/system prompt/memory и
оба редактора; активный id — в localStorage (`ai_active_agent`).

## Управление и дебаг памяти (memory layers)

UI в табе `Debug` -> «Модель памяти — слои» показывает 3 типа памяти и
позволяет управлять ими:

- 🟡 **краткосрочная** — текущий диалог: `session_summary` + окно последних N
  сообщений (`recent_window`/`recent_messages_sent`). Эфемерно; KV-факты не
  хранит. Кнопка «Очистить» сбрасывает summary.
- 🔵 **рабочая** — данные текущей задачи: KV-факты слоя `working` (goal,
  constraints).
- 🟢 **долговременная** — профиль/решения/знания: KV-факты слоя `long-term`.

`POST /api/memory/update` (`memory_update` handler) грузит память сессии,
применяет действие и сохраняет per-session + profile-shared long-term, затем
возвращает свежий `context_debug` (с полем `layers`). Действия: `set`
(ключ+значение+слой), `delete` (по ключу), `clear-layer` (по слою). Сервер
отклоняет запись KV в `short-term` и запись чувствительных значений в
`long-term` (`looks_sensitive`).

## Поток `/api/chat`

```text
ChatWebRequest
  -> selected_agent()
  -> request.profile()
  -> resolve_web_token()
  -> load/create LocalSessionStore session
  -> apply selected context strategy
  -> ChatAgent::respond_with_debug()
  -> save session + context state + metrics
  -> ChatWebResponse
```

## UI контекста

- Переключатель стратегий находится в `Параметры -> Контекст`.
- `Sliding Window` показывает только размер окна N. Оно ограничивает provider request, но не удаляет сообщения из UI/session history.
- `Summary` показывает размер окна raw сообщений, пороги compaction и настраиваемый summary prompt.
- `Sticky Facts` показывает размер окна N, facts extraction prompt, facts preamble, persisted KV facts и точный facts block из provider request. Extraction prompt выбирает, что сохранять; preamble описывает provider request.
- Каждый persisted KV-факт помечен бейджем слоя памяти (`short-term` / `working` / `long-term`): `FactDebugView.layer` приходит из `AgentMemory::fact_layer()`. Provider facts block группирует KV по слоям (`[working]` / `[long-term]`), так что видно источник факта. `long-term` факты переживают новую сессию, `working` — нет.
- `Branching` показывает кнопки checkpoint, создания двух веток и переключения между ними. Каждая ветка отправляется как отдельная local session, чтобы histories не смешивались.
- `Scoped Branches` в UI называется и ощущается как auto topics в одном окне: юзер не переключает темы руками, агент сам выбирает/создает тему, а debug-блок показывает выбранную тему и счетчики. Все остается в одной session, но provider request получает только выбранную тему.
- UI показывает только настройки, нужные выбранной strategy: summary prompt не должен исчезать, но и не должен шуметь в других strategies.

## Где искать баг

- UI не видит provider/model defaults: `providers()` handler и `applyProviderDefaults()` в `ui.html`.
- Токен из web не используется: `tokens::resolve_web_token()`.
- Статус/сохранение токена в web: `token_status()`, `token_save()`, `tokens::web_token_present()`.
- Сессия web не продолжается: `chat_session()`, `chat()`, `LocalSessionStore`.
- Метрики запроса/диалога неверные: `TokenUsage`, `add_request_metrics()`, `setMetrics()`, `setSessionMetrics()`.
- Debug JSON странный: `ChatDebugView` и provider debug в `ChatAgent`.
- Ошибка приходит без понятного текста: `error::WebError::into_response()`.
- Strategy/topic control странно работает: `WebMemoryConfig::into_memory_config()`, `ContextDebugView` и topic helpers в `ui.html`.
- Sticky facts не видно, неясно что собирается или что отправлено: `WebMemoryConfig::facts_extraction_prompt`, `ContextDebugView.facts`, `updateFactsPreview()` и `AgentMemory::facts_block()`.
- Бейдж слоя факта неверный или long-term не переносится в новую сессию: `FactDebugView.layer`, `AgentMemory::fact_layer()` и `LocalSessionStore::{save_long_term, seed_long_term}` в `chat_session()`/`chat()`/`chat_stream()`.
- Branch switch потерял сообщения: branch state в `ui.html`, `session_id/new_session/messages` payload.

## Инварианты UI

- После изменения `ui.html` нужен `rtk cargo build`, потому что файл вшивается в бинарник.
- Токен из формы можно сохранять только для текущего provider.
- Debug view должен редактировать секреты.
- Web API не должен обходить `ProviderClient` или `ChatAgent`.
- UI разделяет метрики последнего запроса и накопленные метрики диалога.
- UI не должен показывать нерелевантные controls: summary controls только для `Summary`, facts extraction prompt/facts preamble только для `Sticky Facts`, auto-topic debug только для `Scoped Branches`.
