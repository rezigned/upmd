## [0.2.6]

### 🐛 Bug Fixes

- *(tui)* Allow concurrent standalone tasks (#37)

### 🚜 Refactor

- *(tui)* Extract mode renderers and preserve viewport (#34)
- *(tui)* Separate content cache preparation (#36)

### ⚡ Performance

- *(tui)* Batch and reuse syntax highlights (#35)
## [v0.2.5]

### 🚀 Features

- *(runtime)* Schedule delayed actions via Cmd::after
- *(cli)* Add --ci alias for --cli --yes
- *(parser)* Recognize and render leading frontmatter (#24)
- *(tui)* Add markup preview and retain parser source ranges (#29)

### 🐛 Bug Fixes

- *(parser)* Match spaces around colons with ASCII attr regex
- *(tui)* Render blockquote prefixes consistently (#30)
- *(tui)* Preserve indentation of nested list blocks (#31)

### 🚜 Refactor

- *(theme)* Resolve markdown styles from scope rules directly
- *(tui)* Simplify layout rendering (#27)
- *(tui)* Extract gutter glyph to config constant (#32)

### ⚡ Performance

- *(theme)* Load bundled themes lazily instead of building a ThemeSet at startup
- *(tui)* Drop tick thread to save thread stack memory
- *(tui)* Skip full re-render on resize

### ⚙️ Miscellaneous Tasks

- *(release)* Bump version to 0.2.3 (#23)
- *(release)* Bump version to 0.2.4 (#28)
- *(release)* Bump version to 0.2.5 (#33)
## [v0.2.2]

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

### ⚙️ Miscellaneous Tasks

- *(release)* Bump version to 0.2.2
## [v0.2.1]

### 🚀 Features

- *(tui)* Render inline markdown formatting in the preview

### 🚜 Refactor

- Replace pre-rendered `VisualLine` lines with layout metadata from `LogicalLine` (#11)

### ⚙️ Miscellaneous Tasks

- *(release)* Prepare 0.2.1 with cargo-release automation
- *(release)* Bump version to 0.2.1
## [v0.1.1]

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
## [v0.1.0]

### 💼 Other

- V0.1.0
