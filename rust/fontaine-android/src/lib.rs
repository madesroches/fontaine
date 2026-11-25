//! The Fontaine Android app: a remote-control device that scans the QR code a host shows, pairs
//! through the host-terminal confirmation and drives the session. Everything outside the
//! `android` module is portable and tested on the host; the module holds the activity, camera,
//! GLES and input glue that only runs on a phone.

pub mod app;
pub mod commands;
pub mod field;
pub mod ime;
pub mod keys;
pub mod layout;
pub mod link;
pub mod model;
pub mod scan;
pub mod scroll;
pub mod select;
pub mod store;

#[cfg(target_os = "android")]
mod android;
