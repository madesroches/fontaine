//! The data the host and the client persist, and the store traits that carry it. This crate does
//! no file I/O of its own: the caller reads and writes through these traits.

use std::sync::Mutex;

use iroh::EndpointAddr;
use serde::{Deserialize, Serialize};

pub const PAIRED_VERSION: u32 = 1;

fn current_version() -> u32 {
    PAIRED_VERSION
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairedDevice {
    /// The device's iroh node id.
    pub node: String,
    /// What the device called itself when it paired; display only.
    pub label: String,
    /// Unix seconds.
    pub paired_at: u64,
    pub last_seen: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairedDevices {
    #[serde(default = "current_version")]
    pub version: u32,
    #[serde(default)]
    pub devices: Vec<PairedDevice>,
}

impl Default for PairedDevices {
    fn default() -> PairedDevices {
        PairedDevices {
            version: PAIRED_VERSION,
            devices: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub enum ParseError {
    Json(serde_json::Error),
    /// Written by a newer build than this one.
    Version(u32),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::Json(_) => f.write_str("the file is not valid JSON of the expected shape"),
            ParseError::Version(v) => write!(
                f,
                "the file is version {v}; this build reads version {PAIRED_VERSION}"
            ),
        }
    }
}

impl std::error::Error for ParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ParseError::Json(e) => Some(e),
            ParseError::Version(_) => None,
        }
    }
}

/// Why a revoke or a host lookup named no single entry.
#[derive(Debug, PartialEq, Eq)]
pub enum SelectorError {
    NoMatch,
    Ambiguous(usize),
}

impl std::fmt::Display for SelectorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SelectorError::NoMatch => f.write_str("nothing matches"),
            SelectorError::Ambiguous(n) => {
                write!(f, "{n} entries match; use more of the node id")
            }
        }
    }
}

impl std::error::Error for SelectorError {}

/// The entries `selector` names: a node-id prefix (at least 4 characters) or an exact label.
fn select<'a, T>(
    entries: &'a [T],
    selector: &str,
    node: impl Fn(&T) -> &str,
    label: impl Fn(&T) -> &str,
) -> Vec<&'a T> {
    let selector = selector.trim();
    if selector.is_empty() {
        return Vec::new();
    }
    entries
        .iter()
        .filter(|entry| {
            node(entry) == selector
                || (selector.len() >= 4 && node(entry).starts_with(selector))
                || label(entry) == selector
        })
        .collect()
}

impl PairedDevices {
    pub fn parse(text: &str) -> Result<PairedDevices, ParseError> {
        let devices: PairedDevices = serde_json::from_str(text).map_err(ParseError::Json)?;
        if devices.version > PAIRED_VERSION {
            return Err(ParseError::Version(devices.version));
        }
        Ok(devices)
    }

    pub fn to_text(&self) -> String {
        serde_json::to_string_pretty(self).expect("paired devices serialize")
    }

    pub fn contains(&self, node: &str) -> bool {
        self.devices.iter().any(|d| d.node == node)
    }

    /// Adds the device, or refreshes the label of one already paired.
    pub fn upsert(&mut self, device: PairedDevice) {
        match self.devices.iter_mut().find(|d| d.node == device.node) {
            Some(existing) => {
                existing.label = device.label;
                existing.last_seen = device.last_seen;
            }
            None => self.devices.push(device),
        }
    }

    pub fn touch(&mut self, node: &str, now: u64) {
        if let Some(device) = self.devices.iter_mut().find(|d| d.node == node) {
            device.last_seen = now;
        }
    }

    pub fn find(&self, selector: &str) -> Result<&PairedDevice, SelectorError> {
        let found = select(&self.devices, selector, |d| &d.node, |d| &d.label);
        match found.as_slice() {
            [] => Err(SelectorError::NoMatch),
            [one] => Ok(one),
            many => Err(SelectorError::Ambiguous(many.len())),
        }
    }

    /// Removes the one device `selector` names and returns it.
    pub fn revoke(&mut self, selector: &str) -> Result<PairedDevice, SelectorError> {
        let node = self.find(selector)?.node.clone();
        let at = self
            .devices
            .iter()
            .position(|d| d.node == node)
            .expect("the device was just found");
        Ok(self.devices.remove(at))
    }
}

/// Host-side persistence: the endpoint key and the paired list.
pub trait DeviceStore: Send + Sync {
    /// The persistent endpoint secret key, created on first use.
    fn host_key(&self) -> std::io::Result<[u8; 32]>;

    fn devices(&self) -> std::io::Result<PairedDevices>;

    /// Reads, changes and writes the list as one step other processes cannot interleave with.
    fn update(&self, change: &mut dyn FnMut(&mut PairedDevices)) -> std::io::Result<()>;
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredHost {
    pub node: String,
    pub label: String,
    /// The addresses it was last reached at.
    pub addr: EndpointAddr,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttachHosts {
    #[serde(default = "current_version")]
    pub version: u32,
    #[serde(default)]
    pub hosts: Vec<StoredHost>,
}

impl Default for AttachHosts {
    fn default() -> AttachHosts {
        AttachHosts {
            version: PAIRED_VERSION,
            hosts: Vec::new(),
        }
    }
}

impl AttachHosts {
    pub fn parse(text: &str) -> Result<AttachHosts, ParseError> {
        let hosts: AttachHosts = serde_json::from_str(text).map_err(ParseError::Json)?;
        if hosts.version > PAIRED_VERSION {
            return Err(ParseError::Version(hosts.version));
        }
        Ok(hosts)
    }

    pub fn to_text(&self) -> String {
        serde_json::to_string_pretty(self).expect("attach hosts serialize")
    }

    pub fn upsert(&mut self, host: StoredHost) {
        match self.hosts.iter_mut().find(|h| h.node == host.node) {
            Some(existing) => *existing = host,
            None => self.hosts.push(host),
        }
    }

    pub fn find(&self, selector: &str) -> Result<&StoredHost, SelectorError> {
        let found = select(&self.hosts, selector, |h| &h.node, |h| &h.label);
        match found.as_slice() {
            [] => Err(SelectorError::NoMatch),
            [one] => Ok(one),
            many => Err(SelectorError::Ambiguous(many.len())),
        }
    }
}

/// Client-side persistence: the device key and the hosts it has paired with.
pub trait ClientStore: Send + Sync {
    fn device_key(&self) -> std::io::Result<[u8; 32]>;

    fn hosts(&self) -> std::io::Result<AttachHosts>;

    fn update_hosts(&self, change: &mut dyn FnMut(&mut AttachHosts)) -> std::io::Result<()>;
}

/// A new random 32-byte key, for the caller that persists it.
pub fn fresh_key() -> [u8; 32] {
    let mut key = [0u8; 32];
    key[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    key[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    key
}

/// A store that keeps everything in memory, for tests and for embedders with their own storage.
pub struct MemoryDeviceStore {
    key: [u8; 32],
    devices: Mutex<PairedDevices>,
}

impl Default for MemoryDeviceStore {
    fn default() -> MemoryDeviceStore {
        MemoryDeviceStore {
            key: fresh_key(),
            devices: Mutex::default(),
        }
    }
}

impl DeviceStore for MemoryDeviceStore {
    fn host_key(&self) -> std::io::Result<[u8; 32]> {
        Ok(self.key)
    }

    fn devices(&self) -> std::io::Result<PairedDevices> {
        Ok(self
            .devices
            .lock()
            .expect("the paired list lock is poisoned")
            .clone())
    }

    fn update(&self, change: &mut dyn FnMut(&mut PairedDevices)) -> std::io::Result<()> {
        change(
            &mut self
                .devices
                .lock()
                .expect("the paired list lock is poisoned"),
        );
        Ok(())
    }
}

pub struct MemoryClientStore {
    key: [u8; 32],
    hosts: Mutex<AttachHosts>,
}

impl Default for MemoryClientStore {
    fn default() -> MemoryClientStore {
        MemoryClientStore {
            key: fresh_key(),
            hosts: Mutex::default(),
        }
    }
}

impl ClientStore for MemoryClientStore {
    fn device_key(&self) -> std::io::Result<[u8; 32]> {
        Ok(self.key)
    }

    fn hosts(&self) -> std::io::Result<AttachHosts> {
        Ok(self
            .hosts
            .lock()
            .expect("the host list lock is poisoned")
            .clone())
    }

    fn update_hosts(&self, change: &mut dyn FnMut(&mut AttachHosts)) -> std::io::Result<()> {
        change(&mut self.hosts.lock().expect("the host list lock is poisoned"));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(node: &str, label: &str) -> PairedDevice {
        PairedDevice {
            node: node.into(),
            label: label.into(),
            paired_at: 1,
            last_seen: 1,
        }
    }

    #[test]
    fn the_paired_list_round_trips_and_refuses_a_newer_version() {
        let mut list = PairedDevices::default();
        list.upsert(device("abcdef0123", "laptop"));
        let back = PairedDevices::parse(&list.to_text()).unwrap();
        assert_eq!(back, list);
        assert!(matches!(
            PairedDevices::parse(r#"{"version":99,"devices":[]}"#),
            Err(ParseError::Version(99))
        ));
        assert!(PairedDevices::parse("{").is_err());
        assert_eq!(
            PairedDevices::parse("{}").unwrap(),
            PairedDevices::default()
        );
    }

    #[test]
    fn a_revoke_names_one_device_by_node_prefix_or_label() {
        let mut list = PairedDevices::default();
        list.upsert(device("abcdef0123", "laptop"));
        list.upsert(device("abcdff9999", "phone"));
        assert_eq!(list.find("abc"), Err(SelectorError::NoMatch), "too short");
        assert_eq!(list.find("abcd"), Err(SelectorError::Ambiguous(2)));
        assert_eq!(list.find("abcde").unwrap().label, "laptop");
        assert_eq!(list.revoke("phone").unwrap().node, "abcdff9999");
        assert_eq!(list.devices.len(), 1);
        assert_eq!(list.revoke("nobody"), Err(SelectorError::NoMatch));
    }

    #[test]
    fn upserting_a_paired_node_refreshes_instead_of_duplicating() {
        let mut list = PairedDevices::default();
        list.upsert(device("n1", "old"));
        list.upsert(PairedDevice {
            last_seen: 5,
            ..device("n1", "new")
        });
        assert_eq!(list.devices.len(), 1);
        assert_eq!(
            (list.devices[0].label.as_str(), list.devices[0].last_seen),
            ("new", 5)
        );
        list.touch("n1", 9);
        assert_eq!(list.devices[0].last_seen, 9);
    }
}
