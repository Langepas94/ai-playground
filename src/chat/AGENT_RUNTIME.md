# Agent runtime and context

Этот модуль реализует локальный runtime агента. Его нельзя называть provider API
или бекендом поставщика: это наш слой внутри приложения, который выбирает
локального агента, управляет историей, собирает контекст и только после этого
вызывает API оператора/провайдера.

## Что здесь реализовано

В `src/chat` есть локальный чат-агент с накоплением истории и явными стратегиями
контекста:

1. Пользовательский интерфейс CLI или Web принимает сообщение.
2. Локальный agent runtime выбирает агента по `agent_id`.
3. Runtime загружает локальную сессию из `LocalSessionStore`.
4. Runtime загружает context state сессии `AgentMemory`.
5. `ChatAgent` собирает управляемый контекст для provider API.
6. Provider API получает выбранное окно контекста, а не скрытую summary-подмену.
7. После ответа `ChatAgent` сохраняет новый turn.
8. Агент применяет выбранную strategy: sliding window, sticky facts или branching.
9. Runtime сохраняет историю ветки и context sidecar локально.

## Основные сущности

### `ChatAgent`

Файл: `agent.rs`.

`ChatAgent` - собственная локальная сущность агента. Он инкапсулирует:

- профиль провайдера и модель;
- токен для вызова API;
- локальную историю текущей сессии или ветки;
- `AgentMemory`;
- настройки ответа `ResponseControl`;
- pricing/billing context;
- сборку `ChatRequest`;
- commit ответа в историю;
- применение context strategy.

Новый чатовый код должен идти через `ChatAgent`, а не собирать `ChatRequest`
напрямую. Иначе легко обойти context layer и снова получить one-shot или
raw-history поведение.

### `AgentMemory`

Файл: `memory.rs`.

`AgentMemory` хранит:

- `facts` - key-value факты текущей сессии;
- legacy-поля для чтения старых sidecar-файлов.

Это не vector memory и не долговременная память пользователя. Facts относятся к
одной локальной сессии или ветке.

### `MemoryConfig`

Файл: `memory.rs`.

Основные параметры:

- `strategy` - `sliding-window`, `sticky-facts` или `branching`;
- `recent_messages` - размер окна N для обычных сообщений;
- `facts_prompt` - system prompt, который вводит блок facts для provider request.

UI должен показывать минимум настроек: strategy и N. Summary controls для этой
модели не нужны.

### `LocalSessionStore`

Файл: `store.rs`.

`LocalSessionStore` хранит локальную историю и context state:

- история сообщений: `<session_id>.toon`;
- context sidecar: `<session_id>.memory.toon`;
- индекс последней сессии по ключу профиля/агента/модели.

Для branching каждая ветка использует отдельную local session. Checkpoint - это
снимок сообщений, из которого UI создает две независимые ветки.

## Стратегии

### Sliding Window

```text
system prompt
+ last N non-system messages
+ new user prompt
```

После ответа агент сохраняет только system messages и последние N обычных
сообщений. Старое отбрасывается намеренно.

### Sticky Facts

```text
system prompt
+ facts system block with configurable prompt
+ last N non-system messages
+ new user prompt
```

Facts обновляются после каждого user message. Хранятся устойчивые данные:

- цель;
- ограничения;
- предпочтения;
- решения;
- договоренности.

### Branching

UI сохраняет checkpoint текущей истории, создает две ветки от одного места и
дальше отправляет каждую ветку как отдельную local session. Runtime получает уже
выбранную ветку как обычную историю, поэтому сообщения веток не смешиваются.

## Что хранится локально

Локально сохраняется:

- история текущей session/branch;
- facts текущей session/branch;
- индекс последней сессии.

Токены не пишутся в историю или memory-файлы.

## Что отправляется провайдеру

Для обычного ответа провайдер получает:

- system prompt, если он есть;
- facts как system message, если выбрана sticky facts и facts не пустые;
- выбранное окно raw messages;
- текущий user prompt;
- control/pricing/billing параметры.

Для новых context strategies нет отдельного summary-запроса. Настраиваемый
`facts_prompt` влияет только на system block перед facts в основном запросе.

## CLI и Web

CLI и Web используют одну и ту же context-логику.

CLI:

- `session.rs` загружает `LocalSessionStore`;
- загружает `AgentMemory`;
- создает `ChatAgent`;
- после ответа сохраняет history и context state.

Web:

- `web/mod.rs` выбирает локального агента по `agent_id`;
- загружает session и context state;
- создает `ChatAgent`;
- после ответа сохраняет history и context state.

Web routes `/api/agent/chat` и `/api/agent/session` - это routes локального
runtime, а не routes провайдера. Провайдер вызывается только внутри
`ProviderClient`.

## Что еще не реализовано

Сейчас нет:

- vector embeddings;
- semantic retrieval;
- долговременной памяти пользователя между разными сессиями;
- self-reflective memory.

Текущая стратегия - явный request-time context builder:

> branch/session history + local context sidecar + selected strategy.

## Инварианты

- Агент - наша локальная сущность.
- Provider API не знает про `agent_id`.
- Strategy всегда явная: sliding window, sticky facts или branching.
- В provider API нельзя отправлять всю историю без выбранной strategy.
- Facts не являются глобальной пользовательской памятью.
- Branch histories не должны смешиваться.
- CLI и Web должны использовать один и тот же `ChatAgent` и `LocalSessionStore`.
