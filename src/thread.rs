//! Kernel Threads

mod imp;
pub mod manager;
pub mod scheduler;
pub mod switch;

pub use self::imp::*;
pub use self::manager::Manager;
pub(self) use self::scheduler::{Schedule, Scheduler};

use alloc::sync::Arc;

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

/// Wake up a previously blocked thread, mark it as [`Ready`](Status::Ready),
/// and register it into the scheduler.
pub fn wake_up(thread: Arc<Thread>) {
    assert_eq!(thread.status(), Status::Blocked);
    thread.set_status(Status::Ready);

    #[cfg(feature = "debug")]
    kprintln!("[THREAD] Wake up {:?}", thread);

    Manager::get().scheduler.lock().register(thread);
}

/// (Lab1) Sets the current thread's priority to a given value
pub fn set_priority(_priority: u32) {}

/// (Lab1) Returns the current thread's effective priority.
pub fn get_priority() -> u32 {
    0
}

pub fn wake_sleeping_threads(now: i64) {
    let mut sleeping = SLEEPING_TASK_LIST.lock();
    while let Some(top) = sleeping.peek() {
        if top.wake_tick > now {
            break;
        }

        let entry = sleeping.pop().unwrap();
        wake_up(entry.thread);
    }
}

/// (Lab1) Make the current thread sleep for the given ticks.
pub fn sleep(ticks: i64) {
    use crate::sbi::{interrupt, timer::timer_ticks};

    if ticks <= 0 {
        return;
    }
    let old = interrupt::set(false);

    let wake_tick = timer_ticks() + ticks;
    let current = Manager::get().current.lock().clone();

    SLEEPING_TASK_LIST.lock().push(SleepEntry {
        wake_tick,
        thread: current.clone(),
    });
    block();

    interrupt::set(old);
}

use alloc::collections::BinaryHeap;
use core::cmp::Ordering;

use crate::sync::Lazy;

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

// 默认是大根堆，通过重定义比较改为小根堆
static SLEEPING_TASK_LIST: Lazy<Mutex<BinaryHeap<SleepEntry>>> =
    Lazy::new(|| Mutex::new(BinaryHeap::new()));
