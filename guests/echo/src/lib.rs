//! # kanbrick-guest-echo
//!
//! The minimal Phase 3 WASM guest (issue #21): it returns its input bytes
//! unchanged. It implements the raw host↔guest calling convention from ADR-0002
//! by hand (no SDK yet — that arrives in #39):
//!
//! * `kbk_alloc(len) -> ptr` — reserve `len` bytes of guest linear memory and
//!   return the offset the host should write input into.
//! * `kbk_run(ptr, len) -> packed` — process the `len` input bytes at `ptr` and
//!   return `(out_ptr << 32) | out_len`, with the output living in guest memory.
//!
//! Each dispatch uses a fresh instance whose `Store` is dropped afterwards, so
//! the small leaks here (we hand raw buffers to the host and forget them) are
//! reclaimed wholesale when the instance goes away.

/// Reserve `len` bytes and return the pointer (linear-memory offset) to them.
///
/// # Safety
/// The returned pointer is valid until the instance is torn down. The host must
/// not write more than `len` bytes.
#[no_mangle]
pub extern "C" fn kbk_alloc(len: u32) -> u32 {
    let mut buf: Vec<u8> = Vec::with_capacity(len as usize);
    let ptr = buf.as_mut_ptr() as u32;
    std::mem::forget(buf);
    ptr
}

/// Echo: copy the input bytes to a fresh buffer and return its packed
/// `(ptr, len)`. The high 32 bits are the output pointer, the low 32 the length.
///
/// # Safety
/// `ptr`/`len` must describe a region previously filled by the host via
/// [`kbk_alloc`].
#[no_mangle]
pub extern "C" fn kbk_run(ptr: u32, len: u32) -> u64 {
    let input = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
    let mut out = input.to_vec();
    let out_ptr = out.as_mut_ptr() as u64;
    let out_len = out.len() as u64;
    std::mem::forget(out);
    (out_ptr << 32) | (out_len & 0xffff_ffff)
}
