//! UTF-16 conversion helpers for the Win32 boundary.
//!
//! Two shapes are needed and they are not interchangeable. Win32 APIs that take
//! a bare pointer want a NUL terminator; APIs that take a pointer *and* a count
//! (`RegisterProfile`, for instance) want the count to exclude it. Passing the
//! wrong one shows a stray `\0` in the language bar, so the two live in
//! separately named functions rather than one function with a flag.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

/// UTF-16 with a trailing NUL, for pointer-only APIs.
pub fn to_wide_nul(s: &str) -> Vec<u16> {
    let mut v: Vec<u16> = s.encode_utf16().collect();
    v.push(0);
    v
}

/// UTF-16 without a terminator, for pointer-plus-count APIs.
pub fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// UTF-16 of an OS string without a terminator, for pointer-plus-count APIs.
pub fn os_to_wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().collect()
}

/// Little-endian bytes of a UTF-16 string, the form `RegSetValueExW` stores for
/// `REG_SZ`. The terminator is included because the registry expects the stored
/// value to carry it.
pub fn to_reg_sz_bytes(s: &str) -> Vec<u8> {
    to_wide_nul(s)
        .iter()
        .flat_map(|unit| unit.to_le_bytes())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nul_terminated_form_ends_with_zero() {
        assert_eq!(to_wide_nul("ab"), vec![0x61, 0x62, 0x00]);
    }

    #[test]
    fn counted_form_has_no_terminator() {
        assert_eq!(to_wide("ab"), vec![0x61, 0x62]);
    }

    #[test]
    fn reg_sz_bytes_are_little_endian_and_terminated() {
        assert_eq!(to_reg_sz_bytes("A"), vec![0x41, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn non_bmp_text_survives_as_a_surrogate_pair() {
        // U+1F338 CHERRY BLOSSOM: two units plus the terminator.
        assert_eq!(to_wide_nul("\u{1F338}"), vec![0xD83C, 0xDF38, 0x0000]);
    }
}
