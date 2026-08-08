//! Opt-in, fail-closed update discovery and installation.
//!
//! Network access is confined to this settings process. The engine, renderer,
//! TSF DLL, and logon stub never link WinHTTP. Every stateful branch returns an
//! explicit terminal outcome, and an installer cannot be launched until the
//! exact size, SHA-256 digest, and Authenticode trust checks all pass.

use std::ffi::c_void;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders,
    WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetOption,
    WinHttpSetTimeouts, INTERNET_DEFAULT_HTTPS_PORT, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
    WINHTTP_FLAG_REFRESH, WINHTTP_FLAG_SECURE, WINHTTP_OPEN_REQUEST_FLAGS,
    WINHTTP_OPTION_CONNECT_RETRIES, WINHTTP_OPTION_MAX_RESPONSE_HEADER_SIZE,
    WINHTTP_OPTION_REDIRECT_POLICY, WINHTTP_OPTION_REDIRECT_POLICY_NEVER,
    WINHTTP_OPTION_REJECT_USERPWD_IN_URL, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_LOCATION,
    WINHTTP_QUERY_RETRY_AFTER, WINHTTP_QUERY_STATUS_CODE,
};
use windows::Win32::Security::Cryptography::{
    BCryptCloseAlgorithmProvider, BCryptCreateHash, BCryptDestroyHash, BCryptFinishHash,
    BCryptGetProperty, BCryptHashData, BCryptOpenAlgorithmProvider, BCRYPT_ALG_HANDLE,
    BCRYPT_HANDLE, BCRYPT_HASH_HANDLE, BCRYPT_HASH_LENGTH, BCRYPT_OBJECT_LENGTH,
    BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS, BCRYPT_SHA256_ALGORITHM,
};
use windows::Win32::Security::WinTrust::{
    WinVerifyTrust, WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_DATA_0,
    WINTRUST_FILE_INFO, WTD_CHOICE_FILE, WTD_DISABLE_MD2_MD4,
    WTD_REVOCATION_CHECK_CHAIN_EXCLUDE_ROOT, WTD_REVOKE_WHOLECHAIN, WTD_STATEACTION_CLOSE,
    WTD_STATEACTION_VERIFY, WTD_UICONTEXT_INSTALL, WTD_UI_NONE,
};
use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
use windows::Win32::UI::Shell::{
    ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::storage::atomic_write;

pub const MANIFEST_URL: &str =
    "https://github.com/tsuyoshi-otake/sakura-input/releases/latest/download/release-manifest.txt";
pub const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
pub const MAX_INSTALLER_BYTES: u64 = 200 * 1024 * 1024;
pub const MAX_REDIRECTS: usize = 5;
pub const INSTALL_TIMEOUT: Duration = Duration::from_secs(30 * 60);

const HTTP_TOTAL_TIMEOUT: Duration = Duration::from_secs(60);
const INSTALLER_TOTAL_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const UPDATE_SETTINGS_SCHEMA: &str = "1";
const MANIFEST_SCHEMA: &str = "1";
const HTTP_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    pub fn parse(value: &str) -> Result<Self, String> {
        let mut fields = value.split('.');
        let major = parse_version_field(fields.next(), "major")?;
        let minor = parse_version_field(fields.next(), "minor")?;
        let patch = parse_version_field(fields.next(), "patch")?;
        if fields.next().is_some() {
            return Err("version must contain exactly major.minor.patch".to_owned());
        }
        Ok(Self {
            major,
            minor,
            patch,
        })
    }
}

impl fmt::Display for Version {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

fn parse_version_field(value: Option<&str>, name: &str) -> Result<u32, String> {
    let value = value.ok_or_else(|| format!("version is missing its {name} field"))?;
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!(
            "version {name} field must be an unsigned decimal integer"
        ));
    }
    if value.len() > 1 && value.starts_with('0') {
        return Err(format!(
            "version {name} field has an ambiguous leading zero"
        ));
    }
    value
        .parse::<u32>()
        .map_err(|_| format!("version {name} field is out of range"))
}

pub fn current_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("package version is valid semantic version")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseManifest {
    pub version: Version,
    pub installer_url: String,
    pub sha256: [u8; 32],
    pub size: u64,
}

impl ReleaseManifest {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.is_empty() {
            return Err("release manifest is empty".to_owned());
        }
        if bytes.len() as u64 > MAX_MANIFEST_BYTES {
            return Err(format!(
                "release manifest exceeds the {MAX_MANIFEST_BYTES}-byte limit"
            ));
        }
        let source = std::str::from_utf8(bytes)
            .map_err(|_| "release manifest is not canonical UTF-8".to_owned())?;
        if source.contains('\0') {
            return Err("release manifest contains a NUL character".to_owned());
        }

        let mut schema = None;
        let mut version = None;
        let mut installer_url = None;
        let mut sha256 = None;
        let mut size = None;
        let mut line_count = 0usize;
        for (index, line) in source.lines().enumerate() {
            line_count += 1;
            if line.is_empty() || line.len() > 2_048 || line.trim() != line {
                return Err(format!(
                    "release manifest line {} is empty, padded, or too long",
                    index + 1
                ));
            }
            let (key, value) = line.split_once('=').ok_or_else(|| {
                format!("release manifest line {} has no '=' separator", index + 1)
            })?;
            if value.is_empty() || value.contains('=') {
                return Err(format!(
                    "release manifest line {} has an invalid value",
                    index + 1
                ));
            }
            match key {
                "schema" => assign_once(&mut schema, value.to_owned(), key)?,
                "version" => assign_once(&mut version, Version::parse(value)?, key)?,
                "installer_url" => {
                    assign_once(&mut installer_url, value.to_owned(), key)?;
                }
                "sha256" => assign_once(&mut sha256, decode_sha256(value)?, key)?,
                "size" => assign_once(&mut size, parse_size(value)?, key)?,
                _ => return Err(format!("release manifest contains unknown key {key:?}")),
            }
        }
        if line_count != 5 {
            return Err(format!(
                "release manifest must contain exactly five fields, found {line_count}"
            ));
        }
        if schema.as_deref() != Some(MANIFEST_SCHEMA) {
            return Err("release manifest schema is missing or unsupported".to_owned());
        }
        let version = version.ok_or_else(|| "release manifest has no version".to_owned())?;
        let installer_url =
            installer_url.ok_or_else(|| "release manifest has no installer_url".to_owned())?;
        let expected_url = installer_url_for(version);
        if installer_url != expected_url {
            return Err(format!(
                "installer URL must be the canonical release asset URL {expected_url:?}"
            ));
        }
        Ok(Self {
            version,
            installer_url,
            sha256: sha256.ok_or_else(|| "release manifest has no sha256".to_owned())?,
            size: size.ok_or_else(|| "release manifest has no size".to_owned())?,
        })
    }

    pub fn canonical_text(&self) -> String {
        format!(
            "schema={MANIFEST_SCHEMA}\nversion={}\ninstaller_url={}\nsha256={}\nsize={}\n",
            self.version,
            self.installer_url,
            encode_hex(&self.sha256),
            self.size
        )
    }
}

pub fn installer_url_for(version: Version) -> String {
    format!(
        "https://github.com/tsuyoshi-otake/sakura-input/releases/download/v{version}/sakura_setup.exe"
    )
}

fn assign_once<T>(slot: &mut Option<T>, value: T, key: &str) -> Result<(), String> {
    if slot.replace(value).is_some() {
        Err(format!("release manifest contains duplicate key {key:?}"))
    } else {
        Ok(())
    }
}

fn parse_size(value: &str) -> Result<u64, String> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("installer size must be an unsigned decimal integer".to_owned());
    }
    if value.len() > 1 && value.starts_with('0') {
        return Err("installer size has an ambiguous leading zero".to_owned());
    }
    let size = value
        .parse::<u64>()
        .map_err(|_| "installer size is out of range".to_owned())?;
    if !(1..=MAX_INSTALLER_BYTES).contains(&size) {
        return Err(format!(
            "installer size must be between 1 and {MAX_INSTALLER_BYTES} bytes"
        ));
    }
    Ok(size)
}

fn decode_sha256(value: &str) -> Result<[u8; 32], String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("SHA-256 must be exactly 64 lowercase hexadecimal characters".to_owned());
    }
    let mut digest = [0u8; 32];
    for (index, byte) in digest.iter_mut().enumerate() {
        let offset = index * 2;
        *byte =
            (hex_nibble(value.as_bytes()[offset]) << 4) | hex_nibble(value.as_bytes()[offset + 1]);
    }
    Ok(digest)
}

const fn hex_nibble(value: u8) -> u8 {
    match value {
        b'0'..=b'9' => value - b'0',
        b'a'..=b'f' => value - b'a' + 10,
        _ => 0,
    }
}

pub fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 0x0f) as usize] as char);
    }
    result
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UpdatePreferences {
    pub enabled: bool,
}

impl UpdatePreferences {
    pub fn load(path: &Path) -> io::Result<Self> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error),
        };
        if bytes.len() > 4_096 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "update settings exceed the 4096-byte limit",
            ));
        }
        let source = std::str::from_utf8(&bytes).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "update settings are not UTF-8")
        })?;
        let mut schema = None;
        let mut enabled = None;
        let mut count = 0usize;
        for (index, line) in source.lines().enumerate() {
            count += 1;
            if line.is_empty() || line.trim() != line {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("update settings line {} is empty or padded", index + 1),
                ));
            }
            let (key, value) = line.split_once('=').ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("update settings line {} has no '='", index + 1),
                )
            })?;
            match key {
                "schema" if schema.replace(value).is_none() => {}
                "enabled" if enabled.is_none() => {
                    enabled = Some(match value {
                        "true" => true,
                        "false" => false,
                        _ => {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "update enabled value must be true or false",
                            ));
                        }
                    });
                }
                "schema" | "enabled" => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("duplicate update setting {key:?}"),
                    ));
                }
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("unknown update setting {key:?}"),
                    ));
                }
            }
        }
        if count != 2 || schema != Some(UPDATE_SETTINGS_SCHEMA) || enabled.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "update settings schema is missing, incomplete, or unsupported",
            ));
        }
        Ok(Self {
            enabled: enabled.expect("checked above"),
        })
    }

    pub fn save(self, path: &Path) -> io::Result<()> {
        atomic_write(
            path,
            format!(
                "schema={UPDATE_SETTINGS_SCHEMA}\nenabled={}\n",
                if self.enabled { "true" } else { "false" }
            )
            .as_bytes(),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadReceipt {
    pub size: u64,
    pub sha256: [u8; 32],
}

pub trait UpdateTransport {
    fn fetch_manifest(&mut self, url: &str, limit: u64) -> Result<Vec<u8>, String>;
    fn download_installer(
        &mut self,
        url: &str,
        path: &Path,
        limit: u64,
    ) -> Result<DownloadReceipt, String>;
}

pub trait SignatureVerifier {
    fn verify(&mut self, path: &Path) -> Result<(), String>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallerTerminal {
    Installed,
    RestartRequired,
    TimedOutStillRunning,
    Failed(u32),
}

pub trait InstallerRunner {
    fn run(&mut self, installer: &Path, log: &Path) -> Result<InstallerTerminal, String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatePaths {
    pub installer: PathBuf,
    pub log: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateStage {
    Worker,
    ManifestDownload,
    ManifestValidation,
    InstallerPreparation,
    InstallerDownload,
    InstallerSize,
    InstallerHash,
    SignatureVerification,
    InstallerLaunch,
    InstallerExit,
}

impl fmt::Display for UpdateStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Worker => "update worker",
            Self::ManifestDownload => "manifest download",
            Self::ManifestValidation => "manifest validation",
            Self::InstallerPreparation => "installer preparation",
            Self::InstallerDownload => "installer download",
            Self::InstallerSize => "installer size verification",
            Self::InstallerHash => "installer SHA-256 verification",
            Self::SignatureVerification => "installer Authenticode verification",
            Self::InstallerLaunch => "installer launch",
            Self::InstallerExit => "installer completion",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateFailure {
    pub stage: UpdateStage,
    pub message: String,
}

impl fmt::Display for UpdateFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} failed: {}", self.stage, self.message)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateCheckOutcome {
    Disabled,
    UpToDate { current: Version, latest: Version },
    Available(ReleaseManifest),
    Failed(UpdateFailure),
}

impl UpdateCheckOutcome {
    pub fn describe(&self) -> String {
        match self {
            Self::Disabled => {
                "Automatic updates are disabled (no network request made).".to_owned()
            }
            Self::UpToDate { current, latest } => {
                format!("Sakura Input {current} is current (latest release: {latest}).")
            }
            Self::Available(manifest) => {
                format!("Sakura Input {} is available.", manifest.version)
            }
            Self::Failed(failure) => failure.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateOutcome {
    Disabled,
    UpToDate {
        current: Version,
        latest: Version,
    },
    Installed {
        version: Version,
    },
    RestartRequired {
        version: Version,
    },
    TimedOutStillRunning {
        version: Version,
    },
    Failed {
        version: Option<Version>,
        failure: UpdateFailure,
    },
}

impl UpdateOutcome {
    pub fn describe(&self) -> String {
        match self {
            Self::Disabled => "Automatic updates are disabled (no network request made).".to_owned(),
            Self::UpToDate { current, latest } => {
                format!("Sakura Input {current} is current (latest release: {latest}).")
            }
            Self::Installed { version } => format!("Sakura Input {version} was installed."),
            Self::RestartRequired { version } => format!(
                "Sakura Input {version} was installed, but the installer requested a Windows restart for legacy cleanup. The active version may already be available."
            ),
            Self::TimedOutStillRunning { version } => format!(
                "The Sakura Input {version} installer is still running after the 30-minute wait limit."
            ),
            Self::Failed { failure, .. } => failure.to_string(),
        }
    }

    pub const fn is_failure(&self) -> bool {
        matches!(self, Self::Failed { .. })
    }
}

pub fn check_for_update<T: UpdateTransport>(
    enabled: bool,
    current: Version,
    transport: &mut T,
) -> UpdateCheckOutcome {
    if !enabled {
        return UpdateCheckOutcome::Disabled;
    }
    let bytes = match transport.fetch_manifest(MANIFEST_URL, MAX_MANIFEST_BYTES) {
        Ok(bytes) => bytes,
        Err(message) => {
            return UpdateCheckOutcome::Failed(UpdateFailure {
                stage: UpdateStage::ManifestDownload,
                message,
            });
        }
    };
    let manifest = match ReleaseManifest::parse(&bytes) {
        Ok(manifest) => manifest,
        Err(message) => {
            return UpdateCheckOutcome::Failed(UpdateFailure {
                stage: UpdateStage::ManifestValidation,
                message,
            });
        }
    };
    if manifest.version <= current {
        UpdateCheckOutcome::UpToDate {
            current,
            latest: manifest.version,
        }
    } else {
        UpdateCheckOutcome::Available(manifest)
    }
}

pub fn apply_update<T, V, R>(
    enabled: bool,
    current: Version,
    paths: &UpdatePaths,
    transport: &mut T,
    verifier: &mut V,
    runner: &mut R,
) -> UpdateOutcome
where
    T: UpdateTransport,
    V: SignatureVerifier,
    R: InstallerRunner,
{
    let manifest = match check_for_update(enabled, current, transport) {
        UpdateCheckOutcome::Disabled => return UpdateOutcome::Disabled,
        UpdateCheckOutcome::UpToDate { current, latest } => {
            return UpdateOutcome::UpToDate { current, latest };
        }
        UpdateCheckOutcome::Available(manifest) => manifest,
        UpdateCheckOutcome::Failed(failure) => {
            return UpdateOutcome::Failed {
                version: None,
                failure,
            };
        }
    };
    let version = manifest.version;

    if let Err(message) = prepare_installer_path(&paths.installer) {
        return failed(version, UpdateStage::InstallerPreparation, message);
    }
    let receipt = match transport.download_installer(
        &manifest.installer_url,
        &paths.installer,
        MAX_INSTALLER_BYTES,
    ) {
        Ok(receipt) => receipt,
        Err(message) => {
            cleanup_staged(&paths.installer);
            return failed(version, UpdateStage::InstallerDownload, message);
        }
    };
    if receipt.size != manifest.size {
        cleanup_staged(&paths.installer);
        return failed(
            version,
            UpdateStage::InstallerSize,
            format!(
                "expected {} bytes, downloaded {} bytes",
                manifest.size, receipt.size
            ),
        );
    }
    if receipt.sha256 != manifest.sha256 {
        cleanup_staged(&paths.installer);
        return failed(
            version,
            UpdateStage::InstallerHash,
            format!(
                "expected {}, downloaded {}",
                encode_hex(&manifest.sha256),
                encode_hex(&receipt.sha256)
            ),
        );
    }
    if let Err(message) = verifier.verify(&paths.installer) {
        cleanup_staged(&paths.installer);
        return failed(version, UpdateStage::SignatureVerification, message);
    }

    let terminal = match runner.run(&paths.installer, &paths.log) {
        Ok(terminal) => terminal,
        Err(message) => {
            cleanup_staged(&paths.installer);
            return failed(version, UpdateStage::InstallerLaunch, message);
        }
    };
    match terminal {
        InstallerTerminal::Installed => {
            cleanup_staged(&paths.installer);
            UpdateOutcome::Installed { version }
        }
        InstallerTerminal::RestartRequired => {
            cleanup_staged(&paths.installer);
            UpdateOutcome::RestartRequired { version }
        }
        InstallerTerminal::TimedOutStillRunning => UpdateOutcome::TimedOutStillRunning { version },
        InstallerTerminal::Failed(exit_code) => {
            cleanup_staged(&paths.installer);
            failed(
                version,
                UpdateStage::InstallerExit,
                format!("silent installer exited with code {exit_code}"),
            )
        }
    }
}

fn failed(version: Version, stage: UpdateStage, message: String) -> UpdateOutcome {
    UpdateOutcome::Failed {
        version: Some(version),
        failure: UpdateFailure { stage, message },
    }
}

fn prepare_installer_path(path: &Path) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "staged installer path has no parent directory".to_owned())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "could not remove the previous staged installer: {error}"
        )),
    }
}

fn cleanup_staged(path: &Path) {
    let _ = fs::remove_file(path);
}

pub fn check_real(enabled: bool) -> UpdateCheckOutcome {
    check_for_update(enabled, current_version(), &mut WinHttpTransport)
}

pub fn apply_real(enabled: bool, paths: &UpdatePaths) -> UpdateOutcome {
    apply_update(
        enabled,
        current_version(),
        paths,
        &mut WinHttpTransport,
        &mut AuthenticodeVerifier,
        &mut SilentInstaller,
    )
}

#[derive(Debug, Default)]
pub struct WinHttpTransport;

impl UpdateTransport for WinHttpTransport {
    fn fetch_manifest(&mut self, url: &str, limit: u64) -> Result<Vec<u8>, String> {
        if url != MANIFEST_URL {
            return Err("manifest request did not use the canonical release URL".to_owned());
        }
        let mut bytes = Vec::new();
        stream_http_get(url, limit, HTTP_TOTAL_TIMEOUT, |chunk| {
            bytes.extend_from_slice(chunk);
            Ok(())
        })?;
        Ok(bytes)
    }

    fn download_installer(
        &mut self,
        url: &str,
        path: &Path,
        limit: u64,
    ) -> Result<DownloadReceipt, String> {
        let parsed_version = canonical_installer_version(url)?;
        let _ = parsed_version;
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)
            .map_err(|error| format!("could not create staged installer: {error}"))?;
        let mut hasher = Sha256::new()?;
        let size = match stream_http_get(url, limit, INSTALLER_TOTAL_TIMEOUT, |chunk| {
            file.write_all(chunk)
                .map_err(|error| format!("could not write staged installer: {error}"))?;
            hasher.update(chunk)
        }) {
            Ok(size) => size,
            Err(error) => {
                drop(file);
                cleanup_staged(path);
                return Err(error);
            }
        };
        file.sync_all()
            .map_err(|error| format!("could not flush staged installer: {error}"))?;
        Ok(DownloadReceipt {
            size,
            sha256: hasher.finish()?,
        })
    }
}

fn canonical_installer_version(url: &str) -> Result<Version, String> {
    const PREFIX: &str = "https://github.com/tsuyoshi-otake/sakura-input/releases/download/v";
    const SUFFIX: &str = "/sakura_setup.exe";
    let value = url
        .strip_prefix(PREFIX)
        .and_then(|value| value.strip_suffix(SUFFIX))
        .ok_or_else(|| "installer URL is not a canonical Sakura Input release asset".to_owned())?;
    let version = Version::parse(value)?;
    if installer_url_for(version) != url {
        return Err("installer URL canonicalization mismatch".to_owned());
    }
    Ok(version)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HttpsUrl {
    host: String,
    path_and_query: String,
}

fn parse_allowed_https_url(url: &str) -> Result<HttpsUrl, String> {
    if !url.is_ascii()
        || url
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b'\\')
    {
        return Err("update URL must be printable ASCII without backslashes".to_owned());
    }
    let remainder = url
        .strip_prefix("https://")
        .ok_or_else(|| "update URL must use HTTPS".to_owned())?;
    let (authority, path) = remainder
        .split_once('/')
        .ok_or_else(|| "update URL must contain an absolute path".to_owned())?;
    if authority.is_empty()
        || authority.contains('@')
        || authority.contains(':')
        || path.contains('#')
    {
        return Err("update URL contains credentials, a port, or a fragment".to_owned());
    }
    let host = authority.to_ascii_lowercase();
    if !matches!(
        host.as_str(),
        "github.com" | "release-assets.githubusercontent.com" | "objects.githubusercontent.com"
    ) {
        return Err(format!("update URL host {host:?} is not allowlisted"));
    }
    Ok(HttpsUrl {
        host,
        path_and_query: format!("/{path}"),
    })
}

fn resolve_redirect(current: &HttpsUrl, location: &str) -> Result<String, String> {
    if location.starts_with("https://") {
        parse_allowed_https_url(location)?;
        Ok(location.to_owned())
    } else if location.starts_with('/') && !location.starts_with("//") {
        let resolved = format!("https://{}{}", current.host, location);
        parse_allowed_https_url(&resolved)?;
        Ok(resolved)
    } else {
        Err("redirect Location must be an absolute HTTPS URL or root-relative path".to_owned())
    }
}

fn stream_http_get(
    initial_url: &str,
    limit: u64,
    total_timeout: Duration,
    mut consume: impl FnMut(&[u8]) -> Result<(), String>,
) -> Result<u64, String> {
    if limit == 0 || limit > MAX_INSTALLER_BYTES {
        return Err("HTTP response limit is invalid".to_owned());
    }
    let deadline = Instant::now() + total_timeout;
    let session = InternetHandle::open_session()?;
    let mut url = initial_url.to_owned();

    for redirect_count in 0..=MAX_REDIRECTS {
        if Instant::now() >= deadline {
            return Err(format!(
                "HTTP operation exceeded its {}-second total deadline",
                total_timeout.as_secs()
            ));
        }
        let parsed = parse_allowed_https_url(&url)?;
        let host = wide(&parsed.host);
        let connect = InternetHandle::connect(session.0, PCWSTR(host.as_ptr()))?;
        let path = wide(&parsed.path_and_query);
        let request = InternetHandle::request(connect.0, PCWSTR(path.as_ptr()))?;
        request.configure()?;
        request.send()?;
        let status = request.status_code()?;
        if matches!(status, 301 | 302 | 303 | 307 | 308) {
            if redirect_count == MAX_REDIRECTS {
                return Err(format!(
                    "HTTP redirect limit of {MAX_REDIRECTS} was exceeded"
                ));
            }
            let location = request.text_header(WINHTTP_QUERY_LOCATION)?;
            url = resolve_redirect(&parsed, &location)?;
            continue;
        }
        if status == 429 {
            let retry_after = request
                .text_header(WINHTTP_QUERY_RETRY_AFTER)
                .unwrap_or_else(|_| "not supplied".to_owned());
            return Err(format!(
                "GitHub rate limited the request (Retry-After: {retry_after}); no immediate retry was attempted"
            ));
        }
        if status != 200 {
            return Err(format!("HTTP request returned status {status}"));
        }

        let mut total = 0u64;
        let mut buffer = vec![0u8; HTTP_BUFFER_BYTES];
        loop {
            if Instant::now() >= deadline {
                return Err(format!(
                    "HTTP operation exceeded its {}-second total deadline",
                    total_timeout.as_secs()
                ));
            }
            let read = request.read(&mut buffer)?;
            if read == 0 {
                return Ok(total);
            }
            total = total
                .checked_add(read as u64)
                .ok_or_else(|| "HTTP response byte count overflowed".to_owned())?;
            if total > limit {
                return Err(format!("HTTP response exceeds the {limit}-byte limit"));
            }
            consume(&buffer[..read])?;
        }
    }
    Err("HTTP redirect state did not reach a terminal response".to_owned())
}

struct InternetHandle(*mut c_void);

impl InternetHandle {
    fn open_session() -> Result<Self, String> {
        let agent = windows::core::w!("Sakura Input Updater/1");
        // SAFETY: all strings are static and the returned handle is owned by
        // this RAII wrapper.
        let handle = unsafe {
            WinHttpOpen(
                agent,
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            )
        };
        let session = Self::from_raw(handle, "WinHttpOpen")?;
        // SAFETY: this is a live session handle and all option buffers are
        // fixed-size native-endian scalars consumed during the calls.
        unsafe {
            WinHttpSetTimeouts(session.0, 10_000, 15_000, 30_000, 30_000)
                .map_err(|error| format!("WinHttpSetTimeouts failed: {error}"))?;
            set_u32_option(
                session.0,
                WINHTTP_OPTION_REDIRECT_POLICY,
                WINHTTP_OPTION_REDIRECT_POLICY_NEVER,
            )?;
            set_u32_option(session.0, WINHTTP_OPTION_CONNECT_RETRIES, 1)?;
            set_u32_option(
                session.0,
                WINHTTP_OPTION_MAX_RESPONSE_HEADER_SIZE,
                MAX_MANIFEST_BYTES as u32,
            )?;
        }
        Ok(session)
    }

    fn connect(session: *mut c_void, host: PCWSTR) -> Result<Self, String> {
        // SAFETY: `session` is live and `host` points to a NUL-terminated
        // buffer that outlives the synchronous call.
        let handle = unsafe { WinHttpConnect(session, host, INTERNET_DEFAULT_HTTPS_PORT, 0) };
        Self::from_raw(handle, "WinHttpConnect")
    }

    fn request(connection: *mut c_void, path: PCWSTR) -> Result<Self, String> {
        let flags: WINHTTP_OPEN_REQUEST_FLAGS = WINHTTP_FLAG_SECURE | WINHTTP_FLAG_REFRESH;
        // SAFETY: `connection` is live, `path` is NUL-terminated, and the
        // remaining optional string pointers are intentionally null.
        let handle = unsafe {
            WinHttpOpenRequest(
                connection,
                windows::core::w!("GET"),
                path,
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                flags,
            )
        };
        Self::from_raw(handle, "WinHttpOpenRequest")
    }

    fn from_raw(handle: *mut c_void, operation: &str) -> Result<Self, String> {
        if handle.is_null() {
            Err(format!(
                "{operation} failed: {}",
                windows::core::Error::from_thread()
            ))
        } else {
            Ok(Self(handle))
        }
    }

    fn configure(&self) -> Result<(), String> {
        // SAFETY: the request handle is live and the option is a native u32.
        unsafe { set_u32_option(self.0, WINHTTP_OPTION_REJECT_USERPWD_IN_URL, 1) }
    }

    fn send(&self) -> Result<(), String> {
        let headers: Vec<u16> = "Accept: application/octet-stream\r\nAccept-Encoding: identity\r\nCache-Control: no-cache\r\n"
            .encode_utf16()
            .collect();
        // SAFETY: the request is live. WinHTTP consumes the bounded header
        // slice synchronously and no request body is supplied.
        unsafe {
            WinHttpSendRequest(self.0, Some(&headers), None, 0, 0, 0)
                .map_err(|error| format!("WinHttpSendRequest failed: {error}"))?;
            WinHttpReceiveResponse(self.0, std::ptr::null_mut())
                .map_err(|error| format!("WinHttpReceiveResponse failed: {error}"))
        }
    }

    fn status_code(&self) -> Result<u32, String> {
        let mut status = 0u32;
        let mut bytes = std::mem::size_of::<u32>() as u32;
        let mut index = 0u32;
        // SAFETY: `status` is a correctly sized writable u32 buffer and the
        // numeric query flag tells WinHTTP to write that representation.
        unsafe {
            WinHttpQueryHeaders(
                self.0,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                PCWSTR::null(),
                Some((&mut status as *mut u32).cast()),
                &mut bytes,
                &mut index,
            )
            .map_err(|error| format!("could not read HTTP status: {error}"))?;
        }
        Ok(status)
    }

    fn text_header(&self, query: u32) -> Result<String, String> {
        let mut buffer = vec![0u16; (MAX_MANIFEST_BYTES as usize / 2) + 1];
        let mut bytes = (buffer.len() * std::mem::size_of::<u16>()) as u32;
        let mut index = 0u32;
        // SAFETY: `buffer` is writable for `bytes`; the query is one of the
        // standard string-valued response headers used above.
        unsafe {
            WinHttpQueryHeaders(
                self.0,
                query,
                PCWSTR::null(),
                Some(buffer.as_mut_ptr().cast()),
                &mut bytes,
                &mut index,
            )
            .map_err(|error| format!("could not read HTTP response header: {error}"))?;
        }
        let units = (bytes as usize / 2).min(buffer.len());
        let end = buffer[..units]
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(units);
        let value = String::from_utf16(&buffer[..end])
            .map_err(|_| "HTTP response header is not valid UTF-16".to_owned())?;
        if value.is_empty() {
            Err("HTTP response header is empty".to_owned())
        } else {
            Ok(value)
        }
    }

    fn read(&self, buffer: &mut [u8]) -> Result<usize, String> {
        let mut read = 0u32;
        // SAFETY: `buffer` is writable for the supplied length and the request
        // remains live for the duration of the synchronous read.
        unsafe {
            WinHttpReadData(
                self.0,
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
                &mut read,
            )
            .map_err(|error| format!("WinHttpReadData failed: {error}"))?;
        }
        Ok(read as usize)
    }
}

impl Drop for InternetHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: this wrapper owns the non-null WinHTTP handle exactly
            // once; close failures cannot be recovered during Drop.
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

unsafe fn set_u32_option(handle: *mut c_void, option: u32, value: u32) -> Result<(), String> {
    // SAFETY: caller guarantees a live WinHTTP handle. WinHTTP copies the four
    // option bytes during this call.
    unsafe {
        WinHttpSetOption(
            Some(handle.cast_const()),
            option,
            Some(&value.to_ne_bytes()),
        )
        .map_err(|error| format!("WinHttpSetOption({option}) failed: {error}"))
    }
}

struct Sha256 {
    algorithm: BCRYPT_ALG_HANDLE,
    hash: BCRYPT_HASH_HANDLE,
    object: Vec<u8>,
    output_len: usize,
    finished: bool,
}

impl Sha256 {
    fn new() -> Result<Self, String> {
        let mut algorithm = BCRYPT_ALG_HANDLE::default();
        // SAFETY: the algorithm identifier is static and the output handle is
        // initialized before inspection.
        let status = unsafe {
            BCryptOpenAlgorithmProvider(
                &mut algorithm,
                BCRYPT_SHA256_ALGORITHM,
                PCWSTR::null(),
                BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS::default(),
            )
        };
        nt_success(status.0, "BCryptOpenAlgorithmProvider")?;

        let lengths = (|| -> Result<(usize, usize), String> {
            let object = bcrypt_u32_property(algorithm, BCRYPT_OBJECT_LENGTH)? as usize;
            let output = bcrypt_u32_property(algorithm, BCRYPT_HASH_LENGTH)? as usize;
            if object == 0 || object > 1024 * 1024 || output != 32 {
                return Err(format!(
                    "SHA-256 provider returned invalid lengths object={object}, digest={output}"
                ));
            }
            Ok((object, output))
        })();
        let (object_len, output_len) = match lengths {
            Ok(lengths) => lengths,
            Err(error) => {
                // SAFETY: the provider handle was opened above and has not
                // been transferred.
                unsafe {
                    let _ = BCryptCloseAlgorithmProvider(algorithm, 0);
                }
                return Err(error);
            }
        };
        let mut object = vec![0u8; object_len];
        let mut hash = BCRYPT_HASH_HANDLE::default();
        // SAFETY: object storage remains owned by `Self` until the hash is
        // destroyed, and no secret key is used for SHA-256.
        let status = unsafe { BCryptCreateHash(algorithm, &mut hash, Some(&mut object), None, 0) };
        if let Err(error) = nt_success(status.0, "BCryptCreateHash") {
            // SAFETY: the provider handle remains owned locally.
            unsafe {
                let _ = BCryptCloseAlgorithmProvider(algorithm, 0);
            }
            return Err(error);
        }
        Ok(Self {
            algorithm,
            hash,
            object,
            output_len,
            finished: false,
        })
    }

    fn update(&mut self, bytes: &[u8]) -> Result<(), String> {
        if self.finished {
            return Err("SHA-256 state is already finalized".to_owned());
        }
        // SAFETY: the hash is live and the input slice is borrowed only for
        // the synchronous operation.
        let status = unsafe { BCryptHashData(self.hash, bytes, 0) };
        nt_success(status.0, "BCryptHashData")
    }

    fn finish(mut self) -> Result<[u8; 32], String> {
        let mut output = [0u8; 32];
        if self.output_len != output.len() {
            return Err("SHA-256 digest length changed unexpectedly".to_owned());
        }
        // SAFETY: the hash is live and `output` has the provider-reported
        // digest size.
        let status = unsafe { BCryptFinishHash(self.hash, &mut output, 0) };
        nt_success(status.0, "BCryptFinishHash")?;
        self.finished = true;
        Ok(output)
    }
}

impl Drop for Sha256 {
    fn drop(&mut self) {
        // Keep the provider object buffer observably live through destruction.
        let _ = self.object.len();
        // SAFETY: each handle is owned by this value and destroyed once, in
        // dependency order. Drop cannot meaningfully report cleanup failures.
        unsafe {
            if !self.hash.0.is_null() {
                let _ = BCryptDestroyHash(self.hash);
            }
            if !self.algorithm.0.is_null() {
                let _ = BCryptCloseAlgorithmProvider(self.algorithm, 0);
            }
        }
    }
}

fn bcrypt_u32_property(algorithm: BCRYPT_ALG_HANDLE, property: PCWSTR) -> Result<u32, String> {
    let mut bytes = [0u8; 4];
    let mut written = 0u32;
    // SAFETY: the provider is live and the four-byte result buffer matches the
    // documented u32 properties queried here.
    let status = unsafe {
        BCryptGetProperty(
            BCRYPT_HANDLE(algorithm.0),
            property,
            Some(&mut bytes),
            &mut written,
            0,
        )
    };
    nt_success(status.0, "BCryptGetProperty")?;
    if written != bytes.len() as u32 {
        return Err(format!(
            "BCryptGetProperty returned {written} bytes instead of {}",
            bytes.len()
        ));
    }
    Ok(u32::from_ne_bytes(bytes))
}

fn nt_success(status: i32, operation: &str) -> Result<(), String> {
    if status >= 0 {
        Ok(())
    } else {
        Err(format!(
            "{operation} failed with NTSTATUS 0x{:08x}",
            status as u32
        ))
    }
}

#[derive(Debug, Default)]
pub struct AuthenticodeVerifier;

impl SignatureVerifier for AuthenticodeVerifier {
    fn verify(&mut self, path: &Path) -> Result<(), String> {
        let path_wide = wide_os(path.as_os_str());
        let mut file_info = WINTRUST_FILE_INFO {
            cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as u32,
            pcwszFilePath: PCWSTR(path_wide.as_ptr()),
            ..Default::default()
        };
        let mut trust_data = WINTRUST_DATA {
            cbStruct: std::mem::size_of::<WINTRUST_DATA>() as u32,
            dwUIChoice: WTD_UI_NONE,
            fdwRevocationChecks: WTD_REVOKE_WHOLECHAIN,
            dwUnionChoice: WTD_CHOICE_FILE,
            Anonymous: WINTRUST_DATA_0 {
                pFile: &mut file_info,
            },
            dwStateAction: WTD_STATEACTION_VERIFY,
            dwProvFlags: WTD_REVOCATION_CHECK_CHAIN_EXCLUDE_ROOT | WTD_DISABLE_MD2_MD4,
            dwUIContext: WTD_UICONTEXT_INSTALL,
            ..Default::default()
        };
        let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
        // SAFETY: all structures carry their exact ABI sizes, contain pointers
        // to live local data, and WinVerifyTrust consumes them synchronously.
        let verify_status = unsafe {
            WinVerifyTrust(
                HWND::default(),
                &mut action,
                (&mut trust_data as *mut WINTRUST_DATA).cast(),
            )
        };
        trust_data.dwStateAction = WTD_STATEACTION_CLOSE;
        // SAFETY: every VERIFY state is paired with CLOSE using the same state
        // data, including verification failures.
        let close_status = unsafe {
            WinVerifyTrust(
                HWND::default(),
                &mut action,
                (&mut trust_data as *mut WINTRUST_DATA).cast(),
            )
        };
        if verify_status != 0 {
            return Err(format!(
                "WinVerifyTrust rejected the installer with status 0x{:08x}",
                verify_status as u32
            ));
        }
        if close_status != 0 {
            return Err(format!(
                "WinVerifyTrust state cleanup failed with status 0x{:08x}",
                close_status as u32
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct SilentInstaller;

impl InstallerRunner for SilentInstaller {
    fn run(&mut self, installer: &Path, log: &Path) -> Result<InstallerTerminal, String> {
        if let Some(parent) = log.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("could not create installer log directory: {error}"))?;
        }
        let installer_wide = wide_os(installer.as_os_str());
        let directory_wide = wide_os(
            installer
                .parent()
                .ok_or_else(|| "installer path has no parent directory".to_owned())?
                .as_os_str(),
        );
        let parameters = format!(
            "/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /RESTARTEXITCODE=3010 /LOG=\"{}\"",
            log.display()
        );
        let parameters_wide = wide(&parameters);
        let mut execute = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
            // Start Setup normally and let its `PrivilegesRequired=admin`
            // bootstrap request elevation. Inno can retain the pre-UAC token
            // only when it performs that transition itself; launching Setup
            // with `runas` here would make its `runasoriginaluser` profile
            // step run elevated and create per-user state with the wrong ACL.
            lpVerb: windows::core::w!("open"),
            lpFile: PCWSTR(installer_wide.as_ptr()),
            lpParameters: PCWSTR(parameters_wide.as_ptr()),
            lpDirectory: PCWSTR(directory_wide.as_ptr()),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        // SAFETY: every string buffer is NUL-terminated and remains alive
        // through the call. The returned process handle is transferred below.
        unsafe {
            ShellExecuteExW(&mut execute)
                .map_err(|error| format!("ShellExecuteExW failed: {error}"))?;
        }
        if execute.hProcess.is_invalid() {
            return Err("ShellExecuteExW returned no installer process handle".to_owned());
        }
        let process = OwnedHandle(execute.hProcess);
        // SAFETY: this wrapper owns a live process handle. The timeout is a
        // finite constant and timeout deliberately leaves the process running.
        let wait = unsafe { WaitForSingleObject(process.0, INSTALL_TIMEOUT.as_millis() as u32) };
        if wait == WAIT_TIMEOUT {
            return Ok(InstallerTerminal::TimedOutStillRunning);
        }
        if wait != WAIT_OBJECT_0 {
            return Err(format!(
                "WaitForSingleObject returned unexpected status 0x{:08x}",
                wait.0
            ));
        }
        let mut exit_code = 0u32;
        // SAFETY: the process signaled and the output pointer is valid.
        unsafe {
            GetExitCodeProcess(process.0, &mut exit_code)
                .map_err(|error| format!("GetExitCodeProcess failed: {error}"))?;
        }
        Ok(match exit_code {
            0 => InstallerTerminal::Installed,
            3010 => InstallerTerminal::RestartRequired,
            code => InstallerTerminal::Failed(code),
        })
    }
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: this wrapper owns the process handle exactly once.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(core::iter::once(0)).collect()
}

fn wide_os(value: &std::ffi::OsStr) -> Vec<u16> {
    value.encode_wide().chain(core::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_PATH: AtomicU64 = AtomicU64::new(1);
    const DIGEST: [u8; 32] = [0x5a; 32];

    fn manifest(version: Version) -> ReleaseManifest {
        ReleaseManifest {
            version,
            installer_url: installer_url_for(version),
            sha256: DIGEST,
            size: 123,
        }
    }

    fn temp_paths(name: &str) -> UpdatePaths {
        let id = NEXT_PATH.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("sakura-updater-{}-{name}-{id}", std::process::id()));
        UpdatePaths {
            installer: root.join("sakura_setup.pending.exe"),
            log: root.join("install.log"),
        }
    }

    #[derive(Debug)]
    struct FakeTransport {
        manifest: Result<Vec<u8>, String>,
        receipt: Result<DownloadReceipt, String>,
        manifest_calls: usize,
        download_calls: usize,
    }

    impl FakeTransport {
        fn success(release: &ReleaseManifest) -> Self {
            Self {
                manifest: Ok(release.canonical_text().into_bytes()),
                receipt: Ok(DownloadReceipt {
                    size: release.size,
                    sha256: release.sha256,
                }),
                manifest_calls: 0,
                download_calls: 0,
            }
        }
    }

    impl UpdateTransport for FakeTransport {
        fn fetch_manifest(&mut self, url: &str, limit: u64) -> Result<Vec<u8>, String> {
            assert_eq!(url, MANIFEST_URL);
            assert_eq!(limit, MAX_MANIFEST_BYTES);
            self.manifest_calls += 1;
            self.manifest.clone()
        }

        fn download_installer(
            &mut self,
            _url: &str,
            path: &Path,
            limit: u64,
        ) -> Result<DownloadReceipt, String> {
            assert_eq!(limit, MAX_INSTALLER_BYTES);
            self.download_calls += 1;
            fs::write(path, b"fake installer").map_err(|error| error.to_string())?;
            self.receipt.clone()
        }
    }

    #[derive(Debug)]
    struct FakeVerifier {
        result: Result<(), String>,
        calls: usize,
    }

    impl SignatureVerifier for FakeVerifier {
        fn verify(&mut self, _path: &Path) -> Result<(), String> {
            self.calls += 1;
            self.result.clone()
        }
    }

    #[derive(Debug)]
    struct FakeRunner {
        result: Result<InstallerTerminal, String>,
        calls: usize,
    }

    impl InstallerRunner for FakeRunner {
        fn run(&mut self, _installer: &Path, _log: &Path) -> Result<InstallerTerminal, String> {
            self.calls += 1;
            self.result.clone()
        }
    }

    fn run_fake(
        release: &ReleaseManifest,
        receipt: DownloadReceipt,
        signature: Result<(), String>,
        terminal: InstallerTerminal,
    ) -> (
        UpdateOutcome,
        FakeTransport,
        FakeVerifier,
        FakeRunner,
        UpdatePaths,
    ) {
        let paths = temp_paths("pipeline");
        let mut transport = FakeTransport::success(release);
        transport.receipt = Ok(receipt);
        let mut verifier = FakeVerifier {
            result: signature,
            calls: 0,
        };
        let mut runner = FakeRunner {
            result: Ok(terminal),
            calls: 0,
        };
        let outcome = apply_update(
            true,
            Version {
                major: 1,
                minor: 0,
                patch: 0,
            },
            &paths,
            &mut transport,
            &mut verifier,
            &mut runner,
        );
        (outcome, transport, verifier, runner, paths)
    }

    #[test]
    fn versions_are_strict_and_ordered() {
        assert_eq!(
            Version::parse("1.2.3").map(|value| value.to_string()),
            Ok("1.2.3".to_owned())
        );
        assert!(Version::parse("01.2.3").is_err());
        assert!(Version::parse("1.2").is_err());
        assert!(Version::parse("1.2.3-beta").is_err());
        assert!(Version::parse("1.2.3.4").is_err());
        assert!(Version::parse("2.0.0").unwrap() > Version::parse("1.99.99").unwrap());
    }

    #[test]
    fn canonical_manifest_roundtrips_and_rejects_ambiguity() {
        let release = manifest(Version {
            major: 1,
            minor: 2,
            patch: 3,
        });
        assert_eq!(
            ReleaseManifest::parse(release.canonical_text().as_bytes()),
            Ok(release.clone())
        );
        let mut wrong_url = release.canonical_text();
        wrong_url = wrong_url.replace("sakura_setup.exe", "other.exe");
        assert!(ReleaseManifest::parse(wrong_url.as_bytes()).is_err());
        let duplicate = format!("{}schema=1\n", release.canonical_text());
        assert!(ReleaseManifest::parse(duplicate.as_bytes()).is_err());
        let uppercase = release.canonical_text().replace("5a5a", "5A5A");
        assert!(ReleaseManifest::parse(uppercase.as_bytes()).is_err());
        let unknown = release.canonical_text().replace("size=123", "bytes=123");
        assert!(ReleaseManifest::parse(unknown.as_bytes()).is_err());
    }

    #[test]
    fn update_preference_defaults_off_and_roundtrips_strictly() {
        let paths = temp_paths("preference");
        let path = paths.installer.with_file_name("settings.txt");
        assert_eq!(
            UpdatePreferences::load(&path).unwrap(),
            UpdatePreferences::default()
        );
        UpdatePreferences { enabled: true }.save(&path).unwrap();
        assert_eq!(
            UpdatePreferences::load(&path).unwrap(),
            UpdatePreferences { enabled: true }
        );
        fs::write(&path, b"schema=1\nenabled=true\nunknown=x\n").unwrap();
        assert_eq!(
            UpdatePreferences::load(&path).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn disabled_check_makes_no_network_request() {
        let release = manifest(Version {
            major: 2,
            minor: 0,
            patch: 0,
        });
        let mut transport = FakeTransport::success(&release);
        assert_eq!(
            check_for_update(false, Version::default(), &mut transport),
            UpdateCheckOutcome::Disabled
        );
        assert_eq!(transport.manifest_calls, 0);
        assert_eq!(transport.download_calls, 0);
    }

    #[test]
    fn hash_and_signature_failures_never_reach_installer() {
        let release = manifest(Version {
            major: 2,
            minor: 0,
            patch: 0,
        });
        let (hash_outcome, _, hash_verifier, hash_runner, hash_paths) = run_fake(
            &release,
            DownloadReceipt {
                size: release.size,
                sha256: [0; 32],
            },
            Ok(()),
            InstallerTerminal::Installed,
        );
        assert!(matches!(
            hash_outcome,
            UpdateOutcome::Failed {
                failure: UpdateFailure {
                    stage: UpdateStage::InstallerHash,
                    ..
                },
                ..
            }
        ));
        assert_eq!(hash_verifier.calls, 0);
        assert_eq!(hash_runner.calls, 0);
        assert!(!hash_paths.installer.exists());

        let (signature_outcome, _, verifier, runner, signature_paths) = run_fake(
            &release,
            DownloadReceipt {
                size: release.size,
                sha256: release.sha256,
            },
            Err("untrusted".to_owned()),
            InstallerTerminal::Installed,
        );
        assert!(matches!(
            signature_outcome,
            UpdateOutcome::Failed {
                failure: UpdateFailure {
                    stage: UpdateStage::SignatureVerification,
                    ..
                },
                ..
            }
        ));
        assert_eq!(verifier.calls, 1);
        assert_eq!(runner.calls, 0);
        assert!(!signature_paths.installer.exists());
        let _ = fs::remove_dir_all(hash_paths.installer.parent().unwrap());
        let _ = fs::remove_dir_all(signature_paths.installer.parent().unwrap());
    }

    #[test]
    fn installer_success_restart_timeout_and_failure_are_distinct_terminals() {
        let release = manifest(Version {
            major: 2,
            minor: 0,
            patch: 0,
        });
        let terminals = [
            (InstallerTerminal::Installed, "installed"),
            (InstallerTerminal::RestartRequired, "restart"),
            (InstallerTerminal::TimedOutStillRunning, "timeout"),
            (InstallerTerminal::Failed(17), "failure"),
        ];
        for (terminal, expected) in terminals {
            let (outcome, _, verifier, runner, paths) = run_fake(
                &release,
                DownloadReceipt {
                    size: release.size,
                    sha256: release.sha256,
                },
                Ok(()),
                terminal,
            );
            assert_eq!(verifier.calls, 1);
            assert_eq!(runner.calls, 1);
            match expected {
                "installed" => assert!(matches!(outcome, UpdateOutcome::Installed { .. })),
                "restart" => assert!(matches!(outcome, UpdateOutcome::RestartRequired { .. })),
                "timeout" => {
                    assert!(matches!(
                        outcome,
                        UpdateOutcome::TimedOutStillRunning { .. }
                    ));
                    assert!(paths.installer.exists());
                }
                "failure" => assert!(matches!(
                    outcome,
                    UpdateOutcome::Failed {
                        failure: UpdateFailure {
                            stage: UpdateStage::InstallerExit,
                            ..
                        },
                        ..
                    }
                )),
                _ => unreachable!(),
            }
            let _ = fs::remove_dir_all(paths.installer.parent().unwrap());
        }
    }

    #[test]
    fn url_parser_bounds_redirects_to_https_github_hosts() {
        assert!(parse_allowed_https_url(MANIFEST_URL).is_ok());
        assert!(parse_allowed_https_url(
            "https://release-assets.githubusercontent.com/github-production-release-asset/x?token=y"
        )
        .is_ok());
        assert!(parse_allowed_https_url("http://github.com/x").is_err());
        assert!(parse_allowed_https_url("https://github.com.evil.example/x").is_err());
        assert!(parse_allowed_https_url("https://user@github.com/x").is_err());
        assert!(resolve_redirect(
            &parse_allowed_https_url(MANIFEST_URL).unwrap(),
            "//evil.example/x"
        )
        .is_err());
    }

    #[test]
    fn authenticode_rejects_an_unsigned_file() {
        let paths = temp_paths("unsigned-authenticode");
        fs::create_dir_all(paths.installer.parent().unwrap()).unwrap();
        fs::write(&paths.installer, b"not a PE image and not signed").unwrap();

        let mut verifier = AuthenticodeVerifier;
        let result = verifier.verify(&paths.installer);

        assert!(result.is_err(), "an unsigned file must fail closed");
        let _ = fs::remove_dir_all(paths.installer.parent().unwrap());
    }

    #[test]
    fn cng_sha256_matches_the_published_test_vector() {
        let mut hash = Sha256::new().unwrap();
        hash.update(b"a").unwrap();
        hash.update(b"bc").unwrap();
        assert_eq!(
            encode_hex(&hash.finish().unwrap()),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
