# upmd feature showcase

This document is both a demo and an executable smoke test.

Open it in the TUI with `upmd DEMO.md` and press Enter on any block to run it.
Run with `upmd` (no args) to auto-open `up.md`, or use the file picker to
select a different file. Headless execution: `upmd --all --yes DEMO.md`.

## Getting started

A regular shell command. Select it and press Enter.

```sh
ls
```

An interactive prompt. Press `i` to focus the running block's inline terminal, type your name, then press Enter.

```sh
read -p "Your name: " ME
echo "Hi, $ME!"
```

Full terminal applications work inside the preview. Press `i` to interact, `o` for the full output view, `ctrl-o` to leave.

```sh
nvim
```

> [!TIP]
> Run `upmd` with no arguments to auto-open `up.md`, or browse the file picker.

## 1. Streamed output

Output appears inline while the block runs. The preview follows new rows only when they extend below the viewport.

```bash [name:stream]
printf 'Starting demo'
for step in 1 2 3 4; do
  sleep 0.15
  printf '.'
done
printf '\nReady.\n'
```

## 2. ANSI colors and Unicode

Every block runs in a real PTY, so ANSI escape codes and Unicode render correctly.

```bash [name:colors]
printf '\033[31mred\033[0m \033[32mgreen\033[0m \033[33myellow\033[0m \033[34mblue\033[0m \033[35mmagenta\033[0m \033[36mcyan\033[0m\n'
printf 'Unicode: café ภาษาไทย 🚀 ★\n'
printf 'Bold: \033[1mimportant\033[0m  Dim: \033[2mquiet\033[0m  Underline: \033[4mlink\033[0m\n'
```

## 3. Shell state persistence

Shell runners automatically capture exported variables and the final working directory.

```bash [name:set-state]
export UPMD_DEMO_MESSAGE="state carried from the previous block"
export UPMD_DEMO_COLOR="cyan"
export UPMD_DEMO_COUNT="3"
cd "${TMPDIR:-/tmp}"
printf 'Saved environment and cwd: %s\n' "$PWD"
```

The next block reads the captured state. Notice that `$UPMD_DEMO_MESSAGE`, `$UPMD_DEMO_COLOR`, and `$PWD` are available despite being set in a separate process.

```bash [name:read-state]
printf 'Message: %s\n' "$UPMD_DEMO_MESSAGE"
printf 'Color: %s\n' "$UPMD_DEMO_COLOR"
printf 'Count: %s\n' "$UPMD_DEMO_COUNT"
printf 'Working directory: %s\n' "$PWD"
```

Press `e` in the TUI to inspect or edit the environment before running another block.

## 4. Multiple language runners

Python inherits state captured by the shell block.

```python [name:python]
import os

message = os.environ.get("UPMD_DEMO_MESSAGE", "missing")
print(f"Python received: {message}")
print(f"Unicode and ANSI stay intact: café ภาษาไทย")
print(f"Count from shell: {os.environ.get('UPMD_DEMO_COUNT', '?')}")
```

JavaScript also picks up the captured environment.

```javascript [name:javascript]
const msg = process.env.UPMD_DEMO_MESSAGE || 'missing';
console.log(`JavaScript received: ${msg}`);
console.log('Node version:', process.version);
```

upmd also supports TypeScript, Ruby, PHP, C, Go, Rust, Zig, Fish, Zsh, Cmd, and PowerShell when their executables are installed.

## 5. Named blocks and goto

Blocks with a `name` attribute are selectable by name or numeric ID.

```bash [name:named]
printf 'This block is named "named".\n'
printf 'Jump to it with: upmd DEMO.md --block named\n'
printf 'Or press ctrl-g in the TUI and type "named".\n'
```

## 6. Workflow dependencies

Blocks can declare dependencies on other blocks. Deps run first, and their captured environment (exported variables, working directory) is inherited by dependents.

```bash [name:build]
sleep 1; export BIN_PATH="/tmp/demo"
printf 'build: BIN_PATH=%s\n' "$BIN_PATH"
```

```bash [name:lint, deps:build]
sleep 0.3; export LINT_REPORT="all clean"
printf 'lint: %s\n' "$LINT_REPORT"
```

```bash [name:test, deps:build]
sleep 0.6; export TEST_STATUS="47 passed, 0 failed"
printf 'test: %s\n' "$TEST_STATUS"
```

```bash [name:verify, deps:"lint | test"]
read CONFIRM
printf 'BIN_PATH=%s\n' "$BIN_PATH"
printf 'LINT_REPORT=%s\n' "$LINT_REPORT"
printf 'TEST_STATUS=%s\n' "$TEST_STATUS"
printf 'verify: complete (%s).\n' "$CONFIRM"
```

`lint` and `test` run in parallel after `build` finishes. Once both pass, `verify` runs and reads their captured state.

Press `'` while a workflow runs to toggle the inline dependency graph below the preview.

## 7. File picker and reload

- Press `f` to browse Markdown files relative to this document.
- Type to filter matches while the selected file is previewed.
- Press `ctrl-r` to reload the active file from disk and clear prior output.
- Directory input works in both frontends: `upmd .` and `upmd --cli .`.

## 8. Themes and help

- Press `t` to search and select a theme (tokyo-night, catppuccin-mocha, dracula, rose-pine, and more).
- Press `ctrl-t` to toggle the terminal background.
- Press `?` to open the searchable, sectioned keymap reference.

Print every configurable key and default binding:

```bash [name:dump-default-config]
upmd --dump-default-config 2>/dev/null | head -20 || echo "dump-default-config unavailable"
```

## 9. Interactive PTY and full output

Every block runs in a pseudo-terminal. While a long-running or full-screen program is active:

- Press `i` to focus its inline terminal and send keys directly.
- Press `ctrl-o` to leave inline input mode.
- Press `o` to open the full output view.
- Mouse input is forwarded when the child enables SGR mouse reporting.
- Otherwise wheel input scrolls local history and drag selection copies text.
- Press `ctrl-v` to paste clipboard text into the process.
