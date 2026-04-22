use alloc::string::String;
use alloc::vec::Vec;
use core::arch::global_asm;

use crate::error::OsError;
use crate::mem::in_kernel_space;
use crate::Result;

/// Read a single byte from user space.
///
/// ## Return
/// - `Ok(byte)`
/// - `Err`: A page fault happened.
pub fn read_user_byte(user_src: *const u8) -> Result<u8> {
    if user_src.is_null() || in_kernel_space(user_src as usize) {
        return Err(OsError::BadPtr);
    }

    let byte: u8 = 0;
    let ret_status: u8 = unsafe { __knrl_read_usr_byte(user_src, &byte as *const u8) };

    if ret_status == 0 {
        Ok(byte)
    } else {
        Err(OsError::BadPtr)
    }
}

/// Write a single byte to user space.
///
/// ## Return
/// - `Ok(())`
/// - `Err`: A page fault happened.
pub fn write_user_byte(user_dst: *mut u8, value: u8) -> Result<()> {
    if user_dst.is_null() || in_kernel_space(user_dst as usize) {
        return Err(OsError::BadPtr);
    }

    let ret_status: u8 = unsafe { __knrl_write_usr_byte(user_dst, value) };

    if ret_status == 0 {
        Ok(())
    } else {
        Err(OsError::BadPtr)
    }
}

/// Copies a user buffer into a kernel buffer.
pub fn copy_from_user(user_src: *const u8, dst: &mut [u8]) -> Result<()> {
    for (i, byte) in dst.iter_mut().enumerate() {
        *byte = read_user_byte(unsafe { user_src.add(i) })?;
    }
    Ok(())
}

/// Copies a kernel buffer into a user buffer.
pub fn copy_to_user(user_dst: *mut u8, src: &[u8]) -> Result<()> {
    for (i, byte) in src.iter().enumerate() {
        write_user_byte(unsafe { user_dst.add(i) }, *byte)?;
    }
    Ok(())
}

/// Ensures a user buffer is writable across the whole range.
pub fn validate_writable(user_dst: *mut u8, len: usize) -> Result<()> {
    for i in 0..len {
        let ptr = unsafe { user_dst.add(i) };
        let byte = read_user_byte(ptr as *const u8)?;
        write_user_byte(ptr, byte)?;
    }
    Ok(())
}

/// Reads a machine word from user space.
pub fn read_usize(user_src: *const usize) -> Result<usize> {
    let mut buf = [0u8; core::mem::size_of::<usize>()];
    copy_from_user(user_src as *const u8, &mut buf)?;
    Ok(usize::from_ne_bytes(buf))
}

/// Reads a `\0`-terminated C string from user space.
pub fn read_cstring(user_src: *const u8, max_len: usize) -> Result<String> {
    if user_src.is_null() {
        return Err(OsError::BadPtr);
    }

    let mut buf = Vec::new();
    for i in 0..max_len {
        let ch = read_user_byte(unsafe { user_src.add(i) })?;
        if ch == 0 {
            return String::from_utf8(buf).map_err(|_| OsError::CstrFormatErr);
        }
        buf.push(ch);
    }

    Err(OsError::ArgumentTooLong)
}

extern "C" {
    pub fn __knrl_read_usr_byte(user_src: *const u8, byte_ptr: *const u8) -> u8;
    pub fn __knrl_read_usr_byte_pc();
    pub fn __knrl_read_usr_exit();
    pub fn __knrl_write_usr_byte(user_src: *const u8, value: u8) -> u8;
    pub fn __knrl_write_usr_byte_pc();
    pub fn __knrl_write_usr_exit();
}

global_asm! {r#"
        .section .text
        .globl __knrl_read_usr_byte
        .globl __knrl_read_usr_exit
        .globl __knrl_read_usr_byte_pc

    __knrl_read_usr_byte:
        mv t1, a1
        li a1, 0
    __knrl_read_usr_byte_pc:
        lb t0, (a0)
    __knrl_read_usr_exit:
        # pagefault handler will set a1 if any error occurs
        sb t0, (t1)
        mv a0, a1
        ret

        .globl __knrl_write_usr_byte
        .globl __knrl_write_usr_exit
        .globl __knrl_write_usr_byte_pc

    __knrl_write_usr_byte:
        mv t1, a1
        li a1, 0
    __knrl_write_usr_byte_pc:
        sb t1, (a0)
    __knrl_write_usr_exit:
        # pagefault handler will set a1 if any error occurs
        mv a0, a1
        ret
"#}
