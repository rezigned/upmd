//! Lazy image decoding and terminal rendering for the preview pane.
//!
//! Decoded images are retained so protocols can be rebuilt after a resize.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use ratatui::Frame;
use ratatui_image::{
    picker::Picker,
    sliced::{SignedPosition, SlicedImage, SlicedProtocol},
    Resize,
};

/// Result produced by a runtime image-decoding command.
#[derive(Debug)]
pub(crate) enum DecodedImage {
    Loaded {
        path: PathBuf,
        image: image::DynamicImage,
    },
    Failed {
        path: PathBuf,
    },
}

enum State {
    Ready {
        sliced: SlicedProtocol,
        image: image::DynamicImage,
        width: usize,
    },
    Failed,
}

/// Loaded images keyed by resolved path.
pub struct ImageCache {
    picker: Option<Picker>,
    entries: HashMap<PathBuf, State>,
    loading: HashSet<PathBuf>,
    pending: Vec<PathBuf>,
    max_rows: u16,
    width: usize,
}

impl ImageCache {
    /// Creates an empty cache. Terminal protocol detection is deferred until
    /// the first decoded image is processed inside the alternate screen.
    pub fn new() -> Self {
        Self {
            picker: None,
            entries: HashMap::new(),
            loading: HashSet::new(),
            pending: Vec::new(),
            max_rows: 30,
            width: 0,
        }
    }

    /// Detects the terminal graphics protocol once.
    fn picker(&mut self) -> &Picker {
        if self.picker.is_none() {
            self.picker = Some(query_picker());
        }
        self.picker.as_ref().expect("picker was just initialized")
    }

    /// Starts decoding an uncached image.
    pub fn request(&mut self, src: &str, base_dir: &Path) {
        if is_url(src) {
            return;
        }
        let path = resolve_path(src, base_dir);
        if self.entries.contains_key(&path) || self.loading.contains(&path) {
            return;
        }
        self.loading.insert(path.clone());
        self.pending.push(path);
    }

    /// Takes decode work waiting to be scheduled by the runtime.
    pub fn take_requests(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.pending)
    }

    /// Applies one runtime decode result. Returns whether image rows changed.
    pub fn complete(&mut self, decoded: DecodedImage, width: usize) -> bool {
        let path = match &decoded {
            DecodedImage::Loaded { path, .. } | DecodedImage::Failed { path } => path,
        };
        if !self.loading.remove(path) {
            return false;
        }
        match decoded {
            DecodedImage::Loaded { path, image } => {
                let state = self.build(width, image);
                let ready = matches!(state, State::Ready { .. });
                self.entries.insert(path, state);
                ready
            }
            DecodedImage::Failed { path } => {
                self.entries.insert(path, State::Failed);
                false
            }
        }
    }

    /// Rebuilds cached protocols for a new width.
    pub fn set_width(&mut self, width: usize) {
        if width == self.width {
            return;
        }
        self.width = width;
        self.resize_all(width);
    }

    /// Builds a `SlicedProtocol` fitted into `width` columns, retaining the
    /// decoded image so the protocol can be resized later.
    fn build(&mut self, width: usize, image: image::DynamicImage) -> State {
        let size = ratatui::layout::Size::new(width.max(1) as u16, self.max_rows);
        match SlicedProtocol::new_with_resize(self.picker(), image.clone(), size, Resize::Fit(None))
        {
            Ok(sliced) => State::Ready {
                sliced,
                image,
                width,
            },
            Err(err) => {
                tracing::debug!("protocol for image: {err}");
                State::Failed
            }
        }
    }

    /// Recreates every ready protocol to fit `width`.
    fn resize_all(&mut self, width: usize) {
        let entries = std::mem::take(&mut self.entries);
        for (path, state) in entries {
            let state = match state {
                State::Ready {
                    image,
                    width: old_width,
                    ..
                } if old_width != width => self.build(width, image),
                state => state,
            };
            self.entries.insert(path, state);
        }
    }

    /// Returns the sliced protocol for an image, if it has finished loading.
    pub fn protocol(&self, src: &str, base_dir: &Path) -> Option<&SlicedProtocol> {
        if is_url(src) {
            return None;
        }
        let path = resolve_path(src, base_dir);
        match self.entries.get(&path) {
            Some(State::Ready { sliced, .. }) => Some(sliced),
            _ => None,
        }
    }

    /// Returns the number of terminal rows an image occupies.
    pub fn rows(&self, src: &str, base_dir: &Path) -> usize {
        self.protocol(src, base_dir)
            .map(|sliced| sliced.size().height.max(1) as usize)
            .unwrap_or(1)
    }

    /// Renders the image at `src` into `area`, positioned relative to it.
    pub fn render(
        &self,
        frame: &mut Frame,
        src: &str,
        base_dir: &Path,
        area: ratatui::layout::Rect,
        position: SignedPosition,
    ) {
        let Some(sliced) = self.protocol(src, base_dir) else {
            return;
        };
        frame.render_widget(SlicedImage::new(sliced, position), area);
    }
}

impl Default for ImageCache {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) fn decode_image(path: PathBuf) -> DecodedImage {
    match image::ImageReader::open(&path) {
        Ok(reader) => match reader.decode() {
            Ok(image) => DecodedImage::Loaded { path, image },
            Err(err) => {
                tracing::debug!("image {path:?}: decode failed: {err}");
                DecodedImage::Failed { path }
            }
        },
        Err(err) => {
            tracing::debug!("image {path:?}: open failed: {err}");
            DecodedImage::Failed { path }
        }
    }
}

/// Adds the tmux hint missing from ratatui-image when `TERM=screen-*`.
fn query_picker() -> Picker {
    let previous = std::env::var_os("TERM_PROGRAM");
    let needs_tmux_hint = std::env::var_os("TMUX").is_some()
        && !std::env::var("TERM").is_ok_and(|term| term.starts_with("tmux"))
        && previous.as_deref() != Some(std::ffi::OsStr::new("tmux"));
    if needs_tmux_hint {
        std::env::set_var("TERM_PROGRAM", "tmux");
    }
    let picker = Picker::from_query_stdio();
    if needs_tmux_hint {
        match previous {
            Some(value) => std::env::set_var("TERM_PROGRAM", value),
            None => std::env::remove_var("TERM_PROGRAM"),
        }
    }
    picker.unwrap_or_else(|err| {
        tracing::debug!("protocol query failed, using halfblocks: {err}");
        Picker::halfblocks()
    })
}

fn is_url(src: &str) -> bool {
    let Some((scheme, _)) = src.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    scheme.len() > 1
        && chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// Resolves an image source against its document directory.
fn resolve_path(src: &str, base_dir: &Path) -> PathBuf {
    let path = Path::new(src);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir.join(path)
    }
}

/// Returns the directory that relative image paths resolve against: the parent
/// of the markdown file, or the current directory when no file is configured.
pub fn image_base_dir(file: Option<&str>) -> std::path::PathBuf {
    file.map(Path::new)
        .and_then(|path| path.parent().map(|parent| parent.to_path_buf()))
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_path() {
        let base = Path::new("/repo/docs");
        let cases = [
            ("/abs/image.png", PathBuf::from("/abs/image.png")),
            ("img/a.png", PathBuf::from("/repo/docs/img/a.png")),
        ];
        for (src, expected) in cases {
            assert_eq!(resolve_path(src, base), expected);
        }
    }

    #[test]
    fn test_image_base_dir_from_file_parent() {
        let dir = image_base_dir(Some("/repo/docs/runbook.md"));
        assert_eq!(dir, PathBuf::from("/repo/docs"));
    }

    #[test]
    fn test_decode_request_lifecycle() {
        let base = Path::new("/repo/docs");
        let path = resolve_path("missing.png", base);
        let mut cache = ImageCache::new();

        cache.request("missing.png", base);
        cache.request("missing.png", base);
        assert_eq!(cache.take_requests(), vec![path.clone()]);
        assert!(cache.take_requests().is_empty());

        assert!(!cache.complete(DecodedImage::Failed { path }, 80));
        cache.request("missing.png", base);
        assert!(cache.take_requests().is_empty());
    }

    #[test]
    fn test_url_sources() {
        for src in [
            "HTTPS://example.com/image.png",
            "data:image/png;base64,AAAA",
            "file:///tmp/image.png",
        ] {
            assert!(is_url(src), "input: {src}");
            let mut cache = ImageCache::new();
            cache.request(src, Path::new("/repo/docs"));
            assert!(cache.loading.is_empty(), "input: {src}");
            assert!(cache.entries.is_empty(), "input: {src}");
        }

        for src in ["img/a.png", "/abs/image.png", r"C:\images\a.png"] {
            assert!(!is_url(src), "input: {src}");
        }
    }
}
