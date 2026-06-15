# Chat runtime

`chat/` держит локальную диалоговую логику поверх provider API: одноразовые запросы, REPL, сравнения, goal mode, history и стратегии контекста.

## Файлы

- `mod.rs` - публичный API: `ask_once`, `compare_*`, `format_request_metrics`, re-export ключевых типов.
- `agent.rs` - `ChatAgent`: история, context strategy, сборка `ChatRequest`, вызов provider client.
- `agent_tests.rs` - unit-тесты `ChatAgent`.
- `session.rs` - интерактивный `ai chat`, slash-команды и terminal I/O.
- `goal.rs` - `ConversationGoal`, `GoalState`, stop modes и сравнение goal режимов.
- `memory.rs` - `AgentMemory`, `MemoryConfig`, `MemoryLayer`, summary, sticky facts, слои памяти и выбор сообщений для context strategies.
- `memory_tests.rs` - unit-тесты `AgentMemory`.
- `token_accounting.rs` - локальная оценка токенов запроса, истории, ответа, стоимости и overflow по context limit.
- `store.rs` - `LocalSessionStore`: TOON-сессии, context sidecar, индекс последней сессии.
- `history.rs` - сохранение истории в файл.
- `AGENT_RUNTIME.md` - подробная модель локального agent runtime.

## Поток `ask`

```text
ask_once()
  -> get_config_profile_token()
  -> ChatAgent::new(...)
  -> ChatAgent::respond()
  -> ProviderClient::chat_completion()
```

## Поток сессии

```text
interactive_chat()
  -> load_or_create_latest()
  -> ChatAgent(history + context state)
  -> respond()
  -> save_session() + save_memory()
```

## Стратегии контекста

```text
Summary
  -> сжимает старые сообщения отдельным provider-запросом
  -> отправляет summary + последние N raw сообщений
  -> prompt summary настраивается отдельно от facts prompt

Sliding Window
  -> хранит system prompt + последние N обычных сообщений
  -> старые обычные сообщения не отправляет провайдеру, но не удаляет из local history/UI

Sticky Facts
  -> обновляет key-value facts после каждого user message через настраиваемый extraction prompt
  -> хранит facts в `AgentMemory.facts` как local key-value sidecar
  -> использует настраиваемый facts extraction prompt, чтобы выбрать категории KV
  -> извлекает атомарные KV-факты, а не сохраняет весь user prompt по слову-триггеру
  -> отправляет отдельный read-only facts block + последние N сообщений, включая текущий user prompt
  -> Web debug показывает persisted KV facts и точный facts block для provider request

Branching
  -> работает с независимой веткой истории
  -> UI создает checkpoint и две ветки от одного места
  -> переключение ветки меняет session/history без смешивания сообщений

Scoped Branches
  -> одна session и одно окно агента
  -> агент сам раскладывает сообщения по темам без ручного переключения юзером
  -> каждое сообщение получает внутренний topic/branch label
  -> в provider request попадает только автоматически выбранная тема + system/facts
```

## Слои памяти (memory layers)

`MemoryLayer` задаёт явную модель памяти поверх существующих типов. Минимум 3 слоя:

```text
short-term
  -> recent message window + AgentMemory.session_summary (Summary/SlidingWindow)
  -> lifetime: текущий диалог; эфемерно, в новую сессию НЕ переносится
  -> в контексте помечается блоком "[memory:short-term] ..."

working
  -> данные текущей задачи: goal, constraints (+ ConversationGoal/GoalState в goal.rs)
  -> lifetime: текущая сессия; хранится в per-session memory sidecar
  -> в новой сессии пусто

long-term
  -> устойчивые знания: preferences, decisions, профиль (location/age/interests/...)
  -> lifetime: между сессиями; дублируется в profile-shared store
     (`longterm-<profile>.memory.toon`) и seed-ится в каждую новую сессию
```

- Явный выбор слоя — это работа агента: при Sticky Facts экстрактор
  (`facts_extraction_prompt`) возвращает для каждого факта `layer`
  (`{"facts":[{"key","value","layer"}]}`), и `merge_extracted_facts_with_layers`
  пишет факт в выбранный слой. `ShortTerm` от модели демотируется в `Working`
  (short-term не хранит KV). Это и есть "агент сам решает, что и куда сохранять".
- Fallback-роутинг, когда слой не задан (legacy/flat JSON или keyword-путь без
  LLM): `default_fact_layer(key)` -> `goal`/`constraints` = `working`, всё
  остальное = `long-term`. `short-term` не хранит KV-факты.
- Ручное переопределение слоя при записи:
  `AgentMemory::set_fact_in_layer(key, value, layer)`.
  Слой каждого ключа -> `AgentMemory::fact_layer(key)`; факты слоя ->
  `facts_in_layer(layer)`. Ручное управление: `remove_fact(key)`,
  `clear_layer(layer)` (для `short-term` также чистит summary). `looks_sensitive()`
  — публичный guard, чтобы не писать секреты в `long-term` (web `/api/memory/update`).
- `facts_block` группирует KV по слоям с метками `[working]` / `[long-term]`, так
  что в provider request видно, из какого слоя пришёл факт.
- trace: установите env `AI_MEMORY_TRACE=1`, чтобы в stderr печаталось
  `[memory] fact <key> → layer <layer>` при каждой записи.
- Персист: `LocalSessionStore::save_long_term` / `load_long_term` /
  `seed_long_term` (профильный файл). `save_memory` хранит per-session sidecar
  как раньше. Сценарий «новая сессия»: long-term остаётся, short-term пуст
  (тест `long_term_survives_new_session_short_term_does_not` в `store.rs`).

## Stateful-агенты (SavedAgent)

Персистентная сущность агента поверх трёх слоёв памяти (`store.rs`). Память
**заполняется самим агентом**, не вручную: юзер задаёт только домен + инварианты.

- `SavedAgent` — настройки (provider/base_url/model/system_prompt + `domain` +
  `invariants` + `SavedMemoryConfig`). Токен НЕ хранится (keyring).
- `TaskContext` — **рабочая** память: `stage` (FSM) + title/goal/plan/results/notes
  + `violations`. Стадия ведётся автоматически и общая для всех сессий агента.
- `AgentProfile` (`Vec<ProfileField>`) — **долговременная** память: схема интервью
  (key/question/required) + заполненные агентом значения.
- **краткосрочная** память — обычная сессия чата, `session_key = "agent:<id>"`.

`TaskStage` (`clarify→planning→execution→validation→done`) — переходы валидируются
кодом (`can_transition`/`allowed_next`), нелегальные отклоняются. Сериализуется как
строка (как `MemoryLayer`), чтобы TOON хранил её как plain-значение.

Хранение через `LocalSessionStore`: `agent-<id>.agent.toon`, `*.task.toon`,
`*.profile.toon`, `*.dialogs.toon`, индекс `agents-index.toon` (со стадией). Методы
`list_agents`, `load_agent`, `save_agent`, `delete_agent`, `load_task`/`save_task`
(синхронит стадию в индекс), `load_profile`/`save_profile`; общий atomic-io
`write_toon`/`read_toon`.

**Несколько диалогов на агента**: `DialogMeta` (id/title/timestamps), индекс
`agent-<id>.dialogs.toon`. Все чаты агента делят рабочую+долговременную память, но
у каждого своя история (`session_key = "agent:<id>"`, конкретный чат — по
`session_id`). `save_session` авто-регистрирует диалог, если ключ — `agent:*`
(`agent_id_from_key`), title — из первого user-сообщения. Методы `list_dialogs`,
`register_dialog`, `rename_dialog`, `delete_dialog` (чистит session/metrics/memory).

`ChatAgent` инъектит память в каждый provider request (`inject_memory_layers`):
`[agent:domain]`, `[memory:long-term]` (профиль + что ещё спросить),
`[memory:working]` (стадия + allowed-next + план + «не перепрыгивать стадии»),
`[invariants]` (+ прошлые нарушения как фидбэк).

`ChatAgent::stateful_postprocess()` после хода (LLM-driven, реюз паттернов
`update_sticky_facts`/классификатора): заполняет профиль из сообщения юзера
(фильтр `looks_sensitive`), предлагает стадию → валидирует FSM → применяет,
проверяет ответ против инвариантов. Возвращает `StatefulReport` (pending-вопросы,
стадия, `StageTransition`, нарушения, метрики). `build_profile_schema()` —
LLM-генерация схемы интервью из домена. Каждый шаг gated на наличии данных:
ad-hoc чат без агента не платит ничего.

## Что важно не ломать

- Provider API не знает про `agent_id`; это локальная сущность.
- Для summary source of truth - полная local history плюс sidecar summary; raw history не режется sliding-policy.
- Для sliding window source of truth - полная local history. N применяется только при сборке provider request; UI/session history не режется.
- Для sticky facts source of truth - полная local history + `AgentMemory.facts` в memory sidecar. N влияет только на provider request и включает текущий user prompt. Нельзя обрезать сохраненную историю sticky facts policy.
- Sticky facts extraction не должна превращать фразы вида “поэтому я хочу …” в `preferences=<весь prompt>`. Нужно выделять смысловой KV: `goal=...`, `location=...`, `age=...`, `appearance_hair=...`, `interests=...`, `preferences=...` только для настоящих style/preference-инструкций.
- `facts_extraction_prompt` управляет тем, какие KV-факты собирать. `facts_prompt`/Facts preamble управляет только тем, как уже сохраненный KV-блок отправляется provider в request.
- Для branching каждая ветка имеет свою историю/session; сообщения разных веток не смешиваются.
- Для scoped branches история хранится в одной session, а агент автоматически выбирает тему для каждого нового user message. Web debug должен показывать выбранную тему и счетчики сообщений по темам.
- Slash-команды должны менять локальное состояние предсказуемо и не отправлять служебный текст провайдеру.
- History, facts и memory не должны содержать токены. Чувствительные строки
  отсекаются `is_sensitive_text()` ДО роутинга, поэтому в `long-term` (и в
  profile-shared store) секреты не попадают.
- `long-term` факты переживают новую сессию, `working`/`short-term` — нет. Не
  сохраняйте working-данные в `save_long_term` и не seed-ите short-term в новую
  сессию.
- Новый чатовый сценарий должен идти через `ChatAgent`, если только это не узкий unit test.

## Где искать баг

- Ответ не учитывает нужный контекст: `agent.rs` и `memory.rs`.
- `/profile`, `/model`, `/goal` ведут себя странно: `session.rs`.
- Сессия не восстанавливается: `store.rs`.
- Summary не обновляется или prompt не применяется: `ChatAgent::precompact_before_request()`, `summary_prompt` и `AgentMemory::next_summary_range()`.
- Facts не обновились, не видны или неверно ушли в запрос: `ChatAgent::update_sticky_facts()`, `facts_extraction_prompt`, fallback `AgentMemory::update_facts_from_user_message()`, `facts_block`, `ContextDebugView.facts` и `AgentMemory::build_context()`.
- Факт ушёл не в тот слой или long-term не переносится в новую сессию: `MemoryLayer`, `default_fact_layer()`, `AgentMemory::fact_layer()`/`facts_in_layer()` и `LocalSessionStore::{save_long_term, seed_long_term}`.
- Branching в UI смешал ветки: `ui.html` branch state и `ChatWebRequest::initial_history()`.
- Scoped topics смешали context или не видны в debug: `AgentMemory.branch_assignments`, `active_branch`, `AgentMemory::build_context()` и `ContextDebugView` в web.
- Goal завершается рано/поздно: `goal.rs`.
- Метрики не печатаются или выглядят неверно: `format_request_metrics` в `mod.rs`.
- Локальная оценка токенов/overflow неверная: `token_accounting.rs` и `ChatAgent::estimate_next_exchange()`.
