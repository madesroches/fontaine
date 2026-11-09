//! A shared value that a session replaces as a whole between turns. Every holder keeps the same
//! `Arc<Live<T>>`, so a swap reaches all of them. Readers take a snapshot (`Arc<T>`) and never
//! hold the lock across work.

use std::sync::{Arc, RwLock};

use crate::lock;

#[derive(Debug)]
pub struct Live<T>(RwLock<Arc<T>>);

impl<T> Live<T> {
    pub fn new(value: T) -> Live<T> {
        Live(RwLock::new(Arc::new(value)))
    }

    pub fn from_arc(value: Arc<T>) -> Live<T> {
        Live(RwLock::new(value))
    }

    /// The current value. It stays what the reader saw even if a swap follows.
    pub fn get(&self) -> Arc<T> {
        // The contents feed permission decisions (a skill's `allowed-tools`, spawn checks), so a
        // poisoned lock panics.
        lock::read(&self.0, "live value").clone()
    }

    pub fn set(&self, value: Arc<T>) {
        *lock::write(&self.0, "live value") = value;
    }
}

impl<T: Default> Default for Live<T> {
    fn default() -> Live<T> {
        Live::new(T::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_swap_reaches_every_holder_and_leaves_snapshots_alone() {
        let cell = Arc::new(Live::new(1));
        let other = cell.clone();
        let before = cell.get();
        other.set(Arc::new(2));
        assert_eq!(*cell.get(), 2);
        assert_eq!(*before, 1);
    }
}
