# Release 1.0.1

Patch release для web UI.

## Исправления

- `max_tokens` и `max_completion_tokens` больше не заполняются значением `1024` по умолчанию.
- Если пользователь не ввел лимит вручную, UI отправляет `null`, а backend не включает token limit в provider payload.
- Composer стал компактнее: `System prompt` перенесен во вкладку `Параметры`.
- Header диалога стал компактнее, поэтому transcript получает больше высоты.
- Скриншоты web UI обновлены.
