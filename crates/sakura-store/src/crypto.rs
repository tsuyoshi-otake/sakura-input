//! Encryption boundary for durable store payloads.
//!
//! The caller owns framing, checksums, retention, and lifecycle. A sealer only
//! transforms one already-bounded payload and does not persist it.

use std::io;

/// Reversible protection for one durable payload.
pub trait Sealer {
    fn seal(&self, bytes: &[u8]) -> io::Result<Vec<u8>>;
    fn open(&self, bytes: &[u8]) -> io::Result<Vec<u8>>;
}

/// Current-user Windows DPAPI protection with no optional entropy.
#[cfg(all(windows, feature = "dpapi"))]
#[derive(Debug, Default, Clone, Copy)]
pub struct DpapiSealer;

#[cfg(all(windows, feature = "dpapi"))]
impl Sealer for DpapiSealer {
    fn seal(&self, bytes: &[u8]) -> io::Result<Vec<u8>> {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::{LocalFree, HLOCAL};
        use windows::Win32::Security::Cryptography::{CryptProtectData, CRYPT_INTEGER_BLOB};

        let input = CRYPT_INTEGER_BLOB {
            cbData: u32::try_from(bytes.len())
                .map_err(|_| invalid_data("input history payload too large"))?,
            pbData: bytes.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: `input` borrows `bytes` for the duration of the call and
        // `output` is writable. DPAPI allocates `output.pbData`; it is copied
        // before being released exactly once with LocalFree.
        unsafe {
            CryptProtectData(&input, PCWSTR::null(), None, None, None, 0, &mut output)
                .map_err(|error| io::Error::other(format!("DPAPI protect: {error}")))?;
            let protected = if output.pbData.is_null() {
                Err(io::Error::other("DPAPI returned an empty payload"))
            } else {
                Ok(std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec())
            };
            let _ = LocalFree(Some(HLOCAL(output.pbData.cast())));
            protected
        }
    }

    fn open(&self, bytes: &[u8]) -> io::Result<Vec<u8>> {
        use windows::Win32::Foundation::{LocalFree, HLOCAL};
        use windows::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};

        let input = CRYPT_INTEGER_BLOB {
            cbData: u32::try_from(bytes.len())
                .map_err(|_| invalid_data("protected payload too large"))?,
            pbData: bytes.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: `input` borrows the protected bytes for the call and
        // `output` is writable. DPAPI owns the returned allocation until the
        // matching LocalFree after the plaintext has been copied.
        unsafe {
            CryptUnprotectData(&input, None, None, None, None, 0, &mut output)
                .map_err(|error| io::Error::other(format!("DPAPI unprotect: {error}")))?;
            let plain = if output.pbData.is_null() {
                Err(io::Error::other("DPAPI returned an empty payload"))
            } else {
                Ok(std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec())
            };
            let _ = LocalFree(Some(HLOCAL(output.pbData.cast())));
            plain
        }
    }
}

#[cfg(all(windows, feature = "dpapi"))]
fn invalid_data(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct PlainSealer;

    impl Sealer for PlainSealer {
        fn seal(&self, bytes: &[u8]) -> io::Result<Vec<u8>> {
            Ok(bytes.to_vec())
        }

        fn open(&self, bytes: &[u8]) -> io::Result<Vec<u8>> {
            Ok(bytes.to_vec())
        }
    }

    #[test]
    fn plain_sealer_roundtrips_without_platform_dependencies() {
        let payload = b"synthetic-history-payload";
        let sealed = PlainSealer.seal(payload).unwrap();
        assert_eq!(PlainSealer.open(&sealed).unwrap(), payload);
    }

    #[cfg(all(windows, feature = "dpapi"))]
    #[test]
    fn dpapi_sealer_roundtrips_for_current_user() {
        let payload = b"synthetic-history-payload";
        let sealed = DpapiSealer.seal(payload).unwrap();
        assert_ne!(sealed, payload);
        assert_eq!(DpapiSealer.open(&sealed).unwrap(), payload);
    }

    #[cfg(all(windows, feature = "dpapi"))]
    #[test]
    fn dpapi_sealer_rejects_malformed_ciphertext() {
        let error = DpapiSealer.open(b"not-a-dpapi-blob").unwrap_err();
        assert!(error.to_string().starts_with("DPAPI unprotect:"));
    }
}
