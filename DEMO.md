# upmd feature showcase

This document is both a demo and an executable smoke test.

Open it in the TUI with `upmd DEMO.md` and press Enter on any block to run it.
Run one named block without the TUI with
`upmd --cli --block stream --yes DEMO.md`.

## Getting started

A regular shell command. Select it and press Enter.

```sh
ls
```

## Markdown inline styles

Plain text, **bold**, *italic*, ~~strikethrough~~, `inline code`,
[a link](https://github.com/rezigned/upmd "upmd repository"), and
![the upmd logo](.github/pages/upmd-logo.png).

Styles can be nested: ***bold italic***, **[a bold link](https://github.com/rezigned/upmd)**,
and ~~*struck italic*~~.

### Images

A standalone image renders in the preview pane:

![the upmd logo](.github/pages/upmd-logo.png)

### A heading with **bold**, *italic*, and `code`

- A list item with **bold**, *italic*, ~~strikethrough~~, and `inline code`.
- A linked image: [![upmd logo](.github/pages/upmd-square.svg)](https://upmd.rezigned.com/).

| Context | Styled example |
|---|---|
| Emphasis | **bold**, *italic*, and ***both*** |
| Other | ~~strikethrough~~, `inline code`, and [link](https://upmd.rezigned.com/) |

### Raw HTML

Inline tags render with HTML syntax highlighting and keep their source text:
a <b>bold</b> tag, an <em>emphasis</em> tag, a <br/> self-closing tag, and a
<!-- comment -->.

A complete HTML block is shown verbatim, line by line:

<div class="card">
  <h3>Example card</h3>
  <p>HTML blocks keep their attributes and line breaks.</p>
  <ul>
    <li>first item</li>
    <li>second item</li>
  </ul>
</div>

## Streamed output

Output appears inline while the block runs. The preview follows new rows only when they extend below the viewport.

```bash [name:stream]
printf 'Starting demo'
for step in 1 2 3 4; do
  sleep 0.15
  printf '.'
done
printf '\nReady.\n'
```

## ANSI colors and Unicode

Every block runs in a real PTY, so ANSI escape codes and Unicode render correctly.

```bash [name:colors]
printf '\033[31mred\033[0m \033[32mgreen\033[0m \033[33myellow\033[0m \033[34mblue\033[0m \033[35mmagenta\033[0m \033[36mcyan\033[0m\n'
printf 'Unicode: café ภาษาไทย 🚀 ★\n'
printf 'Bold: \033[1mimportant\033[0m  Dim: \033[2mquiet\033[0m  Underline: \033[4mlink\033[0m\n'
```

## Shell state persistence

Shell runners carry exported variables and the final working directory into later blocks.

```bash [name:set-state]
export UPMD_DEMO_MESSAGE="state carried from the previous block"
export UPMD_DEMO_COLOR="cyan"
export UPMD_DEMO_COUNT="3"
cd "${TMPDIR:-/tmp}"
printf 'Saved environment and cwd: %s\n' "$PWD"
```

The next block reads the state captured above.

```bash [name:read-state]
printf 'Message: %s\n' "$UPMD_DEMO_MESSAGE"
printf 'Color: %s\n' "$UPMD_DEMO_COLOR"
printf 'Count: %s\n' "$UPMD_DEMO_COUNT"
printf 'Working directory: %s\n' "$PWD"
```

## Multiple language runners

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

## Named blocks and goto

Blocks with a `name` attribute are selectable by name or numeric ID.

```bash [name:named]
printf 'This block is named "named".\n'
printf 'Jump to it with: upmd DEMO.md --block named\n'
printf 'Or press ctrl-g in the TUI and type "named".\n'
```

## Workflow dependencies

Dependencies run first and pass their captured environment and working directory to dependents.

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
printf "bin: $BIN_PATH, lint: $LINT_REPORT, test: $TEST_STATUS\n"
printf 'verify: complete (%s).\n' "$CONFIRM"
```

`build` runs first. `lint` and `test` then run in parallel; `verify` waits for both.

## TUI controls

Run an interactive prompt and press `i` to send it input:

```sh
read -p "Your name: " ME
echo "Hi, $ME!"
```

Full-screen programs work inside the preview:

```sh
nvim
```

- `i` focuses the selected process; `ctrl-o` leaves input mode.
- `o` opens the full output view.
- `f` browses Markdown files; typing filters the list.
- `ctrl-r` reloads the active file and clears prior output.
- `t` selects a theme; `ctrl-t` toggles the terminal background.
- `e` inspects or edits captured environment state.
- `'` toggles the dependency graph while a workflow runs.
- `?` opens the searchable keymap reference.
- Mouse input, wheel scrolling, drag selection, and `ctrl-v` paste work in PTY views.

Print every configurable key and default binding:

```bash [name:dump-default-config]
upmd --dump-default-config 2>/dev/null | head -20 || echo "dump-default-config unavailable"
```
