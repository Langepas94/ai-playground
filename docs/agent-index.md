# Agent Index

Короткий вход для Codex/Claude. Цель - не читать лишнее.

## Правило чтения

1. `rtk ast-index search|symbol|outline` перед чтением кода.
2. Файлы больше 500 строк читать только с `outline` и точным срезом.
3. Не открывать `src/web/ui.html` целиком: искать по `id`, `function`, тексту кнопки.
4. Не открывать тесты вместе с логикой: web/chat тесты вынесены в `*_tests.rs` или `src/web/tests.rs`.
5. Для строк/HTML/текстов используй узкий `rtk rg -n "pattern" path`.

## Маршруты

| Задача | Читать |
|---|---|
| CLI dispatch/flags | `src/cli/README.md`, `src/cli/mod.rs`, `src/cli/args.rs`, нужный `src/cli/commands/*` |
| Web API handler | `src/web/README.md`, `src/web/mod.rs`, соседний helper |
| Web UI | `rtk rg -n "id|function|label" src/web/ui.html`, затем короткий `sed` slice |
| Chat runtime | `src/chat/README.md`, `src/chat/agent.rs`, `src/chat/memory.rs` |
| Chat tests | `src/chat/agent_tests.rs`, `src/chat/memory_tests.rs` |
| Provider payload/debug/billing | `src/providers/README.md`, `src/providers/openai_compatible.rs` |
| Config/secrets/errors | `src/config.rs`, `src/secrets.rs`, `src/errors.rs` |
| HTTP integration tests | `tests/http_mock.rs` |

## Большие файлы

| Файл | Как читать |
|---|---|
| `src/web/ui.html` | `rtk rg -n "function NAME|id=\"ID\"" src/web/ui.html` |
| `src/providers/openai_compatible.rs` | `rtk ast-index outline src/providers/openai_compatible.rs` |
| `tests/http_mock.rs` | `rtk rg -n "test_name|provider|endpoint" tests/http_mock.rs` |
| `src/chat/agent_tests.rs` | читать только конкретный тест |
| `src/chat/memory_tests.rs` | читать только конкретный тест |

## Команды

```bash
rtk ast-index outline src/web/mod.rs
rtk ast-index symbol ChatAgent
rtk ast-index usages ResponseControl
rtk rg -n "function chatPayload" src/web/ui.html
```

## Не дублировать

- `AGENTS.md` и `CLAUDE.md` держат только workflow и ссылки.
- Подробные карты живут в `src/*/README.md`.
- Если `ast-index` дал точный файл/символ, не перечитывай README “для уверенности”.
