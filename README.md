# .claude

Claude code global settings worth saving.

## Noteworthy

- ***Critical Git rules*** in [CLAUDE.md](./CLAUDE.md).
- ***settings.json***: References the items below and includes a few other odds and ends.

- ***global-git-guard.sh*** hook:
  - Blocks Claude Code from executing destructive shell and Git actions unless the current branch allows them. *(It probably doesn't block all such possible operations, but the list is pretty comprehensive.)*
  - A branch named `aicode` or any branch listed in an `allowed-branches` file under the project's `.claude` directory permits Claude Code to perform destructive shell and Git actions.
- ***statusline-command.sh***: Adds a useful status line at the bottom of the Claude Code console that includes, among other things, the model name, thinking level, context size, session usage %.

## Alternatives

There are alternatives to the approach used here. For example, see [claude-code-guardian](https://github.com/idnotbe/claude-code-guardian).
