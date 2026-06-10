# Providers

`providers/` изолирует внешний HTTP-мир. CLI, web и chat не должны знать детали конкретного API кроме `ProviderKind` и общих типов.

## Файлы

- `mod.rs` - публичные типы, `ProviderKind`, `ProviderSpec`, `ProviderClient`, `ReqwestProviderClient`.
- `openai_compatible.rs` - основной Chat Completions/list models/billing/debug implementation.
- `openrouter.rs` - spec и OpenRouter overrides.
- `deepseek.rs` - DeepSeek spec.
- `gigachat.rs` - GigaChat spec, OAuth bearer token cache/refresh и custom CA bundle.
- `kimi.rs` - Kimi/Moonshot spec.

## Поток HTTP-запроса

```text
ReqwestProviderClient::chat_completion()
  -> provider bearer token / overrides
  -> openai_compatible::chat_completion()
  -> build JSON payload
  -> send request
  -> parse response
  -> calculate metrics/cost
```

## Где менять

- Новый provider: новый файл spec + `ProviderKind` в `mod.rs` + тест в `tests/http_mock.rs`.
- Новый response parameter: `ResponseControl`/`ChatRequest` в `mod.rs`, payload в `openai_compatible.rs`.
- Новый cost источник: `RequestMetrics`, `RequestCost`, billing helpers в `openai_compatible.rs`.
- Provider-specific auth/header/base URL: spec или override рядом с provider-файлом.

## GigaChat auth

Для профиля `gigachat` в secret store сохраняется Authorization key из личного кабинета GigaChat API, а не временный access token. `ReqwestProviderClient` меняет его на OAuth access token через `/api/v2/oauth`, кэширует access token до `expires_at` и обновляет его заранее или после `401 Unauthorized`.

Scope по умолчанию: `GIGACHAT_API_PERS`. Для B2B/CORP можно задать `AI_PLAYGROUND_GIGACHAT_SCOPE` или legacy `AITEACH_GIGACHAT_SCOPE`.

## Инварианты

- HTTP timeout сейчас 300 секунд; reasoning-модели могут отвечать долго.
- `validate_base_url` должен защищать CLI/web от мусорных URL до HTTP-запроса.
- Provider error должен вернуться как `AppError`, без потери status/body context.
- Debug request/response не должен раскрывать полный токен.
- OpenAI-compatible payload собирается централизованно, без копипасты в CLI/web.
