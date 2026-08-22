//! Permission handling for files replaced by an atomic write.
//!
//! Writing through a temporary file and renaming it into place gives
//! crash safety, but the file that lands is a *new* one: it carries the
//! temporary file's mode, not the mode of what it replaced. Re-applying
//! a fixed mode afterwards trades one bug for another — it overwrites
//! whatever the owner or an administrator had chosen.
//!
//! The rule these helpers implement: a restrictive mode is a *creation*
//! default, never something re-imposed on a file that already existed.

use std::io;
use std::path::Path;

/// The mode `path` currently has, or `None` when it does not exist.
///
/// Call before an atomic replace so the mode can be carried across.
/// Always `None` off Unix, where the concept does not apply.
pub fn current_mode(path: &Path) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).ok().map(|m| m.permissions().mode())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// Give `path` the mode it had before the replace, or `create_mode` when
/// there was nothing there to preserve.
///
/// `previous` comes from [`current_mode`], captured before the rename.
/// No-op off Unix.
pub fn apply_mode_after_replace(
    path: &Path,
    previous: Option<u32>,
    create_mode: u32,
) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = previous.unwrap_or(create_mode);
        let mut perms = std::fs::metadata(path)?.permissions();
        if perms.mode() == mode {
            return Ok(());
        }
        perms.set_mode(mode);
        std::fs::set_permissions(path, perms)?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (path, previous, create_mode);
        Ok(())
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn mode_of(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn set_mode(path: &Path, mode: u32) {
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(mode);
        std::fs::set_permissions(path, perms).unwrap();
    }

    #[test]
    fn reports_no_mode_for_a_file_that_does_not_exist() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(current_mode(&dir.path().join("absent")).is_none());
    }

    /// A file the owner widened stays widened. Re-imposing the creation
    /// mode on every save is what this exists to prevent.
    #[test]
    fn carries_an_existing_mode_across_a_replace() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("state");
        std::fs::write(&path, "before").unwrap();
        set_mode(&path, 0o640);

        let previous = current_mode(&path);
        // Stand in for the rename: the landed file carries the temp
        // file's restrictive mode.
        std::fs::write(&path, "after").unwrap();
        set_mode(&path, 0o600);

        apply_mode_after_replace(&path, previous, 0o600).unwrap();
        assert_eq!(mode_of(&path), 0o640);
    }

    /// With nothing to preserve, the restrictive creation mode applies —
    /// a fresh credential file must not land world-readable.
    #[test]
    fn falls_back_to_the_creation_mode_for_a_new_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("state");

        let previous = current_mode(&path);
        assert!(previous.is_none());

        std::fs::write(&path, "new").unwrap();
        set_mode(&path, 0o644);
        apply_mode_after_replace(&path, previous, 0o600).unwrap();
        assert_eq!(mode_of(&path), 0o600);
    }

    /// A file already at the target mode is left untouched rather than
    /// being rewritten, so nothing is disturbed on the common path.
    #[test]
    fn leaves_a_matching_mode_alone() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("state");
        std::fs::write(&path, "x").unwrap();
        set_mode(&path, 0o600);

        apply_mode_after_replace(&path, Some(0o600), 0o600).unwrap();
        assert_eq!(mode_of(&path), 0o600);
    }
}
