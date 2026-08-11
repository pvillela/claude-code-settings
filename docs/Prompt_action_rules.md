# Action Rules

We are going to resume the crafting of rules and hooks for potentially destructive commands, to be defined in `~/.claude`.

For now, I have removed the PreToolUse hook section of `settings.json`.

I want to pivot the implementation of command hooks to use the framework defined in `action-rules/src/action_rules.rs` for both the specification of rules and their implementation.

The build process for the `action-rules` Rust project should include the generation of a Rustdoc page in the `docs` folder of the Rust project. The page should include non-public items insofar as they are relevant to understand rule semantics. Other non-public items should be doc-hidden.

As a result, the CLAUDE.md section about the rules should simply have a very brief text and a relative link to the aformentioned Rustdoc page.

For reference, your earlier `GUARD-REWRITE-PLAN-20260810.md` is saved in `docs/archived`.
