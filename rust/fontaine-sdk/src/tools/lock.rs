//! Lock-poisoning policy. A lock whose contents feed a permission or guardian decision, a spend
//! tally, the model in use, or a durable record uses the strict helpers: poison panics, so a
//! session with corrupted state ends instead of continuing to decide. Display or advisory state,
//! where a half-updated value is harmless, uses the `*_recovering` helpers; each call site says why.

use std::sync::{Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

pub fn lock<'a, T>(m: &'a Mutex<T>, what: &str) -> MutexGuard<'a, T> {
    m.lock()
        .unwrap_or_else(|_| panic!("the {what} lock is not poisoned"))
}

pub fn read<'a, T>(l: &'a RwLock<T>, what: &str) -> RwLockReadGuard<'a, T> {
    l.read()
        .unwrap_or_else(|_| panic!("the {what} lock is not poisoned"))
}

pub fn write<'a, T>(l: &'a RwLock<T>, what: &str) -> RwLockWriteGuard<'a, T> {
    l.write()
        .unwrap_or_else(|_| panic!("the {what} lock is not poisoned"))
}

pub fn lock_recovering<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

pub fn read_recovering<T>(l: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    l.read().unwrap_or_else(PoisonError::into_inner)
}

pub fn write_recovering<T>(l: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    l.write().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn poisoned_mutex() -> Arc<Mutex<i32>> {
        let m = Arc::new(Mutex::new(7));
        let m2 = m.clone();
        let _ = std::thread::spawn(move || {
            let _g = m2.lock().unwrap();
            panic!("poison");
        })
        .join();
        m
    }

    fn poisoned_rwlock() -> Arc<RwLock<i32>> {
        let l = Arc::new(RwLock::new(7));
        let l2 = l.clone();
        let _ = std::thread::spawn(move || {
            let _g = l2.write().unwrap();
            panic!("poison");
        })
        .join();
        l
    }

    #[test]
    #[should_panic(expected = "the widget lock is not poisoned")]
    fn strict_lock_panics_on_poison() {
        let m = poisoned_mutex();
        let _g = lock(&m, "widget");
    }

    #[test]
    #[should_panic(expected = "the widget lock is not poisoned")]
    fn strict_read_panics_on_poison() {
        let l = poisoned_rwlock();
        let _g = read(&l, "widget");
    }

    #[test]
    #[should_panic(expected = "the widget lock is not poisoned")]
    fn strict_write_panics_on_poison() {
        let l = poisoned_rwlock();
        let _g = write(&l, "widget");
    }

    #[test]
    fn recovering_helpers_return_the_stored_value() {
        assert_eq!(*lock_recovering(&poisoned_mutex()), 7);
        assert_eq!(*read_recovering(&poisoned_rwlock()), 7);
        assert_eq!(*write_recovering(&poisoned_rwlock()), 7);
    }

    #[test]
    fn healthy_locks_pass_through() {
        let m = Mutex::new(1);
        assert_eq!(*lock(&m, "x"), 1);
        let l = RwLock::new(2);
        assert_eq!(*read(&l, "x"), 2);
        *write(&l, "x") = 3;
        assert_eq!(*read(&l, "x"), 3);
    }
}
