//! File utilities for reading local files and detecting their types.

use std::path::Path;

/// Information about a local file
#[derive(Debug, Clone)]
pub struct FileInfo {
    /// File extension (without dot), empty string if none
    pub extension: String,
    /// MIME type
    pub mime_type: String,
    /// Whether this is a text file (vs binary)
    pub is_text: bool,
}

impl FileInfo {
    /// Create FileInfo from a file path
    pub fn from_path(path: &Path) -> Self {
        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_string();

        let mime_type = guess_mime_type(path);
        let is_text = is_text_mime_type(&mime_type);

        Self {
            extension,
            mime_type,
            is_text,
        }
    }
}

/// Guess the MIME type from a file path
pub fn guess_mime_type(path: &Path) -> String {
    mime_guess::from_path(path)
        .first()
        .map(|m| m.to_string())
        .unwrap_or_else(|| "application/octet-stream".to_string())
}

/// Determine if a MIME type represents a text file
///
/// Text files include:
/// - All text/* types
/// - application/json
/// - application/javascript
/// - application/typescript
/// - application/xml
/// - Various other application/* types that are text-based
pub fn is_text_mime_type(mime_type: &str) -> bool {
    if mime_type.starts_with("text/") {
        return true;
    }

    // Common text-based application types
    matches!(
        mime_type,
        "application/json"
            | "application/javascript"
            | "application/typescript"
            | "application/xml"
            | "application/xhtml+xml"
            | "application/x-yaml"
            | "application/yaml"
            | "application/toml"
            | "application/x-toml"
            | "application/x-sh"
            | "application/x-shellscript"
            | "application/graphql"
            | "application/ld+json"
            | "application/manifest+json"
            | "application/schema+json"
    )
}

/// Get Unix file permissions (mode) from a path
#[cfg(unix)]
pub fn get_file_permissions(path: &Path) -> std::io::Result<u32> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = std::fs::metadata(path)?;
    Ok(metadata.permissions().mode() & 0o777)
}

#[cfg(not(unix))]
pub fn get_file_permissions(_path: &Path) -> std::io::Result<u32> {
    // Default to 644 on non-Unix systems
    Ok(0o644)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;

    #[test]
    fn test_guess_mime_type_txt() {
        let path = PathBuf::from("test.txt");
        assert_eq!(guess_mime_type(&path), "text/plain");
    }

    #[test]
    fn test_guess_mime_type_json() {
        let path = PathBuf::from("config.json");
        assert_eq!(guess_mime_type(&path), "application/json");
    }

    #[test]
    fn test_guess_mime_type_js() {
        let path = PathBuf::from("script.js");
        // mime_guess returns text/javascript for .js files
        let mime = guess_mime_type(&path);
        assert!(mime == "text/javascript" || mime == "application/javascript");
    }

    #[test]
    fn test_guess_mime_type_rs() {
        let path = PathBuf::from("main.rs");
        let mime = guess_mime_type(&path);
        // Rust files may be detected as text/x-rust or text/plain
        assert!(mime.starts_with("text/"));
    }

    #[test]
    fn test_guess_mime_type_unknown() {
        let path = PathBuf::from("file.unknownext");
        assert_eq!(guess_mime_type(&path), "application/octet-stream");
    }

    #[test]
    fn test_guess_mime_type_png() {
        let path = PathBuf::from("image.png");
        assert_eq!(guess_mime_type(&path), "image/png");
    }

    #[test]
    fn test_is_text_mime_type() {
        assert!(is_text_mime_type("text/plain"));
        assert!(is_text_mime_type("text/html"));
        assert!(is_text_mime_type("text/css"));
        assert!(is_text_mime_type("application/json"));
        assert!(is_text_mime_type("application/javascript"));
        assert!(is_text_mime_type("application/xml"));

        assert!(!is_text_mime_type("image/png"));
        assert!(!is_text_mime_type("application/octet-stream"));
        assert!(!is_text_mime_type("application/pdf"));
    }

    #[test]
    fn test_file_info_from_path() {
        let path = PathBuf::from("document.txt");
        let info = FileInfo::from_path(&path);

        assert_eq!(info.extension, "txt");
        assert_eq!(info.mime_type, "text/plain");
        assert!(info.is_text);
    }

    #[test]
    fn test_file_info_no_extension() {
        let path = PathBuf::from("Makefile");
        let info = FileInfo::from_path(&path);

        assert_eq!(info.extension, "");
    }

    #[test]
    #[cfg(unix)]
    fn test_get_file_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();

        // Set specific permissions
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let perms = get_file_permissions(path).unwrap();
        assert_eq!(perms, 0o644);
    }
}
