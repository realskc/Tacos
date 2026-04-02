//! Kernel Threads

mod imp;
pub mod manager;
pub mod scheduler;
pub mod switch;

pub use self::imp::*;
pub use self::manager::Manager;
pub(self) use self::scheduler::{Schedule, Scheduler};

use alloc::collections::BinaryHeap;
use alloc::sync::Arc;
use core::cmp::Ordering;

use crate::sbi::interrupt;
use crate::sync::Lazy;

/// Create a new thread
pub fn spawn<F>(name: &'static str, f: F) -> Arc<Thread>
where
    F: FnOnce() + Send + 'static,
{
    Builder::new(f).name(name).spawn()
}

/// Get the current running thread
pub fn current() -> Arc<Thread> {
    Manager::get().current.lock().clone()
}

/// Yield the control to another thread (if there's another one ready to run).
pub fn schedule() {
    Manager::get().schedule()
}

/// Gracefully shut down the current thread, and schedule another one.
pub fn exit() -> ! {
    {
        let current = Manager::get().current.lock();

        #[cfg(feature = "debug")]
        kprintln!("Exit: {:?}", *current);

        current.set_status(Status::Dying);
    }

    schedule();

    unreachable!("An exited thread shouldn't be scheduled again");
}

/// Mark the current thread as [`Blocked`](Status::Blocked) and
/// yield the control to another thread
pub fn block() {
    let current = current();
    current.set_status(Status::Blocked);

    #[cfg(feature = "debug")]
    kprintln!("[THREAD] Block {:?}", current);

    schedule();
}

fn wake_up_inner(thread: Arc<Thread>, preempt: bool) {
    assert_eq!(thread.status(), Status::Blocked);
    thread.set_status(Status::Ready);

    #[cfg(feature = "debug")]
    kprintln!("[THREAD] Wake up {:?}", thread);

    Manager::get().scheduler.lock().register(thread);

    if preempt {
        maybe_preempt();
    }
}

/// Wake up a previously blocked thread, mark it as [`Ready`](Status::Ready),
/// and register it into the scheduler.
pub fn wake_up(thread: Arc<Thread>) {
    wake_up_inner(thread, true);
}

/// Yield the CPU if there is a higher-priority ready thread.
pub(crate) fn maybe_preempt() {
    let old = interrupt::set(false);
    let current_priority = current().priority();
    let should_preempt = Manager::get()
        .scheduler
        .lock()
        .max_priority()
        .map_or(false, |p| p > current_priority);

    if should_preempt {
        schedule();
    }

    interrupt::set(old);
}

pub(crate) fn propagate_priority(start: Arc<Thread>) {
    let mut donor = start;

    for _ in 0..64 {
        let lock_id = match donor.waiting_lock() {
            Some(lock_id) => lock_id,
            None => break,
        };

        let holder = match crate::sync::sleep::holder_of(lock_id) {
            Some(holder) => holder,
            None => break,
        };

        if holder.id() == donor.id() {
            break;
        }

        let changed = holder.update_donation(donor.id(), lock_id, donor.priority());
        if !changed {
            break;
        }

        donor = holder;
    }
}

/// (Lab1) Sets the current thread's priority to a given value
pub fn set_priority(priority: u32) {
    assert!(priority <= PRI_MAX);

    let old = interrupt::set(false);

    let current = current();
    current.set_base_priority(priority);
    let changed = current.refresh_priority();

    if changed && current.waiting_lock().is_some() {
        propagate_priority(current.clone());
    }

    maybe_preempt();
    interrupt::set(old);
}

/// (Lab1) Returns the current thread's effective priority.
pub fn get_priority() -> u32 {
    current().priority()
}

pub fn wake_sleeping_threads(now: i64) {
    let mut sleeping = SLEEPING_TASK_LIST.lock();
    let mut woke_any = false;

    while let Some(top) = sleeping.peek() {
        if top.wake_tick > now {
            break;
        }

        let entry = sleeping.pop().unwrap();
        wake_up_inner(entry.thread, false);
        woke_any = true;
    }

    drop(sleeping);

    if woke_any {
        maybe_preempt();
    }
}

/// (Lab1) Make the current thread sleep for the given ticks.
pub fn sleep(ticks: i64) {
    use crate::sbi::timer::timer_ticks;

    if ticks <= 0 {
        return;
    }

    let old = interrupt::set(false);
    let wake_tick = timer_ticks() + ticks;
    let current = current();

    SLEEPING_TASK_LIST.lock().push(SleepEntry {
        wake_tick,
        thread: current,
    });

    block();
    interrupt::set(old);
}

pub struct SleepEntry {
    wake_tick: i64,
    thread: Arc<Thread>,
}

impl PartialEq for SleepEntry {
    fn eq(&self, other: &Self) -> bool {
        self.wake_tick == other.wake_tick && self.thread.id() == other.thread.id()
    }
}

impl Eq for SleepEntry {}

impl PartialOrd for SleepEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SleepEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .wake_tick
            .cmp(&self.wake_tick)
            .then_with(|| other.thread.id().cmp(&self.thread.id()))
    }
}

// BinaryHeap 默认是大根堆，这里通过重定义比较把它改成“小根堆”。
static SLEEPING_TASK_LIST: Lazy<Mutex<BinaryHeap<SleepEntry>>> =
    Lazy::new(|| Mutex::new(BinaryHeap::new()));
