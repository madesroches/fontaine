//! Remote control of a live session over iroh: a paired device attaches to the session's
//! terminal UI from another machine. The crate knows nothing of the session; the host plugs in
//! through [`HostSession`] and the two stores, and does all file I/O itself.

pub mod client;
pub mod hub;
pub mod inbox;
pub mod paired;
pub mod protocol;
pub mod server;
pub mod session;
pub mod ticket;

pub use client::{Accepted, AttachError, AttachOptions, Client, ClientEvent, Target, send_message};
pub use hub::{Close, Hub, HubLimits, Registration, Waker};
pub use inbox::{Clock, MintError, Pairing, SystemClock};
pub use iroh::{EndpointAddr, PublicKey, RelayMap, RelayMode, SecretKey};
pub use paired::{
    AttachHosts, ClientStore, DeviceStore, MemoryClientStore, MemoryDeviceStore, PairedDevice,
    PairedDevices, SelectorError, StoredHost, fresh_key,
};
pub use protocol::{
    ALPN, Auth, ClientMsg, HOST_FRAME_CAP, HostMsg, Level, MAX_FEEDBACK_BYTES, MAX_MESSAGE_BYTES,
    MAX_TTL, Reason, Sender, SnapshotAssembler, WireAnswer, short_node,
};
pub use server::{RemoteServer, ServerConfig, ServerError};
pub use session::{BoxFuture, Handled, HostSession};
pub use ticket::{LINK_PREFIX, PREFIX, Ticket, TicketError};
