## [0.2.2] - 2026-08-09

### 🚀 Features

- Render markdown images in the TUI preview via ratatui-image
- Render raw HTML in the Markdown preview
- Add rules under H1 and H2 headings

### 🚜 Refactor

- Consolidate TUI modes into unified overlay state
- Extract workflow state into dedicated TUI module
- Introduce inline PTY sizing and unified preview search
- Unify menu and preview into a single Content component
- Move help keymap collection into the help module
- Extract overlay handling into a dedicated module
- Redesign component effects

### 🎨 Styling

- Simplify overlay effects
## [0.2.1] - 2026-08-02

### 🚀 Features

- *(tui)* Render inline markdown formatting in the preview

### 🚜 Refactor

- Replace pre-rendered `VisualLine` lines with layout metadata from `LogicalLine` (#11)

### ⚙️ Miscellaneous Tasks

- *(release)* Prepare 0.2.1 with cargo-release automation
- *(release)* Bump version to 0.2.1
## [0.1.1] - 2026-07-25

### 🚀 Features

- *(tui)* Render menu items horizontally in vertical layout

### 🐛 Bug Fixes

- Use kebab-case theme names in docs
- *(cli)* Drain PTY output on engine exit and skip raw mode on non-TTY stdout

### 📚 Documentation

- Add short demo
- Improve README and landing page copy

### ⚙️ Miscellaneous Tasks

- Add badges, version link, and copyright year
- Skip ci on readme-only changes
- Change demo font to 'Ioskeley Mono Term'
- Add dependabot
- *(release)* Bump version to 0.1.1
## [0.1.0] - 2026-07-14

### 💼 Other

- V0.1.0
