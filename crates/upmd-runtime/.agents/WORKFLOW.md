# AI Agent Workflow Guidelines

Guidelines for AI agents working on this codebase.

## 1. Always Propose Multiple Solutions

Before implementing any non-trivial change, present at least 2-3 approaches:

```
Option A: [description] - [pros] - [cons]
Option B: [description] - [pros] - [cons]
Option C: [description] - [pros] - [cons]
```

Wait for user approval before proceeding.

**Exception**: Trivial changes (typo fixes, formatting, obvious bug fixes) can be done directly.

## 2. Always Check Compile Errors

After any code modification:

```bash
cargo check
cargo check --all-features  # if applicable
cargo check --example todo --features tui  # examples with specific features
```

Fix all compilation errors before presenting the result.

Also check examples with specific feature flags:
```bash
cargo check --example todo --features cli,tui
```

## 3. Always Run Lint + Clippy

Before suggesting a commit is complete:

```bash
cargo clippy --all-features -- -D warnings
cargo fmt --check
```

Fix all clippy warnings and formatting issues.

## 4. Suggest Commit, Ask for Approval

When work is complete and compiling/linting clean:

1. Present a summary of changes
2. Suggest a commit message following conventional commits:
   ```
   <type>(<scope>): <description>
   
   [body explaining why]
   ```
3. **Ask for explicit approval** before running `git commit`

**Never commit without user approval.**

## 5. Test Changes When Possible

If the project has tests:

```bash
cargo test
cargo test --all-features
```

Ensure all tests pass. If tests fail, investigate and fix.

## 6. Documentation Updates (Before Suggesting Commit)

When modifying public APIs or behavior:

- [ ] Update `README.md` if user-facing behavior changes (types, methods, signatures)
- [ ] Update `.agents/TASKS.md` if completing a task
- [ ] Add/update doc comments on public types/functions

**Do this BEFORE suggesting commit.** README must reflect latest code.

## 7. Suggest Commit, Ask for Approval

When work is complete and compiling/linting clean:

1. Present a summary of changes
2. Suggest a commit message following conventional commits:
   ```
   <type>(<scope>): <description>

   [body explaining why]
   ```
3. **Ask for explicit approval** before running `git commit`

**Never commit without user approval.**

- [ ] Code compiles (`cargo check`)
- [ ] Clippy clean (`cargo clippy`)
- [ ] Formatted (`cargo fmt`)
- [ ] Tests pass (`cargo test`) if available
- [ ] Documentation updated if needed
- [ ] User asked for approval before commit
