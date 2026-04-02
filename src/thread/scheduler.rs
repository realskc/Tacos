//! Scheduler
//!
//! [`Manager`](crate::thread::Manager) relies on scheduler to support Kernel Thread Scheduling.
//! FCFS is an example implementation of a scheduler, you can add new schedulers by implementing
//! [`Schedule`] trait.
//!

pub mod fcfs;
#[cfg(feature = "thread-scheduler-priority")]
pub mod priority;

use alloc::sync::Arc;

use crate::thread::Thread;

#[cfg(feature = "thread-scheduler-priority")]
// (Lab1) Your task: priority scheduling
pub type Scheduler = self::priority::Priority;
#[cfg(not(feature = "thread-scheduler-priority"))]
pub type Scheduler = self::fcfs::Fcfs;

/// Basic functionalities of thread schedulers
pub trait Schedule: Default {
    /// Notify the scheduler that a thread is able to run. Then, this thread
    /// becomes a candidate of [`schedule`](Schedule::schedule).
    fn register(&mut self, thread: Arc<Thread>);

    /// Choose the next thread to run. `None` if scheduler decides to keep running
    /// the current thread.
    fn schedule(&mut self) -> Option<Arc<Thread>>;

    /// The highest priority currently in ready queue.
    fn max_priority(&self) -> Option<u32> {
        None
    }

    /// Requeue a ready thread after its effective priority changes.
    fn reprioritize(&mut self, _tid: isize, _priority: u32) {}
}
