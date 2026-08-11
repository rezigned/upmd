# upmd

<p align="center">
  <img src=".github/pages/upmd-logo.png" alt="upmd" width="250">
</p>

<p align="center">Run tasks and workflows from Markdown.</p>

<p align="center">
  <a href="https://github.com/rezigned/upmd/releases/latest"><img src="https://img.shields.io/github/v/release/rezigned/upmd?include_prereleases&style=flat-square" alt="Release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/rezigned/upmd?style=flat-square" alt="License"></a>
  <a href="https://github.com/rezigned/upmd/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/rezigned/upmd/ci.yml?branch=main&style=flat-square" alt="CI"></a>
  <a href="https://github.com/rezigned/upmd/releases"><img src="https://img.shields.io/github/downloads/rezigned/upmd/total?style=flat-square" alt="Downloads"></a>
</p>

<p align="center">
  <img src=".github/pages/demos/overview.gif" width="600" alt="upmd overview">
</p>

`upmd` turns Markdown code blocks into runnable tasks and dependency-aware workflows. Run them interactively in a real terminal, with instructions, commands, and output kept together.

## What it does

- Treats fenced code blocks as named tasks with dependencies.
- Runs one task or a workflow from the TUI, lightweight CLI, or CI.
- Gives each task a real pseudo-terminal, including prompts, colors, editors, and pagers.
- Shows output beside the source and can pass shell environment and working-directory changes to later tasks.

## Installation

**Shell installer (macOS and Linux)**

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/rezigned/upmd/releases/latest/download/upmd-installer.sh | sh
```

**Homebrew (macOS)**

```bash
brew install rezigned/tap/upmd
```

**Windows and archives:** Download a prebuilt archive and checksum from the [latest release](https://github.com/rezigned/upmd/releases/latest). Builds are available for macOS, Linux, and Windows on supported x86_64 and ARM64 platforms.

**From source** (requires Rust 1.82 or newer):

```bash
cargo install --git https://github.com/rezigned/upmd.git
```

## Quick start

```bash
# Open up.md or UP.md when present, otherwise browse the current directory
upmd

# Open a document in the TUI
upmd README.md

# Run one named task and its dependencies
upmd DEMO.md --block lint --yes

# Run all tasks non-interactively (CI)
upmd --ci --all README.md
```

> [!TIP]
> With no path, `upmd` opens `up.md` or `UP.md` from the current directory when either is present. Otherwise, it opens the current-directory file picker. Directories are searched recursively, while piped stdin is read as Markdown. A failed task or startup error produces a non-zero exit status.

Run `upmd --help` for all command-line options.

## Tasks and workflows

The workflow in [`DEMO.md`](DEMO.md) uses `name` and `deps` attributes after the language token:

````markdown
```bash [name:build]
cargo build
```

```bash [name:lint, deps:build]
cargo clippy
```

```bash [name:test, deps:build]
cargo test
```

```bash [name:verify, deps:"lint | test"]
echo "verified ✅"
```
````

`name` makes a task addressable from `--block` and the goto menu. Numeric IDs also work:

```bash
upmd DEMO.md --block verify
```

### Dependencies

```text
· build ──┬─→ · lint ──┬─→ · verify
          │            │
          └─→ · test ──┘
```

`lint` and `test` each use `deps:build`, so they start together after `build` succeeds. `verify` uses `deps:"lint | test"` and waits for both. Within `deps`, `|` puts tasks in the same stage and `,` starts the next stage.

If a task fails, later stages are skipped. Under `--all`, unrelated tasks continue and the command exits non-zero.

Override a task runner with an attribute such as `[bin:zsh]`. Runner settings resolve in this order: block attribute, user configuration, then built-in defaults.

## TUI shortcuts

| Key | Action |
|---|---|
| `up` / `k`, `down` / `j` | Move between tasks |
| `Enter` | Run the selected task |
| `/` | Search document text |
| `ctrl-g` | Find and jump to a task |
| `o` | Open the full terminal output |
| `i` | Send input to the selected running task |
| `?` | Search all shortcuts |
| `q` / `ctrl-c` | Quit |

Press `o` for the full terminal screen and scrollback. Running programs receive keyboard, paste, and SGR mouse input directly. Press `ctrl-o` to return home. Default bindings are listed in [`src/apps/config.toml`](src/apps/config.toml).

## State persistence

Successful shell tasks pass exported variables and working-directory changes to later tasks:

````markdown
```bash [name:prepare]
export API_URL="http://127.0.0.1:8080"
cd /tmp
```

```bash [name:inspect, deps:prepare]
printf 'API_URL=%s\n' "$API_URL"
pwd
```
````

Python, Go, Rust, and TypeScript can opt into experimental state capture with `--capture-state`. Press `e` to inspect or edit the environment before running the next task.

## Supported languages

Built-in runners cover Bash, POSIX shell, Zsh, Fish, Cmd, PowerShell, Python, JavaScript, TypeScript, Ruby, PHP, C, Go, Rust, and Zig.

TypeScript tries Node.js native strip-types, `npx tsx`, then `ts-node`. Rust uses `rustc` or `cargo rustc --`. Override a binary with `[bin:...]` on a task or `binaries.<language>` in the configuration. Compiled runners use isolated temporary workspaces.

## Configuration and diagnostics

Persistent settings live at `~/.config/upmd/config.toml`:

```toml
theme = "catppuccin-mocha"
transparent = true

[tui]
inline_max_lines = 20
```

Runner binaries and key bindings can also be overridden. Print every available setting and keymap section with:

```bash
upmd --dump-default-config
```

Set `RUST_LOG` to write tracing output to the platform cache directory:

```bash
RUST_LOG=upmd=debug upmd README.md
```

## License

MIT
