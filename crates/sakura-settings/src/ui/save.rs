//! One Apply operation across independent stores. Earlier successful writes
//! remain committed on failure; the UI must report that partial outcome.
use super::{user_preferences, AiTextKey, AiTextPreferences, ConfigurationDocument};
use std::fmt;
use std::path::Path;

/// Owns the temporary edit copy, including on validation/persistence failure.
pub(super) struct ApiKeyDraft(String);

impl ApiKeyDraft {
    pub(super) fn new(value: String) -> Result<Self, String> {
        let draft = Self(value);
        if !draft.0.trim().is_empty() {
            user_preferences::validate_api_key(&draft.0)
                .map_err(|_| "APIキーが長すぎます。入力内容を確認してください。".to_owned())?;
        }
        Ok(draft)
    }
}

impl Drop for ApiKeyDraft {
    fn drop(&mut self) {
        // SAFETY: zero is valid UTF-8; no reference to the bytes outlives this owner.
        unsafe { self.0.as_bytes_mut() }.fill(0);
    }
}

pub(super) struct PreparedSave {
    pub(super) configuration: ConfigurationDocument,
    pub(super) ai_text_key: AiTextKey,
    pub(super) ai_preferences: AiTextPreferences,
    pub(super) api_key: ApiKeyDraft,
}

/// A failed call must stop the operation. It may have changed its own store;
/// callers must not assume rollback or proceed to the next store on error.
pub(super) trait SettingsStore {
    fn configuration(&mut self, document: &mut ConfigurationDocument) -> Result<(), String>;
    fn ai_text_key(&mut self, value: AiTextKey) -> Result<(), String>;
    fn ai_preferences(&mut self, value: &AiTextPreferences) -> Result<(), String>;
    fn api_key(&mut self, value: &str) -> Result<(), String>;
}

pub(super) struct NativeSettingsStore<'a> {
    pub(super) configuration_path: &'a Path,
}

impl SettingsStore for NativeSettingsStore<'_> {
    fn configuration(&mut self, document: &mut ConfigurationDocument) -> Result<(), String> {
        document
            .save(self.configuration_path)
            .map_err(|e| e.to_string())
    }
    fn ai_text_key(&mut self, value: AiTextKey) -> Result<(), String> {
        user_preferences::write_ai_text_key(value).map_err(|e| e.to_string())
    }
    fn ai_preferences(&mut self, value: &AiTextPreferences) -> Result<(), String> {
        user_preferences::write_ai_text_preferences(value).map_err(|e| e.to_string())
    }
    fn api_key(&mut self, value: &str) -> Result<(), String> {
        user_preferences::write_api_key(value).map_err(|e| e.to_string())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SaveStage {
    Configuration,
    AiTextKey,
    AiPreferences,
    ApiKey,
}

impl SaveStage {
    const ALL: [Self; 4] = [
        Self::Configuration,
        Self::AiTextKey,
        Self::AiPreferences,
        Self::ApiKey,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Configuration => "入力・変換の設定",
            Self::AiTextKey => "文章変換キー",
            Self::AiPreferences => "AI文章変換の設定",
            Self::ApiKey => "APIキー",
        }
    }
}

#[derive(Debug)]
pub(super) struct SaveFailure {
    stage: SaveStage,
    writes_api_key: bool,
    detail: String,
}

impl fmt::Display for SaveFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let stages = &SaveStage::ALL[..if self.writes_api_key { 4 } else { 3 }];
        let failed = stages
            .iter()
            .position(|stage| *stage == self.stage)
            .unwrap();
        let labels = |items: &[SaveStage]| {
            if items.is_empty() {
                "なし".to_owned()
            } else {
                items
                    .iter()
                    .map(|stage| stage.label())
                    .collect::<Vec<_>>()
                    .join("、")
            }
        };
        write!(f,
            "設定の保存を完了できませんでした。\n\n保存済み: {}\n保存を確認できない項目: {}（一部のみ保存されている場合があります）\n未実行: {}\n\n保存済みの変更はキャンセルでは元に戻りません。原因を解消後、もう一度［適用］を押してください。\n\n詳細: {}",
            labels(&stages[..failed]), self.stage.label(), labels(&stages[failed + 1..]), self.detail)
    }
}

impl PreparedSave {
    pub(super) fn persist(
        mut self,
        saved: &mut ConfigurationDocument,
        store: &mut impl SettingsStore,
    ) -> Result<(), SaveFailure> {
        let writes_api_key = !self.api_key.0.trim().is_empty();
        let failure = |stage, detail| SaveFailure {
            stage,
            writes_api_key,
            detail,
        };
        store
            .configuration(&mut self.configuration)
            .map_err(|e| failure(SaveStage::Configuration, e))?;
        // This file is already committed even if a later independent store fails.
        *saved = self.configuration;
        store
            .ai_text_key(self.ai_text_key)
            .map_err(|e| failure(SaveStage::AiTextKey, e))?;
        store
            .ai_preferences(&self.ai_preferences)
            .map_err(|e| failure(SaveStage::AiPreferences, e))?;
        if writes_api_key {
            store
                .api_key(&self.api_key.0)
                .map_err(|e| failure(SaveStage::ApiKey, e))?;
        }
        Ok(())
    }
}
