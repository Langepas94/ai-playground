# Chat runtime

`chat/` держит локальную диалоговую логику поверх provider API: одноразовые запросы, REPL, сравнения, goal mode, history и стратегии контекста.

## Файлы

- `mod.rs` - публичный API: `ask_once`, `compare_*`, `format_request_metrics`, re-export ключевых типов.
- `agent.rs` - `ChatAgent`: история, context strategy, сборка `ChatRequest`, вызов provider client.
- `session.rs` - интерактивный `ai chat`, slash-команды и terminal I/O.
- `goal.rs` - `ConversationGoal`, `GoalState`, stop modes и сравнение goal режимов.
- `memory.rs` - `AgentMemory`, `MemoryConfig`, summary, sticky facts и выбор сообщений для context strategies.
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
  -> обновляет key-value facts после каждого user message
  -> хранит facts в `AgentMemory.facts` как local key-value sidecar
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

## Что важно не ломать

- Provider API не знает про `agent_id`; это локальная сущность.
- Для summary source of truth - полная local history плюс sidecar summary; raw history не режется sliding-policy.
- Для sliding window source of truth - полная local history. N применяется только при сборке provider request; UI/session history не режется.
- Для sticky facts source of truth - полная local history + `AgentMemory.facts` в memory sidecar. N влияет только на provider request и включает текущий user prompt. Нельзя обрезать сохраненную историю sticky facts policy.
- Sticky facts extraction не должна превращать фразы вида “поэтому я хочу …” в `preferences=<весь prompt>`. Нужно выделять смысловой KV: `goal=...`, `location=...`, `age=...`, `appearance_hair=...`, `interests=...`, `preferences=...` только для настоящих style/preference-инструкций.
- Для branching каждая ветка имеет свою историю/session; сообщения разных веток не смешиваются.
- Для scoped branches история хранится в одной session, а агент автоматически выбирает тему для каждого нового user message. Web debug должен показывать выбранную тему и счетчики сообщений по темам.
- Slash-команды должны менять локальное состояние предсказуемо и не отправлять служебный текст провайдеру.
- History, facts и memory не должны содержать токены.
- Новый чатовый сценарий должен идти через `ChatAgent`, если только это не узкий unit test.

## Где искать баг

- Ответ не учитывает нужный контекст: `agent.rs` и `memory.rs`.
- `/profile`, `/model`, `/goal` ведут себя странно: `session.rs`.
- Сессия не восстанавливается: `store.rs`.
- Summary не обновляется или prompt не применяется: `ChatAgent::precompact_before_request()`, `summary_prompt` и `AgentMemory::next_summary_range()`.
- Facts не обновились, не видны или неверно ушли в запрос: `AgentMemory::update_facts_from_user_message()`, `facts_block`, `ContextDebugView.facts` и `AgentMemory::build_context()`.
- Branching в UI смешал ветки: `ui.html` branch state и `ChatWebRequest::initial_history()`.
- Scoped topics смешали context или не видны в debug: `AgentMemory.branch_assignments`, `active_branch`, `AgentMemory::build_context()` и `ContextDebugView` в web.
- Goal завершается рано/поздно: `goal.rs`.
- Метрики не печатаются или выглядят неверно: `format_request_metrics` в `mod.rs`.
- Локальная оценка токенов/overflow неверная: `token_accounting.rs` и `ChatAgent::estimate_next_exchange()`.
