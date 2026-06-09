# Release 1.0.0

Major release: web UI теперь построен вокруг большого диалога и правого inspector.

## Главное

- Новый layout `Chat + Inspector`.
- Ввод запроса находится внизу основной области и не теряется при работе с диалогом.
- Настройки вынесены во вкладки `Профиль`, `Параметры`, `Метрики`, `Debug`.
- Метрики явно разделены на последний запрос и текущий диалог.
- Usage breakdown показывает prompt/completion/reasoning/cache/visible output, если provider возвращает эти поля.
- Web token можно сохранить отдельной кнопкой `Сохранить токен`.
- Secret store использует keychain и локальный TOON fallback для случаев, когда keychain недоступен.
- Session metrics снова пишутся в TOON; старый JSON читается как legacy fallback.

## Документация

- How-to: `docs/web-ui.md`.
- Скриншоты: `docs/assets/web-chat-inspector-profile.png`, `docs/assets/web-chat-inspector-metrics.png`.
