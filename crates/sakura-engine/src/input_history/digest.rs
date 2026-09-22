//! Windows-owned SHA-256 state for history publication validation.

use std::io;
use windows::Win32::Security::Cryptography::{
    BCryptCreateHash, BCryptDestroyHash, BCryptFinishHash, BCryptHashData, BCRYPT_HASH_HANDLE,
    BCRYPT_SHA256_ALG_HANDLE,
};

pub(super) struct Sha256(BCRYPT_HASH_HANDLE);

impl Sha256 {
    pub(super) fn new() -> io::Result<Self> {
        let mut handle = BCRYPT_HASH_HANDLE::default();
        // SAFETY: Windows 11 supports the SHA-256 pseudo-provider. With no
        // caller object buffer, BCrypt owns the object until DestroyHash.
        unsafe { BCryptCreateHash(BCRYPT_SHA256_ALG_HANDLE, &mut handle, None, None, 0) }
            .ok()
            .map_err(io::Error::other)?;
        Ok(Self(handle))
    }

    pub(super) fn update(&mut self, bytes: &[u8]) -> io::Result<()> {
        // SAFETY: the owned handle is live and the bounded input stays valid
        // for this synchronous call. Finish consumes the state.
        unsafe { BCryptHashData(self.0, bytes, 0) }
            .ok()
            .map_err(io::Error::other)
    }

    pub(super) fn finish(self) -> io::Result<[u8; 32]> {
        let mut output = [0u8; 32];
        // SAFETY: SHA-256 produces exactly 32 bytes; this is the sole finish.
        unsafe { BCryptFinishHash(self.0, &mut output, 0) }
            .ok()
            .map_err(io::Error::other)?;
        Ok(output)
    }

    pub(super) fn digest(bytes: &[u8]) -> io::Result<[u8; 32]> {
        let mut state = Self::new()?;
        state.update(bytes)?;
        state.finish()
    }
}

impl Drop for Sha256 {
    fn drop(&mut self) {
        // SAFETY: this value exclusively owns the hash and its provider
        // storage; success, failure and abandonment all release it once.
        unsafe {
            let _ = BCryptDestroyHash(self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Sha256;

    #[test]
    fn known_answers_and_incremental_boundaries() {
        let hex = |bytes: [u8; 32]| {
            bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        };
        assert_eq!(
            hex(Sha256::digest(b"").unwrap()),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let mut state = Sha256::new().unwrap();
        state.update(b"a").unwrap();
        state.update(b"").unwrap();
        state.update(b"bc").unwrap();
        assert_eq!(
            hex(state.finish().unwrap()),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
