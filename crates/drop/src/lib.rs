//! Pure drop planning. Execution and privilege confirmation belong to the caller.
use std::fmt;

/// Encode bounded clipboard RGBA without retaining desktop image data on disk.
pub fn screenshot_png(
    width: u32,
    height: u32,
    rgba: &[u8],
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    use image::ImageEncoder;
    let bytes = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|n| n.checked_mul(4))
        .ok_or("image dimensions overflow")?;
    if width == 0 || height == 0 || bytes > 64 * 1024 * 1024 || bytes != rgba.len() as u64 {
        return Err("clipboard image must be nonempty RGBA and at most 64 MiB".into());
    }
    let mut output = Vec::new();
    image::codecs::png::PngEncoder::new(&mut output).write_image(
        rgba,
        width,
        height,
        image::ExtendedColorType::Rgba8,
    )?;
    Ok(output)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    Posix,
    PowerShell,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidPath;
impl fmt::Display for InvalidPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("path contains unsupported control characters or traversal")
    }
}
impl std::error::Error for InvalidPath {}

/// Quote one literal argument, never a command or shell expansion.
pub fn quote(path: &str, shell: Shell) -> Result<String, InvalidPath> {
    if path.chars().any(char::is_control) {
        return Err(InvalidPath);
    }
    Ok(match shell {
        Shell::Posix => format!("'{}'", path.replace('\'', "'\\''")),
        Shell::PowerShell => format!("'{}'", path.replace('\'', "''")),
    })
}

pub fn join_remote(directory: &str, filename: &str) -> Result<String, InvalidPath> {
    if filename.is_empty()
        || filename == "."
        || filename == ".."
        || filename.contains(['/', '\\'])
        || filename.chars().any(char::is_control)
        || directory.chars().any(char::is_control)
    {
        return Err(InvalidPath);
    }
    Ok(format!("{}/{}", directory.trim_end_matches('/'), filename))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DropTarget {
    /// Only use a folder positively reported by shell integration at a prompt.
    ShellPrompt { directory: String },
    /// Foreground application or unknown shell state: private staging only.
    Application { private_directory: String },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropPlan {
    pub remote_path: String,
    pub insert_text: Option<String>,
    pub requires_private_directory: bool,
}
pub fn plan(target: &DropTarget, filename: &str, shell: Shell) -> Result<DropPlan, InvalidPath> {
    let (directory, insert) = match target {
        DropTarget::ShellPrompt { directory } => (directory, false),
        DropTarget::Application { private_directory } => (private_directory, true),
    };
    if !directory.starts_with('/') {
        return Err(InvalidPath);
    }
    let path = join_remote(directory, filename)?;
    Ok(DropPlan {
        insert_text: if insert {
            Some(quote(&path, shell)?)
        } else {
            None
        },
        remote_path: path,
        requires_private_directory: insert,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn screenshot_encodes_and_rejects_invalid_dimensions() {
        let png = screenshot_png(1, 1, &[255, 0, 0, 255]).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert!(screenshot_png(u32::MAX, u32::MAX, &[]).is_err());
        assert!(screenshot_png(2, 2, &[0; 4]).is_err());
    }
    #[test]
    fn quotes_literal_shell_metacharacters() {
        assert_eq!(
            quote("a'$(touch bad)", Shell::Posix).unwrap(),
            "'a'\\''$(touch bad)'"
        );
        assert_eq!(quote("a'b", Shell::PowerShell).unwrap(), "'a''b'");
        assert!(quote("a\nb", Shell::Posix).is_err());
    }
    #[test]
    fn rejects_traversal_and_absolute_filenames() {
        for name in ["..", "a/b", "a\\b", "/tmp/x", ""] {
            assert!(join_remote("/safe", name).is_err());
        }
    }
    #[test]
    fn only_application_drop_inserts_path() {
        let app = plan(
            &DropTarget::Application {
                private_directory: "/home/u/.cache/opsssh/drop-1".into(),
            },
            "photo.png",
            Shell::Posix,
        )
        .unwrap();
        assert!(app.requires_private_directory);
        assert!(app.insert_text.is_some());
        assert!(
            plan(
                &DropTarget::ShellPrompt {
                    directory: "/opt".into()
                },
                "photo.png",
                Shell::Posix
            )
            .unwrap()
            .insert_text
            .is_none()
        );
    }
}
