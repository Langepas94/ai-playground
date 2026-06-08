# Web

`web/` - локальный Axum server и вшитый single-file UI. Backend использует те же config, secrets, chat и provider слои, что CLI.

## Файлы

- `mod.rs` - routes, handlers, DTO, web error mapping, tests.
- `ui.html` - HTML/CSS/JS целиком; подключается через `include_str!("ui.html")`.

## Routes

| Метод | Путь | Назначение |
| --- | --- | --- |
| `GET` | `/` | Отдать UI |
| `GET` | `/api/agents` | Список локальных агентов |
| `GET` | `/api/providers` | Список providers и ограничения параметров |
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
  -> ChatAgent::respond_with_debug()
  -> save session + memory
  -> ChatWebResponse
```

## Где искать баг

- UI не видит provider/model defaults: `providers()` handler и `applyProviderDefaults()` в `ui.html`.
- Токен из web не используется: `resolve_web_token()`.
- Сессия web не продолжается: `chat_session()`, `chat()`, `LocalSessionStore`.
- Debug JSON странный: `ChatDebugView` и provider debug в `ChatAgent`.
- Ошибка приходит без понятного текста: `WebError::into_response()`.

## Инварианты UI

- После изменения `ui.html` нужен `rtk cargo build`, потому что файл вшивается в бинарник.
- Токен из формы можно сохранять только для текущего provider.
- Debug view должен редактировать секреты.
- Web API не должен обходить `ProviderClient` или `ChatAgent`.
