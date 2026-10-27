//! The seam through which a file-editing tool's target is saved before the tool changes it.
//! `fontaine-tools` only names the moment; what is kept, and where, belongs to the implementer.

use std::path::Path;

/// Saves a file's state ahead of an edit.
pub trait PreImages: Send + Sync {
    /// Called after the gate allowed the call and before the tool runs, with the path the call
    /// names resolved the way the file tools open it. `None` when nothing is kept for it.
    fn before(&self, agent: &str, tool: &str, path: &Path) -> Option<Box<dyn Capture>>;
}

/// A pending capture, finished once the tool has run.
pub trait Capture: Send {
    /// Records what the tool left behind. `succeeded` is the tool's own outcome, not the result
    /// pipeline's.
    fn finish(self: Box<Self>, succeeded: bool);
}
