# Chat модуль

## Структура

```
chat/
  AGENT_RUNTIME.md — подробная документация локального agent runtime и context strategies
  agent.rs    — ChatAgent: локальный агент, история, context strategy, сборка ChatRequest
  memory.rs   — AgentMemory, MemoryConfig, sliding window + sticky facts context
  mod.rs       — публичный API: ask_once, compare_*, compare_goal_stop, format_request_metrics
  goal.rs      — ConversationGoal, GoalState, ConversationStopMode, GoalRun/GoalComparison
  session.rs   — interactive_chat (REPL), describe_control/goal, terminal I/O
  store.rs     — LocalSessionStore: локальные TOON-сессии, context sidecar, индекс последней сессии
  history.rs   — save_history (сохранение в файл)
```

## Публичный API (re-exported из mod.rs)

```rust
// Один запрос
ask_once(client, secrets, config, profile_name, profile, prompt, control, pricing, billing)

// Сессионный агент
ChatAgent::new(...).respond(client, prompt)

// Сравнение без/с контролем
compare_response_control(...)

// Сравнение трёх режимов остановки goal
compare_goal_stop(...)

// Интерактивный чат в терминале
interactive_chat(...)

// Форматирование метрик для CLI вывода
format_request_metrics(metrics)
```

## Система ConversationGoal

Goal — механизм сбора структурированных данных через диалог.

- `ConversationGoal { required_fields, mode }` — что собираем и когда останавливаться
- `GoalState` — текущее заполнение полей
- Режимы остановки:
  - `State` — все поля заполнены по оценке парсера
  - `Instruction` — модель вернула `"done": true`
  - `Combined` — оба условия одновременно

`apply_to_control(control)` инжектирует system messages с JSON-схемой в `ResponseControl`.

## Локальный agent runtime

Подробно описан в `AGENT_RUNTIME.md`.

Термины:

- `agent runtime` — наш локальный слой, который выбирает агента, загружает историю,
  управляет context strategy и вызывает provider API.
- `provider API` — внешний API оператора/провайдера модели.
- `backend` в описании этого модуля не использовать: в архитектуре проекта это
  сбивает смысл, потому что агент работает локально и напрямую вызывает API
  операторов через `ProviderClient`.

Текущий агент:

- выбирается по локальному `agent_id`;
- хранит полную историю локально;
- хранит `AgentMemory` отдельно от истории;
- отправляет провайдеру layered context, а не всю историю подряд;
- после ответа применяет выбранную context strategy и обновляет sticky facts.

Контекст provider API собирается так:

```text
system prompt
+ facts block / selected history window
+ new user prompt
```

`AgentMemory` сейчас является сессионной context state, а не vector memory и
не долговременной памятью пользователя.

## /commands в интерактивном чате

Все `/`-команды живут в `session.rs` в `interactive_chat()`.
Чтобы добавить команду — добавь match-ветку в loop.

## Инварианты

- Токен резолвится через `get_config_profile_token` — общий путь для CLI и сессии
- History и memory не содержат токенов
- Новый чатовый код должен идти через `ChatAgent`, а не собирать `ChatRequest` напрямую
- История диалога должна сохраняться локально через `LocalSessionStore`
- Context state должен сохраняться как sidecar через `LocalSessionStore`
- Context strategy явная: sliding window, sticky facts или branching; summary не основной путь
- Провайдеру отправляется только контекст текущего запроса, собранный agent runtime
- Provider API не знает про `agent_id`; агент — локальная сущность проекта
