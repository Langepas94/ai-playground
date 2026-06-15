# Продуктовая документация

## Web UI: текущее поведение

### Основной layout

- Слева находится диалог: история, composer, `📎`, `Отправить`, `Стоп`, `Новая сессия` и переключатель `Стримить по чанкам`.
- Справа находится inspector с вкладками `Профиль`, `Параметры`, `Метрики`, `Debug`.
- `Профиль` показывает agent mode, provider, model, badge контекстного окна, token status, `Base URL`, `Custom model id`.
- `Параметры` содержит `system_prompt`, memory strategy, response controls, pricing overrides, Billing API и extra API parameters.

### Сессии

- На старте и при смене provider/model UI запрашивает `/api/agent/session` или alias `/api/chat/session`.
- `session_id` кешируется в `localStorage`; серверная часть хранит сообщения, memory state и накопленные metrics в `LocalSessionStore`.
- `Новая сессия` сбрасывает текущую session, но не трогает сохраненный токен.

### Вложения

- Web UI читает текстовые файлы на фронте через `FileReader` и держит их в `pendingAttachments`.
- При отправке они уходят в JSON как `{name, content}` и на бэке склеиваются в prompt через `build_web_prompt()`.
- Body limit для web routes: 50 MB.

### Стриминг и полный ответ

- Если `Стримить по чанкам` включен, UI использует `POST /api/agent/chat/stream` и допечатывает ответ по SSE `event: token`.
- Если тумблер выключен, UI использует `POST /api/agent/chat` и показывает временный bubble с анимацией ожидания.
- В обоих режимах после завершения UI обновляет `messages`, `metrics`, `context_metrics`, `session_metrics`, `debug` и `context_debug`.
- `Стоп` отменяет активный запрос через `AbortController`.

## Контекст и память

### Доступные стратегии

- `Summary` - summary + последние N raw сообщений, с preflight-сжатием при давлении на context window.
- `Sliding Window` - только последние N raw сообщений.
- `Sticky Facts` - локальные KV-факты, извлекаемые из пользовательских сообщений и отправляемые как read-only facts block.
- `Branching` - ручной checkpoint с двумя ветками `A/B`.
- `Scoped Branches` - темы внутри одной session; маршрутизация может быть auto или manual.

### Правила UI

- UI показывает только controls, относящиеся к выбранной strategy.
- Блок `Debug -> Context topics` всегда доступен, но заполняется детально только для `Scoped Branches`.
- Для `Sticky Facts` UI показывает persisted facts и точный provider facts block из последнего debug.

## Метрики и цена

### Что показывает UI

- `Время запроса`, `Токены запроса`, `Стоимость запроса` - только основной последний ответ.
- `Контекст` - отдельный служебный context step, если он выполнялся; иначе человекочитаемый status стратегии.
- `Токены диалога` - накопление за всю локальную session.

### Breakdown usage

Если provider прислал детализированный usage, UI выводит:

- `prompt`
- `completion`
- `total`
- `prompt cached`
- `prompt uncached`
- `prompt audio`
- `visible output`
- `reasoning`
- `output audio`
- `accepted prediction`
- `rejected prediction`

### Источники цены

- Стоимость берется из provider response и локального pricing catalog/override.
- Для USD UI умеет показать приблизительный пересчет в RUB по локально сохраненному `rubRate`.
- Session cost считается как сумма через `add_request_metrics()`.

## Debug и диагностика

- `Debug` показывает raw provider request и raw provider response.
- Authorization должен оставаться отредактированным.
- Вкладка также содержит ссылку на документацию выбранного provider и debug по facts/topics.
- При HTTP-ошибке или ошибке стрима debug всё равно должен сохранить полезный контекст для расследования.

## API и хранение

### Основные web routes

- `GET /api/agents`
- `GET /api/providers`
- `POST /api/token/status`
- `POST /api/token/save`
- `POST /api/models`
- `POST /api/agent/session` и alias `POST /api/chat/session`
- `POST /api/agent/chat`
- `POST /api/agent/chat/stream`

### Local persistence

- Session messages и memory sidecars живут в `LocalSessionStore`.
- Metrics пишутся в `.metrics.toon`.
- Legacy `.metrics.json` читается как fallback, но новые записи делаются в TOON.
- API токены не пишутся в config, history, memory или debug output.
