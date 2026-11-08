//! Unpacking the verified archive and replacing the installed binary.

use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};

/// Upper bound on any one decompressed or extracted stream, so a hostile archive that passed
/// verification (a compromised key) still cannot fill the disk.
const MAX_UNPACKED: u64 = 1 << 30;

#[derive(Debug)]
pub enum InstallError {
    Io(&'static str, io::Error),
    Archive(&'static str),
    Zip(zip::result::ZipError),
    Xz(lzma_rs::error::Error),
    NoBinary(String),
    TooLarge,
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstallError::Io(what, _) => f.write_str(what),
            InstallError::Archive(what) => write!(f, "the archive is unusable: {what}"),
            InstallError::Zip(_) => f.write_str("the zip archive cannot be read"),
            InstallError::Xz(_) => f.write_str("the xz stream cannot be decoded"),
            InstallError::NoBinary(name) => {
                write!(f, "the archive holds no regular file named {name}")
            }
            InstallError::TooLarge => f.write_str("the archive unpacks to more than 1 GiB"),
        }
    }
}

impl std::error::Error for InstallError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            InstallError::Io(_, e) => Some(e),
            InstallError::Zip(e) => Some(e),
            InstallError::Xz(e) => Some(e),
            _ => None,
        }
    }
}

fn io_err(what: &'static str) -> impl FnOnce(io::Error) -> InstallError {
    move |e| InstallError::Io(what, e)
}

/// A writer that fails once more than `MAX_UNPACKED` bytes went through it.
struct Capped<W> {
    inner: W,
    written: u64,
    tripped: bool,
}

impl<W> Capped<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            written: 0,
            tripped: false,
        }
    }
}

impl<W: Write> Write for Capped<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.written += buf.len() as u64;
        if self.written > MAX_UNPACKED {
            self.tripped = true;
            return Err(io::Error::other("unpacked size limit exceeded"));
        }
        self.inner.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// A member path that names the binary: `<name>` or `<one directory>/<name>`, made of normal
/// components only. The path is matched, never used to write anything.
fn names_binary(path: &Path, name: &str) -> bool {
    let parts: Vec<_> = path.components().collect();
    parts.iter().all(|c| matches!(c, Component::Normal(_)))
        && matches!(parts.len(), 1 | 2)
        && parts.last().is_some_and(|c| c.as_os_str() == name)
}

/// Copies the single regular-file member that names `name` out of the archive into `dest`.
/// `scratch` is a directory for the intermediate decompressed tar.
pub fn extract_binary(
    archive: &Path,
    name: &str,
    scratch: &Path,
    dest: &mut File,
) -> Result<(), InstallError> {
    let is_zip = archive.extension().is_some_and(|e| e == "zip");
    if is_zip {
        extract_zip(archive, name, dest)
    } else {
        extract_tar_xz(archive, name, scratch, dest)
    }
}

fn extract_zip(archive: &Path, name: &str, dest: &mut File) -> Result<(), InstallError> {
    let file = File::open(archive).map_err(io_err("the archive cannot be opened"))?;
    let mut zip = zip::ZipArchive::new(BufReader::new(file)).map_err(InstallError::Zip)?;
    let mut found = None;
    for index in 0..zip.len() {
        let entry = zip.by_index_raw(index).map_err(InstallError::Zip)?;
        // A symlink is a file with a mode bit; only a plain file may become the binary.
        let plain = entry.is_file() && !entry.is_symlink();
        let matches = entry
            .enclosed_name()
            .is_some_and(|p| names_binary(&p, name));
        if plain && matches {
            if found.is_some() {
                return Err(InstallError::Archive("two members name the binary"));
            }
            found = Some(index);
        }
    }
    let index = found.ok_or_else(|| no_binary(name))?;
    let mut entry = zip.by_index(index).map_err(InstallError::Zip)?;
    copy_capped(&mut entry, dest)
}

fn extract_tar_xz(
    archive: &Path,
    name: &str,
    scratch: &Path,
    dest: &mut File,
) -> Result<(), InstallError> {
    let tar_path = scratch.join("archive.tar");
    {
        let mut input =
            BufReader::new(File::open(archive).map_err(io_err("the archive cannot be opened"))?);
        let tar_file =
            File::create(&tar_path).map_err(io_err("the unpack area is not writable"))?;
        let mut out = Capped::new(io::BufWriter::new(tar_file));
        lzma_rs::xz_decompress(&mut input, &mut out).map_err(|e| match e {
            lzma_rs::error::Error::IoError(_) if out.tripped => InstallError::TooLarge,
            e => InstallError::Xz(e),
        })?;
        out.flush()
            .map_err(io_err("the unpack area is not writable"))?;
    }
    let mut tar = tar::Archive::new(BufReader::new(
        File::open(&tar_path).map_err(io_err("the unpacked archive cannot be reopened"))?,
    ));
    let mut found = false;
    for entry in tar
        .entries()
        .map_err(io_err("the tar stream is unreadable"))?
    {
        let mut entry = entry.map_err(io_err("the tar stream is unreadable"))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry
            .path()
            .map_err(io_err("a tar member path is unreadable"))?;
        if !names_binary(&path, name) {
            continue;
        }
        if found {
            return Err(InstallError::Archive("two members name the binary"));
        }
        found = true;
        copy_capped(&mut entry, dest)?;
    }
    if found { Ok(()) } else { Err(no_binary(name)) }
}

fn no_binary(name: &str) -> InstallError {
    InstallError::NoBinary(name.to_string())
}

fn copy_capped(from: &mut impl Read, to: &mut File) -> Result<(), InstallError> {
    let mut capped = Capped::new(&mut *to);
    io::copy(from, &mut capped).map_err(|e| {
        if capped.tripped {
            InstallError::TooLarge
        } else {
            InstallError::Io("the binary cannot be unpacked", e)
        }
    })?;
    to.sync_all()
        .map_err(io_err("the binary cannot be written"))
}

/// The file name of the binary inside the archive for this platform.
pub fn binary_name() -> &'static str {
    if cfg!(windows) {
        "fontaine.exe"
    } else {
        "fontaine"
    }
}

fn aside_path(target: &Path) -> PathBuf {
    let mut name = target.as_os_str().to_os_string();
    name.push(".old");
    PathBuf::from(name)
}

/// Deletes the previous update's leftover, which Windows could not delete while it ran.
pub fn remove_stale(target: &Path) -> io::Result<()> {
    match std::fs::remove_file(aside_path(target)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Unpacks `archive` and replaces `target` with the binary in it. `work` is a directory on the
/// same filesystem as `target`; everything created there is removed on return.
pub fn replace_binary(archive: &Path, target: &Path, work: &Path) -> Result<(), InstallError> {
    let name = binary_name();
    let mut staged = tempfile::Builder::new()
        .prefix(".fontaine-new-")
        .tempfile_in(work)
        .map_err(io_err(
            "a temporary file cannot be created beside the binary",
        ))?;
    extract_binary(archive, name, work, staged.as_file_mut())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(target)
            .map(|m| m.permissions().mode() & 0o7777)
            .unwrap_or(0o755);
        std::fs::set_permissions(staged.path(), std::fs::Permissions::from_mode(mode))
            .map_err(io_err("the new binary's mode cannot be set"))?;
    }
    if cfg!(windows) {
        // A running exe can be renamed but not overwritten or deleted.
        let aside = aside_path(target);
        std::fs::rename(target, &aside)
            .map_err(io_err("the running executable cannot be moved aside"))?;
        if let Err(e) = staged.persist(target) {
            let _ = std::fs::rename(&aside, target);
            return Err(InstallError::Io(
                "the new binary cannot be moved into place",
                e.error,
            ));
        }
        return Ok(());
    }
    staged
        .persist(target)
        .map_err(|e| InstallError::Io("the new binary cannot be moved into place", e.error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tar_xz(entries: &[(&str, tar::EntryType, &[u8], Option<&str>)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, kind, data, link) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(*kind);
            header.set_size(data.len() as u64);
            header.set_mode(0o755);
            if let Some(link) = link {
                header.set_link_name(link).unwrap();
            }
            // `set_path` refuses `..`, which is exactly what a hostile archive carries.
            let bytes = path.as_bytes();
            header.as_old_mut().name[..bytes.len()].copy_from_slice(bytes);
            header.set_cksum();
            builder.append(&header, *data).unwrap();
        }
        let tar = builder.into_inner().unwrap();
        let mut xz = Vec::new();
        lzma_rs::xz_compress(&mut io::Cursor::new(tar), &mut xz).unwrap();
        xz
    }

    fn extract(archive: &[u8], ext: &str) -> Result<Vec<u8>, InstallError> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("a.{ext}"));
        std::fs::write(&path, archive).unwrap();
        let mut out = tempfile::tempfile().unwrap();
        extract_binary(&path, "fontaine", dir.path(), &mut out)?;
        let mut text = Vec::new();
        io::Seek::rewind(&mut out).unwrap();
        out.read_to_end(&mut text).unwrap();
        Ok(text)
    }

    use tar::EntryType::{Regular, Symlink};

    #[test]
    fn extracts_the_binary_from_a_dist_layout() {
        let xz = tar_xz(&[
            ("t/README.md", Regular, b"readme", None),
            ("t/fontaine", Regular, b"BIN", None),
            ("t/LICENSE", Regular, b"lic", None),
        ]);
        assert_eq!(extract(&xz, "tar.xz").unwrap(), b"BIN");
    }

    #[test]
    fn hostile_members_are_never_the_binary() {
        let xz = tar_xz(&[
            ("fontaine", Symlink, b"", Some("/etc/passwd")),
            ("../fontaine", Regular, b"EVIL", None),
            ("/abs/fontaine", Regular, b"EVIL", None),
            ("a/b/fontaine", Regular, b"EVIL", None),
        ]);
        assert!(matches!(
            extract(&xz, "tar.xz"),
            Err(InstallError::NoBinary(_))
        ));
    }

    #[test]
    fn a_symlink_beside_the_real_binary_is_ignored() {
        let xz = tar_xz(&[
            ("x/fontaine-link", Symlink, b"", Some("/etc/passwd")),
            ("x/fontaine", Regular, b"BIN", None),
        ]);
        assert_eq!(extract(&xz, "tar.xz").unwrap(), b"BIN");
    }

    #[test]
    fn two_binaries_are_refused() {
        let xz = tar_xz(&[
            ("a/fontaine", Regular, b"1", None),
            ("b/fontaine", Regular, b"2", None),
        ]);
        assert!(matches!(
            extract(&xz, "tar.xz"),
            Err(InstallError::Archive(_))
        ));
    }

    fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        for (name, data) in entries {
            writer
                .start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn zip_extracts_only_the_named_member() {
        let z = zip_of(&[("t/readme", b"r"), ("t/fontaine", b"BIN")]);
        assert_eq!(extract(&z, "zip").unwrap(), b"BIN");
        let z = zip_of(&[("../fontaine", b"EVIL")]);
        assert!(matches!(extract(&z, "zip"), Err(InstallError::NoBinary(_))));
    }

    #[test]
    fn replace_binary_swaps_the_file_and_keeps_the_mode() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join(binary_name());
        std::fs::write(&target, b"old").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o750)).unwrap();
        }
        let xz = tar_xz(&[("t/fontaine", Regular, b"new", None)]);
        let archive = dir.path().join("a.tar.xz");
        std::fs::write(&archive, xz).unwrap();
        let work = tempfile::tempdir_in(dir.path()).unwrap();
        if cfg!(windows) {
            return;
        }
        replace_binary(&archive, &target, work.path()).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o750);
        }
    }
}
