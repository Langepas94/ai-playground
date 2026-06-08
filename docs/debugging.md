# Маршруты расследования багов

Этот файл нужен, когда баг уже есть, а неизвестно, куда копать.

## CLI команда падает

1. Найди команду в `src/cli/args.rs`.
2. Найди handler в `src/cli/commands/`.
3. Проверь dispatch в `src/cli/mod.rs`.
4. Если ошибка про profile/config - иди в `src/config.rs`.
5. Если ошибка про token/keychain - иди в `src/secrets.rs`.
6. Если ошибка пришла от provider - иди в `src/providers/openai_compatible.rs`.

## Web UI отвечает не так

1. Повтори запрос через UI и открой debug JSON.
2. Найди endpoint в `src/web/mod.rs`.
3. Для состояния сессии проверь `src/chat/store.rs`.
4. Для prompt/history/memory проверь `src/chat/agent.rs` и `src/chat/memory.rs`.
5. Для HTML/localStorage проверь `src/web/ui.html`.

## Provider вернул неожиданный ответ

1. Посмотри `HttpDebugRequest`/`HttpDebugResponse` в debug output.
2. Проверь payload assembly в `src/providers/openai_compatible.rs`.
3. Проверь provider spec в `src/providers/<provider>.rs`.
4. Добавь или обнови wiremock-сценарий в `tests/http_mock.rs`.

## Токен не находится

1. Проверь `profile.token_ref` в config, но не ожидай увидеть сам токен.
2. Проверь `profile_token_refs()` в `src/secrets.rs`.
3. Проверь fallback из legacy service/profile-scoped token.
4. Проверь, что web передает `token_provider` только для текущего provider.

## Сессия или memory странные

1. Проверь `LocalSessionStore` в `src/chat/store.rs`.
2. Убедись, что полная история сохранена, а summary только sidecar.
3. Проверь сборку контекста в `ChatAgent`.
4. Если баг только в terminal REPL - проверь slash-команды в `src/chat/session.rs`.

## Перед фиксом

- Сначала локализуй слой: CLI, web, chat, providers, config или secrets.
- Не исправляй provider-логику в UI/CLI.
- Не печатай токены даже временно.
- Если меняешь Rust-код, обязательно запусти:

```bash
rtk cargo build 2>&1 | head -30
rtk cargo fmt
rtk cargo test
```
