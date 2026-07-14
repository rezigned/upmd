use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputTarget {
    Stdin,
    File(PathBuf),
    Directory(PathBuf),
}

pub fn resolve_input_target(file: &Option<String>) -> io::Result<InputTarget> {
    match file {
        Some(file) => {
            let path = PathBuf::from(file);
            let metadata = fs::metadata(&path)?;
            if metadata.is_dir() {
                Ok(InputTarget::Directory(path))
            } else {
                Ok(InputTarget::File(path))
            }
        }
        None => Ok(InputTarget::Stdin),
    }
}

/// Reads input data from file or stdin.
pub fn read_input(file: &Option<String>) -> io::Result<String> {
    match file {
        Some(file) => read_from_file(file),
        None => read_from_stdin(),
    }
}

/// Reads input from a path.
pub fn read_from_path(path: &Path) -> io::Result<String> {
    fs::read_to_string(path)
}

/// Reads input from file.
pub fn read_from_file(file: &str) -> io::Result<String> {
    read_from_path(Path::new(file))
}

/// Reads input from stdin.
pub fn read_from_stdin() -> io::Result<String> {
    let mut input = String::new();
    io::stdin().lock().read_to_string(&mut input)?;
    Ok(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_read_from_file_existing() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "# Hello\n\n```bash\necho test\n```\n").unwrap();
        let path = tmp.path().to_str().unwrap();
        let content = read_from_file(path).unwrap();
        assert!(content.contains("# Hello"));
        assert!(content.contains("echo test"));
    }

    #[test]
    fn test_read_from_file_missing() {
        let result = read_from_file("/nonexistent/path/upmd_test.md");
        assert!(result.is_err());
    }

    #[test]
    fn test_read_from_file_empty() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "").unwrap();
        let path = tmp.path().to_str().unwrap();
        let content = read_from_file(path).unwrap();
        assert!(content.is_empty());
    }

    #[test]
    fn test_read_input_with_file() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "content").unwrap();
        let path = tmp.path().to_str().unwrap().to_string();
        let content = read_input(&Some(path)).unwrap();
        assert_eq!(content, "content");
    }

    #[test]
    fn resolve_input_target_classifies_existing_files_and_directories() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let dir = tempfile::tempdir().unwrap();

        let file_arg = file.path().to_string_lossy().into_owned();
        let dir_arg = dir.path().to_string_lossy().into_owned();

        assert_eq!(
            resolve_input_target(&Some(file_arg)).unwrap(),
            InputTarget::File(file.path().to_path_buf())
        );
        assert_eq!(
            resolve_input_target(&Some(dir_arg)).unwrap(),
            InputTarget::Directory(dir.path().to_path_buf())
        );
    }

    #[test]
    #[ignore = "reads from stdin, requires interactive pipe"]
    fn test_read_input_with_none_uses_stdin() {
        let _ = read_input(&None);
    }
}
