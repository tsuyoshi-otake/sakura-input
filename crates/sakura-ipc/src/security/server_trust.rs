//! Server trust (#104): the image-path policy a client applies to the process
//! behind its pipe handle, and the named refusal when that check fails.

use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use windows::core::HRESULT;
use windows::Win32::System::Threading::{PROCESS_NAME_NATIVE, PROCESS_NAME_WIN32};

use super::admission::{classify_token, ClientTrust};
use super::process::{ProcessHandle, ProcessToken};

/// The process identity a production client will accept on a pipe handle.
///
/// This is deliberately a typed policy rather than a string prefix. A
/// versioned install may have an older TSF DLL talking to a newly installed
/// engine, so callers use [`InstalledRoot`](Self::InstalledRoot) for production. [`Exact`](Self::Exact) and
/// [`ExactNative`](Self::ExactNative) are for ownership-safe tests and diagnostics where one
/// image is intentionally fixed by the trusted caller rather than the peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerTrustPolicy {
    /// Accept exactly one canonical image path.
    Exact(PathBuf),
    /// Accept exactly one strict native-device image path.
    ///
    /// The trusted caller must obtain this expected identity from a process
    /// it owns. This variant selects `PROCESS_NAME_NATIVE` for the single
    /// admission-time image query; it does not accept a path claimed by the
    /// pipe peer or perform ambient drive mapping.
    ExactNative(PathBuf),
    /// Accept `root\\versions\\<one direct release directory>\\sakura_engine.exe`.
    InstalledRoot(PathBuf),
}

pub(super) const ENGINE_IMAGE_NAME: &str = "sakura_engine.exe";
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;

impl ServerTrustPolicy {
    /// Returns whether a queried image path satisfies this policy.
    ///
    /// Both sides are canonicalized and every existing component is checked
    /// for `FILE_ATTRIBUTE_REPARSE_POINT`. Lexical `..` is rejected before
    /// canonicalization so a policy cannot accidentally become a broad path
    /// check. `Exact` also has a strictly-equal lexical fallback for
    /// AppContainer tests/diagnostics: restricted tokens may query the engine
    /// process but have no filesystem traversal right to a developer target
    /// directory. Production uses `InstalledRoot`, which never takes that
    /// fallback. No textual-prefix comparison is used.
    pub fn matches_image_path(&self, image: &Path) -> bool {
        match self {
            Self::Exact(expected) => {
                match (
                    canonical_non_reparse(expected),
                    canonical_non_reparse(image),
                ) {
                    (Some(expected), Some(image)) => {
                        is_engine_image(&image) && same_windows_path(&expected, &image)
                    }
                    _ => {
                        lexically_safe_engine_path(expected)
                            && lexically_safe_engine_path(image)
                            && same_windows_path(expected, image)
                    }
                }
            }
            Self::ExactNative(expected) => {
                let expected: Vec<u16> = expected.as_os_str().encode_wide().collect();
                let image: Vec<u16> = image.as_os_str().encode_wide().collect();
                strict_native_engine_path(&expected)
                    && strict_native_engine_path(&image)
                    && expected == image
            }
            Self::InstalledRoot(root) => {
                let Some(image) = canonical_non_reparse(image) else {
                    return false;
                };
                installed_layout_matches(root, &image)
            }
        }
    }
}

/// Why a verified connection refused its server.
///
/// The caller used to reduce this to `.is_err()`, which is how Issue #104 — a
/// CI rejection that has now happened four times — produced no evidence at all
/// beyond "rejected". Every variant names exactly one call or one policy
/// decision and carries nothing but an OS error code, so it is safe to print
/// wherever the fault itself is printed.
///
/// The distinction that matters is between a question answered "no" and a
/// question that could not be asked: `ImagePathRejected` means the observed
/// path did not satisfy policy, while `ImagePathUnreadable` means we never
/// obtained an image path. Rejection alone does not identify which lexical,
/// filesystem or equality condition failed, or prove a different executable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerRejection {
    /// The trust policy itself could not be built, so nothing was verified.
    /// The connection is refused because the question could not be put, not
    /// because the peer failed it.
    PolicyUnavailable,
    /// The kernel reported no usable server process for this pipe handle.
    NoServerProcessId,
    /// `OpenProcess` failed for the kernel-reported server PID.
    ProcessUnopenable(HRESULT),
    /// `QueryFullProcessImageNameW` failed for that process.
    ImagePathUnreadable(HRESULT),
    /// The image path was read, and it is not one this policy accepts.
    ImagePathRejected,
    /// `OpenProcessToken` failed for that process.
    TokenUnopenable(HRESULT),
    /// The token was opened but could not be classified.
    TokenUnclassifiable(HRESULT),
    /// The token classified below medium integrity.
    IntegrityRejected,
}

impl core::fmt::Display for ServerRejection {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::PolicyUnavailable => write!(f, "trust policy unavailable"),
            Self::NoServerProcessId => write!(f, "no server process id"),
            Self::ProcessUnopenable(code) => write!(f, "OpenProcess failed ({code:?})"),
            Self::ImagePathUnreadable(code) => {
                write!(f, "QueryFullProcessImageNameW failed ({code:?})")
            }
            Self::ImagePathRejected => write!(f, "image path is not the one the policy accepts"),
            Self::TokenUnopenable(code) => write!(f, "OpenProcessToken failed ({code:?})"),
            Self::TokenUnclassifiable(code) => write!(f, "token classification failed ({code:?})"),
            Self::IntegrityRejected => write!(f, "token integrity below medium"),
        }
    }
}

/// Verifies the process attached to one exact client pipe handle.
///
/// The caller must obtain `process_id` from `GetNamedPipeServerProcessId` on
/// that same handle. The image path and token are then read from the kernel;
/// a peer cannot satisfy this contract by sending a forged Hello field.
///
/// The refusal is returned as a [`ServerRejection`] rather than an `Error`
/// because every step below can fail for `ERROR_ACCESS_DENIED`, so an HRESULT
/// alone cannot say which one did. What the caller does is unchanged: any
/// `Err` refuses the connection.
pub fn verify_server_process(
    process_id: u32,
    policy: &ServerTrustPolicy,
) -> core::result::Result<(), ServerRejection> {
    let process = ProcessHandle::open(process_id)
        .map_err(|error| ServerRejection::ProcessUnopenable(error.code()))?;
    let image_format = match policy {
        ServerTrustPolicy::ExactNative(_) => PROCESS_NAME_NATIVE,
        ServerTrustPolicy::Exact(_) | ServerTrustPolicy::InstalledRoot(_) => PROCESS_NAME_WIN32,
    };
    let image = process
        .image_path(image_format)
        .map_err(|error| ServerRejection::ImagePathUnreadable(error.code()))?;
    if !policy.matches_image_path(&image) {
        return Err(ServerRejection::ImagePathRejected);
    }
    let token = ProcessToken::open_process(&process)
        .map_err(|error| ServerRejection::TokenUnopenable(error.code()))?;
    let trust = classify_token(&token)
        .map_err(|error| ServerRejection::TokenUnclassifiable(error.code()))?;
    if trust != ClientTrust::MediumOrHigher {
        return Err(ServerRejection::IntegrityRejected);
    }
    Ok(())
}

fn installed_layout_matches(root: &Path, image: &Path) -> bool {
    let Some(root) = canonical_non_reparse(root) else {
        return false;
    };
    let Some(versions) = canonical_non_reparse(&root.join("versions")) else {
        return false;
    };
    let Some(parent) = image.parent() else {
        return false;
    };
    let Some(release_name) = parent.file_name() else {
        return false;
    };
    let Some(version_parent) = parent.parent() else {
        return false;
    };
    // Comparing the canonical directory itself, rather than the original
    // strings, prevents both sibling-prefix confusion and `..` aliases.
    if !same_windows_path(version_parent, &versions) {
        return false;
    }
    !release_name.is_empty() && is_engine_image(image)
}

fn is_engine_image(path: &Path) -> bool {
    path.file_name().is_some_and(|name| {
        name.to_string_lossy()
            .eq_ignore_ascii_case(ENGINE_IMAGE_NAME)
    })
}

fn lexically_safe_engine_path(path: &Path) -> bool {
    path.is_absolute()
        && is_engine_image(path)
        && !path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
}

fn same_windows_path(left: &Path, right: &Path) -> bool {
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

fn strict_native_engine_path(path: &[u16]) -> bool {
    let native_prefix: Vec<u16> = r"\Device\".encode_utf16().collect();
    let engine_name: Vec<u16> = ENGINE_IMAGE_NAME.encode_utf16().collect();
    path.starts_with(&native_prefix)
        && path[native_prefix.len()..].contains(&u16::from(b'\\'))
        && !path.contains(&0)
        && !path.iter().any(|unit| *unit == u16::from(b'/'))
        && path
            .split(|unit| *unit == u16::from(b'\\'))
            .skip(1)
            .all(|part| {
                !part.is_empty()
                    && part != [u16::from(b'.')]
                    && part != [u16::from(b'.'), u16::from(b'.')]
            })
        && path
            .rsplit(|unit| *unit == u16::from(b'\\'))
            .next()
            .is_some_and(|name| name == engine_name.as_slice())
}

fn canonical_non_reparse(path: &Path) -> Option<PathBuf> {
    if contains_parent_component(path) || reject_reparse_components(path).is_err() {
        return None;
    }
    let canonical = std::fs::canonicalize(path).ok()?;
    if reject_reparse_components(&canonical).is_err() {
        return None;
    }
    Some(canonical)
}

fn contains_parent_component(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, Component::ParentDir))
}

fn reject_reparse_components(path: &Path) -> std::io::Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => current.push(prefix.as_os_str()),
            Component::RootDir => current.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => return Err(std::io::ErrorKind::InvalidInput.into()),
            Component::Normal(name) => {
                current.push(name);
                let metadata = std::fs::symlink_metadata(&current)?;
                if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "reparse-point path is not trusted",
                    ));
                }
            }
        }
    }
    Ok(())
}
