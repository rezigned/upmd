//! Language runner implementations and definitions for all supported languages.
//!
//! Shell quoting utilities live in [`crate::quoting`].

use crate::Kind;

pub use crate::quoting::{cmd_quote, posix_quote, powershell_quote};

pub mod c;
pub mod cmd;
pub mod go;
pub mod javascript;
pub mod php;
pub mod python;
pub mod ruby;
pub mod rust;
pub mod shell;
pub mod typescript;
pub mod zig;

// The `languages!` macro generates a struct per language, a shared lazy
// instance, and a declarative `REGISTRY` table with the lookup functions.
// `aliases` is listed first because it is reused for both the Language
// metadata and the registry entry, so it cannot be duplicated.
crate::languages!(
    JavaScript {
        aliases: &["js", "javascript", "node"],
        kind: Kind::Interpreted,
        syntax: "javascript".to_string(),
        binaries: &["node"],
        file_extension: "js",
        supports_inline: true,
        supports_file: true,
        package_manager: Some("npm")
    },
    TypeScript {
        aliases: &["ts", "typescript"],
        kind: Kind::Compiled,
        syntax: "typescript".to_string(),
        binaries: &["ts-node", "npx"],
        file_extension: "ts",
        supports_inline: false,
        supports_file: true,
        package_manager: Some("npm")
    },
    PHP {
        aliases: &["php"],
        kind: Kind::Interpreted,
        syntax: "php".to_string(),
        binaries: &["php"],
        file_extension: "php",
        supports_inline: true,
        supports_file: true,
        package_manager: Some("composer")
    },
    Python {
        aliases: &["py", "python", "python3"],
        kind: Kind::Interpreted,
        syntax: "python".to_string(),
        binaries: &["python3", "python"],
        file_extension: "py",
        supports_inline: true,
        supports_file: true,
        package_manager: Some("pip")
    },
    Ruby {
        aliases: &["rb", "ruby"],
        kind: Kind::Interpreted,
        syntax: "ruby".to_string(),
        binaries: &["ruby"],
        file_extension: "rb",
        supports_inline: true,
        supports_file: true,
        package_manager: Some("gem")
    },
    C {
        aliases: &["c"],
        kind: Kind::Compiled,
        syntax: "c".to_string(),
        binaries: &["gcc", "clang"],
        file_extension: "c",
        supports_inline: false,
        supports_file: true,
        package_manager: None
    },
    Go {
        aliases: &["go", "golang"],
        kind: Kind::Compiled,
        syntax: "go".to_string(),
        binaries: &["go"],
        file_extension: "go",
        supports_inline: false,
        supports_file: true,
        package_manager: Some("go")
    },
    Rust {
        aliases: &["rs", "rust"],
        kind: Kind::Compiled,
        syntax: "rust".to_string(),
        binaries: &["rustc", "cargo"],
        file_extension: "rs",
        supports_inline: false,
        supports_file: true,
        package_manager: Some("cargo")
    },
    Zig {
        aliases: &["zig"],
        kind: Kind::Compiled,
        syntax: "zig".to_string(),
        binaries: &["zig"],
        file_extension: "zig",
        supports_inline: false,
        supports_file: true,
        package_manager: None
    },
    Bash {
        aliases: &["bash"],
        kind: Kind::Shell,
        syntax: "sh".to_string(),
        binaries: &["bash"],
        file_extension: "sh",
        supports_inline: true,
        supports_file: true,
        package_manager: None
    },
    Fish {
        aliases: &["fish"],
        kind: Kind::Shell,
        syntax: "sh".to_string(),
        binaries: &["fish"],
        file_extension: "fish",
        supports_inline: true,
        supports_file: true,
        package_manager: None
    },
    Shell {
        aliases: &["sh", "shell"],
        kind: Kind::Shell,
        syntax: "sh".to_string(),
        binaries: &["sh"],
        file_extension: "sh",
        supports_inline: true,
        supports_file: true,
        package_manager: None
    },
    Zsh {
        aliases: &["zsh"],
        kind: Kind::Shell,
        syntax: "sh".to_string(),
        binaries: &["zsh"],
        file_extension: "sh",
        supports_inline: true,
        supports_file: true,
        package_manager: None
    },
    Cmd {
        aliases: &["cmd", "bat", "batch"],
        kind: Kind::Shell,
        syntax: "batch".to_string(),
        binaries: &["cmd"],
        file_extension: "bat",
        supports_inline: true,
        supports_file: true,
        package_manager: None
    },
    PowerShell {
        aliases: &["powershell", "ps1", "pwsh"],
        kind: Kind::Shell,
        syntax: "powershell".to_string(),
        binaries: &["powershell", "pwsh"],
        file_extension: "ps1",
        supports_inline: true,
        supports_file: true,
        package_manager: None
    }
);
