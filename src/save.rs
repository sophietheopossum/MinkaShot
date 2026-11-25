//! Making a picture into a PNG file's bytes, and putting those bytes in a
//! file. The two are kept apart, so the bytes can also go elsewhere (the
//! clipboard, say) without a file.

use anti_webui::core::images::Bitmap;
use anti_webui::shell::LocalTime;
use std::ffi::OsStr;
use std::fs::{File, OpenOptions, Permissions};
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::geometry::Crop;

/// Where shots go when no path is given, as the QML had it:
/// `~/Pictures/Screenshots`.
pub fn default_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
    Some(PathBuf::from(home).join("Pictures/Screenshots"))
}

/// `minkashot-yyyyMMdd-hhmmss.png`, the QML's name, at `time`.
pub fn default_name(time: &LocalTime) -> String {
    format!(
        "minkashot-{:04}{:02}{:02}-{:02}{:02}{:02}.png",
        time.year, time.month, time.day, time.hour, time.minute, time.second
    )
}

/// Where a shot is to be saved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A path that was given. A file there is replaced; a link there stays
    /// a link, and the file it leads to is replaced. Anything else there (a
    /// device, a pipe, a directory) is refused.
    Path(PathBuf),
    /// The default name in the default directory, which is made if it is
    /// missing. A shot there never replaces another: a second one in the
    /// same second gets `-1` (then `-2`, ...) before `.png`.
    Default { dir: PathBuf, name: String },
    /// Standard output (`-`), for another program to take, as
    /// `wl-copy -t image/png` does.
    Stdout,
}

/// The pixels of `crop` of `picture`, row by row, opaque, for
/// [`anti_webui::app::headless::encode_png`]. A screen has no
/// transparency, so alpha is made opaque: for a picture the compositor sent
/// with alpha, that is how its premultiplied pixels look on black, which is
/// how it showed. `None` if the crop is not inside the picture, or its
/// pixels are gone.
pub fn crop_rgba(picture: &Bitmap, crop: Crop) -> Option<Vec<u8>> {
    let (pw, ph) = (picture.width() as usize, picture.height() as usize);
    let (x, y, w, h) = (
        crop.x as usize,
        crop.y as usize,
        crop.width as usize,
        crop.height as usize,
    );
    if w == 0 || h == 0 || x + w > pw || y + h > ph {
        return None;
    }
    let mut out = None;
    picture.with_levels(|level, _, pixels| {
        if level != 0 || pixels.len() < pw * ph * 4 {
            return;
        }
        let mut rows = Vec::with_capacity(w * h * 4);
        for row in y..y + h {
            let start = (row * pw + x) * 4;
            rows.extend_from_slice(&pixels[start..start + w * 4]);
        }
        for px in rows.chunks_exact_mut(4) {
            px[3] = 255;
        }
        out = Some(rows);
    });
    out
}

/// Saves `bytes` as the target says, and returns where the shot is now:
/// `None` when it went to standard output. Nothing is ever left
/// half-written under a file's name: the bytes go to a temporary file
/// beside it, are synced to the disk, and that is renamed to the name. The
/// temporary file is removed if anything fails. A file that is replaced
/// keeps its permission bits (who may read a screenshot), not its owner.
pub fn save(target: &Target, bytes: &[u8]) -> io::Result<Option<PathBuf>> {
    match target {
        Target::Stdout => {
            let mut out = io::stdout().lock();
            out.write_all(bytes)?;
            out.flush()?;
            Ok(None)
        }
        Target::Path(path) => {
            let old = match std::fs::metadata(path) {
                Ok(meta) if meta.is_file() => Some(meta.permissions()),
                Ok(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "not a regular file (to write the PNG to standard output, give - as the path)",
                    ));
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => None,
                Err(e) => return Err(e),
            };
            // A link stays a link: the file it leads to is replaced. A link
            // to nothing is an error here.
            let real = if path.is_symlink() {
                std::fs::canonicalize(path)?
            } else {
                path.clone()
            };
            save_atomically(&real, bytes, Place::Replace(old))?;
            Ok(Some(path.clone()))
        }
        Target::Default { dir, name } => {
            std::fs::create_dir_all(dir)?;
            save_atomically(&dir.join(name), bytes, Place::Beside).map(Some)
        }
    }
}

/// What to do when there is a file at the path already.
enum Place {
    /// Replace it, giving the new file these permissions: the old file's,
    /// if there is one.
    Replace(Option<Permissions>),
    /// Take the first free name of `name-1.png`, `name-2.png`, ...
    Beside,
}

/// How many other names [`Place::Beside`] tries.
const OTHER_NAMES: u32 = 999;

/// Saves through a temporary file, and returns where the shot is.
fn save_atomically(path: &Path, bytes: &[u8], place: Place) -> io::Result<PathBuf> {
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "names no file"))?;
    // No wider than the file it replaces from the start: whoever opens the
    // temporary file can read what is written to it later.
    let mode = match &place {
        Place::Replace(Some(old)) => old.mode() & 0o777,
        _ => 0o666,
    };
    let (temp, mut file) = create_temp(dir, name, mode).map_err(|e| {
        // The error names the directory: the path itself was not written.
        io::Error::new(
            e.kind(),
            format!("cannot make a temporary file in {}: {e}", dir.display()),
        )
    })?;
    let saved = (|| -> io::Result<PathBuf> {
        file.write_all(bytes)?;
        if let Place::Replace(Some(old)) = &place {
            // As far as the file system allows: FAT has one mode for all.
            let _ = file.set_permissions(old.clone());
        }
        // An error the disk only reports on write-back comes here, not at close.
        file.sync_all()?;
        drop(file);
        let placed = match place {
            Place::Replace(_) => {
                std::fs::rename(&temp, path)?;
                path.to_owned()
            }
            Place::Beside => put_beside(&temp, path)?,
        };
        // The rename itself, on the disk; the file is complete either way.
        if let Ok(d) = File::open(dir) {
            let _ = d.sync_all();
        }
        Ok(placed)
    })();
    if saved.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    saved
}

/// A new file beside the path, named so that it is hidden and plainly not
/// a finished shot: `.<name>.<pid>-<n>.part`, made with `mode` (less the
/// umask).
fn create_temp(dir: &Path, name: &OsStr, mode: u32) -> io::Result<(PathBuf, File)> {
    let name = name.to_string_lossy();
    let pid = std::process::id();
    let mut n = 0u32;
    loop {
        let temp = dir.join(format!(".{name}.{pid}-{n}.part"));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&temp)
        {
            Ok(file) => return Ok((temp, file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && n < 100 => n += 1,
            Err(e) => return Err(e),
        }
    }
}

/// Renames `temp` to `path`, or to the first free name beside it.
fn put_beside(temp: &Path, path: &Path) -> io::Result<PathBuf> {
    for n in 0..=OTHER_NAMES {
        let candidate = if n == 0 {
            path.to_path_buf()
        } else {
            numbered(path, n)
        };
        match rename_noreplace(temp, &candidate) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!(
            "{} and {OTHER_NAMES} other names beside it are taken",
            path.display()
        ),
    ))
}

/// `dir/name.png` as `dir/name-n.png`.
fn numbered(path: &Path, n: u32) -> PathBuf {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy())
        .unwrap_or_default();
    let name = match path.extension() {
        Some(ext) => format!("{stem}-{n}.{}", ext.to_string_lossy()),
        None => format!("{stem}-{n}"),
    };
    path.with_file_name(name)
}

/// Renames, failing with `AlreadyExists` rather than replacing a file. On a
/// file system that cannot rename that way, it looks first, which a shot
/// taken in the same instant could still race.
fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    use std::ffi::CString;
    let cstr = |p: &Path| {
        CString::new(p.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a NUL in the path"))
    };
    let (f, t) = (cstr(from)?, cstr(to)?);
    // SAFETY: both are NUL-terminated paths that live through the call.
    let r = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            f.as_ptr(),
            libc::AT_FDCWD,
            t.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if r == 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    match err.raw_os_error() {
        Some(libc::EINVAL) | Some(libc::ENOSYS) => {
            if std::fs::symlink_metadata(to).is_ok() {
                return Err(io::Error::from(io::ErrorKind::AlreadyExists));
            }
            std::fs::rename(from, to)
        }
        _ => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, symlink};

    /// A directory of its own for one test, removed when it is done.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("minkashot-test-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn names(&self) -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(&self.0)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_default_name_is_the_qmls() {
        let t = LocalTime {
            year: 2026,
            month: 11,
            day: 4,
            hour: 9,
            minute: 5,
            second: 7,
            weekday: 3,
            unix: 0,
        };
        assert_eq!(default_name(&t), "minkashot-20261104-090507.png");
        let t = LocalTime {
            month: 12,
            day: 31,
            hour: 23,
            minute: 59,
            second: 59,
            ..t
        };
        assert_eq!(default_name(&t), "minkashot-20261231-235959.png");
    }

    #[test]
    fn a_crop_takes_its_rows_and_is_opaque() {
        // 3x2, each pixel [x, y, 7, 128].
        let mut pixels = Vec::new();
        for y in 0..2u8 {
            for x in 0..3u8 {
                pixels.extend_from_slice(&[x, y, 7, 128]);
            }
        }
        let picture = Bitmap::from_premultiplied(3, 2, pixels, false);
        let crop = Crop {
            x: 1,
            y: 1,
            width: 2,
            height: 1,
        };
        assert_eq!(
            crop_rgba(&picture, crop),
            Some(vec![1, 1, 7, 255, 2, 1, 7, 255])
        );
        let whole = crop_rgba(
            &picture,
            Crop {
                x: 0,
                y: 0,
                width: 3,
                height: 2,
            },
        )
        .unwrap();
        assert_eq!(whole.len(), 3 * 2 * 4);
        assert!(
            crop_rgba(
                &picture,
                Crop {
                    x: 2,
                    y: 0,
                    width: 2,
                    height: 1
                }
            )
            .is_none()
        );
        assert!(
            crop_rgba(
                &picture,
                Crop {
                    x: 0,
                    y: 0,
                    width: 0,
                    height: 1
                }
            )
            .is_none()
        );
    }

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().mode() & 0o7777
    }

    #[test]
    fn a_given_path_gets_the_bytes_and_replaces_a_file() {
        let s = Scratch::new("given");
        let path = s.0.join("shot.png");
        std::fs::write(&path, b"old").unwrap();
        let old = std::fs::metadata(&path).unwrap().ino();
        let saved = save(&Target::Path(path.clone()), b"new bytes").unwrap();
        assert_eq!(saved, Some(path.clone()));
        // Replaced, not written over: a reader of the old file keeps it.
        assert_ne!(std::fs::metadata(&path).unwrap().ino(), old);
        assert_eq!(std::fs::read(&path).unwrap(), b"new bytes");
        assert_eq!(s.names(), ["shot.png"]);
        let new = s.0.join("new.png");
        save(&Target::Path(new.clone()), b"bytes").unwrap();
        assert_eq!(std::fs::read(&new).unwrap(), b"bytes");
    }

    #[test]
    fn a_replaced_file_keeps_its_mode() {
        let s = Scratch::new("mode");
        for m in [0o600, 0o640, 0o444] {
            let path = s.0.join(format!("shot-{m:o}.png"));
            std::fs::write(&path, b"old").unwrap();
            std::fs::set_permissions(&path, Permissions::from_mode(m)).unwrap();
            save(&Target::Path(path.clone()), b"new").unwrap();
            assert_eq!(mode(&path), m, "{}", path.display());
            assert_eq!(std::fs::read(&path).unwrap(), b"new");
        }
    }

    /// The bytes never sit in a file that more people may open than the one
    /// it replaces: the temporary file is made with that file's mode.
    #[test]
    fn the_temporary_file_is_no_wider_than_the_file_it_replaces() {
        let s = Scratch::new("temp-mode");
        let (temp, _file) = create_temp(&s.0, OsStr::new("shot.png"), 0o600).unwrap();
        assert_eq!(mode(&temp) & 0o077, 0, "{:o}", mode(&temp));
    }

    #[test]
    fn a_link_stays_a_link_and_its_file_gets_the_bytes() {
        let s = Scratch::new("link");
        std::fs::create_dir(s.0.join("shots")).unwrap();
        let real = s.0.join("shots/a.png");
        std::fs::write(&real, b"old").unwrap();
        std::fs::set_permissions(&real, Permissions::from_mode(0o600)).unwrap();
        // latest.png -> shots/a.png, and one more link on the way.
        symlink("shots/a.png", s.0.join("latest.png")).unwrap();
        symlink(s.0.join("latest.png"), s.0.join("again.png")).unwrap();
        let link = s.0.join("again.png");
        assert_eq!(
            save(&Target::Path(link.clone()), b"new").unwrap(),
            Some(link)
        );
        for l in ["latest.png", "again.png"] {
            assert!(s.0.join(l).is_symlink(), "{l}");
        }
        assert_eq!(std::fs::read(&real).unwrap(), b"new");
        assert_eq!(mode(&real), 0o600);
        assert_eq!(s.names(), ["again.png", "latest.png", "shots"]);
        // A link to nothing, and links in a ring, are errors that leave
        // nothing behind.
        symlink("shots/b.png", s.0.join("dangling.png")).unwrap();
        symlink("ring-b", s.0.join("ring-a")).unwrap();
        symlink("ring-a", s.0.join("ring-b")).unwrap();
        for bad in ["dangling.png", "ring-a"] {
            assert!(save(&Target::Path(s.0.join(bad)), b"x").is_err(), "{bad}");
        }
        assert!(s.0.join("dangling.png").is_symlink());
        assert!(!s.0.join("shots/b.png").exists());
        assert!(s.names().iter().all(|n| !n.ends_with(".part")));
    }

    /// `/dev/stdout` redirected to a file is `/proc/self/fd/1`, a link to
    /// it: the file is replaced as by its own name.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_descriptor_of_a_named_file_is_saved_beside_it() {
        use std::os::fd::AsRawFd;
        let s = Scratch::new("fd-named");
        let path = s.0.join("out.png");
        let open = File::create(&path).unwrap();
        let fd_path = PathBuf::from(format!("/proc/self/fd/{}", open.as_raw_fd()));
        save(&Target::Path(fd_path), b"png").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"png");
        assert_eq!(s.names(), ["out.png"]);
    }

    /// A path that is there but is no file is refused, with the way to send
    /// the PNG down a pipe after all.
    #[test]
    fn a_path_that_is_not_a_file_is_refused() {
        let s = Scratch::new("not-a-file");
        let fifo = s.0.join("fifo");
        let c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: a NUL-terminated path that lives through the call.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        symlink("fifo", s.0.join("to-fifo")).unwrap();
        let paths = [
            PathBuf::from("/dev/null"),
            fifo,
            s.0.join("to-fifo"),
            s.0.clone(),
        ];
        for path in paths {
            let err = save(&Target::Path(path.clone()), b"png").unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{path:?}");
            assert!(err.to_string().contains("give - as the path"), "{err}");
        }
        assert_eq!(s.names(), ["fifo", "to-fifo"]);
    }

    #[test]
    fn the_default_directory_is_made_and_a_second_shot_goes_beside_the_first() {
        let s = Scratch::new("default");
        let dir = s.0.join("Pictures/Screenshots");
        let target = Target::Default {
            dir: dir.clone(),
            name: "minkashot-1.png".into(),
        };
        let first = save(&target, b"first").unwrap().unwrap();
        let second = save(&target, b"second").unwrap().unwrap();
        let third = save(&target, b"third").unwrap();
        assert_eq!(first, dir.join("minkashot-1.png"));
        assert_eq!(second, dir.join("minkashot-1-1.png"));
        assert_eq!(third, Some(dir.join("minkashot-1-2.png")));
        assert_eq!(std::fs::read(&first).unwrap(), b"first");
        assert_eq!(std::fs::read(&second).unwrap(), b"second");
    }

    #[test]
    fn a_failed_save_leaves_nothing_behind() {
        let s = Scratch::new("failed");
        // Renaming a file over a directory that holds something fails, after
        // the bytes went to the temporary file.
        let path = s.0.join("taken");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("inside"), b"x").unwrap();
        assert!(save_atomically(&path, b"bytes", Place::Replace(None)).is_err());
        assert_eq!(s.names(), ["taken"]);
        // A directory that is not there: the error says the temporary file
        // could not be made there.
        let missing = s.0.join("missing/shot.png");
        let err = save(&Target::Path(missing), b"bytes").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(err.to_string().contains("temporary file in"), "{err}");
        assert_eq!(s.names(), ["taken"]);
    }

    #[test]
    fn numbered_names_keep_the_extension() {
        assert_eq!(numbered(Path::new("/a/b.png"), 3), Path::new("/a/b-3.png"));
        assert_eq!(numbered(Path::new("/a/b"), 1), Path::new("/a/b-1"));
    }
}
