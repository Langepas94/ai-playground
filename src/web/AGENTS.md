# Web модуль

## Структура

```
web/
  mod.rs    — Axum server, routes, handlers, типы запросов/ответов, тесты
  ui.html   — весь frontend (HTML + CSS + JS), подключается через include_str!
```

## API endpoints

| Метод | Путь | Описание |
|-------|------|----------|
| GET | `/` | Отдаёт `ui.html` |
| GET | `/api/providers` | Список провайдеров + ограничения параметров |
| POST | `/api/models` | Список моделей для провайдера (требует токен или keychain) |
| POST | `/api/chat` | Отправить запрос к LLM, вернуть ответ + debug + metrics |

## Как редактировать UI

Открой `src/web/ui.html` — это обычный HTML-файл с подсветкой в IDE.
После изменений пересобери проект (`rtk cargo build`) — файл вшивается через `include_str!`.

Структура JS:
- `init()` — загружает провайдеры, восстанавливает кеш
- `applyProviderDefaults()` — при смене провайдера: очищает карты, восстанавливает из localStorage
- `setModelOptions(models, selected)` — обновляет dropdown, мержит цены (не перезаписывает!)
- `applySelectedModelPricing()` — применяет цены: сначала `userPricingOverrides`, потом `modelPricingById`
- `saveCurrentPricingOverride()` — сохраняет ручные правки цен в `userPricingOverrides` + localStorage

## localStorage ключи

| Ключ | Значение |
|------|----------|
| `ai_pricing:${provider}:${modelId}` | Цены из API для модели |
| `ai_override:${provider}:${modelId}` | Ручные правки пользователя |
| `ai_models:${provider}` | Список моделей (чтобы не грузить каждый раз) |
| `ai_sel:${provider}` | Последняя выбранная модель |
| `rubRate` | Курс USD→RUB |

## Инварианты

- `WebPricing::into_model_pricing` требует `output_per_million`; `input_per_million` опционален
- Токен из формы сохраняется в keychain только если `token_provider` совпадает с текущим провайдером
- Debug-блок редактирует токен перед отображением
