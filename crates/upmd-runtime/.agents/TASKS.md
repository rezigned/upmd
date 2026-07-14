# Cha - Issues & Improvement Tasks

A task list for AI agents to address code quality, consistency, and missing features.

---

## Pre-Release Checklist

**Must complete before publishing to crates.io:**

- [x] Fix all doc-tests (9/9 passing)
- [x] Add unit tests (13 tests added)
- [x] Add Cargo.toml metadata (description, license, keywords, repository)
- [ ] Remove or fix `server/mod.rs` (orphaned stub, won't compile if used)
- [ ] Verify clippy clean (`cargo clippy --all-features -- -D warnings`)
- [ ] Verify fmt clean (`cargo fmt --check`)
- [ ] Verify tests pass (`cargo test --all-features`)
- [ ] Update README if needed for public consumption

---

## Priority: High

- [x] **1. `Cmd::msg` inefficiency** (`src/core.rs:107-111`) - ~~DEFERRED~~
  - Creates a thread spawn for immediate message delivery - unnecessary overhead
  - Fix: Try using try_send() instead of spawn, but blocking risk makes thread spawn safer
  - **Decision: Keep current async approach. Silent message drop is unacceptable. Thread spawn guarantees delivery.**

- [ ] **2. Missing mouse support in web runtime** (`src/web/mod.rs`)
  - `WebEvent::Mouse` variant exists but is never wired up
  - Fix: Add `web_sys::MouseEvent` listener and map to `WebEvent::Mouse`

- [x] **3. Universal pure constructors + Result-returning run()**
  - `Runtime` trait now returns `Result<(), Self::Error>`
  - `new()` is pure (no side effects) across all platforms (CLI, TUI, Web, Macroquad)
  - Setup moved into `run()` for CLI/TUI; Web/Macroquad already pure
  - Examples updated to handle `Result`

- [x] **11. Make CLI/TUI configurable with start/stop closures and Config struct**
  - CLI/TUI now accept custom `start()` and `stop()` closures
  - Default closures provide sensible crossterm setup/cleanup
  - TUI has `Config` struct with `poll()` for poll timeout configuration
  - User can replace either to customize behavior (e.g., skip mouse capture)

- [x] **10. Make runtime configurable** (`src/core.rs`)
  - Added `Config` struct with `Default` trait and builder methods
  - `Engine::new()` uses default config; `Engine::with_config()` for custom
  - Example: `Engine::with_config(component, Config::default().cmd_bound(64))`

---

## Priority: Medium

- [ ] **4. `Engine::component` is public** (`src/core.rs:261`)
  - Direct mutation bypasses `update()` - violates Elm architecture
  - Fix: Make `component` private or add accessor methods

- [ ] **5. Channel buffer overflow silent drop** (`src/core.rs:283`)
  - `cmd_rx` bounded to 32; high-volume commands drop messages
  - Fix: Consider unbounded or larger bound; add backpressure handling

- [ ] **6. No feedback on message send failure** (`src/cli/mod.rs:124`, `src/tui/mod.rs:87`)
  - `engine.send_msg(msg).ok()` silently drops errors
  - Fix: Log warning or expose error to component

- [ ] **7. Missing keyup/keypress in web runtime** (`src/web/mod.rs:128`)
  - Only `keydown` is handled
  - Fix: Add keyup/keypress listeners

---

## Priority: Low

- [ ] **8. `Cmd::map` requires `Clone` bound** (`src/core.rs:128`)
  - Could avoid the `F: Clone` requirement

- [ ] **9. No cleanup in web runtime**
  - Missing `Drop` impl or cleanup handler

---

## Cleanup Tasks

- [ ] **A. Add integration tests**
  - Test CLI/TUI/Web runtime initialization
  - Test message passing between components

- [ ] **B. Document error handling strategy**
  - Decide: `Result` vs `panic` vs `log` for terminal/rendering failures