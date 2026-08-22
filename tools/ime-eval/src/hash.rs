use sha2::{Digest, Sha256};

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex(&hasher.finalize())
}

pub fn sha256_file(path: &std::path::Path) -> Result<String, crate::types::Error> {
    let bytes = std::fs::read(path)
        .map_err(|error| crate::types::err(format!("read {}: {error}", path.display())))?;
    Ok(sha256_hex(&bytes))
}

pub fn hex(bytes: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(TABLE[(byte >> 4) as usize] as char);
        out.push(TABLE[(byte & 0x0f) as usize] as char);
    }
    out
}

pub fn toml_quoted(value: &str) -> Result<String, crate::types::Error> {
    if value.contains("'''") {
        return Err(crate::types::err(
            "developer instructions contain ''' and cannot be TOML-encoded",
        ));
    }
    Ok(format!("'''{value}'''"))
}
