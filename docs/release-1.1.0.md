# Release 1.1.0

## Token accounting demo

- Добавлен локальный модуль `chat::token_accounting` для оценки токенов текущего запроса, всей истории и ответа модели.
- Добавлена CLI-команда `ai token-demo`, которая сравнивает короткий, длинный и превышающий context limit диалог.
- Демо показывает рост request/history/response/total tokens, накопленных токенов, примерной стоимости и момент overflow.

Пример:

```bash
rtk cargo run --bin ai -- token-demo --context-limit 4096
```
