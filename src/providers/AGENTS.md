# Providers модуль

## Структура

```
providers/
  mod.rs               — ProviderKind, ProviderSpec, trait ProviderClient,
                         ReqwestProviderClient, все публичные типы
  openai_compatible.rs — Chat Completions impl (используется всеми провайдерами)
  openrouter.rs        — spec + X-Title header
  deepseek.rs          — spec
  gigachat.rs          — spec + OAuth bearer token + кастомный CA bundle
  kimi.rs              — spec (Moonshot)
```

## Как добавить провайдера

1. `src/providers/<name>.rs` — реализовать `pub fn spec() -> ProviderSpec`.
2. `src/providers/mod.rs` — добавить вариант в `ProviderKind`, `all()`, `Display`, `FromStr`, `spec()`.
3. Тест в `tests/http_mock.rs` по образцу существующих.

## Ключевые типы

```rust
ModelPricing {
    currency: String,
    input_per_million: Option<f64>,  // None = провайдер не берёт за input
    output_per_million: f64,
    cache_hit_input_per_million: Option<f64>,
    cache_miss_input_per_million: Option<f64>,
}

ChatRequest { model, messages, control: ResponseControl, pricing, billing }
ChatResponse { text, finish_reason, metrics: RequestMetrics }
RequestMetrics { elapsed_ms, usage: Option<TokenUsage>, cost: Option<RequestCost> }
```

## Поток запроса

```
ReqwestProviderClient::chat_completion()
  → bearer_token()          # GigaChat: OAuth; остальные: pass-through
  → openai_compatible::chat_completion()
      → authorized().json(payload).send().await
      → response.text().await     # elapsed_ms снимается после этого
      → parse_chat_response()     # парсинг + расчёт стоимости
      → apply_billing_cost()      # опционально: запрос billing API
```

## Расчёт стоимости

Приоритет источников:
1. `usage.cost` в ответе провайдера → `CostSource::ProviderReported`
2. Configured pricing из `ModelPricing` → `CostSource::ConfiguredPricing`
3. Billing API (OpenAI `/organization/costs`) → `CostSource::BillingApi`

Если `input_per_million = None` → считается только output стоимость (не ошибка).

## Timeout

HTTP timeout = 300 сек (DeepSeek-R1 reasoning может занимать >60 сек).
Задаётся в `ReqwestProviderClient::new()` в `mod.rs`.
