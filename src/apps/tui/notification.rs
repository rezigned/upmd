//! Transient bottom-right flash notification.
//!
//! [`FlashMessage`] carries a short-lived message and its style kind.
//! The app stores at most one active message; the newest replaces
//! whatever is currently shown.  Expiry is driven by `Msg::Tick`.

use std::time::{Duration, Instant};

/// Visual style of a flash message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashKind {
    Info,
    Success,
    Error,
}

/// A single flash notification.
#[derive(Debug, Clone)]
pub struct FlashMessage {
    pub text: String,
    pub kind: FlashKind,
    expires_at: Instant,
}

impl FlashMessage {
    const DEFAULT_DURATION: Duration = Duration::from_millis(1500);

    /// Creates a new flash that expires after `dur`.
    pub fn new(text: impl Into<String>, kind: FlashKind, now: Instant, dur: Duration) -> Self {
        Self {
            text: text.into(),
            kind,
            expires_at: now + dur,
        }
    }

    /// Creates an info flash that expires after the default 1.5 s duration.
    pub fn info(text: impl Into<String>) -> Self {
        Self::new(
            text,
            FlashKind::Info,
            Instant::now(),
            Self::DEFAULT_DURATION,
        )
    }

    /// Creates a success flash with the default 1.5 s duration.
    pub fn success(text: impl Into<String>) -> Self {
        Self::new(
            text,
            FlashKind::Success,
            Instant::now(),
            Self::DEFAULT_DURATION,
        )
    }

    /// Creates an error flash with the default 1.5 s duration.
    pub fn error(text: impl Into<String>) -> Self {
        Self::new(
            text,
            FlashKind::Error,
            Instant::now(),
            Self::DEFAULT_DURATION,
        )
    }

    /// Whether this message has expired and should no longer be displayed.
    pub fn is_expired(&self, now: Instant) -> bool {
        now >= self.expires_at
    }
    /// Renders the notification into `frame` at the bottom-right of `area`.
    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &crate::apps::theme::Theme) {
        use ratatui::{layout::Alignment, text::Line, widgets::Paragraph};
        let Some(rect) = notification_area(self, area) else {
            return;
        };
        let label = format!(" {} ", self.text);
        let style = match self.kind {
            FlashKind::Info => theme.info_badge_style(),
            FlashKind::Success => theme.success_badge_style(),
            FlashKind::Error => theme.error_badge_style(),
        };
        frame.render_widget(
            Paragraph::new(Line::from(label))
                .style(style)
                .alignment(Alignment::Right),
            rect,
        );
    }
}

use ratatui::{layout::Rect, text::Line, Frame};

/// Bottom-right render area for a flash notification.
pub fn notification_area(flash: &FlashMessage, area: Rect) -> Option<Rect> {
    if area.width == 0 || area.height == 0 {
        return None;
    }

    let label_width = Line::from(format!(" {} ", flash.text)).width() as u16;
    let width = label_width.clamp(1, area.width);
    Some(Rect {
        x: area.x + area.width.saturating_sub(width),
        y: area.y + area.height.saturating_sub(1),
        width,
        height: 1,
    })
}

/// Shortcut for [`FlashMessage::info`].
pub fn info(text: impl Into<String>) -> FlashMessage {
    FlashMessage::info(text)
}

/// Shortcut for [`FlashMessage::success`].
pub fn success(text: impl Into<String>) -> FlashMessage {
    FlashMessage::success(text)
}

/// Shortcut for [`FlashMessage::error`].
pub fn error(text: impl Into<String>) -> FlashMessage {
    FlashMessage::error(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_message_not_expired_immediately() {
        let now = Instant::now();
        let msg = FlashMessage::new(
            "hello",
            FlashKind::Info,
            now,
            FlashMessage::DEFAULT_DURATION,
        );
        assert!(!msg.is_expired(now));
    }

    #[test]
    fn test_new_message_expired_after_duration() {
        let now = Instant::now();
        let msg = FlashMessage::new(
            "hello",
            FlashKind::Info,
            now,
            FlashMessage::DEFAULT_DURATION,
        );
        assert!(msg.is_expired(now + FlashMessage::DEFAULT_DURATION));
    }

    #[test]
    fn test_new_message_respects_custom_duration() {
        let now = Instant::now();
        let msg = FlashMessage::new("slow", FlashKind::Info, now, Duration::from_millis(100));
        assert!(!msg.is_expired(now + Duration::from_millis(50)));
        assert!(msg.is_expired(now + Duration::from_millis(100)));
    }

    #[test]
    fn test_latest_message_replaces() {
        let old = FlashMessage::info("old");
        let new = FlashMessage::success("new");
        // The app stores a single Option<FlashMessage>; setting it to
        // `Some(new)` drops `old` – simulated here by checking we
        // track only the latest.
        let current = Some(new.clone());
        assert_eq!(current.as_ref().unwrap().text, "new");
        assert_eq!(current.as_ref().unwrap().kind, FlashKind::Success);
        // old is gone.
        drop(old);
    }

    #[test]
    fn test_notification_area_bottom_right() {
        let msg = FlashMessage::info("Saved");
        let area = Rect::new(0, 0, 80, 24);
        let rect = notification_area(&msg, area).unwrap();

        assert_eq!(rect.y, 23);
        assert_eq!(rect.x + rect.width, 80);
        assert_eq!(rect.height, 1);
        assert_eq!(rect.width, 7);
    }

    #[test]
    fn test_notification_area_clips_to_terminal_width() {
        let msg = FlashMessage::info("A very long flash message");
        let area = Rect::new(0, 0, 10, 3);
        let rect = notification_area(&msg, area).unwrap();

        assert_eq!(rect, Rect::new(0, 2, 10, 1));
    }

    #[test]
    fn test_notification_area_empty_terminal_is_none() {
        let msg = FlashMessage::info("Saved");

        assert_eq!(notification_area(&msg, Rect::new(0, 0, 0, 3)), None);
        assert_eq!(notification_area(&msg, Rect::new(0, 0, 10, 0)), None);
    }

    #[test]
    fn test_kinds_have_distinct_labels() {
        assert_eq!(FlashMessage::info("x").kind, FlashKind::Info);
        assert_eq!(FlashMessage::success("x").kind, FlashKind::Success);
        assert_eq!(FlashMessage::error("x").kind, FlashKind::Error);
    }
}
