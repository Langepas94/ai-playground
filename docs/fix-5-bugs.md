# Промпт для исправления 5 багов — web UI / billing

Используй как задание для агента (Claude Code, Codex и т.п.).

---

## Промпт

Ниже 5 багов в проекте `ai-playground` (Rust CLI + Axum web UI). Весь UI встроен в `src/web.rs` как HTML-строка. Billing/timing логика — в `src/providers/openai_compatible.rs`. Типы — в `src/providers/mod.rs`.

---

### Баг 1 — При переключении модели цены сбрасываются

**Файл:** `src/web.rs`

**Корень:** Строки 1486–1487 вешают `applySelectedModelPricing` на события `change`/`input` модели. Эта функция (`~line 1326`) безусловно перезаписывает все ценовые поля из `modelPricingById`, стирая то, что пользователь вводил вручную. То же происходит при `setModelOptions` → `applySelectedModelPricing` (`~line 1421`).

**Исправление:**
- Завести `Map<modelId, userPricing>` — `userPricingOverrides`.
- При изменении любого ценового поля руками — сохранять в `userPricingOverrides` для текущей модели.
- При переключении модели: сначала смотреть `userPricingOverrides`, потом `modelPricingById`, только потом сбрасывать в пустое.
- Функцию `applySelectedModelPricing` заменить на `restorePricingForModel(modelId)`.

---

### Баг 2 — Цена не считается, если не заполнено поле `input` (DeepSeek не имеет цены за input)

**Файлы:** `src/web.rs` (~line 469), `src/providers/mod.rs` (~line 331)

**Корень:**
1. `WebPricing::into_model_pricing` использует `self.input_per_million?` — если поле пустое, вся `ModelPricing` становится `None` и расчёт не происходит совсем.
2. `ModelPricing.input_per_million` объявлен как `f64` (не `Option<f64>`), поэтому нельзя передать «нет цены».

**Исправление:**
- В `src/providers/mod.rs`: изменить `pub input_per_million: f64` на `pub input_per_million: Option<f64>`.
- В `src/web.rs` `into_model_pricing`: убрать `?` у `input_per_million`, заменить на `.unwrap_or(0.0)`. Требование оставить только на `output_per_million` (или убрать и его, если и он может быть 0).
- В `src/providers/openai_compatible.rs` `configured_cost`: обновить использование `pricing.input_per_million` — брать `unwrap_or(0.0)`.
- Итог: если `input = None/0` и `output` задан — стоимость считается только по output-токенам.

---

### Баг 3 — Время запроса показывается неправильно (~401 мс вместо ~10 с)

**Файл:** `src/providers/openai_compatible.rs`

**Корень:** Проверить два пути:
- **Путь A** (~line 79–88): `started = Instant::now()` до `send().await`, `elapsed` берётся после получения тела — должен быть корректным.
- **Путь B** (~line 124–130): аналогично, но `elapsed_ms` берётся на строке 130. Убедиться, что `started` стоит ДО `send().await`, а не после `await` на теле.

Вероятная причина 401 мс: либо `started` выставляется **после** получения первого чанка (стриминг), либо frontend JS замеряет время от получения первого SSE-события, а не от момента отправки запроса.

**Исправление:**
1. Убедиться, что в обоих путях `started = Instant::now()` стоит строго до `client.post(...).send().await` — не после.
2. Если backend стримит (SSE/chunked) — `elapsed_ms` должен измеряться от начала запроса до получения **последнего** чанка/токена, а не первого.
3. Если время меряет JS в `web.rs`: найти `Date.now()` / `performance.now()` в JS-части и убедиться, что `t0` снимается при `fetch(...)`, а `t1` — при полном завершении ответа (не при первом chunk).

После фикса: для запроса с 10-секундным ответом должно показываться ≥ 10 000 мс.

---

### Баг 4 — Показывать цену в рублях рядом с долларом

**Файл:** `src/web.rs`

**Текущий вывод** (~line 1361):
```js
`${Number(metrics.cost.amount).toFixed(8)} ${metrics.cost.currency} (${metrics.cost.source})`
```

**Исправление:**
1. Добавить в HTML-форму поле: `<input id="rubRate" type="number" value="90" step="0.01" min="0" placeholder="USD→RUB">` рядом с ценовыми полями.
2. В `setMetrics` считать `rubAmount = metrics.cost.amount * parseFloat($('rubRate').value || '0')`.
3. Если `rubRate > 0` и currency === `'USD'` — выводить: `$0.00000123 USD ≈ 0.00011 ₽ (configured-pricing)`.
4. Сохранять значение `rubRate` в `localStorage` между сессиями.

---

### Баг 5 — Корректный расчёт стоимости каждого запроса

Этот баг — следствие багов 2 и 3. После их исправления:

1. **Проверить `configured_cost`** (`openai_compatible.rs`): убедиться, что при `input_per_million = None/0` расчёт не возвращает `None`, а возвращает стоимость только по output.
2. **Проверить `request_cost`**: если провайдер возвращает `usage.cost` в ответе (provider-reported) — использовать его, иначе `configured_cost`. Логика уже есть, убедиться что `usage.prompt_tokens` не блокирует расчёт когда `input_per_million` равен 0.
3. **`into_model_pricing` в `web.rs`**: если `output_per_million` тоже пустой — возвращать `None` (нечего считать). Если задан хотя бы `output` — возвращать `Some(ModelPricing)` с `input = 0.0`.

---

## После правок

```bash
rtk cargo fmt
rtk cargo test
```

Версию поднять: `patch` в `Cargo.toml` и `crates/aiteach-compat/Cargo.toml` + `Cargo.lock`.
