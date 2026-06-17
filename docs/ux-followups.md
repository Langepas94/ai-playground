# UX Demo Notes

## Agent demo flow issues

- `Новая сессия` actually creates a new dialog/chat inside the active agent. This
  branch renames the visible control to `Новый чат`.
- Saved agents appeared in the list with no selected option, so clicking `Войти`
  could appear to do nothing. This branch selects the first available agent when
  there is no active-agent match.
- User profiles could be created and selected, but the visible profile pane had
  no delete action. This branch adds `Удалить профиль` and clears stale bindings.
- Choosing `— без профиля —` could report success and then revert to the previous
  profile. This branch sends an explicit empty binding and refreshes profile
  state from the server response.
- Streaming DeepSeek responses can render the answer but leave the UI in
  `Жду ответ модели...`, so stateful post-processing / task refresh is risky
  during demos. This branch defaults DeepSeek saved agents to non-streaming until
  the user explicitly chooses a stream mode, and handles streams that close
  without a final done event.
- In the legal-agent demo flow, the task context auto-filled `title`/`goal`, but
  the visible task FSM stayed at `clarify`. This branch exposes
  `current_step`, `expected_action`, `paused`, and `resume_hint`, and adds a
  deterministic state fallback for natural task language.
- Repeated demo facts (jurisdiction, user role, legal area, style) should live in
  the agent `Профиль` / long-term memory, not in every user message. This branch
  wires the create-agent gate's initial facts into the existing profile memory.
