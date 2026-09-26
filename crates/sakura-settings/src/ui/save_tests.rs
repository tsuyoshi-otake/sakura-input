use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

struct Fixture {
    app: App,
    directory: PathBuf,
    _desktop: std::sync::MutexGuard<'static, ()>,
}

impl Fixture {
    fn new() -> Self {
        let desktop = native_test_guard();
        register_window_class().expect("register settings classes");
        let window = create_main_window().expect("create settings window");
        let mut app = App::new(window).expect("create settings controls");
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = PathBuf::from(std::env::var_os("USERPROFILE").unwrap())
            .join("tmp")
            .join(format!(
                "sakura-settings-save-{unique}-{}",
                std::process::id()
            ));
        std::fs::create_dir_all(&directory).unwrap();
        app.configuration_path = directory.join("config.toml");
        app.configuration = ConfigurationDocument::default();
        app.configuration.preferences.input_support.enabled = !InputSupport::default().enabled;
        app.configuration.save(&app.configuration_path).unwrap();
        app.populate_general();
        let provider = app
            .general
            .ai_providers
            .iter()
            .position(|p| *p == AiProvider::OpenAi)
            .unwrap();
        select_combo(app.general.ai_provider, provider);
        set_text(
            app.general.ai_endpoint,
            AiProvider::OpenAi.default_endpoint(),
        );
        set_text(app.general.profile_process, "settings-regression.exe");
        Self {
            app,
            directory,
            _desktop: desktop,
        }
    }

    fn saved(&self) -> ConfigurationDocument {
        ConfigurationDocument::load(&self.app.configuration_path).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // SAFETY: this fixture owns both native windows; no message pump or worker uses them.
        unsafe {
            let owner = GetWindow(self.app.window, GW_OWNER).ok();
            let _ = DestroyWindow(self.app.window);
            if let Some(owner) = owner {
                let _ = DestroyWindow(owner);
            }
        }
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[test]
fn reset_then_profile_save_keeps_unapplied_global_preferences() {
    let mut fixture = Fixture::new();
    let original = fixture.saved().preferences;
    let reset = fixture.app.general.input_support_reset;
    fixture.app.handle_command(reset, 0).unwrap();
    assert_eq!(
        is_checked(fixture.app.general.input_support_enabled),
        InputSupport::default().enabled
    );
    fixture.app.save_profile().unwrap();
    assert_eq!(
        fixture.saved().preferences,
        original,
        "profile save must not publish the staged reset"
    );
    assert!(fixture
        .saved()
        .profiles
        .iter()
        .any(|p| p.process_name == "settings-regression.exe"));
}

#[test]
fn failed_global_validation_does_not_leak_into_profile_save() {
    let mut fixture = Fixture::new();
    let original = fixture.saved().preferences;
    let mut store = Store::new(&fixture, None);
    select_combo(fixture.app.general.keymap, 1);
    let provider = fixture
        .app
        .general
        .ai_providers
        .iter()
        .position(|p| *p != AiProvider::ChatGptCodex)
        .unwrap();
    select_combo(fixture.app.general.ai_provider, provider);
    set_text(fixture.app.general.ai_endpoint, "invalid-endpoint");
    assert!(
        fixture.app.save_global_settings_to(&mut store).is_err(),
        "invalid endpoint must fail before any registry write"
    );
    assert!(store.calls.is_empty());
    fixture.app.save_profile().unwrap();
    assert_eq!(
        fixture.saved().preferences,
        original,
        "validation failure must leave the saved snapshot intact"
    );
}

#[test]
fn failed_profile_save_does_not_change_the_saved_snapshot() {
    let mut fixture = Fixture::new();
    let original = fixture.app.configuration.clone();
    let blocked = fixture.directory.join("not-a-directory");
    std::fs::write(&blocked, b"block publication").unwrap();
    fixture.app.configuration_path = blocked.join("config.toml");
    assert!(fixture.app.save_profile().is_err());
    assert_eq!(fixture.app.configuration, original);
}

#[test]
fn failed_profile_delete_preserves_the_saved_snapshot() {
    let mut fixture = Fixture::new();
    fixture.app.save_profile().unwrap();
    let original = fixture.app.configuration.clone();
    let blocked = fixture.directory.join("not-a-directory");
    std::fs::write(&blocked, b"block publication").unwrap();
    fixture.app.configuration_path = blocked.join("config.toml");
    assert!(fixture.app.delete_profile().is_err());
    assert_eq!(fixture.app.configuration, original);
}

#[test]
fn reset_then_profile_delete_does_not_publish_the_reset() {
    let mut fixture = Fixture::new();
    fixture.app.save_profile().unwrap();
    let original = fixture.saved().preferences;
    fixture
        .app
        .handle_command(fixture.app.general.input_support_reset, 0)
        .unwrap();
    fixture.app.delete_profile().unwrap();
    assert_eq!(fixture.saved().preferences, original);
    assert!(!fixture
        .saved()
        .profiles
        .iter()
        .any(|p| p.process_name == "settings-regression.exe"));
}

/// Real isolated TOML publication, injected registry/credential stores. No
/// current-user registry or Credential Manager writes are permitted here.
struct Store {
    path: PathBuf,
    fail: Option<save::SaveStage>,
    calls: Vec<save::SaveStage>,
    credential_saved: bool,
}

impl Store {
    fn new(fixture: &Fixture, fail: Option<save::SaveStage>) -> Self {
        Self {
            path: fixture.app.configuration_path.clone(),
            fail,
            calls: Vec::new(),
            credential_saved: false,
        }
    }

    fn attempt(&mut self, stage: save::SaveStage) -> Result<(), String> {
        self.calls.push(stage);
        if self.fail == Some(stage) {
            Err("injected write failure".to_owned())
        } else {
            Ok(())
        }
    }
}

impl save::SettingsStore for Store {
    fn configuration(&mut self, document: &mut ConfigurationDocument) -> Result<(), String> {
        self.attempt(save::SaveStage::Configuration)?;
        document.save(&self.path).map_err(display)
    }
    fn ai_text_key(&mut self, _: AiTextKey) -> Result<(), String> {
        self.attempt(save::SaveStage::AiTextKey)
    }
    fn ai_preferences(&mut self, _: &AiTextPreferences) -> Result<(), String> {
        self.attempt(save::SaveStage::AiPreferences)
    }
    fn api_key(&mut self, _: &str) -> Result<(), String> {
        self.attempt(save::SaveStage::ApiKey)?;
        self.credential_saved = true;
        Ok(())
    }
}

#[test]
fn oversized_api_key_prevents_every_write_and_keeps_the_saved_snapshot() {
    let mut fixture = Fixture::new();
    let original = fixture.saved();
    let mut store = Store::new(&fixture, None);
    select_combo(fixture.app.general.keymap, 1);
    let invalid = "x".repeat(2049);
    set_text(fixture.app.general.ai_api_key, &invalid);
    let error = fixture.app.save_global_settings_to(&mut store).unwrap_err();
    assert!(error.contains("APIキー"));
    assert!(!error.contains(&invalid));
    assert!(store.calls.is_empty());
    assert_eq!(fixture.saved(), original);
    assert_eq!(fixture.app.configuration, original);
    fixture.app.save_profile().unwrap();
    assert_eq!(fixture.saved().preferences, original.preferences);
}

#[test]
fn each_store_failure_reports_committed_uncertain_and_unattempted_settings() {
    use save::SaveStage::*;
    let stages = [Configuration, AiTextKey, AiPreferences, ApiKey];
    let labels = [
        "入力・変換の設定",
        "文章変換キー",
        "AI文章変換の設定",
        "APIキー",
    ];
    for (index, stage) in stages.into_iter().enumerate() {
        let mut fixture = Fixture::new();
        let original = fixture.saved();
        let mut store = Store::new(&fixture, Some(stage));
        select_combo(fixture.app.general.keymap, 1);
        set_text(
            fixture.app.general.ai_api_key,
            "fake-credential-for-injected-store",
        );
        let error = fixture.app.save_global_settings_to(&mut store).unwrap_err();
        assert_eq!(store.calls, stages[..=index]);
        let completed = if index == 0 {
            "なし".to_owned()
        } else {
            labels[..index].join("、")
        };
        let remaining = if index == 3 {
            "なし".to_owned()
        } else {
            labels[index + 1..].join("、")
        };
        assert!(
            error.contains(&format!("保存済み: {completed}\n")),
            "{error}"
        );
        assert!(
            error.contains(&format!("保存を確認できない項目: {}", labels[index])),
            "{error}"
        );
        assert!(error.contains(&format!("未実行: {remaining}\n")), "{error}");
        assert!(error.contains("もう一度［適用］"));
        assert!(!error.contains("fake-credential"));
        assert!(
            !window_text(fixture.app.general.ai_api_key).is_empty(),
            "retain failed edit for retry"
        );
        let committed_mode = if index == 0 {
            original.preferences.keymap_preset
        } else {
            Preset::Atok
        };
        assert_eq!(fixture.saved().preferences.keymap_preset, committed_mode);
        assert_eq!(fixture.app.configuration, fixture.saved());
        // An immediate profile save after partial Apply must preserve exactly
        // the already-published TOML, never restore an obsolete snapshot.
        fixture.app.save_profile().unwrap();
        assert_eq!(fixture.saved().preferences.keymap_preset, committed_mode);
        store.fail = None;
        store.calls.clear();
        fixture.app.save_global_settings_to(&mut store).unwrap();
        assert_eq!(store.calls, stages);
        assert_eq!(fixture.saved().preferences.keymap_preset, Preset::Atok);
        assert!(store.credential_saved);
        assert!(window_text(fixture.app.general.ai_api_key).is_empty());
    }
}

#[test]
fn blank_api_key_is_preserved_and_reset_is_published_only_by_apply() {
    use save::SaveStage::*;
    let mut fixture = Fixture::new();
    let mut store = Store::new(&fixture, None);
    fixture
        .app
        .handle_command(fixture.app.general.input_support_reset, 0)
        .unwrap();
    set_text(fixture.app.general.ai_api_key, " \t ");
    fixture.app.save_global_settings_to(&mut store).unwrap();
    assert_eq!(store.calls, [Configuration, AiTextKey, AiPreferences]);
    assert!(!store.credential_saved);
    assert_eq!(
        fixture.saved().preferences.input_support,
        InputSupport::default()
    );
    assert_eq!(fixture.app.configuration, fixture.saved());
}
