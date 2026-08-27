//! Small cross-bitness preferences needed inside the in-process TSF frontend.
//!
//! The canonical AI key is intentionally a DWORD in HKCU's 64-bit view so a
//! 32-bit and a 64-bit host observe the same value without parsing configuration
//! files on the keystroke path.

use windows::Win32::Foundation::ERROR_NOT_FOUND;
use windows::Win32::Security::Credentials::{
    CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE,
    CRED_TYPE_GENERIC,
};
use windows::Win32::System::Registry::HKEY_CURRENT_USER;
use windows_core::{Result, HRESULT, PCWSTR, PWSTR};

use crate::registry::{RegKey, RegistryView};
use crate::wide::to_wide_nul;

const KEY: &str = r"SOFTWARE\SakuraInput\Preferences";
const AI_TEXT_KEY: &str = "AiTextKey";
const AI_PROVIDER: &str = "AiProvider";
const AI_ENDPOINT: &str = "AiEndpoint";
const AI_AUTH: &str = "AiAuth";
const AI_STYLE: &str = "AiStyle";
const AI_EFFORT: &str = "AiEffort";
const AI_SERVICE_TIER: &str = "AiServiceTier";
const API_KEY_TARGET: &str = "SakuraInput/AI/APIKey";
const MAX_API_KEY_BYTES: usize = 2_048;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum AiProvider {
    #[default]
    OpenAi = 0,
    AzureOpenAi = 1,
    AwsBedrock = 2,
    Cloudflare = 3,
    Custom = 4,
    ChatGptCodex = 5,
}

impl AiProvider {
    pub const ALL: [Self; 6] = [
        Self::OpenAi,
        Self::AzureOpenAi,
        Self::AwsBedrock,
        Self::Cloudflare,
        Self::Custom,
        Self::ChatGptCodex,
    ];

    pub const fn from_dword(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::OpenAi),
            1 => Some(Self::AzureOpenAi),
            2 => Some(Self::AwsBedrock),
            3 => Some(Self::Cloudflare),
            4 => Some(Self::Custom),
            5 => Some(Self::ChatGptCodex),
            _ => None,
        }
    }

    pub const fn default_endpoint(self) -> &'static str {
        match self {
            Self::OpenAi => "https://api.openai.com/v1",
            Self::AzureOpenAi => "https://YOUR-RESOURCE-NAME.openai.azure.com/openai/v1",
            Self::AwsBedrock => "https://bedrock-mantle.ap-northeast-1.api.aws/v1",
            Self::Cloudflare => {
                "https://api.cloudflare.com/client/v4/accounts/YOUR-ACCOUNT-ID/ai/v1"
            }
            Self::Custom => "",
            Self::ChatGptCodex => "",
        }
    }

    pub const fn default_auth(self) -> AiAuth {
        match self {
            Self::AzureOpenAi => AiAuth::ApiKey,
            Self::OpenAi | Self::AwsBedrock | Self::Cloudflare => AiAuth::Bearer,
            Self::Custom => AiAuth::Bearer,
            Self::ChatGptCodex => AiAuth::None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum AiAuth {
    #[default]
    Bearer = 0,
    ApiKey = 1,
    None = 2,
}

impl AiAuth {
    pub const ALL: [Self; 3] = [Self::Bearer, Self::ApiKey, Self::None];

    pub const fn from_dword(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::Bearer),
            1 => Some(Self::ApiKey),
            2 => Some(Self::None),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum AiStyle {
    Spoken = 0,
    #[default]
    Polite = 1,
    Business = 2,
    Government = 3,
    Technical = 4,
    Academic = 5,
    Contract = 6,
    Novel = 7,
    Social = 8,
    English = 9,
}

impl AiStyle {
    pub const ALL: [Self; 10] = [
        Self::Spoken,
        Self::Polite,
        Self::Business,
        Self::Government,
        Self::Technical,
        Self::Academic,
        Self::Contract,
        Self::Novel,
        Self::Social,
        Self::English,
    ];

    pub const fn from_dword(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::Spoken),
            1 => Some(Self::Polite),
            2 => Some(Self::Business),
            3 => Some(Self::Government),
            4 => Some(Self::Technical),
            5 => Some(Self::Academic),
            6 => Some(Self::Contract),
            7 => Some(Self::Novel),
            8 => Some(Self::Social),
            9 => Some(Self::English),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum AiEffort {
    ProviderDefault = 0,
    None = 1,
    #[default]
    Low = 2,
    Medium = 3,
    High = 4,
    XHigh = 5,
    Max = 6,
}

impl AiEffort {
    pub const ALL: [Self; 7] = [
        Self::ProviderDefault,
        Self::None,
        Self::Low,
        Self::Medium,
        Self::High,
        Self::XHigh,
        Self::Max,
    ];

    pub const fn from_dword(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::ProviderDefault),
            1 => Some(Self::None),
            2 => Some(Self::Low),
            3 => Some(Self::Medium),
            4 => Some(Self::High),
            5 => Some(Self::XHigh),
            6 => Some(Self::Max),
            _ => None,
        }
    }

    pub const fn api_value(self) -> Option<&'static str> {
        match self {
            Self::ProviderDefault => None,
            Self::None => Some("none"),
            Self::Low => Some("low"),
            Self::Medium => Some("medium"),
            Self::High => Some("high"),
            Self::XHigh => Some("xhigh"),
            Self::Max => Some("max"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum AiServiceTier {
    #[default]
    ProviderDefault = 0,
    Priority = 1,
}

impl AiServiceTier {
    pub const ALL: [Self; 2] = [Self::ProviderDefault, Self::Priority];

    pub const fn from_dword(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::ProviderDefault),
            1 => Some(Self::Priority),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiTextPreferences {
    pub provider: AiProvider,
    pub endpoint: String,
    pub auth: AiAuth,
    pub style: AiStyle,
    pub effort: AiEffort,
    pub service_tier: AiServiceTier,
}

impl Default for AiTextPreferences {
    fn default() -> Self {
        let provider = AiProvider::default();
        Self {
            provider,
            endpoint: provider.default_endpoint().to_owned(),
            auth: provider.default_auth(),
            style: AiStyle::default(),
            effort: AiEffort::default(),
            service_tier: AiServiceTier::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum AiTextKey {
    Disabled = 0,
    #[default]
    Henkan = 1,
    CapsLock = 2,
}

impl AiTextKey {
    pub const ALL: [Self; 3] = [Self::Henkan, Self::CapsLock, Self::Disabled];

    pub const fn from_dword(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::Disabled),
            1 => Some(Self::Henkan),
            2 => Some(Self::CapsLock),
            _ => None,
        }
    }
}

pub fn read_ai_text_key() -> AiTextKey {
    let value = RegKey::open_for_read(HKEY_CURRENT_USER, KEY, RegistryView::Bits64)
        .ok()
        .flatten()
        .and_then(|key| key.get_dword(AI_TEXT_KEY).ok().flatten());
    value.and_then(AiTextKey::from_dword).unwrap_or_default()
}

pub fn write_ai_text_key(value: AiTextKey) -> Result<()> {
    let key = RegKey::create(HKEY_CURRENT_USER, KEY, RegistryView::Bits64)?;
    key.set_dword(AI_TEXT_KEY, value as u32)
}

pub fn read_ai_text_preferences() -> AiTextPreferences {
    let defaults = AiTextPreferences::default();
    let Ok(Some(key)) = RegKey::open_for_read(HKEY_CURRENT_USER, KEY, RegistryView::Bits64) else {
        return defaults;
    };
    let provider = key
        .get_dword(AI_PROVIDER)
        .ok()
        .flatten()
        .and_then(AiProvider::from_dword)
        .unwrap_or(defaults.provider);
    let endpoint = key
        .get_string(Some(AI_ENDPOINT))
        .ok()
        .flatten()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| provider.default_endpoint().to_owned());
    let auth = key
        .get_dword(AI_AUTH)
        .ok()
        .flatten()
        .and_then(AiAuth::from_dword)
        .unwrap_or_else(|| provider.default_auth());
    let style = key
        .get_dword(AI_STYLE)
        .ok()
        .flatten()
        .and_then(AiStyle::from_dword)
        .unwrap_or(defaults.style);
    let effort = key
        .get_dword(AI_EFFORT)
        .ok()
        .flatten()
        .and_then(AiEffort::from_dword)
        .unwrap_or(defaults.effort);
    let service_tier = key
        .get_dword(AI_SERVICE_TIER)
        .ok()
        .flatten()
        .and_then(AiServiceTier::from_dword)
        .unwrap_or(defaults.service_tier);
    AiTextPreferences {
        provider,
        endpoint,
        auth,
        style,
        effort,
        service_tier,
    }
}

pub fn write_ai_text_preferences(value: &AiTextPreferences) -> Result<()> {
    let key = RegKey::create(HKEY_CURRENT_USER, KEY, RegistryView::Bits64)?;
    key.set_dword(AI_PROVIDER, value.provider as u32)?;
    key.set_string(Some(AI_ENDPOINT), value.endpoint.trim())?;
    key.set_dword(AI_AUTH, value.auth as u32)?;
    key.set_dword(AI_STYLE, value.style as u32)?;
    key.set_dword(AI_EFFORT, value.effort as u32)?;
    key.set_dword(AI_SERVICE_TIER, value.service_tier as u32)
}

pub fn api_key_is_saved() -> bool {
    read_api_key().ok().flatten().is_some()
}

pub fn read_api_key() -> Result<Option<String>> {
    let target = to_wide_nul(API_KEY_TARGET);
    let mut raw = core::ptr::null_mut::<CREDENTIALW>();
    // SAFETY: `target` is NUL-terminated and `raw` is a valid out pointer.
    let read = unsafe { CredReadW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None, &mut raw) };
    if let Err(error) = read {
        if error.code() == HRESULT::from_win32(ERROR_NOT_FOUND.0) {
            return Ok(None);
        }
        return Err(error);
    }
    if raw.is_null() {
        return Ok(None);
    }
    // SAFETY: CredReadW returned a live CREDENTIALW whose fields remain valid
    // until the paired CredFree below. Treat externally modified, oversized or
    // null blobs as corrupt rather than allocating from an untrusted size.
    let credential = unsafe { &*raw };
    let size = credential.CredentialBlobSize as usize;
    let mut owned =
        if size > MAX_API_KEY_BYTES || (size != 0 && credential.CredentialBlob.is_null()) {
            None
        } else if size == 0 {
            Some(Vec::new())
        } else {
            // SAFETY: the non-null credential blob is live and contains `size`
            // bytes according to the successful CredReadW result.
            Some(unsafe { core::slice::from_raw_parts(credential.CredentialBlob, size).to_vec() })
        };
    let decoded = match owned.take() {
        Some(bytes) => match String::from_utf8(bytes) {
            Ok(value) => Ok(value),
            Err(error) => {
                let mut bytes = error.into_bytes();
                bytes.fill(0);
                Err(invalid_credential_data())
            }
        },
        None => Err(invalid_credential_data()),
    };
    // SAFETY: `raw` is the allocation returned by CredReadW exactly once.
    unsafe { CredFree(raw.cast()) };
    decoded.map(Some)
}

fn invalid_credential_data() -> windows_core::Error {
    windows_core::Error::from_hresult(HRESULT::from_win32(
        windows::Win32::Foundation::ERROR_INVALID_DATA.0,
    ))
}

pub fn write_api_key(api_key: &str) -> Result<()> {
    let trimmed = api_key.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_API_KEY_BYTES {
        return Err(windows_core::Error::from_hresult(HRESULT::from_win32(
            windows::Win32::Foundation::ERROR_INVALID_DATA.0,
        )));
    }
    let mut target = to_wide_nul(API_KEY_TARGET);
    let mut user = to_wide_nul("Sakura Input AI");
    let mut blob = trimmed.as_bytes().to_vec();
    let credential = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: PWSTR(target.as_mut_ptr()),
        CredentialBlobSize: blob.len() as u32,
        CredentialBlob: blob.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        UserName: PWSTR(user.as_mut_ptr()),
        ..Default::default()
    };
    // SAFETY: every pointer in `credential` refers to live storage for the
    // synchronous call. CredWriteW copies the values before returning.
    let result = unsafe { CredWriteW(&credential, 0) };
    blob.fill(0);
    result
}

pub fn clear_api_key() -> Result<()> {
    let target = to_wide_nul(API_KEY_TARGET);
    // SAFETY: `target` is NUL-terminated and live for the synchronous call.
    match unsafe { CredDeleteW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None) } {
        Ok(()) => Ok(()),
        Err(error) if error.code() == HRESULT::from_win32(ERROR_NOT_FOUND.0) => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dword_mapping_is_total_for_declared_values_and_fail_closed_otherwise() {
        for value in AiTextKey::ALL {
            assert_eq!(AiTextKey::from_dword(value as u32), Some(value));
        }
        assert_eq!(AiTextKey::from_dword(3), None);
        assert_eq!(AiTextKey::from_dword(u32::MAX), None);

        for value in AiStyle::ALL {
            assert_eq!(AiStyle::from_dword(value as u32), Some(value));
        }
        assert_eq!(AiStyle::from_dword(10), None);
        assert_eq!(AiStyle::from_dword(u32::MAX), None);
    }
}
