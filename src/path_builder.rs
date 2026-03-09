use std::path::PathBuf;

use directories::UserDirs;

// Platform-aware path length limit.
// Windows: 260 chars (MAX_PATH) unless long-path opt-in is active.
// Linux/macOS: typically 4096, but we stay conservative.
#[cfg(target_os = "windows")]
const MAX_PATH_LEN: usize = 260;
#[cfg(not(target_os = "windows"))]
const MAX_PATH_LEN: usize = 4096;

/// Maximum byte-length of a single path component (NAME_MAX on most POSIX systems).
const MAX_COMPONENT_LEN: usize = 255;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathBuilderError {
    #[error("invalid path component: {0:?}")]
    InvalidComponent(String),
    #[error("base path must be absolute, got: {0:?}")]
    BaseNotAbsolute(PathBuf),
    #[error("no filename was specified")]
    MissingFilename,
    #[error("the special folder could not be resolved")]
    UnresolvableSpecialFolder,
    /// Returned when the assembled path would exceed the platform limit.
    #[error("assembled path exceeds maximum allowed length ({MAX_PATH_LEN} bytes)")]
    PathTooLong,
    /// Returned when a single component is too long.
    #[error("component exceeds maximum allowed length ({MAX_COMPONENT_LEN} bytes): {0:?}")]
    ComponentTooLong(String),
}

// ---------------------------------------------------------------------------
// Reserved name / forbidden character helpers
// ---------------------------------------------------------------------------

fn is_windows_reserved(name: &str) -> bool {
    // Only the stem (before the first dot) is compared.
    let stem = name.split('.').next().unwrap_or(name);
    matches!(
        stem.to_ascii_uppercase().as_str(),
        "CON" | "PRN" | "AUX" | "NUL"
            | "COM0" | "COM1" | "COM2" | "COM3" | "COM4" | "COM5"
            | "COM6" | "COM7" | "COM8" | "COM9"
            | "LPT0" | "LPT1" | "LPT2" | "LPT3" | "LPT4" | "LPT5"
            | "LPT6" | "LPT7" | "LPT8" | "LPT9"
    )
}

#[inline]
fn is_forbidden_char(c: char) -> bool {
    // Covers Windows-forbidden chars + path separators + NUL.
    // NUL (U+0000) is forbidden on every OS.
    matches!(c, '/' | '\\' | '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\0')
        // Also reject all other ASCII control characters.
        || (c.is_ascii() && c.is_ascii_control())
}

/// Validates a single filesystem component (directory name, filename stem, or extension).
///
/// Rules applied (cross-platform conservative superset):
/// * Must not be empty or whitespace-only.
/// * Must not be `.` or `..` (directory traversal).
/// * Must not contain any forbidden character.
/// * Must not end with a dot or ASCII space (Windows limitation).
/// * Must not be a Windows reserved device name.
/// * Must not exceed [`MAX_COMPONENT_LEN`] bytes (UTF-8).
fn validate_component(s: &str) -> Result<(), PathBuilderError> {
    let err = || PathBuilderError::InvalidComponent(s.to_owned());

    if s.trim().is_empty() || s == "." || s == ".." {
        return Err(err());
    }
    if s.chars().any(is_forbidden_char) {
        return Err(err());
    }
    // Windows rejects trailing dots and spaces.
    if s.ends_with('.') || s.ends_with(' ') {
        return Err(err());
    }
    if is_windows_reserved(s) {
        return Err(err());
    }
    // Length check (bytes, not chars – filesystems work on bytes).
    if s.len() > MAX_COMPONENT_LEN {
        return Err(PathBuilderError::ComponentTooLong(s.to_owned()));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// SpecialFolder
// ---------------------------------------------------------------------------

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecialFolder {
    Desktop,
    Downloads,
    Documents,
    Pictures,
    Home,
}

// ---------------------------------------------------------------------------
// Internal target enum – replaces the unsound fn-pointer approach
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum VecTarget {
    Directories,
    FilenameParts,
    ExtensionParts,
}

// ---------------------------------------------------------------------------
// PathBuilder
// ---------------------------------------------------------------------------

/// A type-safe, cross-platform builder for constructing file-system paths.
///
/// ```
/// # use your_crate::{PathBuilder, SpecialFolder};
/// let path = PathBuilder::new()
///     .with_special_folder(SpecialFolder::Documents)
///     .with_directory("MyApp")
///     .with_filename_part("report")
///     .with_filename_part("2024")
///     .with_extension("pdf")
///     .build()
///     .unwrap();
/// // → /home/alice/Documents/MyApp/report.2024.pdf
/// ```
#[must_use = "call .build() to obtain the final PathBuf"]
#[derive(Debug, Clone, Default)]
pub struct PathBuilder {
    base: Option<PathBuf>,
    directories: Vec<String>,
    /// Joined with `.` to form the filename stem.
    filename_parts: Vec<String>,
    /// Each entry is one extension segment; joined with `.`.
    extension_parts: Vec<String>,
    error: Option<PathBuilderError>,
}

impl PathBuilder {
    /// Creates a new, empty [`PathBuilder`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    fn has_error(&self) -> bool {
        self.error.is_some()
    }

    // ------------------------------------------------------------------
    // Special-folder resolution
    // ------------------------------------------------------------------

    fn resolve_special_folder(folder: SpecialFolder) -> Option<PathBuf> {
        // `UserDirs::new()` returns an owned value; we must call `to_path_buf()`
        // before it is dropped so that we don't hold a dangling reference.
        let user_dirs = UserDirs::new()?;

        let path: &std::path::Path = match folder {
            SpecialFolder::Desktop => user_dirs.desktop_dir()?,
            SpecialFolder::Downloads => user_dirs.download_dir()?,
            SpecialFolder::Documents => user_dirs.document_dir()?,
            SpecialFolder::Pictures => user_dirs.picture_dir()?,
            // `home_dir()` returns `&Path`, not `Option<&Path>`.
            SpecialFolder::Home => user_dirs.home_dir(),
        };

        Some(path.to_path_buf())
    }

    // ------------------------------------------------------------------
    // Generic validated push (uses enum instead of fn pointer)
    // ------------------------------------------------------------------

    fn push_validated(mut self, value: impl Into<String>, target: VecTarget) -> Self {
        if self.has_error() {
            return self;
        }

        let value = value.into();
        // Silently skip empty strings – callers don't need to guard these.
        if value.is_empty() {
            return self;
        }

        match validate_component(&value) {
            Ok(()) => match target {
                VecTarget::Directories => self.directories.push(value),
                VecTarget::FilenameParts => self.filename_parts.push(value),
                VecTarget::ExtensionParts => self.extension_parts.push(value),
            },
            Err(err) => self.error = Some(err),
        }

        self
    }

    // ------------------------------------------------------------------
    // Filename assembly
    // ------------------------------------------------------------------

    fn build_filename(&self) -> Result<String, PathBuilderError> {
        if self.filename_parts.is_empty() {
            return Err(PathBuilderError::MissingFilename);
        }

        let stem = self.filename_parts.join(".");
        let extension = self.extension_parts.join(".");

        Ok(if extension.is_empty() {
            stem
        } else {
            format!("{stem}.{extension}")
        })
    }

    // ------------------------------------------------------------------
    // Builder methods
    // ------------------------------------------------------------------

    /// Sets the base path to the given well-known special folder.
    ///
    /// Overwrites any previously set base. Fails if the folder cannot be
    /// resolved on the current system.
    pub fn with_special_folder(mut self, folder: SpecialFolder) -> Self {
        if self.has_error() {
            return self;
        }
        match Self::resolve_special_folder(folder) {
            Some(path) => self.base = Some(path),
            None => self.error = Some(PathBuilderError::UnresolvableSpecialFolder),
        }
        self
    }

    /// Sets the base path to an absolute path.
    ///
    /// Overwrites any previously set base. Returns an error if the path is
    /// not absolute.
    pub fn with_base_path(mut self, path: impl Into<PathBuf>) -> Self {
        if self.has_error() {
            return self;
        }
        let path = path.into();
        if path.is_absolute() {
            self.base = Some(path);
        } else {
            self.error = Some(PathBuilderError::BaseNotAbsolute(path));
        }
        self
    }

    /// Appends a single sub-directory component.
    pub fn with_directory(self, directory: impl Into<String>) -> Self {
        self.push_validated(directory, VecTarget::Directories)
    }

    /// Appends multiple sub-directory components in order.
    ///
    /// Stops and records the first validation error encountered.
    pub fn with_directories(mut self, dirs: impl IntoIterator<Item = impl Into<String>>) -> Self {
        for dir in dirs {
            self = self.with_directory(dir);
            if self.has_error() {
                break;
            }
        }
        self
    }

    /// Appends a part of the filename stem.
    ///
    /// Parts are joined with `.` (e.g. `"report"` + `"2024"` → `"report.2024"`).
    /// The value must not itself contain a dot; pass each segment separately.
    pub fn with_filename_part(self, part: impl Into<String>) -> Self {
        let part = part.into();
        // Reject parts that already contain dots to keep the join unambiguous.
        if part.contains('.') {
            if !self.has_error() {
                return Self {
                    error: Some(PathBuilderError::InvalidComponent(part)),
                    ..self
                };
            }
            return self;
        }
        self.push_validated(part, VecTarget::FilenameParts)
    }

    /// Appends a file extension **segment**.
    ///
    /// Call once for simple extensions (`"pdf"`) or multiple times for
    /// compound extensions (`"tar"` then `"gz"` → `.tar.gz`).
    /// The value must not contain a dot.
    pub fn with_extension(self, ext: impl Into<String>) -> Self {
        let ext = ext.into();
        if ext.contains('.') {
            if !self.has_error() {
                return Self {
                    error: Some(PathBuilderError::InvalidComponent(ext)),
                    ..self
                };
            }
            return self;
        }
        self.push_validated(ext, VecTarget::ExtensionParts)
    }

    /// Assembles and returns the final [`PathBuf`].
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// * any previous builder call recorded an error,
    /// * no filename part was added,
    /// * the resulting path exceeds the platform length limit.
    pub fn build(self) -> Result<PathBuf, PathBuilderError> {
        if let Some(err) = self.error {
            return Err(err);
        }

        let filename = self.build_filename()?;
        let mut path = self.base.unwrap_or_else(|| PathBuf::from("."));

        for dir in &self.directories {
            path.push(dir);
        }
        path.push(&filename);

        // Guard against excessively long paths.
        let path_str = path.to_string_lossy();
        if path_str.len() > MAX_PATH_LEN {
            return Err(PathBuilderError::PathTooLong);
        }

        Ok(path)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn absolute_base() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(r"C:\Users\test")
        } else {
            PathBuf::from("/tmp/test")
        }
    }

    #[test]
    fn simple_path() {
        let path = PathBuilder::new()
            .with_base_path(absolute_base())
            .with_directory("docs")
            .with_filename_part("readme")
            .with_extension("md")
            .build()
            .unwrap();

        assert_eq!(path, absolute_base().join("docs").join("readme.md"));
    }

    #[test]
    fn compound_extension() {
        let path = PathBuilder::new()
            .with_base_path(absolute_base())
            .with_filename_part("archive")
            .with_extension("tar")
            .with_extension("gz")
            .build()
            .unwrap();

        assert_eq!(path, absolute_base().join("archive.tar.gz"));
    }

    #[test]
    fn missing_filename_error() {
        let err = PathBuilder::new()
            .with_base_path(absolute_base())
            .build()
            .unwrap_err();

        assert_eq!(err, PathBuilderError::MissingFilename);
    }

    #[test]
    fn relative_base_error() {
        let err = PathBuilder::new()
            .with_base_path(PathBuf::from("relative/path"))
            .with_filename_part("file")
            .build()
            .unwrap_err();

        assert!(matches!(err, PathBuilderError::BaseNotAbsolute(_)));
    }

    #[test]
    fn dotdot_traversal_rejected() {
        let err = PathBuilder::new()
            .with_base_path(absolute_base())
            .with_directory("..")
            .with_filename_part("file")
            .build()
            .unwrap_err();

        assert!(matches!(err, PathBuilderError::InvalidComponent(_)));
    }

    #[test]
    fn windows_reserved_name_rejected() {
        let err = PathBuilder::new()
            .with_base_path(absolute_base())
            .with_filename_part("CON")
            .build()
            .unwrap_err();

        assert!(matches!(err, PathBuilderError::InvalidComponent(_)));
    }

    #[test]
    fn dot_in_filename_part_rejected() {
        let err = PathBuilder::new()
            .with_base_path(absolute_base())
            .with_filename_part("foo.bar") // dots must not appear inside a single part
            .build()
            .unwrap_err();

        assert!(matches!(err, PathBuilderError::InvalidComponent(_)));
    }

    #[test]
    fn dot_in_extension_rejected() {
        let err = PathBuilder::new()
            .with_base_path(absolute_base())
            .with_filename_part("archive")
            .with_extension("tar.gz") // must use two separate .with_extension() calls
            .build()
            .unwrap_err();

        assert!(matches!(err, PathBuilderError::InvalidComponent(_)));
    }

    #[test]
    fn component_too_long_rejected() {
        let long = "a".repeat(MAX_COMPONENT_LEN + 1);
        let err = PathBuilder::new()
            .with_base_path(absolute_base())
            .with_filename_part(&long)
            .build()
            .unwrap_err();

        assert!(matches!(err, PathBuilderError::ComponentTooLong(_)));
    }

    #[test]
    fn control_char_rejected() {
        let err = PathBuilder::new()
            .with_base_path(absolute_base())
            .with_filename_part("bad\x01name")
            .build()
            .unwrap_err();

        assert!(matches!(err, PathBuilderError::InvalidComponent(_)));
    }

    #[test]
    fn first_error_wins() {
        // Once an error is set, subsequent calls must not overwrite it.
        let err = PathBuilder::new()
            .with_base_path(PathBuf::from("not/absolute")) // sets BaseNotAbsolute
            .with_directory("..")                           // would set InvalidComponent
            .with_filename_part("file")
            .build()
            .unwrap_err();

        assert!(matches!(err, PathBuilderError::BaseNotAbsolute(_)));
    }
}