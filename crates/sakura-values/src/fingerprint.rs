/// Opaque equality token for identifying an exact candidate snapshot.
///
/// The representation is stable for protocol transfer, but the value is not
/// a cryptographic digest and must not be used for authentication or trust.
pub type Fingerprint = u64;
