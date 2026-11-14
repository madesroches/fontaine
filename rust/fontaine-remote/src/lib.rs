//! One-shot remote messages over iroh: a ticket-holding client sends one line of text to a live
//! session, which answers `accepted` once it has queued the line. The crate knows nothing of the
//! session; the host plugs in through [`Delivery`].

pub mod client;
pub mod inbox;
pub mod protocol;
pub mod server;
pub mod ticket;

pub use client::{Accepted, SendError, SendOptions, send};
pub use inbox::{Clock, Delivery, DeliveryError, MintError, RemoteInbox, SystemClock};
pub use iroh::{RelayMap, RelayMode};
pub use protocol::{
    ALPN, MAX_FRAME, MAX_MESSAGE_BYTES, MAX_TTL, Reason, Request, Response, Sender, Status,
};
pub use server::{RemoteServer, ServerError};
pub use ticket::{Ticket, TicketError};
