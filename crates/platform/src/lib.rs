//! Application-owned platform services. No other crate contains OS conditionals.
//!
//! Local terminals use portable-pty's ConPTY/openpty implementations.

pub mod agent;
mod pty;
pub use pty::{NativeTerminal, NativeTerminalFactory, TerminalKiller};

#[cfg(feature = "gui")]
pub fn application() -> gpui::Application {
    gpui_platform::application()
}

use std::io::{self, Read, Write};
use std::path::PathBuf;

use opsssh_term_core::TerminalSize;

#[cfg(target_os = "windows")]
mod native {
    pub const NAME: &str = "windows";
    pub const DEFAULT_SHELL: &str = "powershell.exe";
    pub const PRIMARY_MODIFIER: &str = "Ctrl";
    pub const TERMINAL_FONT: &str = "Consolas";
}

#[cfg(target_os = "macos")]
mod native {
    pub const NAME: &str = "macos";
    pub const DEFAULT_SHELL: &str = "/bin/zsh";
    pub const PRIMARY_MODIFIER: &str = "Cmd";
    pub const TERMINAL_FONT: &str = "Menlo";
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
mod native {
    pub const NAME: &str = std::env::consts::OS;
    pub const DEFAULT_SHELL: &str = "/bin/sh";
    pub const PRIMARY_MODIFIER: &str = "Ctrl";
    pub const TERMINAL_FONT: &str = "DejaVu Sans Mono";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlatformInfo {
    pub os: &'static str,
    pub architecture: &'static str,
    pub primary_modifier: &'static str,
}

pub fn info() -> PlatformInfo {
    PlatformInfo {
        os: native::NAME,
        architecture: std::env::consts::ARCH,
        primary_modifier: native::PRIMARY_MODIFIER,
    }
}

pub fn default_shell() -> PathBuf {
    if native::NAME == "windows" {
        return PathBuf::from(native::DEFAULT_SHELL);
    }
    std::env::var_os("SHELL")
        .filter(|shell| !shell.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(native::DEFAULT_SHELL))
}

pub fn terminal_font_family() -> &'static str {
    native::TERMINAL_FONT
}

pub fn home_dir() -> io::Result<PathBuf> {
    directories::BaseDirs::new()
        .map(|paths| paths.home_dir().to_path_buf())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Home directory is unavailable"))
}

/// Return the current account name when the operating system exposes it.
pub fn default_username() -> Option<String> {
    ["USER", "USERNAME"]
        .into_iter()
        .find_map(std::env::var_os)
        .map(|value| value.to_string_lossy().into_owned())
        .filter(|value| !value.is_empty() && !value.chars().any(|character| character.is_control()))
}

pub fn app_data_dir() -> io::Result<PathBuf> {
    directories::ProjectDirs::from("", "", "opsssh")
        .map(|paths| paths.config_dir().to_path_buf())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "Application directory is unavailable",
            )
        })
}

pub fn default_known_hosts() -> io::Result<PathBuf> {
    Ok(home_dir()?.join(".ssh").join("known_hosts"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalShellOptions {
    pub executable: PathBuf,
    pub size: TerminalSize,
    pub working_directory: Option<PathBuf>,
}

impl Default for LocalShellOptions {
    fn default() -> Self {
        Self {
            executable: default_shell(),
            size: TerminalSize::new(80, 24).expect("constant dimensions are nonzero"),
            working_directory: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessExit {
    pub code: Option<u32>,
}

/// A real PTY, not shell stdio pipes. Reads and writes run on background workers.
pub trait LocalTerminal: Send {
    fn reader(&mut self) -> io::Result<Box<dyn Read + Send>>;
    fn writer(&mut self) -> io::Result<Box<dyn Write + Send>>;
    fn resize(&mut self, size: TerminalSize) -> io::Result<()>;
    fn try_wait(&mut self) -> io::Result<Option<ProcessExit>>;
    fn terminate(&mut self) -> io::Result<()>;
}

pub trait LocalTerminalFactory: Send + Sync {
    type Terminal: LocalTerminal;
    fn spawn(&self, options: LocalShellOptions) -> io::Result<Self::Terminal>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_information_and_shell_options_are_usable() {
        let info = info();
        assert!(!info.os.is_empty());
        assert!(!info.architecture.is_empty());
        assert!(matches!(info.primary_modifier, "Ctrl" | "Cmd"));
        let options = LocalShellOptions::default();
        assert!(!options.executable.as_os_str().is_empty());
        assert_eq!(options.size.rows(), 24);
    }
}
