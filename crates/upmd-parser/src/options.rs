use crate::nodes::{Dependencies, DepsGroups, Options};
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

/// Parses a fence info string like `"sh [name:build, bin:zsh]"`.
///
/// ```text
/// options = language '[' attrs ']'
/// attrs   = attr (',' attr)*
/// attr    = key ':' value
/// ```
pub fn parse(input: &str) -> Options {
    let (language, attrs_input) = split_language(input);
    let (attrs, errors) = parse_attrs(attrs_input);
    let name = attrs.get("name").cloned().unwrap_or_default();
    let deps = Dependencies(parse_deps(attrs.get("deps").map(String::as_str)));

    Options {
        language: language.to_string(),
        name,
        deps,
        attrs,
        errors,
    }
}

/// Parses `key: value` pairs from the bracketed attribute section.
///
/// ```text
/// attrs = attr (',' attr)*
/// attr  = key ':' value
/// ```
fn parse_attrs(input: &str) -> (HashMap<String, String>, Vec<String>) {
    let mut map = HashMap::new();
    let regex = attrs_regex();
    let mut errors = Vec::new();

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
        errors.push(format!("unrecognized attribute syntax: {unconsumed}"));
    }

    (map, errors)
}

/// Parses a `[deps]` value into groups.
///
/// ```text
/// deps  = group (',' group)*
/// group = name ('|' name)*
/// ```
///
/// Example: `"B | C, A"` → `[[B, C], [A]]`.
pub(crate) fn parse_deps(input: Option<&str>) -> Result<DepsGroups, String> {
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

/// Splits a fence info string into (language, attrs).
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
        for (input, expected_lang, expected_attrs) in [
            (
                "sh [name:build, bin:zsh, custom:x]",
                "sh",
                vec![("name", "build"), ("bin", "zsh"), ("custom", "x")],
            ),
            (
                r#"python [name:"data processing", bin:"/usr/bin/python3"]"#,
                "python",
                vec![("name", "data processing"), ("bin", "/usr/bin/python3")],
            ),
            (r#"sh [name:"abc\""]"#, "sh", vec![("name", r#"abc\""#)]),
            (r#"sh [name:"a\\b"]"#, "sh", vec![("name", r#"a\\b"#)]),
            ("c++", "c++", vec![]),
            ("c#", "c#", vec![]),
            ("f# [name:test]", "f#", vec![("name", "test")]),
            ("", "", vec![]),
            ("python [name:test]", "python", vec![("name", "test")]),
            (
                "sh [name:build, bin:zsh]",
                "sh",
                vec![("name", "build"), ("bin", "zsh")],
            ),
            (
                "bash [bin:/usr/bin/zsh]",
                "bash",
                vec![("bin", "/usr/bin/zsh")],
            ),
            (
                "bash [bin:~/.local/bin/zsh]",
                "bash",
                vec![("bin", "~/.local/bin/zsh")],
            ),
        ] {
            let opts = parse(input);
            assert_eq!(opts.language, expected_lang, "input: {input:?}");
            let attrs: HashMap<String, String> = expected_attrs
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            assert_eq!(opts.attrs, attrs, "input: {input:?}");
            assert!(opts.errors.is_empty(), "input: {input:?}");
        }

        for (input, expected_err, expected_attrs) in [
            ("sh [name:foo badvalue]", "badvalue", vec![("name", "foo")]),
            ("sh [a:1 BAD b:2]", "BAD", vec![("a", "1"), ("b", "2")]),
        ] {
            let opts = parse(input);
            assert!(
                opts.errors.iter().any(|error| error.contains(expected_err)),
                "input: {input:?}, errors: {:?}",
                opts.errors
            );
            for (key, value) in expected_attrs {
                assert_eq!(opts.attrs.get(key).map(String::as_str), Some(value));
            }
        }
    }

    #[test]
    fn test_parse_attrs_variants() {
        let expected: HashMap<String, String> =
            HashMap::from([("a".into(), "1".into()), ("b".into(), "1".into())]);

        for input in [
            "[a:1,b:1]",
            "[a:1, b:1]",
            "[a:1 ,b:1]",
            "[a:1 , b:1]",
            "[ a:1 , b:1 ]",
            "[a :1,b :1]",
            "[a: 1,b: 1]",
            "[a : 1,b : 1]",
        ] {
            let (actual, errors) = parse_attrs(input);
            assert_eq!(expected, actual);
            assert!(errors.is_empty(), "input: {input:?}");
        }
    }

    #[test]
    fn test_parse_retains_valid_attrs_when_dependencies_are_invalid() {
        let opts = parse(r#"sh [name:build, deps:"setup || test", bin:zsh]"#);

        assert_eq!(opts.name, "build");
        assert_eq!(opts.attrs.get("bin").map(String::as_str), Some("zsh"));
        assert!(opts.deps.is_err());
        assert!(opts.errors.is_empty());
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
            assert_eq!(parse_deps(input).unwrap(), expected);
        }

        for input in ["setup, , build", "setup || build", "setup build"] {
            assert!(parse_deps(Some(input)).is_err(), "{input:?}");
        }
    }
}
