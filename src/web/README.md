# Web

`web/` - локальный Axum server и вшитый single-file UI. Backend использует те же config, secrets, chat и provider слои, что CLI.

## Файлы

- `mod.rs` - Axum server, routes, handlers, request/response DTO и tests.
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

`/api/agent/*` сохранены как совместимые aliases для chat session/chat.

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
- `Sticky Facts` показывает размер окна N, facts preamble, persisted KV facts и точный facts block из provider request. Пользователь должен видеть, что хранится в memory sidecar и что уйдет модели.
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
- Sticky facts не видно или неясно, что отправлено: `ContextDebugView.facts`, `updateFactsPreview()` и `AgentMemory::facts_block()`.
- Branch switch потерял сообщения: branch state в `ui.html`, `session_id/new_session/messages` payload.

## Инварианты UI

- После изменения `ui.html` нужен `rtk cargo build`, потому что файл вшивается в бинарник.
- Токен из формы можно сохранять только для текущего provider.
- Debug view должен редактировать секреты.
- Web API не должен обходить `ProviderClient` или `ChatAgent`.
- UI разделяет метрики последнего запроса и накопленные метрики диалога.
- UI не должен показывать нерелевантные controls: summary controls только для `Summary`, facts prompt только для `Sticky Facts`, auto-topic debug только для `Scoped Branches`.
