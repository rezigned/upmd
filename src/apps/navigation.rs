use keymap::KeyMap;

#[derive(Debug, Clone, Copy, KeyMap, PartialEq, Eq, Hash)]
pub enum Navigation {
    /// Move to next item
    #[key("down", "ctrl-n", "j")]
    Next,
    /// Move to previous item
    #[key("up", "ctrl-p", "k")]
    Prev,
    /// Move to first item
    #[key("g")]
    First,
    /// Move to last item
    #[key("shift-G", "G")]
    Last,
    /// Page up
    #[key("pageup", "ctrl-b")]
    PageUp,
    /// Page down
    #[key("pagedown", "ctrl-f")]
    PageDown,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
    use keymap::KeyMapConfig;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::empty(),
        }
    }

    #[test]
    fn test_navigation_next_keys() {
        let derived = Navigation::keymap_config();
        assert_eq!(
            derived.get(&key(KeyCode::Down, KeyModifiers::NONE)),
            Some(&Navigation::Next)
        );
        assert_eq!(
            derived.get(&key(KeyCode::Char('j'), KeyModifiers::NONE)),
            Some(&Navigation::Next)
        );
    }

    #[test]
    fn test_navigation_prev_keys() {
        let derived = Navigation::keymap_config();
        assert_eq!(
            derived.get(&key(KeyCode::Up, KeyModifiers::NONE)),
            Some(&Navigation::Prev)
        );
        assert_eq!(
            derived.get(&key(KeyCode::Char('k'), KeyModifiers::NONE)),
            Some(&Navigation::Prev)
        );
    }

    #[test]
    fn test_navigation_first_is_g() {
        let derived = Navigation::keymap_config();
        assert_eq!(
            derived.get(&key(KeyCode::Char('g'), KeyModifiers::NONE)),
            Some(&Navigation::First)
        );
    }

    #[test]
    fn test_navigation_last_is_shift_g() {
        let derived = Navigation::keymap_config();
        assert_eq!(
            derived.get(&key(KeyCode::Char('G'), KeyModifiers::SHIFT)),
            Some(&Navigation::Last)
        );
    }

    #[test]
    fn test_navigation_page_up_down() {
        let derived = Navigation::keymap_config();
        assert_eq!(
            derived.get(&key(KeyCode::PageUp, KeyModifiers::NONE)),
            Some(&Navigation::PageUp)
        );
        assert_eq!(
            derived.get(&key(KeyCode::PageDown, KeyModifiers::NONE)),
            Some(&Navigation::PageDown)
        );
    }

    #[test]
    fn test_navigation_ctrl_bindings() {
        let derived = Navigation::keymap_config();
        assert_eq!(
            derived.get(&key(KeyCode::Char('n'), KeyModifiers::CONTROL)),
            Some(&Navigation::Next)
        );
        assert_eq!(
            derived.get(&key(KeyCode::Char('p'), KeyModifiers::CONTROL)),
            Some(&Navigation::Prev)
        );
    }

    #[test]
    fn test_navigation_unknown_key_returns_none() {
        let derived = Navigation::keymap_config();
        assert_eq!(
            derived.get(&key(KeyCode::Char('x'), KeyModifiers::NONE)),
            None
        );
    }
}
