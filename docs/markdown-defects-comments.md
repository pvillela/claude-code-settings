# Markdown and RustDoc defects and comments

-  Check entire document for bugs similar to any bugs reported below.
- `impl Command` doesn't include function signature for `parse` or even `fn` in front of it like other impls.
- `impl Action`: `### fn forbidden_action`. I don't think we need every function and field to be a section heading. Let's just keep the types and their impls as section headings. But I do want every function to include its signature.
- I like the inclusion of the function implementations where it sheds light on spec semantics. That will probably not be the case when you implement `Command::parse`.
- How does the markdown generation process ditinguish function implementations that will be included in the document from those that won't?
- `struct TargetedEffect`'s fields are not documented.
- I have refactored the type `Outcome` to `Verdict`. There may be dangling doc strings that need fixing.
- There are some portions of doc comments (like "That is what gives rm -rf src/ the branch rule" and "The inverse arrangement — a denylist of writing subcommands — is what lets git add, git fetch and git submodule update pass unexamined today.") that refer to the old hook rules. Once the new rules framework is ready, those portions (together with some context so the text is intelligible) should be moved to a separate historical notes document.
