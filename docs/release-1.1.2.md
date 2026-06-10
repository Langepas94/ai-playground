# Release 1.1.2

## GigaChat auth and usage

- GigaChat Authorization key теперь меняется на OAuth access token с in-memory cache по `expires_at`.
- При `401 Unauthorized` GigaChat access token принудительно обновляется и запрос повторяется один раз.
- Для GigaChat response usage закреплен подсчет `prompt_tokens`, `completion_tokens`, `total_tokens`.

Использование: сохраните в профиле `gigachat` Authorization key из личного кабинета, не временный access token.
