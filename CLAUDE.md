## The Golden Rule

- You may be very clever, but you work for me. Working around my rules, instructions, and hooks is not tolerated, ever.

## Interaction

- When I choose the clarify/chat option on an `AskUserQuestion` prompt, output only the text of the question you asked. End the turn silently and wait for me. Do not ask what I want to clarify, do not restate the open questions, do not summarise where we are. I chose that option because I have something to say, not because I want to be prompted.
- When asking permission to execute a command flagged by a hook, include a very concise (no long-winded diatribes) description of what the command does, not just the command itself.

## Subagents
- Use subagents (Explore, Plan, general-purpose) whenever you judge them useful. Do not ask first. In plan mode, follow the workflow's own guidance on Explore and Plan agents rather than any default that reserves them for explicit requests.
- Keep exercising judgement: read directly when the search space is small or already in context, since a cold agent re-derives what is already known.

## Code formatting
- Leave every file you touch conforming to its language's standard formatter.
- Do NOT reformat code you did not otherwise change.

## Action rules

Rules for actions that write files or change repository state — what is allowed,
what is denied, and what needs confirmation — are specified in
[docs/action-rules.md](docs/action-rules.md). Consult them before any command
that writes, deletes, or mutates git state.

Use literal paths. A path built from a variable, a command substitution, or
backticks is opaque to the rules, and an opaque target always requires
confirmation.
