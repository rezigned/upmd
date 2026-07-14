//! Mouse event encoding helpers for terminal applications.

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

/// Encodes a crossterm mouse event as an SGR mouse sequence.
///
/// Returns `None` for events SGR mouse reporting cannot represent or that do
/// not carry useful input for terminal applications, such as plain movement.
pub fn encode_sgr_mouse(mouse: &MouseEvent, col: u16, row: u16) -> Option<String> {
    let button = sgr_button(mouse)? + modifier_bits(mouse);
    let suffix = if matches!(mouse.kind, MouseEventKind::Up(_)) {
        'm'
    } else {
        'M'
    };

    Some(format!("\x1b[<{};{};{}{}", button, col, row, suffix))
}

fn sgr_button(mouse: &MouseEvent) -> Option<u16> {
    match mouse.kind {
        MouseEventKind::Down(button)
        | MouseEventKind::Drag(button)
        | MouseEventKind::Up(button) => Some(match button {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
        }),
        MouseEventKind::ScrollUp => Some(64),
        MouseEventKind::ScrollDown => Some(65),
        _ => None,
    }
}

fn modifier_bits(mouse: &MouseEvent) -> u16 {
    let mut bits = 0;
    if mouse.modifiers.contains(KeyModifiers::SHIFT) {
        bits += 4;
    }
    if mouse.modifiers.contains(KeyModifiers::ALT) {
        bits += 8;
    }
    if mouse.modifiers.contains(KeyModifiers::CONTROL) {
        bits += 16;
    }
    if matches!(mouse.kind, MouseEventKind::Drag(_)) {
        bits += 32;
    }
    bits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mouse(kind: MouseEventKind, modifiers: KeyModifiers) -> MouseEvent {
        MouseEvent {
            kind,
            column: 0,
            row: 0,
            modifiers,
        }
    }

    #[test]
    fn encode_sgr_mouse_scroll() {
        let event = mouse(MouseEventKind::ScrollDown, KeyModifiers::empty());
        assert_eq!(
            encode_sgr_mouse(&event, 10, 5).as_deref(),
            Some("\x1b[<65;10;5M")
        );
    }

    #[test]
    fn encode_sgr_mouse_click_release_and_modifiers() {
        let event = mouse(
            MouseEventKind::Up(MouseButton::Left),
            KeyModifiers::SHIFT | KeyModifiers::CONTROL,
        );
        assert_eq!(
            encode_sgr_mouse(&event, 3, 2).as_deref(),
            Some("\x1b[<20;3;2m")
        );
    }

    #[test]
    fn encode_sgr_mouse_drag_sets_motion_bit() {
        let event = mouse(MouseEventKind::Drag(MouseButton::Right), KeyModifiers::ALT);
        assert_eq!(
            encode_sgr_mouse(&event, 7, 4).as_deref(),
            Some("\x1b[<42;7;4M")
        );
    }

    #[test]
    fn encode_sgr_mouse_ignores_plain_move() {
        let event = mouse(MouseEventKind::Moved, KeyModifiers::empty());
        assert!(encode_sgr_mouse(&event, 1, 1).is_none());
    }
}
