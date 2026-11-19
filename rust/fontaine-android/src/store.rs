//! `ClientStore` over a directory the activity hands in (the app's private files directory). The
//! device key is the device's identity to every host it paired with, so it is created owner-only
//! and replaced only atomically.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use fontaine_remote::{AttachHosts, ClientStore, fresh_key};

const DEVICE_KEY: &str = "attach_device.key";
const HOSTS: &str = "attach_hosts.json";

pub struct DirStore {
    dir: PathBuf,
    /// One process owns the directory, so a lock in memory is what keeps two updates from
    /// interleaving.
    hosts_lock: Mutex<()>,
}

impl DirStore {
    pub fn new(dir: &Path) -> DirStore {
        DirStore {
            dir: dir.to_path_buf(),
            hosts_lock: Mutex::new(()),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

fn invalid(source: impl std::error::Error + Send + Sync + 'static) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, source)
}

/// Written whole to a sibling file and renamed into place, so a reader or a crash never sees a
/// partial file; the sibling is owner-only from the moment it exists.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().expect("a store path has a parent");
    std::fs::create_dir_all(parent)?;
    let temp = path.with_extension("tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp)?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    if let Err(e) = written.and_then(|()| std::fs::rename(&temp, path)) {
        let _ = std::fs::remove_file(&temp);
        return Err(e);
    }
    Ok(())
}

impl ClientStore for DirStore {
    fn device_key(&self) -> io::Result<[u8; 32]> {
        let path = self.path(DEVICE_KEY);
        match std::fs::read(&path) {
            Ok(bytes) => bytes.try_into().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "the key file is not 32 bytes")
            }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let key = fresh_key();
                write_atomic(&path, &key)?;
                Ok(key)
            }
            Err(e) => Err(e),
        }
    }

    fn hosts(&self) -> io::Result<AttachHosts> {
        match std::fs::read_to_string(self.path(HOSTS)) {
            Ok(text) => AttachHosts::parse(&text).map_err(invalid),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(AttachHosts::default()),
            Err(e) => Err(e),
        }
    }

    fn update_hosts(&self, change: &mut dyn FnMut(&mut AttachHosts)) -> io::Result<()> {
        let _guard = self
            .hosts_lock
            .lock()
            .expect("the hosts lock guards a durable record and is poisoned");
        let mut hosts = self.hosts()?;
        change(&mut hosts);
        write_atomic(&self.path(HOSTS), hosts.to_text().as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use fontaine_remote::{EndpointAddr, SecretKey, StoredHost};

    use super::*;

    fn host(seed: u8, label: &str) -> StoredHost {
        let node = SecretKey::from_bytes(&[seed; 32]).public();
        StoredHost {
            node: node.to_string(),
            label: label.into(),
            addr: EndpointAddr::new(node),
        }
    }

    #[test]
    fn the_device_key_is_stable_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let first = DirStore::new(dir.path()).device_key().unwrap();
        let again = DirStore::new(dir.path()).device_key().unwrap();
        assert_eq!(first, again);
        assert_ne!(first, [0; 32]);
    }

    #[cfg(unix)]
    #[test]
    fn the_device_key_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        DirStore::new(dir.path()).device_key().unwrap();
        let mode = std::fs::metadata(dir.path().join(DEVICE_KEY))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        assert!(!dir.path().join("attach_device.tmp").exists());
    }

    #[test]
    fn a_key_file_of_the_wrong_length_is_an_error_not_a_new_identity() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(DEVICE_KEY), b"short").unwrap();
        let error = DirStore::new(dir.path()).device_key().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn the_hosts_file_round_trips_and_updates_replace_by_node() {
        let dir = tempfile::tempdir().unwrap();
        let store = DirStore::new(dir.path());
        assert!(store.hosts().unwrap().hosts.is_empty());
        store
            .update_hosts(&mut |h| h.upsert(host(1, "work")))
            .unwrap();
        store
            .update_hosts(&mut |h| h.upsert(host(2, "home")))
            .unwrap();
        store
            .update_hosts(&mut |h| h.upsert(host(1, "office")))
            .unwrap();
        let back = DirStore::new(dir.path()).hosts().unwrap();
        let labels: Vec<_> = back.hosts.iter().map(|h| h.label.as_str()).collect();
        assert_eq!(labels, ["office", "home"]);
    }

    #[test]
    fn a_corrupt_hosts_file_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(HOSTS), "{").unwrap();
        let error = DirStore::new(dir.path()).hosts().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
