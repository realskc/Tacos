use alloc::sync::Arc;
use core::cell::RefCell;

use crate::sync::{Lock, Semaphore};
use crate::thread::{self, Thread};

/// Sleep lock. Uses [`Semaphore`] under the hood.
#[derive(Clone)]
pub struct Sleep {
    inner: Semaphore,
    holder: RefCell<Option<Arc<Thread>>>,
}

impl Default for Sleep {
    fn default() -> Self {
        Self {
            inner: Semaphore::new(1),
            holder: Default::default(),
        }
    }
}

impl Sleep {
    #[inline]
    fn id(&self) -> usize {
        self as *const _ as usize
    }
}

pub(crate) fn holder_of(lock_id: usize) -> Option<Arc<Thread>> {
    let lock = unsafe { &*(lock_id as *const Sleep) };
    lock.holder.borrow().clone()
}

impl Lock for Sleep {
    fn acquire(&self) {
        let current = thread::current();
        let lock_id = self.id();

        if self.holder.borrow().is_some() {
            current.set_waiting_lock(Some(lock_id));
            thread::propagate_priority(current.clone());
        }

        self.inner.down();
        current.set_waiting_lock(None);
        self.holder.borrow_mut().replace(current);
    }

    fn release(&self) {
        let current = thread::current();
        assert!(Arc::ptr_eq(
            self.holder.borrow().as_ref().unwrap(),
            &current
        ));

        let lock_id = self.id();
        current.remove_donations_for_lock(lock_id);
        if current.waiting_lock().is_some() {
            thread::propagate_priority(current.clone());
        }

        self.holder.borrow_mut().take().unwrap();
        self.inner.up();
    }
}

unsafe impl Sync for Sleep {}
