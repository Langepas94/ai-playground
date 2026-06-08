# Agent runtime and memory

Этот модуль реализует локальный runtime агента. Его нельзя называть provider API или
бекендом поставщика: это наш слой, который живет внутри приложения, выбирает локального
агента, управляет историей, собирает контекст и только после этого вызывает API
оператора/провайдера.

## Что здесь реализовано

В `src/chat` сейчас есть базовый локальный чат-агент с накоплением истории и слоистой
памятью:

1. Пользовательский интерфейс CLI или Web принимает сообщение.
2. Локальный agent runtime выбирает агента по `agent_id`.
3. Runtime загружает локальную сессию из `LocalSessionStore`.
4. Runtime загружает локальную память сессии `AgentMemory`.
5. `ChatAgent` собирает управляемый контекст для provider API.
6. Provider API получает не всю сырую историю, а слои контекста.
7. После ответа `ChatAgent` сохраняет новый turn в полную локальную историю.
8. Если история стала длинной, агент обновляет summary старой части диалога.
9. Runtime сохраняет полную историю и memory sidecar локально.

## Основные сущности

### `ChatAgent`

Файл: `agent.rs`.

`ChatAgent` - собственная локальная сущность агента. Он инкапсулирует:

- профиль провайдера и модель;
- токен для вызова API;
- полную локальную историю сессии;
- `AgentMemory`;
- настройки ответа `ResponseControl`;
- pricing/billing context;
- сборку `ChatRequest`;
- commit ответа в историю;
- best-effort обновление memory summary.

Новый чатовый код должен идти через `ChatAgent`, а не собирать `ChatRequest`
напрямую. Иначе легко обойти memory layer и снова получить one-shot или raw-history
поведение.

### `AgentMemory`

Файл: `memory.rs`.

`AgentMemory` хранит:

- `session_summary` - summary старой части текущей локальной сессии;
- `summarized_message_count` - сколько сообщений уже вошло в summary.

Это не vector memory и не долговременная память пользователя. Это первый слой
сессионной memory compression.

### `MemoryConfig`

Файл: `memory.rs`.

Текущие значения по умолчанию:

- `recent_messages = 12`;
- `summarize_after_messages = 18`.

Это означает:

- последние 12 сообщений остаются в живом окне;
- когда история достигает 18 сообщений, более старая часть может быть сжата в summary.

### `LocalSessionStore`

Файл: `store.rs`.

`LocalSessionStore` хранит локальную историю и память сессии:

- полная история сообщений: `<session_id>.toon`;
- memory sidecar: `<session_id>.memory.toon`;
- индекс последней сессии по ключу профиля/агента/модели.

Полная история остается источником правды. Summary - это оптимизированный слой для
сборки контекста, а не замена истории.

## Как собирается контекст

Перед вызовом provider API `ChatAgent` вызывает:

```rust
AgentMemory::build_context(history, memory_config)
```

Контекст собирается слоями:

```text
system prompt
+ memory summary
+ recent messages window
+ new user prompt
```

Provider API не получает автоматически всю локальную историю. Он получает только
управляемый контекст текущего запроса.

## Как обновляется summary

После успешного ответа пользователя `ChatAgent`:

1. Добавляет `user` и `assistant` сообщения в полную историю.
2. Проверяет `AgentMemory::next_summary_range(...)`.
3. Если есть старая часть истории за пределами recent window, отправляет отдельный
   compacting-запрос в provider API.
4. Полученный текст сохраняет в `AgentMemory.session_summary`.
5. Обновляет `summarized_message_count`.

Обновление summary сделано best-effort. Если compacting-запрос упал, основной ответ
пользователя не ломается, а память остается прежней.

## Что хранится локально

Локально сохраняется:

- полная история `ChatMessage`;
- summary текущей сессии;
- счетчик уже сжатых сообщений;
- индекс последней сессии.

Токены не пишутся в историю или memory-файлы.

## Что отправляется провайдеру

Для обычного ответа провайдер получает:

- system prompt, если он есть;
- memory summary как system message, если она уже создана;
- последние сообщения из recent window;
- текущий user prompt;
- control/pricing/billing параметры.

Для memory compression провайдер получает отдельный запрос:

- инструкцию compacting module;
- предыдущее summary;
- новый фрагмент старой истории;
- просьбу вернуть обновленное summary.

## CLI и Web

CLI и Web используют одну и ту же memory-логику.

CLI:

- `session.rs` загружает `LocalSessionStore`;
- загружает `AgentMemory`;
- создает `ChatAgent`;
- после ответа сохраняет history и memory.

Web:

- `web/mod.rs` выбирает локального агента по `agent_id`;
- загружает session и memory;
- создает `ChatAgent`;
- после ответа сохраняет history и memory.

Web routes `/api/agent/chat` и `/api/agent/session` - это routes локального runtime,
а не routes провайдера. Провайдер вызывается только внутри `ProviderClient`.

## Что еще не реализовано

Сейчас нет:

- vector embeddings;
- semantic retrieval;
- topic branches;
- долговременной памяти пользователя между разными сессиями;
- rule-based memory;
- self-reflective memory;
- UI для просмотра/edit memory summary.

Текущая стратегия - слоистая сессионная память: full local history + compressed
session summary + recent window.

## Инварианты

- Агент - наша локальная сущность.
- Provider API не знает про `agent_id`.
- Полная история хранится локально и не должна теряться при compression.
- В provider API нельзя отправлять всю историю без необходимости.
- Summary не является источником правды.
- Memory compression не должна ломать основной ответ пользователя.
- CLI и Web должны использовать один и тот же `ChatAgent` и `LocalSessionStore`.
