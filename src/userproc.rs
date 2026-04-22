//! User process.
//!

mod load;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::arch::asm;
use core::mem::MaybeUninit;
use core::slice;
use riscv::register::sstatus;

use crate::fs::File;
use crate::mem::pagetable::{KernelPgTable, PageTable};
use crate::mem::PG_SIZE;
use crate::sync::{Lazy, Semaphore};
use crate::thread::{self, Mutex};
use crate::trap::{trap_exit_u, Frame};
use crate::{OsError, Result};

struct FdEntry {
    file: File,
    readable: bool,
    writable: bool,
}

struct WaitStatus {
    parent_tid: isize,
    exit_status: Mutex<Option<isize>>,
    child_alive: Mutex<bool>,
    parent_alive: Mutex<bool>,
    waited: Mutex<bool>,
    exited: Semaphore,
}

impl WaitStatus {
    fn new(parent_tid: isize) -> Self {
        Self {
            parent_tid,
            exit_status: Mutex::new(None),
            child_alive: Mutex::new(true),
            parent_alive: Mutex::new(true),
            waited: Mutex::new(false),
            exited: Semaphore::new(0),
        }
    }

    fn should_reap(&self) -> bool {
        let child_alive = *self.child_alive.lock();
        let parent_alive = *self.parent_alive.lock();
        let waited = *self.waited.lock();
        !child_alive && (!parent_alive || waited)
    }

    fn mark_exited(&self, value: isize) {
        *self.exit_status.lock() = Some(value);
        *self.child_alive.lock() = false;
        self.exited.up();
    }

    fn mark_waited(&self) -> bool {
        let mut waited = self.waited.lock();
        if *waited {
            return false;
        }
        *waited = true;
        true
    }
}

static WAIT_TABLE: Lazy<Mutex<BTreeMap<isize, Arc<WaitStatus>>>> =
    Lazy::new(|| Mutex::new(BTreeMap::new()));

pub struct UserProc {
    bin: Mutex<Option<File>>,
    fds: Mutex<BTreeMap<isize, FdEntry>>,
    next_fd: Mutex<isize>,
}

impl UserProc {
    pub fn new(file: File) -> Self {
        Self {
            bin: Mutex::new(Some(file)),
            fds: Mutex::new(BTreeMap::new()),
            next_fd: Mutex::new(3),
        }
    }

    pub(crate) fn add_file(&self, file: File, readable: bool, writable: bool) -> isize {
        let mut next_fd = self.next_fd.lock();
        let fd = *next_fd;
        *next_fd += 1;
        self.fds.lock().insert(
            fd,
            FdEntry {
                file,
                readable,
                writable,
            },
        );
        fd
    }

    pub(crate) fn with_fd<R>(
        &self,
        fd: isize,
        f: impl FnOnce(&mut File, bool, bool) -> R,
    ) -> Option<R> {
        let mut fds = self.fds.lock();
        let entry = fds.get_mut(&fd)?;
        Some(f(&mut entry.file, entry.readable, entry.writable))
    }

    pub(crate) fn close_fd(&self, fd: isize) -> bool {
        self.fds.lock().remove(&fd).is_some()
    }

    pub(crate) fn close_all_files(&self) {
        self.fds.lock().clear();
    }

    pub(crate) fn close_exec_file(&self) {
        self.bin.lock().take();
    }

    pub(crate) fn close_on_exit(&self) {
        self.close_all_files();
        self.close_exec_file();
    }
}

fn maybe_reap(child_tid: isize) {
    let should_remove = {
        let table = WAIT_TABLE.lock();
        table
            .get(&child_tid)
            .map_or(false, |status| status.should_reap())
    };

    if should_remove {
        WAIT_TABLE.lock().remove(&child_tid);
    }
}

pub(crate) fn on_parent_exit(parent_tid: isize) {
    let mut reap_list = Vec::new();
    {
        let table = WAIT_TABLE.lock();
        for (child_tid, status) in table.iter() {
            if status.parent_tid == parent_tid {
                *status.parent_alive.lock() = false;
                if !*status.child_alive.lock() {
                    reap_list.push(*child_tid);
                }
            }
        }
    }

    if !reap_list.is_empty() {
        let mut table = WAIT_TABLE.lock();
        for tid in reap_list {
            table.remove(&tid);
        }
    }
}

fn setup_arguments(
    pagetable: &PageTable,
    init_sp: usize,
    argv: &[String],
    frame: &mut Frame,
) -> Result<()> {
    let stack_bottom = init_sp - PG_SIZE;
    let stack_page = pagetable
        .get_pte(stack_bottom)
        .ok_or(OsError::BadPtr)?
        .pa()
        .into_va();
    let stack = unsafe { slice::from_raw_parts_mut(stack_page as *mut u8, PG_SIZE) };

    let mut sp = init_sp;
    let mut arg_ptrs = Vec::with_capacity(argv.len());

    for arg in argv.iter().rev() {
        let bytes = arg.as_bytes();
        let need = bytes.len() + 1;
        if need > PG_SIZE || sp < stack_bottom + need {
            return Err(OsError::ArgumentTooLong);
        }

        sp -= need;
        let off = sp - stack_bottom;
        stack[off..off + bytes.len()].copy_from_slice(bytes);
        stack[off + bytes.len()] = 0;
        arg_ptrs.push(sp);
    }

    arg_ptrs.reverse();

    sp &= !0x7;
    let argv_bytes = (argv.len() + 1) * core::mem::size_of::<usize>();
    if argv_bytes > PG_SIZE || sp < stack_bottom + argv_bytes {
        return Err(OsError::ArgumentTooLong);
    }

    sp = (sp - argv_bytes) & !0xf;
    if sp < stack_bottom {
        return Err(OsError::ArgumentTooLong);
    }

    let argv_user = sp;
    for (idx, ptr) in arg_ptrs.iter().enumerate() {
        let begin = argv_user - stack_bottom + idx * core::mem::size_of::<usize>();
        let end = begin + core::mem::size_of::<usize>();
        stack[begin..end].copy_from_slice(&(*ptr).to_ne_bytes());
    }

    let null_begin = argv_user - stack_bottom + arg_ptrs.len() * core::mem::size_of::<usize>();
    let null_end = null_begin + core::mem::size_of::<usize>();
    stack[null_begin..null_end].copy_from_slice(&0usize.to_ne_bytes());

    frame.x[2] = sp;
    frame.x[10] = argv.len();
    frame.x[11] = argv_user;

    Ok(())
}

/// Execute an object file with arguments.
///
/// ## Return
/// - `-1`: On error.
/// - `tid`: Tid of the newly spawned thread.
pub fn execute(mut file: File, argv: Vec<String>) -> isize {
    #[cfg(feature = "debug")]
    kprintln!(
        "[PROCESS] Kernel thread {} prepare to execute a process with args {:?}",
        thread::current().name(),
        argv
    );

    let parent_tid = thread::current().id();

    // It only copies L2 pagetable. This approach allows the new thread
    // to access kernel code and data during syscall without the need to
    // switch pagetables.
    let mut pt = KernelPgTable::clone();

    let exec_info = match load::load_executable(&mut file, &mut pt) {
        Ok(x) => x,
        Err(_) => unsafe {
            pt.destroy();
            return -1;
        },
    };

    // Initialize frame, pass argument to user.
    let mut frame = unsafe { MaybeUninit::<Frame>::zeroed().assume_init() };
    frame.sepc = exec_info.entry_point;
    frame.x[2] = exec_info.init_sp;

    if setup_arguments(&pt, exec_info.init_sp, &argv, &mut frame).is_err() {
        unsafe { pt.destroy() };
        return -1;
    }

    let userproc = UserProc::new(file);
    let new_thread = thread::Builder::new(move || start(frame))
        .pagetable(pt)
        .userproc(userproc)
        .build();
    let child_tid = new_thread.id();

    WAIT_TABLE
        .lock()
        .insert(child_tid, Arc::new(WaitStatus::new(parent_tid)));
    thread::register(new_thread);

    child_tid
}

/// Exits a process.
///
/// Panic if the current thread doesn't own a user process.
pub fn exit(value: isize) -> ! {
    let current = thread::current();
    if let Some(proc) = current.userproc.as_ref() {
        proc.close_on_exit();
    } else {
        panic!("Current thread does not own a user process");
    }

    let status = { WAIT_TABLE.lock().get(&current.id()).cloned() };
    if let Some(status) = status {
        status.mark_exited(value);
        maybe_reap(current.id());
    }

    thread::exit();
}

/// Waits for a child thread, which must own a user process.
///
/// ## Return
/// - `Some(exit_value)`
/// - `None`: if tid was not created by the current thread.
pub fn wait(tid: isize) -> Option<isize> {
    if tid < 0 {
        return None;
    }

    let current_tid = thread::current().id();
    let status = WAIT_TABLE.lock().get(&tid).cloned()?;
    if status.parent_tid != current_tid {
        return None;
    }

    if !status.mark_waited() {
        return None;
    }

    if *status.child_alive.lock() {
        status.exited.down();
    }

    let exit_value = (*status.exit_status.lock()).unwrap_or(-1);
    maybe_reap(tid);
    Some(exit_value)
}

/// Initializes a user process in current thread.
///
/// This function won't return.
pub fn start(mut frame: Frame) -> ! {
    unsafe { sstatus::set_spp(sstatus::SPP::User) };
    frame.sstatus = sstatus::read();

    // Set kernel stack pointer to intr frame and then jump to `trap_exit_u()`.
    let kernal_sp = (&frame as *const Frame) as usize;

    unsafe {
        asm!(
            "mv sp, t0",
            "jr t1",
            in("t0") kernal_sp,
            in("t1") trap_exit_u as *const u8
        );
    }

    unreachable!();
}
