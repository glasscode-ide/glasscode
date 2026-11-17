//! Saving a file of a worktree so that it is never seen half written and nothing on the disk is
//! overwritten unasked (the editor of a file tab, U20): a temporary file in the same folder,
//! then a rename over the file, and only when the disk still has the version the text is based on.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The largest file the editor reads and compares (2 MB).
pub const MAX_BYTES: u64 = 2 * 1024 * 1024;

/// What the disk says about a file: its length and when it changed. None when it is gone.
pub type Stamp = Option<(u64, SystemTime)>;

pub fn stamp(full: &Path) -> Stamp {
    let meta = std::fs::metadata(full).ok()?;
    Some((meta.len(), meta.modified().unwrap_or(SystemTime::UNIX_EPOCH)))
}

pub fn hash(bytes: &[u8]) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

/// What the disk has: its stamp and a hash of the bytes (None: no file there). A change of the
/// stamp alone (a touch, a checkout of the same text) is no change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Base {
    pub stamp: Stamp,
    pub hash: Option<u64>,
}

impl Base {
    pub const MISSING: Base = Base { stamp: None, hash: None };
}

/// The first `MAX_BYTES` and one more byte of a file, the part the editor reads and compares.
pub fn read_limited(full: &Path) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(full)?.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Why a save wrote nothing.
#[derive(Debug, PartialEq, Eq)]
pub enum SaveError {
    /// The disk has other bytes than the text is based on (an agent wrote the file): the person
    /// decides. What the disk has.
    Changed(Base),
    Failed(String),
}

const KEPT: &str = "Your text is kept here.";

/// A new file in `folder`, named by `name` from a random number nobody can guess, made new: an
/// existing file or a link at that name is never opened or followed (another number is tried).
/// On macOS and Linux it has `mode` from its creation, before any byte is in it. On Windows it
/// takes the access list of its folder (the worktree's folder, or the person's profile).
fn create_new_file(folder: &Path, name: impl Fn(u64) -> String, mode: u32) -> std::io::Result<(std::fs::File, PathBuf)> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    for _ in 0..16 {
        let path = folder.join(name(rand::random()));
        match options.open(&path) {
            Ok(file) => return Ok((file, path)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "no free name for a new file"))
}

/// A copy of text that could not be saved or asked about (a quit from the macOS Dock, a log
/// out): a new file in `folder` (~/.glasscode/unsaved), named by the time, a random part and the
/// file's name. The folder is the person's only (0700 on macOS and Linux) and so is the copy
/// (0600). Nothing removes old copies.
pub fn keep_copy(folder: &Path, name: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(folder)?;
        // A folder made before, wider than that, becomes the person's only.
        if std::fs::metadata(folder)?.permissions().mode() & 0o077 != 0 {
            std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(folder)?;
    let at = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (mut file, path) = create_new_file(folder, |random| format!("{at}-{:08x}-{name}", random as u32), 0o600)?;
    std::io::Write::write_all(&mut file, bytes)?;
    file.sync_all()?;
    Ok(path)
}

fn failed(why: impl Into<String>) -> SaveError {
    SaveError::Failed(why.into())
}

/// Save `bytes` as `full`, a file of `worktree`, in two steps: `prepare` writes a temporary file
/// in the same folder, `finish` renames it over the file. The file is never seen half written,
/// and a failure leaves it as it was. Nothing is written when the disk does not have `expect`
/// (the version the text is based on, or the one the person chose to overwrite).
pub fn save(full: &Path, worktree: &Path, bytes: &[u8], expect: &Base) -> Result<Base, SaveError> {
    let temp = prepare(full, worktree, bytes, expect)?;
    finish(&temp, full, bytes)
}

pub fn prepare(full: &Path, worktree: &Path, bytes: &[u8], expect: &Base) -> Result<PathBuf, SaveError> {
    let (Some(folder), Some(name)) = (full.parent(), full.file_name()) else { return Err(failed("This is not a file Glasscode can save.")) };
    // The tab's own path, inside its own worktree: never a path from typed text, never out of it.
    let inside = match (folder.canonicalize(), worktree.canonicalize()) {
        (Ok(folder), Ok(worktree)) => folder.starts_with(worktree),
        _ => return Err(failed(format!("The folder of this file is gone, so Glasscode did not save it. {KEPT}"))),
    };
    if !inside {
        return Err(failed("This file is not inside its worktree, so Glasscode did not save it."));
    }
    let meta = match std::fs::symlink_metadata(full) {
        Ok(meta) => Some(meta),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(failed(format!("Glasscode could not save this file ({error}). {KEPT}"))),
    };
    if meta.as_ref().is_some_and(|m| m.file_type().is_symlink()) {
        return Err(failed("This file is a link; Glasscode does not save through links."));
    }
    // A rename replaces a read-only file on macOS and Linux: ask the file itself first.
    if meta.as_ref().is_some_and(|m| m.permissions().readonly()) {
        return Err(failed(format!("This file is read only, so Glasscode did not save it. {KEPT}")));
    }
    let now = stamp(full);
    if now != expect.stamp {
        let seen = match now {
            None => Base::MISSING,
            Some(_) => match read_limited(full) {
                Ok(disk) => Base { stamp: now, hash: Some(hash(&disk)) },
                Err(error) => return Err(failed(format!("Glasscode could not read the file before saving it ({error}). {KEPT}"))),
            },
        };
        if seen.hash != expect.hash {
            return Err(SaveError::Changed(seen));
        }
    }
    // macOS and Linux: the temporary file has the file's own mode from its creation (0600 for a
    // new file), so a private file is never readable by others through it. Windows: in the same
    // folder it takes the folder's access list, as the file has it.
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt as _;
        meta.as_ref().map_or(0o600, |m| m.permissions().mode() & 0o7777)
    };
    #[cfg(not(unix))]
    let mode = 0o600;
    let (file, temp) = match create_new_file(folder, |random| format!(".{}.{random:016x}.glasscode-save", name.to_string_lossy()), mode) {
        Ok(made) => made,
        Err(error) => return Err(failed(format!("Glasscode could not save this file ({error}). {KEPT}"))),
    };
    let written = (|| {
        let mut file = file;
        std::io::Write::write_all(&mut file, bytes)?;
        file.sync_all()?;
        drop(file);
        // The file's permissions exactly (the mode above is cut by the process's umask).
        if let Some(meta) = &meta {
            std::fs::set_permissions(&temp, meta.permissions())?;
        }
        Ok::<_, std::io::Error>(())
    })();
    if let Err(error) = written {
        let _ = std::fs::remove_file(&temp);
        return Err(failed(format!("Glasscode could not save this file ({error}). {KEPT}")));
    }
    Ok(temp)
}

pub fn finish(temp: &Path, full: &Path, bytes: &[u8]) -> Result<Base, SaveError> {
    if let Err(error) = std::fs::rename(temp, full) {
        let _ = std::fs::remove_file(temp);
        return Err(failed(format!("Glasscode could not save this file ({error}). {KEPT}")));
    }
    Ok(Base { stamp: stamp(full), hash: Some(hash(bytes)) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("glasscode-save-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn base_of(full: &Path) -> Base {
        Base { stamp: stamp(full), hash: Some(hash(&std::fs::read(full).unwrap())) }
    }

    fn leftovers(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).filter(|n| n.ends_with(".glasscode-save")).collect()
    }

    /// The new text goes into a temporary file of the same folder, and the file keeps its old
    /// bytes until the rename: nobody sees it half written.
    #[test]
    fn a_save_writes_a_temporary_file_and_renames_it() {
        let dir = folder("atomic");
        let full = dir.join("main.rs");
        std::fs::write(&full, b"old\r\n").unwrap();
        let temp = prepare(&full, &dir, b"new\r\n", &base_of(&full)).unwrap();
        assert_eq!(temp.parent(), Some(dir.as_path()), "the same folder, so the rename is one step");
        assert_eq!(std::fs::read(&full).unwrap(), b"old\r\n", "the file is untouched while the new text is written");
        assert_eq!(std::fs::read(&temp).unwrap(), b"new\r\n");
        let after = finish(&temp, &full, b"new\r\n").unwrap();
        assert_eq!(std::fs::read(&full).unwrap(), b"new\r\n");
        assert!(!temp.exists());
        assert_eq!(after, base_of(&full));
        assert!(leftovers(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_read_only_file_is_not_saved_and_says_so() {
        let dir = folder("readonly");
        let full = dir.join("locked.txt");
        std::fs::write(&full, b"keep").unwrap();
        let mut permissions = std::fs::metadata(&full).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&full, permissions.clone()).unwrap();
        let result = save(&full, &dir, b"changed", &base_of(&full));
        assert!(matches!(&result, Err(SaveError::Failed(why)) if why.contains("read only")), "{result:?}");
        assert_eq!(std::fs::read(&full).unwrap(), b"keep");
        assert!(leftovers(&dir).is_empty());
        // Writable again, so the folder can go.
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        std::fs::set_permissions(&full, permissions).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An agent wrote the file after it was read: nothing is written, and the save says what the
    /// disk has. The same bytes with a new time (a touch) are no change.
    #[test]
    fn a_file_changed_on_disk_is_not_overwritten() {
        let dir = folder("changed");
        let full = dir.join("lib.rs");
        std::fs::write(&full, b"one").unwrap();
        let read = base_of(&full);
        std::fs::write(&full, b"agent's").unwrap();
        let result = save(&full, &dir, b"mine", &read);
        assert!(matches!(&result, Err(SaveError::Changed(seen)) if seen.hash == Some(hash(b"agent's"))), "{result:?}");
        assert_eq!(std::fs::read(&full).unwrap(), b"agent's");
        // "Keep mine": the disk's version is the one to overwrite.
        let Err(SaveError::Changed(seen)) = result else { unreachable!() };
        save(&full, &dir, b"mine", &seen).unwrap();
        assert_eq!(std::fs::read(&full).unwrap(), b"mine");
        // A touch: other stamp, same bytes.
        let read = base_of(&full);
        let touched = Base { stamp: Some((4, SystemTime::UNIX_EPOCH)), ..read };
        save(&full, &dir, b"again", &touched).unwrap();
        // Deleted on disk: a change too, unless that is what was kept.
        let read = base_of(&full);
        std::fs::remove_file(&full).unwrap();
        assert_eq!(save(&full, &dir, b"x", &read), Err(SaveError::Changed(Base::MISSING)));
        save(&full, &dir, b"back", &Base::MISSING).unwrap();
        assert_eq!(std::fs::read(&full).unwrap(), b"back");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_save_stays_in_its_worktree() {
        let dir = folder("outside");
        let worktree = dir.join("wt");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(dir.join("other.txt"), b"x").unwrap();
        let full = dir.join("other.txt");
        assert!(matches!(save(&full, &worktree, b"y", &base_of(&full)), Err(SaveError::Failed(why)) if why.contains("not inside")));
        // Its folder gone: an error, nothing left behind.
        let gone = worktree.join("gone").join("a.txt");
        assert!(matches!(save(&gone, &worktree, b"y", &Base::MISSING), Err(SaveError::Failed(why)) if why.contains("folder")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A new file never opens what is already at its name (a file or a link put there): it takes
    /// another name, and the file there keeps its bytes.
    #[test]
    fn a_new_file_never_takes_an_existing_one() {
        let dir = folder("unique");
        std::fs::write(dir.join("taken"), b"theirs").unwrap();
        let tries = std::cell::Cell::new(0);
        let (mut file, path) = create_new_file(
            &dir,
            |_| {
                tries.set(tries.get() + 1);
                if tries.get() == 1 { "taken".to_string() } else { "free".to_string() }
            },
            0o600,
        )
        .unwrap();
        std::io::Write::write_all(&mut file, b"mine").unwrap();
        drop(file);
        assert_eq!(path, dir.join("free"));
        assert_eq!(std::fs::read(dir.join("taken")).unwrap(), b"theirs");
        // Copies of the same file in the same second get names of their own.
        let copies = dir.join("unsaved");
        let a = keep_copy(&copies, "main.rs", b"one").unwrap();
        let b = keep_copy(&copies, "main.rs", b"two").unwrap();
        assert_ne!(a, b);
        assert_eq!((std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap()), (b"one".to_vec(), b"two".to_vec()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The copies of unsaved text are the person's only: the folder 0700 (also one made wider
    /// before), each copy 0600.
    #[cfg(unix)]
    #[test]
    fn kept_copies_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = folder("copies");
        let copies = dir.join("unsaved");
        std::fs::create_dir_all(&copies).unwrap();
        std::fs::set_permissions(&copies, std::fs::Permissions::from_mode(0o755)).unwrap();
        let copy = keep_copy(&copies, ".env", b"KEY=1\n").unwrap();
        assert_eq!(std::fs::metadata(&copies).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(std::fs::metadata(&copy).unwrap().permissions().mode() & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A private file (0600, an .env) stays private: the temporary file has its mode from the
    /// start, and the saved file keeps it.
    #[cfg(unix)]
    #[test]
    fn a_private_file_stays_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = folder("private");
        let full = dir.join(".env");
        std::fs::write(&full, b"KEY=1\n").unwrap();
        std::fs::set_permissions(&full, std::fs::Permissions::from_mode(0o600)).unwrap();
        let temp = prepare(&full, &dir, b"KEY=2\n", &base_of(&full)).unwrap();
        assert_eq!(std::fs::metadata(&temp).unwrap().permissions().mode() & 0o777, 0o600);
        finish(&temp, &full, b"KEY=2\n").unwrap();
        assert_eq!(std::fs::metadata(&full).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::read(&full).unwrap(), b"KEY=2\n");
        assert!(leftovers(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
