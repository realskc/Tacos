use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use crate::thread::{Schedule, Thread, PRI_MAX};

/// Strict priority scheduler with round-robin inside the same priority.
pub struct Priority {
    ready: Vec<VecDeque<Arc<Thread>>>,
}

impl Default for Priority {
    fn default() -> Self {
        Self {
            ready: vec![VecDeque::new(); (PRI_MAX as usize) + 1],
        }
    }
}

impl Schedule for Priority {
    fn register(&mut self, thread: Arc<Thread>) {
        self.ready[thread.priority() as usize].push_back(thread);
    }

    fn schedule(&mut self) -> Option<Arc<Thread>> {
        for p in (0..=PRI_MAX as usize).rev() {
            if let Some(thread) = self.ready[p].pop_front() {
                return Some(thread);
            }
        }
        None
    }

    fn max_priority(&self) -> Option<u32> {
        for p in (0..=PRI_MAX as usize).rev() {
            if !self.ready[p].is_empty() {
                return Some(p as u32);
            }
        }
        None
    }
}
