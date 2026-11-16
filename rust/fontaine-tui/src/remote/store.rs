//! The files remote control keeps under the user-scope data directory: the host's endpoint key and
//! its paired devices, and the client's device key and known hosts. Project scope never reads or
//! writes any of them.

use std::io;
use std::path::{Path, PathBuf};

use fontaine_remote::{AttachHosts, ClientStore, DeviceStore, PairedDevices, fresh_key};
use fontaine_sdk::core::{file_lock, owner_only};

const HOST_KEY: &str = "remote_host.key";
const PAIRED: &str = "paired_devices.json";
const DEVICE_KEY: &str = "attach_device.key";
const HOSTS: &str = "attach_hosts.json";
/// Held while remote control is enabled, so one session is the remote host at a time.
pub(crate) const HOST_LOCK: &str = "remote_host.lock";

pub struct FileStore {
    dir: PathBuf,
}

fn invalid(what: &str, source: impl std::error::Error + Send + Sync + 'static) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, StoreError::new(what, source))
}

#[derive(Debug)]
struct StoreError {
    what: String,
    source: Box<dyn std::error::Error + Send + Sync>,
}

impl StoreError {
    fn new(what: &str, source: impl std::error::Error + Send + Sync + 'static) -> StoreError {
        StoreError {
            what: what.to_string(),
            source: Box::new(source),
        }
    }
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.what)
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

/// The data directory, created owner-only when it does not exist yet.
pub(crate) fn ensure_dir(path: &Path) -> io::Result<()> {
    match owner_only::create_dir(path) {
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        other => other,
    }
}

/// Reads a key file, creating it with a fresh random key when it does not exist. The file is
/// owner-only from the moment it exists.
fn key_at(path: &Path) -> io::Result<[u8; 32]> {
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    let read = |path: &Path| -> io::Result<[u8; 32]> {
        let bytes = std::fs::read(path)?;
        bytes
            .try_into()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "the key file is not 32 bytes"))
    };
    // Written whole and renamed into place, so a reader or a crash never sees a partial key.
    if path.exists() {
        return read(path);
    }
    let key = fresh_key();
    owner_only::write_atomic(path, &key)?;
    Ok(key)
}

fn read_text(path: &Path) -> io::Result<Option<String>> {
    match file_lock::read_shared(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

impl FileStore {
    pub fn new(data_dir: &Path) -> FileStore {
        FileStore {
            dir: data_dir.to_path_buf(),
        }
    }

    pub(crate) fn host_lock_path(&self) -> PathBuf {
        self.dir.join(HOST_LOCK)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

impl DeviceStore for FileStore {
    fn host_key(&self) -> io::Result<[u8; 32]> {
        key_at(&self.path(HOST_KEY))
    }

    fn devices(&self) -> io::Result<PairedDevices> {
        match read_text(&self.path(PAIRED))? {
            Some(text) => PairedDevices::parse(&text).map_err(|e| invalid(PAIRED, e)),
            None => Ok(PairedDevices::default()),
        }
    }

    fn update(&self, change: &mut dyn FnMut(&mut PairedDevices)) -> io::Result<()> {
        let path = self.path(PAIRED);
        ensure_dir(&self.dir)?;
        let _lock = file_lock::lock(&path)?;
        // Read without the shared lock: the exclusive one is already ours.
        let mut list = match std::fs::read_to_string(&path) {
            Ok(text) => PairedDevices::parse(&text).map_err(|e| invalid(PAIRED, e))?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => PairedDevices::default(),
            Err(e) => return Err(e),
        };
        change(&mut list);
        owner_only::write_atomic(&path, list.to_text().as_bytes())
    }
}

impl ClientStore for FileStore {
    fn device_key(&self) -> io::Result<[u8; 32]> {
        key_at(&self.path(DEVICE_KEY))
    }

    fn hosts(&self) -> io::Result<AttachHosts> {
        match read_text(&self.path(HOSTS))? {
            Some(text) => AttachHosts::parse(&text).map_err(|e| invalid(HOSTS, e)),
            None => Ok(AttachHosts::default()),
        }
    }

    fn update_hosts(&self, change: &mut dyn FnMut(&mut AttachHosts)) -> io::Result<()> {
        let path = self.path(HOSTS);
        ensure_dir(&self.dir)?;
        let _lock = file_lock::lock(&path)?;
        let mut hosts = match std::fs::read_to_string(&path) {
            Ok(text) => AttachHosts::parse(&text).map_err(|e| invalid(HOSTS, e))?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => AttachHosts::default(),
            Err(e) => return Err(e),
        };
        change(&mut hosts);
        owner_only::write_atomic(&path, hosts.to_text().as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use fontaine_remote::PairedDevice;

    use super::*;

    fn device(node: &str) -> PairedDevice {
        PairedDevice {
            node: node.into(),
            label: "laptop".into(),
            paired_at: 1,
            last_seen: 1,
        }
    }

    #[test]
    fn the_paired_list_round_trips_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::new(dir.path());
        assert!(store.devices().unwrap().devices.is_empty());
        store.update(&mut |list| list.upsert(device("n1"))).unwrap();
        store.update(&mut |list| list.upsert(device("n2"))).unwrap();
        let list = store.devices().unwrap();
        assert_eq!(list.devices.len(), 2);
        // A second store over the same directory, as another process would be.
        let other = FileStore::new(dir.path());
        other
            .update(&mut |list| {
                list.revoke("n1").unwrap();
            })
            .unwrap();
        assert_eq!(store.devices().unwrap().devices[0].node, "n2");
        assert!(dir.path().join(PAIRED).exists());
    }

    #[test]
    fn a_key_is_created_once_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::new(dir.path());
        let first = store.host_key().unwrap();
        assert_eq!(store.host_key().unwrap(), first);
        let device = store.device_key().unwrap();
        assert_ne!(device, first);
        assert_eq!(std::fs::read(dir.path().join(HOST_KEY)).unwrap().len(), 32);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join(HOST_KEY))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0, "the key is owner-only");
        }
    }

    #[test]
    fn a_corrupt_or_newer_file_is_an_error_not_an_empty_list() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::new(dir.path());
        std::fs::write(dir.path().join(PAIRED), "{not json").unwrap();
        assert!(store.devices().is_err());
        assert!(store.update(&mut |_| {}).is_err());
        std::fs::write(dir.path().join(PAIRED), r#"{"version":9}"#).unwrap();
        assert!(store.devices().is_err());
    }

    #[test]
    fn hosts_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::new(dir.path());
        assert!(store.hosts().unwrap().hosts.is_empty());
        let key = iroh_key_public();
        store
            .update_hosts(&mut |hosts| {
                hosts.upsert(fontaine_remote::StoredHost {
                    node: key.to_string(),
                    label: "work".into(),
                    addr: fontaine_remote::EndpointAddr::new(key),
                })
            })
            .unwrap();
        assert_eq!(store.hosts().unwrap().find("work").unwrap().label, "work");
    }

    fn iroh_key_public() -> fontaine_remote::PublicKey {
        fontaine_remote::SecretKey::from_bytes(&[5; 32]).public()
    }
}
