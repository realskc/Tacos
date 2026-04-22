//! Syscall handlers
//!

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as FmtWrite;
use core::mem::size_of;
use core::slice;

use crate::fs::disk::DISKFS;
use crate::fs::File;
use crate::fs::FileSys;
use crate::io::prelude::*;
use crate::mem::userbuf::{
    copy_from_user, copy_to_user, read_cstring, read_usize, validate_writable,
};
use crate::sbi;
use crate::thread;
use crate::userproc;

/* -------------------------------------------------------------------------- */
/*                               SYSCALL NUMBER                               */
/* -------------------------------------------------------------------------- */

const SYS_HALT: usize = 1;
const SYS_EXIT: usize = 2;
const SYS_EXEC: usize = 3;
const SYS_WAIT: usize = 4;
const SYS_REMOVE: usize = 5;
const SYS_OPEN: usize = 6;
const SYS_READ: usize = 7;
const SYS_WRITE: usize = 8;
const SYS_SEEK: usize = 9;
const SYS_TELL: usize = 10;
const SYS_CLOSE: usize = 11;
const SYS_FSTAT: usize = 12;

const STDIN_FD: isize = 0;
const STDOUT_FD: isize = 1;
const STDERR_FD: isize = 2;
const MAX_ARG_BYTES: usize = crate::mem::PG_SIZE;

const O_RDONLY: usize = 0x000;
const O_WRONLY: usize = 0x001;
const O_RDWR: usize = 0x002;
const O_CREATE: usize = 0x200;
const O_TRUNC: usize = 0x400;
const O_ACCMODE: usize = 0x003;

#[repr(C)]
struct UserStat {
    ino: u32,
    _pad: u32,
    size: u64,
}

fn with_current_proc<R>(f: impl FnOnce(&userproc::UserProc) -> R) -> Option<R> {
    let current = thread::current();
    let proc = current.userproc.as_ref()?;
    Some(f(proc))
}

fn with_current_fd<R>(fd: isize, f: impl FnOnce(&mut File, bool, bool) -> R) -> Option<R> {
    with_current_proc(|proc| proc.with_fd(fd, f)).flatten()
}

fn read_path(pathname: *const u8) -> Option<String> {
    match read_cstring(pathname, MAX_ARG_BYTES) {
        Ok(path) if !path.is_empty() => Some(path),
        _ => None,
    }
}

fn zeroed_vec(size: usize) -> Vec<u8> {
    let mut data = Vec::new();
    data.resize(size, 0);
    data
}

fn open_with_flags(pathname: &str, flags: usize) -> crate::Result<File> {
    if flags & O_CREATE != 0 {
        if flags & O_TRUNC != 0 {
            DISKFS.create(pathname.into())
        } else {
            match DISKFS.open(pathname.into()) {
                Ok(file) => Ok(file),
                Err(_) => DISKFS.create(pathname.into()),
            }
        }
    } else if flags & O_TRUNC != 0 {
        match DISKFS.open(pathname.into()) {
            Ok(_) => DISKFS.create(pathname.into()),
            Err(err) => Err(err),
        }
    } else {
        DISKFS.open(pathname.into())
    }
}

fn read_exec_argv(user_argv: *const usize) -> Option<Vec<String>> {
    if user_argv.is_null() {
        return None;
    }

    let mut argv = Vec::new();
    let mut total_len = 0usize;
    let mut terminated = false;

    for i in 0..(MAX_ARG_BYTES / size_of::<usize>()) {
        let ptr = read_usize(unsafe { user_argv.add(i) }).ok()?;
        if ptr == 0 {
            terminated = true;
            break;
        }

        let remain = MAX_ARG_BYTES.checked_sub(total_len)?;
        if remain == 0 {
            return None;
        }

        let arg = read_cstring(ptr as *const u8, remain).ok()?;
        total_len += arg.len() + 1;
        argv.push(arg);
    }

    if terminated && !argv.is_empty() {
        Some(argv)
    } else {
        None
    }
}

fn sys_exec(pathname: *const u8, user_argv: *const usize) -> isize {
    let pathname = match read_path(pathname) {
        Some(path) => path,
        None => return -1,
    };
    let argv = match read_exec_argv(user_argv) {
        Some(argv) => argv,
        None => return -1,
    };

    let file = match DISKFS.open(pathname.as_str().into()) {
        Ok(file) => file,
        Err(_) => return -1,
    };

    userproc::execute(file, argv)
}

fn sys_wait(pid: isize) -> isize {
    userproc::wait(pid).unwrap_or(-1)
}

fn sys_remove(pathname: *const u8) -> isize {
    let pathname = match read_path(pathname) {
        Some(path) => path,
        None => return -1,
    };

    if DISKFS.remove(pathname.as_str().into()).is_ok() {
        0
    } else {
        -1
    }
}

fn sys_open(pathname: *const u8, flags: usize) -> isize {
    let pathname = match read_path(pathname) {
        Some(path) => path,
        None => return -1,
    };

    let (readable, writable) = match flags & O_ACCMODE {
        O_RDONLY => (true, false),
        O_WRONLY => (false, true),
        O_RDWR => (true, true),
        _ => return -1,
    };

    if (flags & O_TRUNC != 0) && !writable {
        return -1;
    }

    match open_with_flags(pathname.as_str(), flags) {
        Ok(file) => with_current_proc(|proc| proc.add_file(file, readable, writable)).unwrap_or(-1),
        Err(_) => -1,
    }
}

fn sys_read(fd: isize, buffer: *mut u8, size: usize) -> isize {
    if size == 0 {
        return 0;
    }
    if validate_writable(buffer, size).is_err() {
        return -1;
    }

    match fd {
        STDIN_FD => {
            let mut data = Vec::with_capacity(size);
            while data.len() < size {
                let ch = sbi::console_getchar();
                if ch == usize::MAX {
                    thread::schedule();
                    continue;
                }
                data.push(ch as u8);
            }
            match copy_to_user(buffer, data.as_slice()) {
                Ok(()) => data.len() as isize,
                Err(_) => -1,
            }
        }
        STDOUT_FD | STDERR_FD => -1,
        fd if fd > STDERR_FD => with_current_fd(fd, |file, readable, _| {
            if !readable {
                return -1;
            }

            let mut data = zeroed_vec(size);
            match file.read(data.as_mut_slice()) {
                Ok(cnt) => match copy_to_user(buffer, &data[..cnt]) {
                    Ok(()) => cnt as isize,
                    Err(_) => -1,
                },
                Err(_) => -1,
            }
        })
        .unwrap_or(-1),
        _ => -1,
    }
}

fn sys_write(fd: isize, buffer: *const u8, size: usize) -> isize {
    if size == 0 {
        return 0;
    }
    let mut data = Vec::new();
    data.resize(size, 0);
    if copy_from_user(buffer, data.as_mut_slice()).is_err() {
        return -1;
    }

    match fd {
        STDIN_FD => -1,
        STDOUT_FD | STDERR_FD => {
            let mut out = crate::sbi::console::stdout().lock();
            for ch in data {
                let _ = out.write_char(ch as char);
            }
            size as isize
        }
        fd if fd > STDERR_FD => with_current_fd(fd, |file, _, writable| {
            if !writable {
                return -1;
            }
            match file.write(data.as_slice()) {
                Ok(cnt) => cnt as isize,
                Err(_) => -1,
            }
        })
        .unwrap_or(-1),
        _ => -1,
    }
}

fn sys_seek(fd: isize, position: usize) {
    if fd <= STDERR_FD {
        return;
    }

    let _ = with_current_fd(fd, |file, _, _| {
        let _ = file.seek(SeekFrom::Start(position));
    });
}

fn sys_tell(fd: isize) -> isize {
    if fd <= STDERR_FD {
        return -1;
    }

    with_current_fd(fd, |file, _, _| match file.stream_position() {
        Ok(pos) => pos as isize,
        Err(_) => -1,
    })
    .unwrap_or(-1)
}

fn sys_close(fd: isize) -> isize {
    match fd {
        STDIN_FD | STDOUT_FD | STDERR_FD => 0,
        fd if fd > STDERR_FD => {
            with_current_proc(|proc| if proc.close_fd(fd) { 0 } else { -1 }).unwrap_or(-1)
        }
        _ => -1,
    }
}

fn sys_fstat(fd: isize, user_buf: *mut UserStat) -> isize {
    if user_buf.is_null() || fd <= STDERR_FD {
        return -1;
    }

    with_current_fd(fd, |file, _, _| {
        let stat = UserStat {
            ino: file.inum() as u32,
            _pad: 0,
            size: file.len().unwrap_or(0) as u64,
        };
        let bytes = unsafe {
            slice::from_raw_parts(
                (&stat as *const UserStat) as *const u8,
                core::mem::size_of::<UserStat>(),
            )
        };
        match copy_to_user(user_buf as *mut u8, bytes) {
            Ok(()) => 0,
            Err(_) => -1,
        }
    })
    .unwrap_or(-1)
}

pub fn syscall_handler(id: usize, args: [usize; 3]) -> isize {
    match id {
        SYS_HALT => sbi::shutdown(),
        SYS_EXIT => userproc::exit(args[0] as isize),
        SYS_EXEC => sys_exec(args[0] as *const u8, args[1] as *const usize),
        SYS_WAIT => sys_wait(args[0] as isize),
        SYS_REMOVE => sys_remove(args[0] as *const u8),
        SYS_OPEN => sys_open(args[0] as *const u8, args[1]),
        SYS_READ => sys_read(args[0] as isize, args[1] as *mut u8, args[2]),
        SYS_WRITE => sys_write(args[0] as isize, args[1] as *const u8, args[2]),
        SYS_SEEK => {
            sys_seek(args[0] as isize, args[1]);
            0
        }
        SYS_TELL => sys_tell(args[0] as isize),
        SYS_CLOSE => sys_close(args[0] as isize),
        SYS_FSTAT => sys_fstat(args[0] as isize, args[1] as *mut UserStat),
        _ => -1,
    }
}
