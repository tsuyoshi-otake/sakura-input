//! Every identifier Sakura Input publishes to the operating system.
//!
//! These values are part of the installed footprint: once a build ships, a GUID
//! here can never change, because an installed machine locates the text service
//! by exactly these bytes. Adding is safe; editing is not.

use windows::Win32::UI::TextServices::{
    GUID_TFCAT_DISPLAYATTRIBUTEPROVIDER, GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT,
    GUID_TFCAT_TIPCAP_UIELEMENTENABLED, GUID_TFCAT_TIP_KEYBOARD,
};
use windows_core::GUID;

/// COM class id of the text service, registered under `HKCR\CLSID`.
pub const CLSID_SAKURA_TSF: GUID = GUID::from_u128(0xc18f44de_39e0_4b16_8d28_d5de35bb11bc);

/// The ja-JP language profile exposed by [`CLSID_SAKURA_TSF`].
pub const GUID_PROFILE_JA_JP: GUID = GUID::from_u128(0x8466b5f0_210f_408b_a3fe_8d18ecba711d);

/// Undetermined reading — the text the user is still typing (DESIGN 8.1).
pub const GUID_DISPLAY_ATTRIBUTE_RAW: GUID =
    GUID::from_u128(0xb314fb49_12b0_4843_a16a_1407f1e4e33f);

/// Converted but unfocused segment.
pub const GUID_DISPLAY_ATTRIBUTE_CONVERTED: GUID =
    GUID::from_u128(0x29695162_2052_4702_ab09_778061fc327c);

/// The segment the caret is currently on.
pub const GUID_DISPLAY_ATTRIBUTE_FOCUSED: GUID =
    GUID::from_u128(0x5af1f750_55da_41d0_8a06_355077218015);

/// `Kanji` / `Alt+\`` — toggle Japanese input on and off.
pub const GUID_PRESERVEDKEY_IME_TOGGLE: GUID =
    GUID::from_u128(0x3da5bc14_9ad5_4ffa_94f9_35fe27ad7285);

/// `Hiragana` / `Henkan` on a JIS keyboard — force Japanese input on.
pub const GUID_PRESERVEDKEY_IME_ON: GUID = GUID::from_u128(0x8157cd1c_a678_4383_b605_1ec8045d1ae9);

/// `Muhenkan` on a JIS keyboard — force direct (alphanumeric) input.
pub const GUID_PRESERVEDKEY_IME_OFF: GUID = GUID::from_u128(0x0f02845f_2c4d_4bb6_9c5b_97a2b6eae53b);

/// Japanese (Japan). The only language profile Sakura Input registers.
pub const LANGID_JA_JP: u16 = 0x0411;

/// Shown in the language bar and in Settings > Language > Keyboards.
pub const TEXT_SERVICE_DESCRIPTION: &str = "Sakura Input";

/// Icon resource index inside the DLL used as the profile icon.
pub const PROFILE_ICON_INDEX: u32 = 0;

/// TSF categories the text service claims.
///
/// `GUID_TFCAT_TIPCAP_SECUREMODE` is deliberately absent (DESIGN 9): claiming it
/// would let the IME run on the secure desktop and inside password fields, which
/// is a security surface we do not want and a capability we do not need.
pub const CATEGORIES: &[GUID] = &[
    GUID_TFCAT_TIP_KEYBOARD,
    GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT,
    GUID_TFCAT_TIPCAP_UIELEMENTENABLED,
    GUID_TFCAT_DISPLAYATTRIBUTEPROVIDER,
];

/// Formats a GUID the way the registry spells one: `{XXXXXXXX-XXXX-...}`.
pub fn format_guid(guid: &GUID) -> String {
    let d = &guid.data4;
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        guid.data1, guid.data2, guid.data3, d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_guid_matches_registry_spelling() {
        assert_eq!(
            format_guid(&CLSID_SAKURA_TSF),
            "{C18F44DE-39E0-4B16-8D28-D5DE35BB11BC}"
        );
    }

    #[test]
    fn format_guid_zero_pads_every_field() {
        let guid = GUID::from_u128(0x00000001_0002_0003_0405_060708090a0b);
        assert_eq!(format_guid(&guid), "{00000001-0002-0003-0405-060708090A0B}");
    }

    /// A collision would make one registration silently overwrite another.
    #[test]
    fn every_published_guid_is_distinct() {
        let all = [
            CLSID_SAKURA_TSF,
            GUID_PROFILE_JA_JP,
            GUID_DISPLAY_ATTRIBUTE_RAW,
            GUID_DISPLAY_ATTRIBUTE_CONVERTED,
            GUID_DISPLAY_ATTRIBUTE_FOCUSED,
            GUID_PRESERVEDKEY_IME_TOGGLE,
            GUID_PRESERVEDKEY_IME_ON,
            GUID_PRESERVEDKEY_IME_OFF,
        ];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(format_guid(a), format_guid(b));
            }
        }
    }

    #[test]
    fn secure_mode_category_is_not_claimed() {
        use windows::Win32::UI::TextServices::GUID_TFCAT_TIPCAP_SECUREMODE;
        assert!(!CATEGORIES.contains(&GUID_TFCAT_TIPCAP_SECUREMODE));
    }
}
