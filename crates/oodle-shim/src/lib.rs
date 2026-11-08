//! A stand-in for Oodle's `OodleLZ_Decompress`, built on the open-source
//! (MIT) oozextract decoder, so the icon extractor can read the game's
//! Oodle-compressed files without the proprietary `oo2core` DLL.
//!
//! The extractor's Oodle wrapper looks up three functions. Only
//! decompression is real; the other two exist so the lookup succeeds.

use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};

/// Decompresses `comp_len` bytes into exactly `raw_len` bytes. Returns
/// `raw_len` on success and 0 on failure, like Oodle. The trailing
/// arguments (fuzz safety, CRC, callbacks, decoder memory, thread phase)
/// don't apply to this decoder and are ignored.
///
/// # Safety
/// `comp` must point to `comp_len` readable bytes and `raw` to `raw_len`
/// writable bytes, as with Oodle itself.
#[no_mangle]
pub unsafe extern "C" fn OodleLZ_Decompress(
    comp: *const u8,
    comp_len: isize,
    raw: *mut u8,
    raw_len: isize,
    _fuzz_safe: i32,
    _check_crc: i32,
    _verbosity: i32,
    _dec_buf_base: *mut c_void,
    _dec_buf_size: isize,
    _callback: *mut c_void,
    _callback_data: *mut c_void,
    _decoder_memory: *mut c_void,
    _decoder_memory_size: isize,
    _thread_phase: i32,
) -> isize {
    if comp.is_null() || raw.is_null() || comp_len <= 0 || raw_len <= 0 {
        return 0;
    }
    let input = std::slice::from_raw_parts(comp, comp_len as usize);
    let output = std::slice::from_raw_parts_mut(raw, raw_len as usize);
    decompress(input, output).map(|n| n as isize).unwrap_or(0)
}

/// Safe core, also used by the tests. A panic must never cross into C#.
pub fn decompress(input: &[u8], output: &mut [u8]) -> Option<usize> {
    let want = output.len();
    let result = catch_unwind(AssertUnwindSafe(|| oozextract::Extractor::new().read_from_slice(input, output)));
    match result {
        Ok(Ok(n)) if n == want => Some(n),
        _ => None,
    }
}

/// Compression isn't supported; the extractor only reads.
#[no_mangle]
pub extern "C" fn OodleLZ_Compress(
    _compressor: i32,
    _raw: *const u8,
    _raw_len: isize,
    _comp: *mut u8,
    _level: i32,
    _options: *const c_void,
    _dictionary_base: *const c_void,
    _lrm: *const c_void,
    _scratch: *mut c_void,
    _scratch_size: isize,
) -> isize {
    0
}

/// Oodle's documented worst case: raw size plus 274 bytes per 256 KB block.
#[no_mangle]
pub extern "C" fn OodleLZ_GetCompressedBufferSizeNeeded(_compressor: i32, raw_size: isize) -> isize {
    let blocks = (raw_size.max(0) + 0x3FFFF) / 0x40000;
    raw_size + 274 * blocks.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn garbage_fails_without_panicking() {
        let mut out = vec![0u8; 1024];
        assert_eq!(decompress(&[0x8C, 0x0A, 1, 2, 3, 4, 5], &mut out), None);
        assert_eq!(decompress(&[], &mut out), None);
        let r = unsafe {
            OodleLZ_Decompress(
                std::ptr::null(),
                0,
                out.as_mut_ptr(),
                1024,
                1,
                0,
                0,
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
                3,
            )
        };
        assert_eq!(r, 0);
    }
}
