# upmd feature showcase

This document is both a demo and an executable smoke test.

Open it in the TUI with `upmd DEMO.md` and press Enter on any block to run it.
Run every block headlessly with `upmd --cli --all --yes DEMO.md`.

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

Select a block by name with `--block`:

```sh
upmd DEMO.md --block verify --yes
```

> [!TIP]
> Run `upmd` with no arguments to open the Markdown file picker, type `DEMO`, and press Enter.

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

## 6. Markdown rendering

The preview renders standard Markdown, including headings, paragraphs, lists, blockquotes, tables, and thematic breaks.

### Lists

- Syntax-highlighted fenced code
- Inline **bold**, *emphasis*, and `code`
- Nested lists and blockquotes

> A blockquote can contain prose and code while preserving its visual prefix.

### Tables

| Workflow | Command | Result |
|---|---|---|
| Browse files | `upmd` | Opens the current-directory picker |
| Interactive TUI | `upmd DEMO.md` | Navigation, search, PTY input, and inline output |
| Lightweight CLI | `upmd --cli DEMO.md` | Terminal navigation without the full-screen layout |
| Automation | `upmd --cli --all --yes DEMO.md` | Executes every block sequentially |
| One block | `upmd DEMO.md --block verify --yes` | Selects and runs a named block |

## 7. Search and navigation

While this file is open in the TUI:

- `/` then type `SHOWCASE` to highlight search results.
- `ctrl-g` then type `verify` to jump to the final block.
- `0` through `9` to jump to a numeric block ID.
- `pageup` / `pagedown` to scroll the preview.
- `c` to toggle table-of-contents mode.
- `<` / `>` to resize the table of contents.
- `z` to toggle zen mode.

## 8. File picker and reload

- Press `f` to browse Markdown files relative to this document.
- Type to filter matches while the selected file is previewed.
- Press `ctrl-r` to reload the active file from disk and clear prior output.
- Directory input works in both frontends: `upmd .` and `upmd --cli .`.

## 9. Themes and help

- Press `t` to search and select a theme (Tokyo Night, Catppuccin Mocha, Dracula, Rose Pine, and more).
- Press `ctrl-t` to toggle the terminal background.
- Press `?` to open the searchable, sectioned keymap reference.

Print every configurable key and default binding:

```bash [name:dump-default-config]
upmd --dump-default-config 2>/dev/null | head -20 || echo "dump-default-config unavailable"
```

## 10. Interactive PTY and full output

Every block runs in a pseudo-terminal. While a long-running or full-screen program is active:

- Press `i` to focus its inline terminal and send keys directly.
- Press `ctrl-o` to leave inline input mode.
- Press `o` to open the full output view.
- Mouse input is forwarded when the child enables SGR mouse reporting.
- Otherwise wheel input scrolls local history and drag selection copies text.
- Press `ctrl-v` to paste clipboard text into the process.

This block demonstrates a simple interactive prompt.

```bash [name:interactive]
printf 'Hello from the PTY.\n'
printf 'Try pressing "i" then typing here, or "o" for full output.\n'
```

## 11. Final verification

```bash [name:verify]
printf 'upmd demo complete\n'
printf 'Captured message: %s\n' "$UPMD_DEMO_MESSAGE"
printf 'Captured count: %s\n' "$UPMD_DEMO_COUNT"
printf 'Current directory: %s\n' "$PWD"
```

Expected final marker: `upmd demo complete`.
