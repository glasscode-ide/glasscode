//! Files and images pasted or dropped into a terminal become paths the program can read, typed
//! in the way Terminal.app types a dropped file (escaped, then a space). Claude Code and Codex
//! attach an image whose path is pasted.
//!
//! An image on the clipboard (a screenshot, an image copied in a browser) is saved first, as
//! `~/.glasscode/pasted/pasted-<time>-<id>.png` (TIFF and BMP become PNG; kept a week). For a
//! session on the server the file is copied there (`~/.glasscode/pasted/` on the server), since
//! the Mac's paths mean nothing to it.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, bail};
use gpui::{Image, ImageFormat};

/// Where the program in a terminal runs, so its files can be put where it can read them.
#[derive(Clone, Default)]
pub enum FileTarget {
    #[default]
    Local,
    Server(glasscode_core::remote::ServerConfig),
}

/// Something pasted or dropped.
pub enum Incoming {
    Path(PathBuf),
    Image(Image),
}

/// What a Cmd+V brings that is not text: copied files, or (when there is no text) an image.
pub fn from_clipboard(item: &gpui::ClipboardItem) -> Vec<Incoming> {
    let mut incoming: Vec<Incoming> = Vec::new();
    for entry in item.entries() {
        if let gpui::ClipboardEntry::ExternalPaths(paths) = entry {
            incoming.extend(paths.paths().iter().cloned().map(Incoming::Path));
        }
    }
    if incoming.is_empty() && item.text().is_none() {
        for entry in item.entries() {
            if let gpui::ClipboardEntry::Image(image) = entry {
                incoming.push(Incoming::Image(image.clone()));
            }
        }
    }
    incoming
}

/// The biggest file copied to a server this way.
const MAX_UPLOAD: u64 = 100 * 1024 * 1024;

fn pasted_dir() -> PathBuf {
    glasscode_core::paths::root().join("pasted")
}

fn stamp() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    // UTC, sortable; no clock crate needed for a file name.
    let (days, rest) = (secs / 86_400, secs % 86_400);
    let (y, m, d) = civil_from_days(days as i64);
    format!("{y:04}{m:02}{d:02}-{:02}{:02}{:02}", rest / 3600, rest % 3600 / 60, rest % 60)
}

/// Days since 1970-01-01 → (year, month, day) (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// Save a clipboard image where agents can read it; formats they cannot read become PNG.
fn save_image(image: &Image) -> anyhow::Result<PathBuf> {
    let dir = pasted_dir();
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    forget_old(&dir);
    let (bytes, ext) = match image.format {
        ImageFormat::Png => (image.bytes.clone(), "png"),
        ImageFormat::Jpeg => (image.bytes.clone(), "jpg"),
        ImageFormat::Gif => (image.bytes.clone(), "gif"),
        ImageFormat::Webp => (image.bytes.clone(), "webp"),
        ImageFormat::Svg => (image.bytes.clone(), "svg"),
        ImageFormat::Tiff | ImageFormat::Bmp | ImageFormat::Ico | ImageFormat::Pnm => {
            let decoded = image::load_from_memory(&image.bytes).context("the copied image could not be read")?;
            let mut png = Vec::new();
            decoded.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)?;
            (png, "png")
        }
    };
    let path = dir.join(format!("pasted-{}-{:08x}.{ext}", stamp(), image.id as u32));
    std::fs::write(&path, bytes)?;
    Ok(path)
}

/// Pasted images older than a week go.
fn forget_old(dir: &Path) {
    let week = Duration::from_secs(7 * 86_400);
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let old = entry.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > week);
        if old && entry.file_name().to_string_lossy().starts_with("pasted-") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The path the program should be given for one pasted or dropped thing.
fn prepare(incoming: Incoming, target: &FileTarget) -> anyhow::Result<String> {
    let local = match incoming {
        Incoming::Path(path) => path,
        Incoming::Image(image) => save_image(&image)?,
    };
    match target {
        FileTarget::Local => Ok(local.display().to_string()),
        FileTarget::Server(config) => {
            let meta = std::fs::metadata(&local).with_context(|| format!("{} cannot be read", local.display()))?;
            if meta.is_dir() {
                bail!("{} is a folder; only files can be sent to a session on the server", local.display());
            }
            if meta.len() > MAX_UPLOAD {
                bail!("{} is over 100 MB, too big to send to the server", local.display());
            }
            let name = local.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
            let name = if name.starts_with("pasted-") { name } else { format!("{}-{name}", stamp()) };
            config.upload_to_home(&std::fs::read(&local)?, &format!(".glasscode/pasted/{name}"))
        }
    }
}

/// Everything pasted or dropped at once, as the text to type: escaped paths and a space.
pub fn prepare_all(incoming: Vec<Incoming>, target: &FileTarget) -> anyhow::Result<String> {
    let paths = incoming.into_iter().map(|i| prepare(i, target)).collect::<anyhow::Result<Vec<String>>>()?;
    Ok(format!("{} ", paths.iter().map(|p| escape(p)).collect::<Vec<_>>().join(" ")))
}

/// A path as Terminal.app types a dropped file: shell characters behind backslashes.
pub fn escape(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        if c.is_whitespace() || "\\'\"`$&!*?;|<>()[]{}#~^,".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_like_terminal_app() {
        assert_eq!(escape("/Users/a/Desktop/Screenshot 2026-10-20 at 10.15.32.png"), "/Users/a/Desktop/Screenshot\\ 2026-10-20\\ at\\ 10.15.32.png");
        assert_eq!(escape("/tmp/it's (1).png"), "/tmp/it\\'s\\ \\(1\\).png");
        assert_eq!(escape("/tmp/plain.png"), "/tmp/plain.png");
    }

    #[test]
    fn dates_from_days() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_746), (2026, 10, 20));
    }
}
