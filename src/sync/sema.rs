use alloc::collections::VecDeque;
use alloc::sync::Arc;
use core::cell::{Cell, RefCell};

use crate::sbi;
use crate::thread::{self, Thread};

/// Atomic counting semaphore
///
/// # Examples
/// ```
/// let sema = Semaphore::new(0);
/// sema.down();
/// sema.up();
/// ```
#[derive(Clone)]
pub struct Semaphore {
    value: Cell<usize>,
    waiters: RefCell<VecDeque<Arc<Thread>>>,
}

unsafe impl Sync for Semaphore {}
unsafe impl Send for Semaphore {}

impl Semaphore {
    /// Creates a new semaphore of initial value n.
    pub const fn new(n: usize) -> Self {
        Semaphore {
            value: Cell::new(n),
            waiters: RefCell::new(VecDeque::new()),
        }
    }

    /// P operation
    pub fn down(&self) {
        let old = sbi::interrupt::set(false);
        let current = thread::current();

        while self.value() == 0 {
            // Avoid duplicate waiters when a thread is spuriously woken by some
            // other synchronization object before this semaphore is signaled.
            if !self
                .waiters
                .borrow()
                .iter()
                .any(|thread| thread.id() == current.id())
            {
                // 为了在相同优先级下保持 FIFO，这里把新的 waiter 放到队尾。
                self.waiters.borrow_mut().push_back(current.clone());
            }
            thread::block();
        }
        self.waiters
            .borrow_mut()
            .retain(|thread| thread.id() != current.id());
        self.value.set(self.value() - 1);

        sbi::interrupt::set(old);
    }

    fn highest_waiter_index(&self) -> Option<usize> {
        let waiters = self.waiters.borrow();
        let mut best: Option<(usize, u32)> = None;

        for (idx, thread) in waiters.iter().enumerate() {
            let priority = thread.priority();
            if best.map_or(true, |(_, p)| priority > p) {
                best = Some((idx, priority));
            }
        }

        best.map(|(idx, _)| idx)
    }

    fn pop_highest_waiter(&self) -> Option<Arc<Thread>> {
        let idx = self.highest_waiter_index()?;
        self.waiters.borrow_mut().remove(idx)
    }

    /// Highest priority among current waiters.
    pub fn max_priority(&self) -> Option<u32> {
        self.waiters.borrow().iter().map(|t| t.priority()).max()
    }

    /// V operation
    pub fn up(&self) {
        let old = sbi::interrupt::set(false);
        let count = self.value.replace(self.value() + 1);

        if let Some(thread) = self.pop_highest_waiter() {
            assert_eq!(count, 0);
            thread::wake_up(thread);
        }

        sbi::interrupt::set(old);
    }

    /// Get the current value of a semaphore
    pub fn value(&self) -> usize {
        self.value.get()
    }
}
