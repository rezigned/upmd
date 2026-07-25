use crate::nodes::Options;
use regex::Regex;
use std::collections::HashMap;
use std::sync::LazyLock;

fn attrs_regex() -> &'static Regex {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r#"(?x)
        # Attribute key: ASCII letter followed by word chars and hyphens
        (?P<ID>[a-zA-Z][\w\-]*)
        # Colon separator, optional whitespace
        \s*:\s*
        # Value: quoted (backslash escapes supported) or unquoted
        (?:
            # Quoted: double-quoted string allowing \" and \\ escapes
            "(?P<VALUE_QUOTED>[^"\\]*(?:\\.[^"\\]*)*)"
            |
            # Unquoted: word chars, -, +, #, /, ., ~
            # so paths like /usr/bin/zsh work without quotes
            (?P<VALUE_UNQUOTED>[\w+\#/\.\-~]+)
        )
        # Optional separator: comma with optional whitespace
        \s*(?:,\s*)?"#,
        )
        .unwrap()
    });
    &RE
}

/// Parses the full fence info string into Options.
///
/// ````markdown
/// ```sh [name:build, bin:zsh]
/// echo build
/// ```
/// ````
pub fn parse(input: &str) -> Result<Options, String> {
    let (language, attrs_input) = split_language(input);
    let attrs = parse_attrs(attrs_input)?;
    Ok(Options {
        language: language.to_string(),
        attrs,
    })
}

pub(crate) fn with_language(info: &str) -> Options {
    let (language, _) = split_language(info);
    Options {
        language: language.to_string(),
        attrs: HashMap::new(),
    }
}

/// Parses `key:value` pairs from the bracketed attribute section.
///
/// Returns the attribute map. The input is expected to be the text after
/// the language token, typically `[name:foo, bin:/usr/bin/zsh]`.
fn parse_attrs(input: &str) -> Result<HashMap<String, String>, String> {
    let mut map = HashMap::new();
    let regex = attrs_regex();

    // Track which byte ranges are consumed by valid attribute pairs.
    let mut covered = vec![false; input.len()];

    for caps in regex.captures_iter(input) {
        let m = caps.get(0).unwrap();
        for i in m.range() {
            covered[i] = true;
        }

        let id = caps.name("ID").unwrap().as_str().to_string();
        let value = caps
            .name("VALUE_QUOTED")
            .or_else(|| caps.name("VALUE_UNQUOTED"))
            .unwrap()
            .as_str()
            .to_string();
        map.insert(id, value);
    }

    // Check for text that isn't part of any attribute pair, bracket
    // structure, commas, or whitespace. This catches `[name:foo badvalue]`.
    let unconsumed: String = input
        .char_indices()
        .filter(|(i, _)| !covered[*i])
        .map(|(_, c)| c)
        .filter(|c| !c.is_whitespace() && *c != '[' && *c != ']' && *c != ',')
        .collect();
    if !unconsumed.is_empty() {
        return Err(format!("unrecognized attribute syntax: {unconsumed}"));
    }

    Ok(map)
}

pub(crate) fn parse_dependencies(input: Option<&str>) -> Result<Vec<Vec<String>>, String> {
    let Some(input) = input else {
        return Ok(Vec::new());
    };
    if input.trim().is_empty() {
        return Ok(Vec::new());
    }

    input
        .split(',')
        .map(|group| {
            group
                .split('|')
                .map(str::trim)
                .map(|name| {
                    if name.is_empty() {
                        Err("empty name in deps".to_string())
                    } else if name.chars().any(char::is_whitespace) {
                        Err(format!("dependency name contains whitespace: {name:?}"))
                    } else {
                        Ok(name.to_string())
                    }
                })
                .collect()
        })
        .collect()
}

/// Splits a fence info string into (language, attrs_section).
///
/// The language is everything before the first `[`. Both halves are trimmed.
fn split_language(input: &str) -> (&str, &str) {
    let input = input.trim();
    let lang_end = input.find('[').unwrap_or(input.len());
    (input[..lang_end].trim(), input[lang_end..].trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse() {
        let actual = parse(r"sh [name:build, bin:zsh, custom:x]").unwrap();

        assert_eq!("sh", actual.language);
        assert_eq!("build", actual.attrs["name"]);
        assert_eq!("zsh", actual.attrs["bin"]);
        assert_eq!("x", actual.attrs["custom"]);
    }

    #[test]
    fn test_parse_quoted_value() {
        let actual = parse(r#"python [name:"data processing", bin:"/usr/bin/python3"]"#).unwrap();

        assert_eq!("python", actual.language);
        assert_eq!("data processing", actual.attrs["name"]);
        assert_eq!("/usr/bin/python3", actual.attrs["bin"]);
    }

    #[test]
    fn test_parse_quoted_value_with_escaped_quote() {
        // Regex consumes \" as an escaped quote, so the captured value
        // includes the raw backslash (no unescaping is performed).
        let actual = parse(r#"sh [name:"abc\""]"#).unwrap();
        assert_eq!(actual.attrs["name"], r#"abc\""#);
    }

    #[test]
    fn test_parse_quoted_value_with_backslash_escape() {
        let actual = parse(r#"sh [name:"a\\b"]"#).unwrap();
        assert_eq!(actual.attrs["name"], r#"a\\b"#);
    }

    #[test]
    fn test_parse_lang_cpp() {
        let actual = parse("c++").unwrap();
        assert_eq!("c++", actual.language);
        assert!(actual.attrs.is_empty());
    }

    #[test]
    fn test_parse_lang_csharp() {
        let actual = parse("c#").unwrap();
        assert_eq!("c#", actual.language);
        assert!(actual.attrs.is_empty());
    }

    #[test]
    fn test_parse_lang_fsharp() {
        let actual = parse("f# [name:test]").unwrap();
        assert_eq!("f#", actual.language);
        assert_eq!("test", actual.attrs["name"]);
    }

    #[test]
    fn test_parse_attrs() {
        let expected: HashMap<String, String> =
            HashMap::from([("a".into(), "1".into()), ("b".into(), "1".into())]);

        [
            "[a:1,b:1]",
            "[a:1, b:1]",
            "[a:1 ,b:1]",
            "[a:1 , b:1]",
            "[ a:1 , b:1 ]",
            "[a :1,b :1]",
            "[a: 1,b: 1]",
            "[a : 1,b : 1]",
        ]
        .iter()
        .for_each(|input| {
            let actual = parse_attrs(input).unwrap();
            assert_eq!(expected, actual);
        });
    }

    #[test]
    fn test_parse_empty_input() {
        let opts = parse("").unwrap();
        assert_eq!(opts.language, "");
        assert!(opts.attrs.is_empty());
    }

    #[test]
    fn test_parse_language_with_attrs() {
        let opts = parse("python [name:test]").unwrap();
        assert_eq!(opts.language, "python");
        assert_eq!(opts.attrs["name"], "test");
    }

    #[test]
    fn test_parse_name_bin_attrs() {
        let opts = parse("sh [name:build, bin:zsh]").unwrap();
        assert_eq!(opts.attrs["name"], "build");
        assert_eq!(opts.attrs["bin"], "zsh");
    }

    #[test]
    fn test_parse_unquoted_path_value() {
        let opts = parse("bash [bin:/usr/bin/zsh]").unwrap();
        assert_eq!(opts.attrs["bin"], "/usr/bin/zsh");
    }

    #[test]
    fn test_parse_unquoted_home_path_value() {
        let opts = parse("bash [bin:~/.local/bin/zsh]").unwrap();
        assert_eq!(opts.attrs["bin"], "~/.local/bin/zsh");
    }

    #[test]
    fn test_parse_trailing_garbage_rejected() {
        let err = parse("sh [name:foo badvalue]").unwrap_err();
        assert!(
            err.contains("badvalue"),
            "Expected error mentioning badvalue, got: {err}"
        );
    }

    #[test]
    fn test_parse_interstitial_garbage_rejected() {
        let err = parse("sh [a:1 BAD b:2]").unwrap_err();
        assert!(
            err.contains("BAD"),
            "Expected error mentioning BAD, got: {err}"
        );
    }

    #[test]
    fn test_parse_dependencies() {
        for (input, expected) in [
            (None, vec![]),
            (Some(""), vec![]),
            (Some("setup"), vec![vec!["setup"]]),
            (Some("setup, build"), vec![vec!["setup"], vec!["build"]]),
            (
                Some("setup, build | lint, test"),
                vec![vec!["setup"], vec!["build", "lint"], vec!["test"]],
            ),
            (
                Some("  setup , build | lint  "),
                vec![vec!["setup"], vec!["build", "lint"]],
            ),
        ] {
            assert_eq!(parse_dependencies(input).unwrap(), expected);
        }

        for input in ["setup, , build", "setup || build", "setup build"] {
            assert!(parse_dependencies(Some(input)).is_err(), "{input:?}");
        }
    }
}
