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
| `POST` | `/api/agents/manage` | Stateful-агенты + диалоги (list/load/save/delete/build-schema/dialogs/dialog-rename/dialog-delete) |

`/api/agent/*` сохранены как совместимые aliases для chat session/chat.

## Агенто-центричный UI + stateful-агенты

Весь UI работает от агента. Пока агент не выбран — виден только **agent gate**
(`#agentGate`): список агентов (Войти/Удалить) + создание (имя, provider/model,
информация о проекте и агенте, опциональные поля для уточнения, инварианты,
«Сгенерировать интервью»). Чат/настройки/debug
(`#workspace`) скрыты. Войдя — слева сверху всегда виден активный агент + стадия
(`#activeAgentBar`) и кнопка «Сменить агента». Активный id — в localStorage
(`ai_active_agent`); при загрузке агент авто-восстанавливается, иначе показывается gate.

У агента **несколько диалогов** (чатов). Панель «Чаты» в шапке диалога (`#dialogsBar`,
видна только в workspace) — чипы со всеми чатами агента (title из первого сообщения,
активный подсвечен), «+ чат», ✕ для удаления. Все чаты делят долговременный профиль
и инварианты агента; рабочая задача хранится как task-scope и может объединять
несколько чатов одной фичи (`DialogMeta.task_id`, по умолчанию `default`).
`session_key = "agent:<id>"` для всех (`chat_session`/`chat`/`chat_stream`
консистентны), индекс диалогов — `agent-<id>.dialogs.toon`, авто-регистрация в
`save_session` (`agent_id_from_key`). Actions: `dialogs` / `dialog-rename` /
`dialog-delete`. JS: `refreshDialogs` / `switchDialog` / `newDialog` /
`deleteDialog`.

Память агента заполняется **самим агентом**, не вручную:
- **информация о проекте и агенте** — свободное описание роли, области, фактов
  проекта и важных условий (например город, аудитория, язык). Хранится в
  сохранённом агенте и инъектится в каждый запрос как `[agent:domain]`.
- **долговременная** — `AgentProfile` (схема интервью + значения). Агент сам
  спрашивает недостающие поля и заполняет их из диалога. Вкладка `Профиль`.
- **рабочая** — `TaskContext` со стадией FSM (`clarify→planning→execution→
  validation→done`), ведётся автоматически для рабочей задачи/фичи и может быть
  общей для нескольких диалогов. Вкладка `Задача` (бейдж стадии + флоу + план,
  read-only).
- **инварианты** — редактируемый список (вкладка `Инварианты`), проверяются по ответу.
- **краткосрочная** — диалог чата, `session_key = "agent:<id>"`.

`POST /api/agents/manage` (`agents_manage`, action-dispatch): `list` / `load`
(agent+task+profile) / `save` (upsert + project info/инварианты + схема профиля с переносом
значений) / `delete` / `build-schema` (LLM-генерация интервью из домена; нужен
provider+token). Хранение — `LocalSessionStore`: `agent-<id>.agent.toon`,
`agent-<id>.task-<task>.toon`, `*.profile.toon`, индекс `agents-index.toon`. Токен
НЕ хранится в агенте (keyring — инвариант config/secrets раздельно).

Чат с активным агентом несёт `saved_agent_id`; сервер биндит сессию к `agent:<id>`,
грузит profile/task-scope/invariants/domain и инъектит блоками `[agent:domain]` /
`[memory:long-term]` / `[memory:working]` / `[invariants]`. После ответа —
`ChatAgent::stateful_postprocess` (заполнить профиль, продвинуть стадию по FSM,
проверить инварианты), persist task-scope+profile, и `agent_state` в ответе
(`StatefulDebugView`: стадия, pending-вопросы, переход, нарушения) рендерится в UI
(стадия в хедере, нарушения во вкладке Инварианты).

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
- Сохранённые KV-факты (`working`/`long-term`) попадают в provider facts block при любой context strategy; `Sticky Facts` нужен для авто-извлечения новых фактов, а не для чтения уже сохранённых.
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
