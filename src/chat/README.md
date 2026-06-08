# Chat runtime

`chat/` держит локальную диалоговую логику поверх provider API: одноразовые запросы, REPL, сравнения, goal mode, history и memory.

## Файлы

- `mod.rs` - публичный API: `ask_once`, `compare_*`, `format_request_metrics`, re-export ключевых типов.
- `agent.rs` - `ChatAgent`: история, memory, сборка `ChatRequest`, вызов provider client.
- `session.rs` - интерактивный `ai chat`, slash-команды и terminal I/O.
- `goal.rs` - `ConversationGoal`, `GoalState`, stop modes и сравнение goal режимов.
- `memory.rs` - `AgentMemory`, summary и recent-window context layering.
- `store.rs` - `LocalSessionStore`: TOON-сессии, memory sidecar, индекс последней сессии.
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
  -> ChatAgent(history + memory)
  -> respond()
  -> save_session() + save_memory()
```

## Что важно не ломать

- Provider API не знает про `agent_id`; это локальная сущность.
- Полная история - источник правды; memory summary можно пересобрать.
- Slash-команды должны менять локальное состояние предсказуемо и не отправлять служебный текст провайдеру.
- History и memory не должны содержать токены.
- Новый чатовый сценарий должен идти через `ChatAgent`, если только это не узкий unit test.

## Где искать баг

- Ответ не учитывает старый контекст: `agent.rs` и `memory.rs`.
- `/profile`, `/model`, `/goal` ведут себя странно: `session.rs`.
- Сессия не восстанавливается: `store.rs`.
- Goal завершается рано/поздно: `goal.rs`.
- Метрики не печатаются или выглядят неверно: `format_request_metrics` в `mod.rs`.
