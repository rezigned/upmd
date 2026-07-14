use ratatui::layout::{Constraint, Direction, Layout, Rect, Spacing};

/// Screen regions for the menu, preview, and footer areas.
#[derive(Default, Clone, Copy)]
pub struct Area {
    pub menu: Rect,
    pub preview: Rect,
    pub footer: Rect,
    pub last_area: Rect,
    last_menu_width: u16,
}

impl Area {
    pub fn update(&mut self, area: Rect, menu_width: u16) {
        if self.last_area == area && self.last_menu_width == menu_width {
            return;
        }
        self.last_area = area;
        self.last_menu_width = menu_width;

        use crate::apps::config::{VERTICAL_LAYOUT_HEIGHT_THRESHOLD, VERTICAL_MENU_HEIGHT};
        use ratatui::layout::{Constraint, Direction, Layout};

        let main = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .split(area);

        self.footer = main[1];

        let direction = if area.height < VERTICAL_LAYOUT_HEIGHT_THRESHOLD {
            Direction::Vertical
        } else {
            Direction::Horizontal
        };

        let inner = Layout::default()
            .direction(direction)
            .constraints(if direction == Direction::Horizontal {
                [Constraint::Length(menu_width), Constraint::Min(0)]
            } else {
                [Constraint::Length(VERTICAL_MENU_HEIGHT), Constraint::Min(0)]
            })
            .spacing(Spacing::Overlap(1))
            .split(main[0]);

        self.menu = inner[0];
        self.preview = inner[1];
    }

    pub fn total_width(&self) -> u16 {
        self.last_area.width
    }

    /// Recomputes layout with a new menu width but the same area.
    pub fn update_menu_width(&mut self, menu_width: u16) {
        self.update(self.last_area, menu_width);
    }

    /// Returns the printable PTY size based on the preview pane dimensions.
    pub fn pty_size(&self, extra_overhead: u16) -> crate::pty::process::Size {
        let overhead = crate::apps::config::PREVIEW_CODE_WRAP_OVERHEAD as u16 + extra_overhead;
        let cols = self.preview.width.saturating_sub(overhead).max(10);
        let rows = self.preview.height.saturating_sub(3);
        if cols == 0 || rows < 5 {
            crate::pty::process::Size::from((
                crate::apps::config::PTY_DEFAULT_COLS
                    .saturating_sub(overhead)
                    .max(10),
                crate::apps::config::PTY_DEFAULT_ROWS,
            ))
        } else {
            crate::pty::process::Size::from((cols, rows))
        }
    }
}

pub fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let height = ((r.height as u32 * percent_y as u32) / 100).max(6) as u16;
    let width = ((r.width as u32 * percent_x as u32) / 100).max(20) as u16;

    let popup = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Fill(1),
            Constraint::Length(height),
            Constraint::Fill(1),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Fill(1),
            Constraint::Length(width),
            Constraint::Fill(1),
        ])
        .split(popup[1])[1]
}
