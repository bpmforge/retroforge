//! "Show in Finder/Explorer" (ticket W15-03, `docs/design/UX_WAVE_15.md`
//! §3): reveal a ROM's file in the host OS's file manager, with it
//! selected/highlighted where the platform supports that.
//!
//! ## Why `RevealPlatform` is a parameter, not a `cfg!`
//!
//! The three command shapes below (`open -R`, `explorer /select,`,
//! `xdg-open` of the parent) are trivial individually but easy to get
//! subtly wrong (a missing comma, a path that needs its parent instead of
//! itself) — and CLAUDE.md's own ruling is that hosted CI has not run
//! since 2026-08-07 and Linux/Windows are consequently unverified anywhere
//! this project's tests actually execute. Gating each branch behind
//! `#[cfg(target_os = "...")]` would mean only ONE of the three ever
//! compiles, let alone runs, on any given developer's machine — exactly
//! the "unverified" trap CLAUDE.md warns about. Taking the platform as a
//! plain enum parameter means [`reveal_command`] is a pure function all
//! three branches of, tested unconditionally, on every machine, every
//! time; only [`RevealPlatform::current`] (used by the one real call site,
//! [`reveal_in_file_manager`]) needs `cfg!` at all.

use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

/// Which OS's file manager convention to target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevealPlatform {
    MacOs,
    Windows,
    /// Everything else: freedesktop-compliant desktops via `xdg-open`.
    Linux,
}

impl RevealPlatform {
    /// The platform this build is actually running on.
    #[must_use]
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            RevealPlatform::MacOs
        } else if cfg!(target_os = "windows") {
            RevealPlatform::Windows
        } else {
            RevealPlatform::Linux
        }
    }
}

/// Build (never spawn) the command that reveals `path` in the file
/// manager, per `platform`'s convention:
///
/// - macOS: `open -R <path>` — reveals and selects the file itself.
/// - Windows: `explorer /select,<path>` — same; no space after the comma,
///   which is Explorer's own documented syntax, not a formatting choice.
/// - Linux: `xdg-open <parent>` — freedesktop has no single cross-desktop
///   "select this file" verb, so this opens the containing folder, the
///   closest portable equivalent.
#[must_use]
pub fn reveal_command(platform: RevealPlatform, path: &Path) -> Command {
    match platform {
        RevealPlatform::MacOs => {
            let mut cmd = Command::new("open");
            cmd.arg("-R").arg(path);
            cmd
        }
        RevealPlatform::Windows => {
            let mut cmd = Command::new("explorer");
            let mut arg = OsString::from("/select,");
            arg.push(path.as_os_str());
            cmd.arg(arg);
            cmd
        }
        RevealPlatform::Linux => {
            let parent = path.parent().unwrap_or(path);
            let mut cmd = Command::new("xdg-open");
            cmd.arg(parent);
            cmd
        }
    }
}

/// Fire-and-forget: `spawn`, never `output`/`wait`, so a slow or hung file
/// manager can never block the UI thread (law 3, "the UI never blocks
/// emulation" — this runs off the play path entirely, but the same rule
/// applies to the shell as a whole). Errors are deliberately swallowed:
/// there is nothing actionable to tell the user beyond "your file manager
/// didn't open", and `Command::spawn` failing here almost always means the
/// platform tool itself is missing, which a status string cannot fix.
pub fn reveal_in_file_manager(path: &Path) {
    let _ = reveal_command(RevealPlatform::current(), path).spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_reveals_and_selects_the_file_itself() {
        let cmd = reveal_command(RevealPlatform::MacOs, Path::new("/roms/Alpha Quest.nes"));
        assert_eq!(cmd.get_program(), "open");
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(args, ["-R", "/roms/Alpha Quest.nes"]);
    }

    #[test]
    fn windows_uses_the_select_comma_syntax_with_no_space() {
        let cmd = reveal_command(
            RevealPlatform::Windows,
            Path::new(r"C:\roms\Alpha Quest.nes"),
        );
        assert_eq!(cmd.get_program(), "explorer");
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(args, [r"/select,C:\roms\Alpha Quest.nes"]);
    }

    #[test]
    fn linux_opens_the_parent_directory_not_the_file() {
        let cmd = reveal_command(RevealPlatform::Linux, Path::new("/roms/Alpha Quest.nes"));
        assert_eq!(cmd.get_program(), "xdg-open");
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(args, ["/roms"]);
    }

    #[test]
    fn linux_a_bare_relative_filename_has_an_empty_parent() {
        // `Path::parent()` returns `Some("")` for a single-component
        // relative path, not `None` — so `unwrap_or(path)` never actually
        // triggers here, and the real behaviour is an empty `xdg-open`
        // argument. Worth pinning down explicitly: a silently-empty
        // argument is exactly the kind of thing a "surely it just works"
        // assumption misses.
        let cmd = reveal_command(RevealPlatform::Linux, Path::new("game.nes"));
        assert_eq!(cmd.get_program(), "xdg-open");
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(args, [""]);
    }

    #[test]
    fn current_platform_matches_this_build() {
        let current = RevealPlatform::current();
        #[cfg(target_os = "macos")]
        assert_eq!(current, RevealPlatform::MacOs);
        #[cfg(target_os = "windows")]
        assert_eq!(current, RevealPlatform::Windows);
        #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
        assert_eq!(current, RevealPlatform::Linux);
    }
}
