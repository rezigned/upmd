//! Utility functions shared across the application.

use std::cell::RefCell;

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::layout::Rect;

thread_local! {
    static CLIPBOARD: RefCell<Option<arboard::Clipboard>> =
        RefCell::new(arboard::Clipboard::new().ok());
}

/// Copies text to the system clipboard.
///
/// Returns `true` when the clipboard write succeeded, `false` otherwise.
pub fn clipboard_copy(text: &str) -> bool {
    CLIPBOARD.with(|cb| {
        if let Ok(mut inner) = cb.try_borrow_mut() {
            if let Some(clipboard) = inner.as_mut() {
                clipboard.set_text(text).is_ok()
            } else {
                false
            }
        } else {
            false
        }
    })
}

/// Reads text from the system clipboard.
///
/// Returns `None` if clipboard access fails (e.g. in headless environments).
pub fn clipboard_paste() -> Option<String> {
    CLIPBOARD.with(|cb| {
        let mut inner = cb.borrow_mut();
        inner.as_mut()?.get_text().ok()
    })
}

/// Converts a terminal key event to bytes for PTY input.
///
/// Handles special keys like Enter, Backspace, Tab, arrow keys, etc. and converts
/// control characters (Ctrl+A through Ctrl+Z) to their ANSI codes (1-26).
///
/// # Arguments
/// * `key` - The crossterm key event to convert
///
/// # Returns
/// * `Some(Vec<u8>)` - The byte sequence to send to the PTY
/// * `None` - If the key is not supported
pub fn key_to_bytes(key: crossterm::event::KeyEvent) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();

    // Handle Alt (Meta) modifier by prefixing with Escape (0x1b)
    if key.modifiers.contains(KeyModifiers::ALT) {
        bytes.push(27);
    }

    match key.code {
        KeyCode::Char(c) => {
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                let b = c as u8;
                // Terminals handle Ctrl by applying a bitmask of 0x1f (31)
                // This correctly maps 'a' and 'A' to 1, '[' to 27, etc.
                if b.is_ascii() {
                    bytes.push(b & 0x1f);
                    return Some(bytes);
                }
            }

            // Encode UTF-8 directly into a stack buffer to avoid heap allocation.
            let mut buf = [0; 4];
            let encoded = c.encode_utf8(&mut buf);
            bytes.extend_from_slice(encoded.as_bytes());

            Some(bytes)
        }
        KeyCode::Enter => Some(vec![b'\r']),
        KeyCode::Backspace => Some(vec![127]),
        KeyCode::Tab => {
            // Check for Shift+Tab (some terminals send this as BackTab, others as Shift+Tab)
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                Some(vec![27, b'[', b'Z'])
            } else {
                Some(vec![9])
            }
        }
        KeyCode::BackTab => Some(vec![27, b'[', b'Z']),
        KeyCode::Esc => Some(vec![27]),

        // ANSI/VT100 sequences. For full compliance, these eventually need
        // modifier parameters (e.g., 1;5 for Ctrl) injected into the sequence.
        KeyCode::Up => Some(vec![27, b'[', b'A']),
        KeyCode::Down => Some(vec![27, b'[', b'B']),
        KeyCode::Right => Some(vec![27, b'[', b'C']),
        KeyCode::Left => Some(vec![27, b'[', b'D']),
        KeyCode::Home => Some(vec![27, b'[', b'H']),
        KeyCode::End => Some(vec![27, b'[', b'F']),
        KeyCode::PageUp => Some(vec![27, b'[', b'5', b'~']),
        KeyCode::PageDown => Some(vec![27, b'[', b'6', b'~']),
        KeyCode::Delete => Some(vec![27, b'[', b'3', b'~']),
        _ => None,
    }
}

/// Returns true if the mouse event's position falls within the given area.
pub fn mouse_in_area(mouse: &crossterm::event::MouseEvent, area: Rect) -> bool {
    mouse.column >= area.x
        && mouse.column < area.x + area.width
        && mouse.row >= area.y
        && mouse.row < area.y + area.height
}
