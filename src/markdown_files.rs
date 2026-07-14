use std::path::{Path, PathBuf};

use anyhow::Context;
use ignore::WalkBuilder;

const DEFAULT_MAX_DEPTH: usize = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownFile {
    pub path: PathBuf,
    pub display: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarkdownSearchOptions {
    pub max_depth: usize,
    pub include_hidden: bool,
    pub respect_ignore: bool,
}

impl Default for MarkdownSearchOptions {
    fn default() -> Self {
        Self {
            max_depth: DEFAULT_MAX_DEPTH,
            include_hidden: false,
            respect_ignore: true,
        }
    }
}

pub fn find_markdown_files(
    root: &Path,
    opts: MarkdownSearchOptions,
) -> anyhow::Result<Vec<MarkdownFile>> {
    let root = root
        .canonicalize()
        .with_context(|| format!("Failed to canonicalize {}", root.display()))?;

    let mut builder = WalkBuilder::new(&root);
    builder
        .max_depth(Some(opts.max_depth))
        .hidden(!opts.include_hidden)
        .ignore(opts.respect_ignore)
        .git_ignore(opts.respect_ignore)
        .git_global(opts.respect_ignore)
        .git_exclude(opts.respect_ignore)
        .parents(opts.respect_ignore)
        .follow_links(false);

    let mut files = Vec::new();
    for entry in builder.build().flatten() {
        let path = entry.path();
        if !entry.file_type().is_some_and(|ft| ft.is_file()) || !is_markdown_path(path) {
            continue;
        }

        let display = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .display()
            .to_string();

        files.push(MarkdownFile {
            path: path.to_path_buf(),
            display,
        });
    }

    files.sort_by(|a, b| a.display.cmp(&b.display));
    Ok(files)
}

pub fn is_markdown_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "md" | "markdown" | "mdown" | "mkd"
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn finds_supported_markdown_extensions_as_sorted_relative_paths() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("docs")).unwrap();
        fs::write(tmp.path().join("b.mdown"), "# B").unwrap();
        fs::write(tmp.path().join("a.markdown"), "# A").unwrap();
        fs::write(tmp.path().join("readme.md"), "# Readme").unwrap();
        fs::write(tmp.path().join("docs").join("guide.MKD"), "# Guide").unwrap();
        fs::write(tmp.path().join("note.txt"), "no").unwrap();

        let files = find_markdown_files(
            tmp.path(),
            MarkdownSearchOptions {
                max_depth: 6,
                include_hidden: false,
                respect_ignore: true,
            },
        )
        .unwrap();
        let names: Vec<_> = files.iter().map(|f| f.display.as_str()).collect();
        let nested = Path::new("docs").join("guide.MKD").display().to_string();
        assert_eq!(
            names,
            vec!["a.markdown", "b.mdown", nested.as_str(), "readme.md"]
        );
        assert!(files.iter().all(|file| file.path.is_absolute()));
    }

    #[test]
    fn respects_max_depth() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("a").join("b");
        fs::create_dir_all(&nested).unwrap();
        fs::write(tmp.path().join("root.md"), "# Root").unwrap();
        fs::write(nested.join("deep.md"), "# Deep").unwrap();

        let files = find_markdown_files(
            tmp.path(),
            MarkdownSearchOptions {
                max_depth: 2,
                ..Default::default()
            },
        )
        .unwrap();
        let names: Vec<_> = files.iter().map(|f| f.display.as_str()).collect();
        assert_eq!(names, vec!["root.md"]);
    }

    #[test]
    fn default_search_excludes_hidden_and_ignored_files() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join(".git")).unwrap();
        fs::create_dir_all(tmp.path().join("ignored-dir")).unwrap();
        fs::create_dir_all(tmp.path().join(".hidden-dir")).unwrap();
        fs::write(tmp.path().join(".gitignore"), "ignored.md\nignored-dir/\n").unwrap();
        fs::write(tmp.path().join("visible.md"), "# Visible").unwrap();
        fs::write(tmp.path().join("ignored.md"), "# Ignored").unwrap();
        fs::write(
            tmp.path().join("ignored-dir").join("nested.md"),
            "# Ignored",
        )
        .unwrap();
        fs::write(tmp.path().join(".hidden.md"), "# Hidden").unwrap();
        fs::write(tmp.path().join(".hidden-dir").join("secret.md"), "# Hidden").unwrap();

        let files = find_markdown_files(tmp.path(), MarkdownSearchOptions::default()).unwrap();
        let names: Vec<_> = files.iter().map(|f| f.display.as_str()).collect();
        assert_eq!(names, vec!["visible.md"]);
    }
}
