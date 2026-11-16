//! The host's side of an attach, as the crate sees it: what the session decides, so the crate
//! itself knows nothing of terminals, ledgers or dialogs.

use std::future::Future;
use std::pin::Pin;

use crate::protocol::{ClientMsg, Level, Reason, Sender};

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// What a client message came to, to be told to that device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Handled {
    Accepted {
        message_id: String,
    },
    Rejected {
        message_id: String,
        reason: Reason,
    },
    /// Something the device should read: a refused answer, a refused command.
    Notice {
        severity: Level,
        text: String,
    },
    /// Nothing to tell it.
    Silent,
}

pub trait HostSession: Send + Sync {
    /// A prompt, a slash line, an answer or an interrupt from an attached device. Must not
    /// block: it runs on the connection's task and the loop applies the effect.
    fn client_message(&self, from: &Sender, label: &str, message: ClientMsg) -> Handled;

    /// A device presented a valid pairing secret. Resolves to whether the person at the host
    /// terminal confirmed it. Dropping the future is how a client disconnect, or the timeout,
    /// closes the host's dialog.
    fn confirm_pairing(&self, node: &Sender, label: &str) -> BoxFuture<bool>;

    /// A device paired and was stored.
    fn paired(&self, node: &Sender, label: &str);

    /// A device's connection was accepted and registered.
    fn attached(&self, node: &Sender, label: &str);

    fn detached(&self, node: &Sender);

    /// A connection was refused before or during the handshake.
    fn refused(&self, reason: Reason, node: &Sender);

    /// Something the person at the host should hear of, such as a store that could not be
    /// written. The implementation renders the error's chain after the context.
    fn warn(&self, context: &str, error: &(dyn std::error::Error + 'static));
}
