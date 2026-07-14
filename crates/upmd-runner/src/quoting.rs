//! Shell quoting utilities for command and script assembly.
//!
//! All quoting functions unconditionally wrap the input -- they are safe for
//! interpolating untrusted strings into shell command lines.

use crate::ShellQuoteStyle;

/// Quotes a string for POSIX shell single-quote context.
///
/// Embedded single quotes are escaped with the standard `'\''` sequence.
pub fn posix_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Quotes a string for PowerShell single-quote context.
///
/// Embedded single quotes are escaped with PowerShell's `''` doubling.
pub fn powershell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Quotes a string for cmd.exe double-quote context.
///
/// Caret, percent, exclamation, and double-quote are escaped for cmd.exe
/// metacharacter handling inside double quotes.
pub fn cmd_quote(s: &str) -> String {
    let escaped = s
        .replace('"', "\"\"")
        .replace('^', "^^")
        .replace('%', "%%")
        .replace('!', "^!");
    format!("\"{}\"", escaped)
}

/// Conditionally quotes a string based on shell style and content.
///
/// Safe unquoted characters (ASCII alphanumeric, `_`, `.`, `-`, `/`) are
/// left unquoted; everything else is wrapped. Empty strings produce an
/// empty quoted string.
pub fn quote_if_needed(s: &str, style: ShellQuoteStyle) -> String {
    if s.is_empty() {
        return match style {
            ShellQuoteStyle::Posix | ShellQuoteStyle::PowerShell => "''".to_string(),
            ShellQuoteStyle::Cmd => "\"\"".to_string(),
        };
    }
    match style {
        ShellQuoteStyle::Posix => {
            if s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-' | '/'))
            {
                return s.to_string();
            }
            posix_quote(s)
        }
        ShellQuoteStyle::Cmd => cmd_quote(s),
        ShellQuoteStyle::PowerShell => {
            if s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-' | '/'))
            {
                return s.to_string();
            }
            powershell_quote(s)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_posix_quote_plain() {
        assert_eq!(posix_quote("hello"), "'hello'");
    }

    #[test]
    fn test_posix_quote_single_quote() {
        assert_eq!(posix_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn test_posix_quote_empty() {
        assert_eq!(posix_quote(""), "''");
    }

    #[test]
    fn test_cmd_quote_double_quote() {
        assert_eq!(cmd_quote(r#"say "hi""#), r#""say ""hi"""#);
    }

    #[test]
    fn test_cmd_quote_special_chars() {
        assert_eq!(cmd_quote("a^b"), r#""a^^b""#);
        assert_eq!(cmd_quote("a%b"), r#""a%%b""#);
        assert_eq!(cmd_quote("a!b"), r#""a^!b""#);
    }

    #[test]
    fn test_powershell_quote_single_quote() {
        assert_eq!(powershell_quote("it's"), "'it''s'");
    }

    #[test]
    fn test_quote_if_needed_plain() {
        assert_eq!(quote_if_needed("hello", ShellQuoteStyle::Posix), "hello");
        assert_eq!(quote_if_needed("", ShellQuoteStyle::Posix), "''");
    }

    #[test]
    fn test_quote_if_needed_with_space() {
        assert_eq!(
            quote_if_needed("hello world", ShellQuoteStyle::Posix),
            "'hello world'"
        );
    }

    #[test]
    fn test_quote_if_needed_with_single_quote() {
        assert_eq!(
            quote_if_needed("it's", ShellQuoteStyle::Posix),
            "'it'\\''s'"
        );
    }

    #[test]
    fn test_quote_if_needed_with_dollar() {
        assert_eq!(quote_if_needed("$HOME", ShellQuoteStyle::Posix), "'$HOME'");
    }

    #[test]
    fn test_quote_if_needed_cmd() {
        assert_eq!(
            quote_if_needed("path;args", ShellQuoteStyle::Cmd),
            "\"path;args\""
        );
    }

    #[test]
    fn test_quote_if_needed_powershell() {
        assert_eq!(
            quote_if_needed("it's", ShellQuoteStyle::PowerShell),
            "'it''s'"
        );
    }
}
