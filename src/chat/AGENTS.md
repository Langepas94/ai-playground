# Chat модуль

## Структура

```
chat/
  mod.rs       — публичный API: ask_once, compare_*, compare_goal_stop, format_request_metrics
  goal.rs      — ConversationGoal, GoalState, ConversationStopMode, GoalRun/GoalComparison
  session.rs   — interactive_chat (REPL), describe_control/goal, terminal I/O
  history.rs   — save_history (сохранение в файл)
```

## Публичный API (re-exported из mod.rs)

```rust
// Один запрос
ask_once(client, secrets, config, profile_name, profile, prompt, control, pricing, billing)

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

## /commands в интерактивном чате

Все `/`-команды живут в `session.rs` в `interactive_chat()`.
Чтобы добавить команду — добавь match-ветку в loop.

## Инварианты

- Токен резолвится через `get_config_profile_token` — общий путь для CLI и сессии
- History не содержит токенов (сохраняются только `ChatMessage`)
