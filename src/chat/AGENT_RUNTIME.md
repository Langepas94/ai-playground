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
6. Provider API получает выбранный context: summary+окно, sliding window, facts+окно или branch window.
7. После ответа `ChatAgent` сохраняет новый turn.
8. Агент применяет выбранную strategy: summary, sliding window, sticky facts или branching.
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
- `session_summary` и `summarized_message_count` для summary strategy;
- branch labels для scoped branches.

Это не vector memory и не долговременная память пользователя. Facts относятся к
одной локальной сессии или ветке.

### `MemoryConfig`

Файл: `memory.rs`.

Основные параметры:

- `strategy` - `summary`, `sliding-window`, `sticky-facts`, `branching` или `scoped-branches`;
- `recent_messages` - размер окна N для обычных сообщений;
- `summarize_after_messages`, `summary_chunk_messages`, `summarize_at_context_percent` - пороги summary compaction;
- `summary_prompt` - system prompt отдельного summary-запроса;
- `facts_prompt` - system prompt, который вводит блок facts для provider request.
- `active_branch` - id темы, которую агент выбрал для текущего хода в `scoped-branches`.

UI должен показывать только релевантные настройки выбранной strategy. Summary
controls видны только для `summary`, facts prompt - только для `sticky-facts`,
topic switcher/debug - только для `scoped-branches`.

### `LocalSessionStore`

Файл: `store.rs`.

`LocalSessionStore` хранит локальную историю и context state:

- история сообщений: `<session_id>.toon`;
- context sidecar: `<session_id>.memory.toon`;
- индекс последней сессии по ключу профиля/агента/модели.

Для branching каждая ветка использует отдельную local session. Checkpoint - это
снимок сообщений, из которого UI создает две независимые ветки.

## Стратегии

### Summary

```text
system prompt
+ memory summary system message
+ last N raw non-system messages
+ new user prompt
```

Перед обычным ответом агент может сделать отдельный summary-запрос к тому же
provider client. Запрос использует `summary_prompt` как system prompt и получает
предыдущий summary плюс очередной фрагмент старой истории. Raw history не
обрезается этой strategy: summary - это компактный context layer, а не потеря
локального source of truth.

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
+ read-only facts block built from local key-value memory
+ last N non-system messages, including current user prompt
```

Facts обновляются после каждого user message и хранятся локально в
`AgentMemory.facts` как key-value sidecar (`<session_id>.memory.toon`). Это не
summary и не raw history. В provider request отправляется отдельный facts block
с уже сохраненными KV facts плюс последние N raw сообщений, где текущий user
prompt входит в N. Web debug обязан
показывать:

- persisted KV facts;
- точный facts block, который попал в provider request;
- сколько raw messages отправлено вместе с facts.

Хранятся устойчивые данные:

- цель;
- ограничения;
- предпочтения;
- решения;
- договоренности.

### Branching

UI сохраняет checkpoint текущей истории, создает две ветки от одного места и
дальше отправляет каждую ветку как отдельную local session. Runtime получает уже
выбранную ветку как обычную историю, поэтому сообщения веток не смешиваются.

### Scoped Branches

```text
system prompt
+ facts system block, если есть
+ last N messages from active internal branch
+ new user prompt
```

Это optional strategy для одного агентского окна и одной local session. Юзер
ничего вручную не переключает: runtime перед provider request сам выбирает тему
для нового user message по overlap ключевых слов с уже размеченными сообщениями
или создает новую тему. Затем runtime помечает turn этой topic/branch label в
`AgentMemory.branch_assignments`. Context builder отбрасывает сообщения других
labels при сборке request. Debug view должен показывать выбранную тему и счетчик
сообщений по темам, чтобы было видно, что context не смешивается.

## Что хранится локально

Локально сохраняется:

- история текущей session/branch;
- summary текущей session/branch;
- facts текущей session/branch;
- internal topic/branch labels для scoped branches;
- индекс последней сессии.

Токены не пишутся в историю или memory-файлы.

## Что отправляется провайдеру

Для обычного ответа провайдер получает:

- system prompt, если он есть;
- summary как system message, если выбрана `summary` и summary уже есть;
- facts как отдельный read-only context message, если выбрана sticky facts и facts не пустые;
- выбранное окно raw messages или active internal branch window;
- текущий user prompt;
- control/pricing/billing параметры.

Отдельный summary-запрос выполняется только для strategy `summary`. Настраиваемый
`facts_prompt` влияет только на system block перед facts в основном запросе, а
`summary_prompt` влияет только на compaction-запрос.

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
- Strategy всегда явная: summary, sliding window, sticky facts или branching.
- Scoped branches/topics не должны смешивать сообщения разных internal labels.
- В provider API нельзя отправлять всю историю без выбранной strategy.
- Facts не являются глобальной пользовательской памятью.
- Branch histories не должны смешиваться.
- CLI и Web должны использовать один и тот же `ChatAgent` и `LocalSessionStore`.
