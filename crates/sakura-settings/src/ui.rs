//! Native Win32 settings control panel.
//!
//! The UI is intentionally a thin frontend over `sakura-settings`' tested
//! library operations. Every button reaches the same transactional path as the
//! CLI; the window never edits a durable file piecemeal.

use std::ffi::c_void;
use std::path::PathBuf;

use sakura_core::{
    AppProfile, Preset, SuggestAccept, UserDictionary, UserDictionaryEntry, UserPartOfSpeech,
};
use sakura_proto::Mode;
use sakura_settings::configuration::ConfigurationDocument;
use sakura_settings::formats::DictionaryFormat;
use sakura_settings::user_dictionary::{self, ImportMode};
use sakura_settings::{diagnostics, learning, paths, updater};
use windows::core::{Result as WindowsResult, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetStockObject, GetSysColorBrush, UpdateWindow, COLOR_WINDOW, DEFAULT_GUI_FONT,
};
use windows::Win32::UI::Controls::{BST_CHECKED, BST_UNCHECKED};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW, LoadCursorW, MessageBoxW,
    PostMessageW, PostQuitMessage, RegisterClassW, SendMessageW, SetWindowLongPtrW, SetWindowTextW,
    ShowWindow, TranslateMessage, BM_GETCHECK, BM_SETCHECK, BS_AUTOCHECKBOX, BS_DEFPUSHBUTTON,
    CBS_DROPDOWNLIST, CB_ADDSTRING, CB_GETCURSEL, CB_SETCURSEL, CW_USEDEFAULT, ES_AUTOHSCROLL,
    ES_AUTOVSCROLL, ES_MULTILINE, ES_READONLY, ES_WANTRETURN, GWLP_USERDATA, IDC_ARROW, IDYES,
    LBN_SELCHANGE, LBS_NOINTEGRALHEIGHT, LBS_NOTIFY, LB_ADDSTRING, LB_GETCURSEL, LB_RESETCONTENT,
    MB_ICONERROR, MB_ICONWARNING, MB_OK, MB_YESNO, MSG, SW_HIDE, SW_SHOW, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_APP, WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_SETFONT, WNDCLASSW, WS_CHILD,
    WS_CLIPCHILDREN, WS_EX_CLIENTEDGE, WS_HSCROLL, WS_OVERLAPPEDWINDOW, WS_TABSTOP, WS_VISIBLE,
    WS_VSCROLL,
};

use crate::cli::mode_name;

const WINDOW_CLASS: PCWSTR = windows::core::w!("SakuraInputSettingsWindow");
const PANEL_COUNT: usize = 5;
const WM_UPDATE_COMPLETE: u32 = WM_APP + 17;

#[derive(Debug)]
struct GeneralControls {
    keymap: HWND,
    prediction: HWND,
    suggest: HWND,
    save: HWND,
    profile_list: HWND,
    profile_process: HWND,
    profile_mode: HWND,
    profile_prediction: HWND,
    profile_suggest: HWND,
    profile_save: HWND,
    profile_delete: HWND,
}

#[derive(Debug)]
struct DictionaryControls {
    list: HWND,
    reading: HWND,
    surface: HWND,
    part_of_speech: HWND,
    comment: HWND,
    add: HWND,
    update: HWND,
    delete: HWND,
    path: HWND,
    format: HWND,
    import_mode: HWND,
    import: HWND,
    export: HWND,
}

#[derive(Debug)]
struct LearningControls {
    list: HWND,
    export_path: HWND,
    refresh: HWND,
    export: HWND,
    clear: HWND,
}

#[derive(Debug)]
struct DiagnosticsControls {
    text: HWND,
    refresh: HWND,
    clear: HWND,
}

#[derive(Debug)]
struct UpdateControls {
    enabled: HWND,
    save: HWND,
    check: HWND,
    apply: HWND,
    result: HWND,
}

#[derive(Debug, Clone, Copy)]
enum UpdateOperation {
    Check,
    Apply,
}

#[derive(Debug)]
enum UpdateCompletion {
    Check(updater::UpdateCheckOutcome),
    Apply(updater::UpdateOutcome),
}

#[derive(Debug)]
struct App {
    window: HWND,
    panels: [HWND; PANEL_COUNT],
    navigation: [HWND; PANEL_COUNT],
    status: HWND,
    general: GeneralControls,
    dictionary_controls: DictionaryControls,
    learning_controls: LearningControls,
    diagnostics_controls: DiagnosticsControls,
    update_controls: UpdateControls,
    configuration_path: PathBuf,
    dictionary_path: PathBuf,
    learning_path: PathBuf,
    diagnostics_path: PathBuf,
    update_preferences_path: PathBuf,
    update_paths: updater::UpdatePaths,
    configuration: ConfigurationDocument,
    dictionary: UserDictionary,
    update_preferences: updater::UpdatePreferences,
    update_in_flight: bool,
}

pub fn run() -> Result<(), String> {
    // SAFETY: process DPI awareness is selected before creating any window.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    register_window_class().map_err(display)?;
    let window = create_main_window().map_err(display)?;
    let app = App::new(window)?;
    let app = Box::into_raw(Box::new(app));
    // SAFETY: the boxed state remains alive until the message pump exits. Only
    // this UI thread reads the pointer stored on its own window.
    unsafe {
        SetWindowLongPtrW(window, GWLP_USERDATA, app as isize);
        if (*app).update_preferences.enabled {
            if let Err(error) = (*app).start_update(UpdateOperation::Check) {
                (*app).set_status(&format!("Automatic update check could not start: {error}"));
            }
        }
        let _ = ShowWindow(window, SW_SHOW);
        let _ = UpdateWindow(window);
    }
    pump();
    // SAFETY: WM_DESTROY has ended the pump, so no later message can access the
    // pointer. Clear user data before reclaiming the box.
    unsafe {
        SetWindowLongPtrW(window, GWLP_USERDATA, 0);
        drop(Box::from_raw(app));
    }
    Ok(())
}

pub fn show_fatal_error(message: &str) {
    message_box(None, message, "Sakura Input", MB_OK | MB_ICONERROR);
}

impl App {
    fn new(window: HWND) -> Result<Self, String> {
        let configuration_path = paths::configuration().map_err(display)?;
        let dictionary_path = paths::user_dictionary().map_err(display)?;
        let learning_path = paths::learning().map_err(display)?;
        let diagnostics_path = paths::timeout_diagnostics().map_err(display)?;
        let update_preferences_path = paths::update_preferences().map_err(display)?;
        let update_paths = updater::UpdatePaths {
            installer: paths::update_installer().map_err(display)?,
            log: paths::update_log().map_err(display)?,
        };
        let configuration = ConfigurationDocument::load(&configuration_path).map_err(display)?;
        let dictionary = user_dictionary::load(&dictionary_path).map_err(display)?;
        let update_preferences =
            updater::UpdatePreferences::load(&update_preferences_path).map_err(display)?;

        let navigation = [
            button(window, "General & profiles", 12, 12, 174, 34, false).map_err(display)?,
            button(window, "User dictionary", 198, 12, 174, 34, false).map_err(display)?,
            button(window, "Learning", 384, 12, 174, 34, false).map_err(display)?,
            button(window, "Diagnostics", 570, 12, 174, 34, false).map_err(display)?,
            button(window, "Updates", 756, 12, 174, 34, false).map_err(display)?,
        ];
        let panels = [
            panel(window).map_err(display)?,
            panel(window).map_err(display)?,
            panel(window).map_err(display)?,
            panel(window).map_err(display)?,
            panel(window).map_err(display)?,
        ];
        for hidden in &panels[1..] {
            // SAFETY: every panel is a live child window.
            unsafe {
                let _ = ShowWindow(*hidden, SW_HIDE);
            }
        }
        let status = label(window, "Ready", 16, 714, 920, 24).map_err(display)?;

        let general = create_general_controls(panels[0]).map_err(display)?;
        let dictionary_controls = create_dictionary_controls(panels[1]).map_err(display)?;
        let learning_controls = create_learning_controls(panels[2]).map_err(display)?;
        let diagnostics_controls = create_diagnostics_controls(panels[3]).map_err(display)?;
        let update_controls = create_update_controls(panels[4]).map_err(display)?;

        let mut app = Self {
            window,
            panels,
            navigation,
            status,
            general,
            dictionary_controls,
            learning_controls,
            diagnostics_controls,
            update_controls,
            configuration_path,
            dictionary_path,
            learning_path,
            diagnostics_path,
            update_preferences_path,
            update_paths,
            configuration,
            dictionary,
            update_preferences,
            update_in_flight: false,
        };
        app.populate_general();
        app.populate_dictionary();
        app.refresh_learning()?;
        app.refresh_diagnostics()?;
        app.populate_updates();
        Ok(app)
    }

    fn handle_command(&mut self, source: HWND, notification: u16) -> Result<(), String> {
        if let Some(index) = self.navigation.iter().position(|button| *button == source) {
            self.show_panel(index);
            return Ok(());
        }
        if source == self.general.save {
            return self.save_global_settings();
        }
        if source == self.general.profile_save {
            return self.save_profile();
        }
        if source == self.general.profile_delete {
            return self.delete_profile();
        }
        if source == self.general.profile_list && notification == LBN_SELCHANGE as u16 {
            self.select_profile();
            return Ok(());
        }
        if source == self.dictionary_controls.list && notification == LBN_SELCHANGE as u16 {
            self.select_dictionary_entry();
            return Ok(());
        }
        if source == self.dictionary_controls.add {
            return self.add_dictionary_entry();
        }
        if source == self.dictionary_controls.update {
            return self.update_dictionary_entry();
        }
        if source == self.dictionary_controls.delete {
            return self.delete_dictionary_entry();
        }
        if source == self.dictionary_controls.import {
            return self.import_dictionary();
        }
        if source == self.dictionary_controls.export {
            return self.export_dictionary();
        }
        if source == self.learning_controls.refresh {
            return self.refresh_learning();
        }
        if source == self.learning_controls.export {
            return self.export_learning();
        }
        if source == self.learning_controls.clear {
            return self.clear_learning();
        }
        if source == self.diagnostics_controls.refresh {
            return self.refresh_diagnostics();
        }
        if source == self.diagnostics_controls.clear {
            diagnostics::clear(&self.diagnostics_path).map_err(display)?;
            self.refresh_diagnostics()?;
            self.set_status("IPC timeout diagnostics cleared");
        }
        if source == self.update_controls.save {
            return self.save_update_preference();
        }
        if source == self.update_controls.check {
            return self.start_update(UpdateOperation::Check);
        }
        if source == self.update_controls.apply {
            if !confirm(
                "Download, verify, and run the latest signed Sakura Input installer with administrator approval?",
            ) {
                self.set_status("Update installation cancelled");
                return Ok(());
            }
            return self.start_update(UpdateOperation::Apply);
        }
        Ok(())
    }

    fn show_panel(&self, selected: usize) {
        for (index, panel) in self.panels.iter().enumerate() {
            // SAFETY: every handle is a live child owned by the main window.
            unsafe {
                let _ = ShowWindow(*panel, if index == selected { SW_SHOW } else { SW_HIDE });
            }
        }
    }

    fn populate_general(&mut self) {
        select_combo(
            self.general.keymap,
            match self.configuration.preferences.keymap_preset {
                Preset::MsIme => 0,
                Preset::Atok => 1,
            },
        );
        set_checked(
            self.general.prediction,
            self.configuration.preferences.prediction_enabled,
        );
        select_combo(
            self.general.suggest,
            suggest_index(self.configuration.preferences.suggest_accept),
        );
        self.populate_profile_list();
    }

    fn populate_profile_list(&self) {
        reset_list(self.general.profile_list);
        for profile in &self.configuration.profiles {
            add_list(
                self.general.profile_list,
                &format!(
                    "{}  —  {}, prediction {}",
                    profile.process_name,
                    mode_name(profile.default_mode),
                    if profile.prediction_enabled {
                        "on"
                    } else {
                        "off"
                    }
                ),
            );
        }
    }

    fn save_global_settings(&mut self) -> Result<(), String> {
        self.configuration.preferences.keymap_preset = match combo_index(self.general.keymap) {
            Some(0) => Preset::MsIme,
            Some(1) => Preset::Atok,
            _ => return Err("select a keymap preset".to_owned()),
        };
        self.configuration.preferences.prediction_enabled = is_checked(self.general.prediction);
        self.configuration.preferences.suggest_accept =
            suggest_from_index(combo_index(self.general.suggest))?;
        self.configuration
            .save(&self.configuration_path)
            .map_err(display)?;
        self.set_status(&format!("Saved {}", self.configuration_path.display()));
        Ok(())
    }

    fn select_profile(&self) {
        let Some(index) = list_index(self.general.profile_list) else {
            return;
        };
        let Some(profile) = self.configuration.profiles.get(index) else {
            return;
        };
        set_text(self.general.profile_process, &profile.process_name);
        select_combo(self.general.profile_mode, mode_index(profile.default_mode));
        set_checked(self.general.profile_prediction, profile.prediction_enabled);
        select_combo(
            self.general.profile_suggest,
            suggest_index(profile.suggest_accept),
        );
    }

    fn save_profile(&mut self) -> Result<(), String> {
        let process_name = required_text(self.general.profile_process, "process name")?;
        let normalizer = self
            .configuration
            .profiles
            .iter()
            .find(|profile| profile.matches(&process_name))
            .map_or(self.configuration.preferences.normalizer, |profile| {
                profile.normalizer
            });
        self.configuration
            .upsert_profile(AppProfile {
                process_name: process_name.clone(),
                default_mode: mode_from_index(combo_index(self.general.profile_mode))?,
                normalizer,
                prediction_enabled: is_checked(self.general.profile_prediction),
                suggest_accept: suggest_from_index(combo_index(self.general.profile_suggest))?,
            })
            .map_err(display)?;
        self.configuration
            .save(&self.configuration_path)
            .map_err(display)?;
        self.populate_profile_list();
        self.set_status(&format!("Saved profile {process_name}"));
        Ok(())
    }

    fn delete_profile(&mut self) -> Result<(), String> {
        let process_name = required_text(self.general.profile_process, "process name")?;
        self.configuration
            .remove_profile(&process_name)
            .map_err(display)?;
        self.configuration
            .save(&self.configuration_path)
            .map_err(display)?;
        self.populate_profile_list();
        set_text(self.general.profile_process, "");
        self.set_status(&format!("Deleted profile {process_name}"));
        Ok(())
    }

    fn populate_dictionary(&self) {
        reset_list(self.dictionary_controls.list);
        for entry in self.dictionary.entries() {
            add_list(
                self.dictionary_controls.list,
                &format!(
                    "{}  →  {}  [{}]",
                    entry.reading,
                    entry.surface,
                    entry.part_of_speech.spec().name
                ),
            );
        }
    }

    fn select_dictionary_entry(&self) {
        let Some(index) = list_index(self.dictionary_controls.list) else {
            return;
        };
        let Some(entry) = self.dictionary.entry(index) else {
            return;
        };
        set_text(self.dictionary_controls.reading, &entry.reading);
        set_text(self.dictionary_controls.surface, &entry.surface);
        set_text(self.dictionary_controls.comment, &entry.comment);
        select_combo(
            self.dictionary_controls.part_of_speech,
            UserPartOfSpeech::ALL
                .iter()
                .position(|pos| *pos == entry.part_of_speech)
                .unwrap_or(0),
        );
    }

    fn dictionary_entry_from_controls(&self) -> Result<UserDictionaryEntry, String> {
        let index = combo_index(self.dictionary_controls.part_of_speech)
            .ok_or_else(|| "select a part of speech".to_owned())?;
        let part_of_speech = UserPartOfSpeech::ALL
            .get(index)
            .copied()
            .ok_or_else(|| "selected part of speech is invalid".to_owned())?;
        Ok(UserDictionaryEntry {
            reading: required_text(self.dictionary_controls.reading, "reading")?,
            surface: required_text(self.dictionary_controls.surface, "surface")?,
            part_of_speech,
            comment: window_text(self.dictionary_controls.comment),
        })
    }

    fn add_dictionary_entry(&mut self) -> Result<(), String> {
        let entry = self.dictionary_entry_from_controls()?;
        self.dictionary = user_dictionary::add(&self.dictionary_path, entry).map_err(display)?;
        self.populate_dictionary();
        self.set_status(&format!(
            "Dictionary now contains {} entries",
            self.dictionary.len()
        ));
        Ok(())
    }

    fn update_dictionary_entry(&mut self) -> Result<(), String> {
        let index = list_index(self.dictionary_controls.list)
            .ok_or_else(|| "select the dictionary entry to update".to_owned())?;
        let original = self
            .dictionary
            .entry(index)
            .cloned()
            .ok_or_else(|| "selected dictionary entry no longer exists".to_owned())?;
        let replacement = self.dictionary_entry_from_controls()?;
        self.dictionary = user_dictionary::update(
            &self.dictionary_path,
            &original.reading,
            &original.surface,
            replacement,
        )
        .map_err(display)?;
        self.populate_dictionary();
        self.set_status("Dictionary entry updated");
        Ok(())
    }

    fn delete_dictionary_entry(&mut self) -> Result<(), String> {
        let index = list_index(self.dictionary_controls.list)
            .ok_or_else(|| "select the dictionary entry to delete".to_owned())?;
        let entry = self
            .dictionary
            .entry(index)
            .cloned()
            .ok_or_else(|| "selected dictionary entry no longer exists".to_owned())?;
        self.dictionary =
            user_dictionary::delete(&self.dictionary_path, &entry.reading, &entry.surface)
                .map_err(display)?;
        self.populate_dictionary();
        self.set_status("Dictionary entry deleted");
        Ok(())
    }

    fn import_dictionary(&mut self) -> Result<(), String> {
        let source = PathBuf::from(required_text(self.dictionary_controls.path, "import file")?);
        let format = dictionary_format(combo_index(self.dictionary_controls.format), true)?;
        let mode = match combo_index(self.dictionary_controls.import_mode) {
            Some(0) => ImportMode::Merge,
            Some(1) => {
                if !confirm(
                    "Replace every existing Sakura Input user-dictionary entry with this file?",
                ) {
                    self.set_status("Dictionary replacement cancelled");
                    return Ok(());
                }
                ImportMode::Replace
            }
            _ => return Err("select merge or replace".to_owned()),
        };
        let bytes = std::fs::read(&source).map_err(display)?;
        let report = user_dictionary::import(&self.dictionary_path, &bytes, format, mode)
            .map_err(display)?;
        self.dictionary = user_dictionary::load(&self.dictionary_path).map_err(display)?;
        self.populate_dictionary();
        self.set_status(&format!(
            "Imported {} {} entries; {} total",
            report.imported,
            report.format.name(),
            report.total
        ));
        Ok(())
    }

    fn export_dictionary(&self) -> Result<(), String> {
        let destination =
            PathBuf::from(required_text(self.dictionary_controls.path, "export file")?);
        let format = dictionary_format(combo_index(self.dictionary_controls.format), false)?
            .ok_or_else(|| "select an export format".to_owned())?;
        let count = user_dictionary::export(&self.dictionary_path, &destination, format)
            .map_err(display)?;
        self.set_status(&format!(
            "Exported {count} entries to {}",
            destination.display()
        ));
        Ok(())
    }

    fn refresh_learning(&self) -> Result<(), String> {
        let snapshot = learning::view(&self.learning_path).map_err(display)?;
        reset_list(self.learning_controls.list);
        for record in snapshot.records.iter().rev().take(2_000) {
            add_list(
                self.learning_controls.list,
                &format!(
                    "#{}  {} → {}  ({} / {})",
                    record.sequence,
                    record.reading,
                    record.surface.replace(['\r', '\n'], " "),
                    record.left_context,
                    record.right_context
                ),
            );
        }
        self.set_status(&format!(
            "Learning: {} verified records, {} ignored tail bytes",
            snapshot.records.len(),
            snapshot.ignored_tail_bytes
        ));
        Ok(())
    }

    fn export_learning(&self) -> Result<(), String> {
        let destination = PathBuf::from(required_text(
            self.learning_controls.export_path,
            "export file",
        )?);
        let count = learning::export(&self.learning_path, &destination).map_err(display)?;
        self.set_status(&format!(
            "Exported {count} learning records to {}",
            destination.display()
        ));
        Ok(())
    }

    fn clear_learning(&self) -> Result<(), String> {
        if !confirm("Clear all learned conversions and prediction history?") {
            self.set_status("Learning clear cancelled");
            return Ok(());
        }
        let route = learning::clear(&self.learning_path).map_err(display)?;
        self.refresh_learning()?;
        self.set_status(match route {
            learning::ClearRoute::LiveEngine => "Learning cleared through the running engine",
            learning::ClearRoute::Offline { .. } => "Learning cleared while the engine was offline",
        });
        Ok(())
    }

    fn refresh_diagnostics(&self) -> Result<(), String> {
        let data = diagnostics::load(&self.diagnostics_path).map_err(display)?;
        set_text(
            self.diagnostics_controls.text,
            &diagnostics::render_text(&data).replace('\n', "\r\n"),
        );
        self.set_status(&format!("IPC timeouts recorded: {}", data.valid_events));
        Ok(())
    }

    fn populate_updates(&self) {
        set_checked(
            self.update_controls.enabled,
            self.update_preferences.enabled,
        );
        set_text(
            self.update_controls.result,
            &format!(
                "Installed version: {}\r\nAutomatic network checks: {}\r\n\r\nNo update request is made while the option is disabled.",
                updater::current_version(),
                if self.update_preferences.enabled {
                    "enabled"
                } else {
                    "disabled"
                }
            ),
        );
    }

    fn save_update_preference(&mut self) -> Result<(), String> {
        let was_enabled = self.update_preferences.enabled;
        let enabled = is_checked(self.update_controls.enabled);
        updater::UpdatePreferences { enabled }
            .save(&self.update_preferences_path)
            .map_err(display)?;
        self.update_preferences.enabled = enabled;
        self.populate_updates();
        self.set_status(if enabled {
            "Automatic update checks enabled"
        } else {
            "Automatic update checks disabled"
        });
        if enabled && !was_enabled {
            self.start_update(UpdateOperation::Check)?;
        }
        Ok(())
    }

    fn start_update(&mut self, operation: UpdateOperation) -> Result<(), String> {
        if self.update_in_flight {
            return Err("an update operation is already in progress".to_owned());
        }
        let window_value = self.window.0 as usize;
        let enabled = self.update_preferences.enabled;
        let update_paths = self.update_paths.clone();
        std::thread::Builder::new()
            .name("sakura-update-worker".to_owned())
            .spawn(move || {
                let window = HWND(window_value as *mut c_void);
                let completion = std::panic::catch_unwind(|| match operation {
                    UpdateOperation::Check => UpdateCompletion::Check(updater::check_real(enabled)),
                    UpdateOperation::Apply => {
                        UpdateCompletion::Apply(updater::apply_real(enabled, &update_paths))
                    }
                })
                .unwrap_or_else(|_| {
                    let failure = updater::UpdateFailure {
                        stage: updater::UpdateStage::Worker,
                        message: "the update worker terminated unexpectedly".to_owned(),
                    };
                    match operation {
                        UpdateOperation::Check => {
                            UpdateCompletion::Check(updater::UpdateCheckOutcome::Failed(failure))
                        }
                        UpdateOperation::Apply => {
                            UpdateCompletion::Apply(updater::UpdateOutcome::Failed {
                                version: None,
                                failure,
                            })
                        }
                    }
                });
                let pointer = Box::into_raw(Box::new(completion));
                // SAFETY: ownership of `pointer` transfers to the UI thread if
                // the post succeeds. A failed post reclaims it here.
                if unsafe {
                    PostMessageW(
                        Some(window),
                        WM_UPDATE_COMPLETE,
                        WPARAM(pointer as usize),
                        LPARAM(0),
                    )
                }
                .is_err()
                {
                    // SAFETY: PostMessage failed, so no other thread can own or
                    // observe this allocation.
                    unsafe {
                        drop(Box::from_raw(pointer));
                    }
                }
            })
            .map_err(|error| format!("could not start update worker: {error}"))?;
        self.update_in_flight = true;
        let message = match operation {
            UpdateOperation::Check => "Checking the signed release manifest...",
            UpdateOperation::Apply => {
                "Downloading and verifying the signed installer. Do not close this window..."
            }
        };
        set_text(self.update_controls.result, message);
        self.set_status(message);
        Ok(())
    }

    fn finish_update(&mut self, completion: UpdateCompletion) {
        self.update_in_flight = false;
        let (message, failed) = match completion {
            UpdateCompletion::Check(outcome) => {
                let failed = matches!(outcome, updater::UpdateCheckOutcome::Failed(_));
                (outcome.describe(), failed)
            }
            UpdateCompletion::Apply(outcome) => {
                let failed = outcome.is_failure();
                (outcome.describe(), failed)
            }
        };
        set_text(self.update_controls.result, &message.replace('\n', "\r\n"));
        self.set_status(&message.replace(['\r', '\n'], " "));
        if failed {
            message_box(
                Some(self.window),
                &message,
                "Sakura Input update",
                MB_OK | MB_ICONERROR,
            );
        }
    }

    fn set_status(&self, message: &str) {
        set_text(self.status, message);
    }
}

fn register_window_class() -> WindowsResult<()> {
    // SAFETY: the class name and procedure are static; stock objects are
    // process-owned and remain valid for the window class lifetime.
    unsafe {
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_procedure),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hbrBackground: GetSysColorBrush(COLOR_WINDOW),
            lpszClassName: WINDOW_CLASS,
            ..Default::default()
        };
        RegisterClassW(&class);
    }
    Ok(())
}

fn create_main_window() -> WindowsResult<HWND> {
    // SAFETY: the class has just been registered and all text pointers outlive
    // this synchronous call.
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            WINDOW_CLASS,
            windows::core::w!("Sakura Input Settings"),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            970,
            790,
            None,
            None,
            None,
            None,
        )
    }
}

fn panel(parent: HWND) -> WindowsResult<HWND> {
    control(
        parent,
        windows::core::w!("STATIC"),
        "",
        WS_CHILD | WS_VISIBLE,
        WINDOW_EX_STYLE::default(),
        12,
        56,
        930,
        646,
    )
}

fn create_general_controls(parent: HWND) -> WindowsResult<GeneralControls> {
    label(parent, "Global input settings", 16, 12, 260, 24)?;
    label(parent, "Keymap preset", 20, 48, 150, 24)?;
    let keymap = combo(parent, 180, 44, 230, 200)?;
    add_combo(keymap, "Microsoft IME");
    add_combo(keymap, "ATOK");
    let prediction = checkbox(parent, "Enable prediction", 440, 45, 190, 28)?;
    label(parent, "Accept suggestion", 20, 86, 150, 24)?;
    let suggest = combo(parent, 180, 82, 230, 160)?;
    add_combo(suggest, "Tab");
    add_combo(suggest, "Shift+Enter");
    add_combo(suggest, "Disabled");
    let save = button(parent, "Save global settings", 440, 80, 190, 32, true)?;

    label(parent, "Application profiles", 16, 136, 260, 24)?;
    let profile_list = listbox(parent, 20, 170, 430, 390)?;
    label(parent, "Executable", 478, 170, 130, 24)?;
    let profile_process = edit(parent, "", 610, 166, 270, 28, false)?;
    label(parent, "Default mode", 478, 210, 130, 24)?;
    let profile_mode = combo(parent, 610, 206, 270, 220)?;
    for mode in Mode::ALL {
        add_combo(profile_mode, mode_name(mode));
    }
    let profile_prediction = checkbox(parent, "Enable prediction", 610, 250, 220, 28)?;
    label(parent, "Accept suggestion", 478, 294, 130, 24)?;
    let profile_suggest = combo(parent, 610, 290, 270, 160)?;
    add_combo(profile_suggest, "Tab");
    add_combo(profile_suggest, "Shift+Enter");
    add_combo(profile_suggest, "Disabled");
    select_combo(profile_mode, mode_index(Mode::Hiragana));
    select_combo(profile_suggest, suggest_index(SuggestAccept::Tab));
    let profile_save = button(parent, "Add / update profile", 610, 346, 200, 34, false)?;
    let profile_delete = button(parent, "Delete profile", 610, 390, 200, 34, false)?;
    label(
        parent,
        "Profiles are resolved when an application creates its input context.\nUse a plain executable name such as code.exe.",
        478,
        454,
        410,
        60,
    )?;
    Ok(GeneralControls {
        keymap,
        prediction,
        suggest,
        save,
        profile_list,
        profile_process,
        profile_mode,
        profile_prediction,
        profile_suggest,
        profile_save,
        profile_delete,
    })
}

fn create_dictionary_controls(parent: HWND) -> WindowsResult<DictionaryControls> {
    label(parent, "Sakura Input user dictionary", 16, 12, 320, 24)?;
    let list = listbox(parent, 20, 46, 520, 400)?;
    label(parent, "Reading", 564, 46, 100, 24)?;
    let reading = edit(parent, "", 672, 42, 220, 28, false)?;
    label(parent, "Surface", 564, 84, 100, 24)?;
    let surface = edit(parent, "", 672, 80, 220, 28, false)?;
    label(parent, "Part of speech", 564, 122, 110, 24)?;
    let part_of_speech = combo(parent, 672, 118, 220, 350)?;
    for pos in UserPartOfSpeech::ALL {
        add_combo(
            part_of_speech,
            &format!("{} — {}", pos.spec().name, pos.spec().label),
        );
    }
    select_combo(part_of_speech, 0);
    label(parent, "Comment", 564, 160, 100, 24)?;
    let comment = edit(parent, "", 672, 156, 220, 28, false)?;
    let add = button(parent, "Add", 564, 204, 96, 32, false)?;
    let update = button(parent, "Update selected", 670, 204, 128, 32, false)?;
    let delete = button(parent, "Delete selected", 808, 204, 118, 32, false)?;

    label(parent, "Import / export", 16, 476, 240, 24)?;
    label(parent, "File path", 20, 514, 90, 24)?;
    let path = edit(parent, "", 112, 510, 430, 28, false)?;
    label(parent, "Format", 560, 514, 70, 24)?;
    let format = combo(parent, 630, 510, 160, 220)?;
    add_combo(format, "Auto-detect (import)");
    for value in DictionaryFormat::ALL {
        add_combo(format, value.name());
    }
    select_combo(format, 0);
    let import_mode = combo(parent, 20, 554, 180, 120)?;
    add_combo(import_mode, "Merge");
    add_combo(import_mode, "Replace all");
    select_combo(import_mode, 0);
    let import = button(parent, "Import", 214, 552, 120, 32, false)?;
    let export = button(parent, "Export", 344, 552, 120, 32, false)?;
    label(
        parent,
        "MS-IME and ATOK exports use UTF-16LE; Sakura and Mozc use UTF-8.\nUnknown POS and unsupported extra fields are rejected before publication.",
        494,
        552,
        410,
        60,
    )?;
    Ok(DictionaryControls {
        list,
        reading,
        surface,
        part_of_speech,
        comment,
        add,
        update,
        delete,
        path,
        format,
        import_mode,
        import,
        export,
    })
}

fn create_learning_controls(parent: HWND) -> WindowsResult<LearningControls> {
    label(
        parent,
        "Verified learning history (newest first)",
        16,
        12,
        420,
        24,
    )?;
    let list = listbox(parent, 20, 46, 880, 450)?;
    let refresh = button(parent, "Refresh", 20, 518, 120, 32, false)?;
    label(parent, "Export path", 164, 522, 100, 24)?;
    let export_path = edit(parent, "", 266, 518, 390, 28, false)?;
    let export = button(parent, "Export TSV", 670, 516, 110, 32, false)?;
    let clear = button(parent, "Clear learning", 790, 516, 110, 32, false)?;
    label(
        parent,
        "Clear is sent to the running engine. Direct file replacement is used only when Windows proves the engine pipe is absent.",
        20,
        568,
        880,
        48,
    )?;
    Ok(LearningControls {
        list,
        export_path,
        refresh,
        export,
        clear,
    })
}

fn create_diagnostics_controls(parent: HWND) -> WindowsResult<DiagnosticsControls> {
    label(parent, "Bounded IPC timeout counters", 16, 12, 360, 24)?;
    let text = multiline_readonly(parent, 20, 46, 880, 480)?;
    let refresh = button(parent, "Refresh", 20, 548, 120, 32, false)?;
    let clear = button(parent, "Clear counters", 150, 548, 140, 32, false)?;
    label(
        parent,
        "The diagnostics log is checksummed, capped at 1 MiB, and written only on exceptional timeout paths.",
        318,
        552,
        570,
        36,
    )?;
    Ok(DiagnosticsControls {
        text,
        refresh,
        clear,
    })
}

fn create_update_controls(parent: HWND) -> WindowsResult<UpdateControls> {
    label(parent, "Verified Sakura Input updates", 16, 12, 420, 24)?;
    let enabled = checkbox(
        parent,
        "Automatically check for updates when Settings opens",
        20,
        52,
        430,
        28,
    )?;
    let save = button(parent, "Save preference", 470, 50, 150, 32, false)?;
    let check = button(parent, "Check now", 20, 98, 150, 34, false)?;
    let apply = button(
        parent,
        "Download, verify && install",
        184,
        98,
        230,
        34,
        false,
    )?;
    label(
        parent,
        "Update checks are opt-in. The updater accepts only the canonical GitHub release URL, follows at most five HTTPS redirects to explicit GitHub asset hosts, caps downloads, verifies SHA-256, and requires a trusted Authenticode signature before elevation.",
        20,
        158,
        880,
        72,
    )?;
    let result = multiline_readonly(parent, 20, 248, 880, 250)?;
    label(
        parent,
        "The silent installer reports success, restart-required, still-running timeout, and failure as distinct outcomes. A Windows restart is expected only when the in-process TSF DLL must be replaced.",
        20,
        522,
        880,
        54,
    )?;
    Ok(UpdateControls {
        enabled,
        save,
        check,
        apply,
        result,
    })
}

fn label(parent: HWND, text: &str, x: i32, y: i32, width: i32, height: i32) -> WindowsResult<HWND> {
    control(
        parent,
        windows::core::w!("STATIC"),
        text,
        WS_CHILD | WS_VISIBLE,
        WINDOW_EX_STYLE::default(),
        x,
        y,
        width,
        height,
    )
}

fn button(
    parent: HWND,
    text: &str,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    default: bool,
) -> WindowsResult<HWND> {
    let button_style = if default { BS_DEFPUSHBUTTON } else { 0 };
    control(
        parent,
        windows::core::w!("BUTTON"),
        text,
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(button_style as u32),
        WINDOW_EX_STYLE::default(),
        x,
        y,
        width,
        height,
    )
}

fn checkbox(
    parent: HWND,
    text: &str,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) -> WindowsResult<HWND> {
    control(
        parent,
        windows::core::w!("BUTTON"),
        text,
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_AUTOCHECKBOX as u32),
        WINDOW_EX_STYLE::default(),
        x,
        y,
        width,
        height,
    )
}

fn combo(parent: HWND, x: i32, y: i32, width: i32, height: i32) -> WindowsResult<HWND> {
    control(
        parent,
        windows::core::w!("COMBOBOX"),
        "",
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(CBS_DROPDOWNLIST as u32),
        WS_EX_CLIENTEDGE,
        x,
        y,
        width,
        height,
    )
}

fn edit(
    parent: HWND,
    text: &str,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    readonly: bool,
) -> WindowsResult<HWND> {
    let mut style = WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32);
    if readonly {
        style |= WINDOW_STYLE(ES_READONLY as u32);
    }
    control(
        parent,
        windows::core::w!("EDIT"),
        text,
        style,
        WS_EX_CLIENTEDGE,
        x,
        y,
        width,
        height,
    )
}

fn multiline_readonly(
    parent: HWND,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) -> WindowsResult<HWND> {
    control(
        parent,
        windows::core::w!("EDIT"),
        "",
        WS_CHILD
            | WS_VISIBLE
            | WS_TABSTOP
            | WS_VSCROLL
            | WS_HSCROLL
            | WINDOW_STYLE((ES_MULTILINE | ES_AUTOVSCROLL | ES_WANTRETURN | ES_READONLY) as u32),
        WS_EX_CLIENTEDGE,
        x,
        y,
        width,
        height,
    )
}

fn listbox(parent: HWND, x: i32, y: i32, width: i32, height: i32) -> WindowsResult<HWND> {
    control(
        parent,
        windows::core::w!("LISTBOX"),
        "",
        WS_CHILD
            | WS_VISIBLE
            | WS_TABSTOP
            | WS_VSCROLL
            | WS_HSCROLL
            | WINDOW_STYLE((LBS_NOTIFY | LBS_NOINTEGRALHEIGHT) as u32),
        WS_EX_CLIENTEDGE,
        x,
        y,
        width,
        height,
    )
}

#[allow(clippy::too_many_arguments)]
fn control(
    parent: HWND,
    class_name: PCWSTR,
    text: &str,
    style: WINDOW_STYLE,
    extended_style: WINDOW_EX_STYLE,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) -> WindowsResult<HWND> {
    let text = to_wide(text);
    // SAFETY: class names are static, the title buffer outlives this call, and
    // the returned child is owned by `parent`.
    let window = unsafe {
        CreateWindowExW(
            extended_style,
            class_name,
            PCWSTR(text.as_ptr()),
            style,
            x,
            y,
            width,
            height,
            Some(parent),
            None,
            None,
            None,
        )?
    };
    // SAFETY: DEFAULT_GUI_FONT is a process-lifetime stock object. WM_SETFONT
    // borrows the handle and does not transfer ownership.
    unsafe {
        let font = GetStockObject(DEFAULT_GUI_FONT);
        SendMessageW(
            window,
            WM_SETFONT,
            Some(WPARAM(font.0 as usize)),
            Some(LPARAM(1)),
        );
    }
    Ok(window)
}

fn to_wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(core::iter::once(0)).collect()
}

fn set_text(window: HWND, value: &str) {
    let wide = to_wide(value);
    // SAFETY: `wide` is NUL-terminated and remains alive for the call.
    unsafe {
        let _ = SetWindowTextW(window, PCWSTR(wide.as_ptr()));
    }
}

fn window_text(window: HWND) -> String {
    // SAFETY: `window` is a live control. The allocated buffer is one unit
    // larger than the length returned immediately before the read.
    unsafe {
        let length = GetWindowTextLengthW(window).max(0) as usize;
        let mut buffer = vec![0u16; length.saturating_add(1)];
        let written = GetWindowTextW(window, &mut buffer).max(0) as usize;
        String::from_utf16_lossy(&buffer[..written])
    }
}

fn required_text(window: HWND, label: &str) -> Result<String, String> {
    let value = window_text(window);
    if value.trim().is_empty() {
        Err(format!("{label} is required"))
    } else {
        Ok(value)
    }
}

fn add_combo(window: HWND, value: &str) {
    send_string(window, CB_ADDSTRING, value);
}

fn add_list(window: HWND, value: &str) {
    send_string(window, LB_ADDSTRING, value);
}

fn send_string(window: HWND, message: u32, value: &str) {
    let wide = to_wide(value);
    // SAFETY: the message consumes the NUL-terminated text synchronously.
    unsafe {
        SendMessageW(
            window,
            message,
            Some(WPARAM(0)),
            Some(LPARAM(wide.as_ptr() as isize)),
        );
    }
}

fn select_combo(window: HWND, index: usize) {
    // SAFETY: the combobox validates the requested index itself.
    unsafe {
        SendMessageW(window, CB_SETCURSEL, Some(WPARAM(index)), Some(LPARAM(0)));
    }
}

fn combo_index(window: HWND) -> Option<usize> {
    // SAFETY: this is a scalar query with no pointer arguments.
    let result = unsafe { SendMessageW(window, CB_GETCURSEL, Some(WPARAM(0)), Some(LPARAM(0))).0 };
    usize::try_from(result).ok()
}

fn list_index(window: HWND) -> Option<usize> {
    // SAFETY: this is a scalar query with no pointer arguments.
    let result = unsafe { SendMessageW(window, LB_GETCURSEL, Some(WPARAM(0)), Some(LPARAM(0))).0 };
    usize::try_from(result).ok()
}

fn reset_list(window: HWND) {
    // SAFETY: the control owns the strings it is asked to release.
    unsafe {
        SendMessageW(window, LB_RESETCONTENT, Some(WPARAM(0)), Some(LPARAM(0)));
    }
}

fn set_checked(window: HWND, checked: bool) {
    let state = if checked { BST_CHECKED } else { BST_UNCHECKED };
    // SAFETY: this is the documented scalar BM_SETCHECK payload.
    unsafe {
        SendMessageW(
            window,
            BM_SETCHECK,
            Some(WPARAM(state.0 as usize)),
            Some(LPARAM(0)),
        );
    }
}

fn is_checked(window: HWND) -> bool {
    // SAFETY: this is a scalar query with no pointer arguments.
    unsafe {
        SendMessageW(window, BM_GETCHECK, Some(WPARAM(0)), Some(LPARAM(0))).0
            == BST_CHECKED.0 as isize
    }
}

fn dictionary_format(
    selection: Option<usize>,
    allow_auto: bool,
) -> Result<Option<DictionaryFormat>, String> {
    match selection {
        Some(0) if allow_auto => Ok(None),
        Some(0) => Err("auto-detection is available only for import".to_owned()),
        Some(index) => DictionaryFormat::ALL
            .get(index - 1)
            .copied()
            .map(Some)
            .ok_or_else(|| "selected dictionary format is invalid".to_owned()),
        None => Err("select a dictionary format".to_owned()),
    }
}

fn mode_index(mode: Mode) -> usize {
    Mode::ALL
        .iter()
        .position(|candidate| *candidate == mode)
        .unwrap_or(1)
}

fn mode_from_index(index: Option<usize>) -> Result<Mode, String> {
    index
        .and_then(|index| Mode::ALL.get(index).copied())
        .ok_or_else(|| "select a default input mode".to_owned())
}

fn suggest_index(value: SuggestAccept) -> usize {
    SuggestAccept::ALL
        .iter()
        .position(|candidate| *candidate == value)
        .unwrap_or(0)
}

fn suggest_from_index(index: Option<usize>) -> Result<SuggestAccept, String> {
    index
        .and_then(|index| SuggestAccept::ALL.get(index).copied())
        .ok_or_else(|| "select a suggestion binding".to_owned())
}

fn confirm(message: &str) -> bool {
    message_box(None, message, "Sakura Input", MB_YESNO | MB_ICONWARNING) == IDYES
}

fn message_box(
    parent: Option<HWND>,
    message: &str,
    title: &str,
    style: windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_STYLE,
) -> windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_RESULT {
    let message = to_wide(message);
    let title = to_wide(title);
    // SAFETY: both buffers are NUL-terminated and outlive the synchronous call.
    unsafe {
        MessageBoxW(
            parent,
            PCWSTR(message.as_ptr()),
            PCWSTR(title.as_ptr()),
            style,
        )
    }
}

fn display(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn pump() {
    let mut message = MSG::default();
    // SAFETY: `message` outlives each call. A zero or negative return ends the
    // loop so a failed GetMessage cannot become a busy retry.
    unsafe {
        while GetMessageW(&mut message, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

unsafe extern "system" fn window_procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_UPDATE_COMPLETE => {
            let completion = wparam.0 as *mut UpdateCompletion;
            if completion.is_null() {
                return LRESULT(0);
            }
            // SAFETY: the worker transfers exactly one Box allocation in
            // WPARAM after a successful PostMessageW.
            let completion = unsafe { Box::from_raw(completion) };
            // SAFETY: user data is either zero during construction/destruction
            // or the live Box<App> installed by `run`.
            let pointer = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *mut App;
            if !pointer.is_null() {
                // SAFETY: update completions are handled only on the owning UI
                // thread and the App box outlives the message pump.
                unsafe { &mut *pointer }.finish_update(*completion);
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            // SAFETY: user data is either zero during window construction or
            // the `Box<App>` installed before the window is shown.
            let pointer = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *mut App;
            if !pointer.is_null() {
                let source = HWND(lparam.0 as *mut c_void);
                let notification = ((wparam.0 >> 16) & 0xffff) as u16;
                // SAFETY: only this UI thread handles commands and the box
                // outlives the message pump.
                let app = unsafe { &mut *pointer };
                if let Err(error) = app.handle_command(source, notification) {
                    app.set_status(&format!("Error: {error}"));
                    message_box(
                        Some(window),
                        &error,
                        "Sakura Input settings",
                        MB_OK | MB_ICONERROR,
                    );
                }
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            // SAFETY: user data follows the same lifetime contract as in the
            // command and completion handlers above.
            let pointer = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *mut App;
            let update_in_flight = if pointer.is_null() {
                false
            } else {
                // SAFETY: the non-null pointer is the App installed in this
                // window's GWLP_USERDATA and remains live until WM_NCDESTROY.
                unsafe { &*pointer }.update_in_flight
            };
            if update_in_flight {
                message_box(
                    Some(window),
                    "An update operation is still in progress. Wait for its terminal result before closing Settings.",
                    "Sakura Input update",
                    MB_OK | MB_ICONWARNING,
                );
                return LRESULT(0);
            }
            // SAFETY: this is the live top-level window receiving WM_CLOSE.
            unsafe {
                let _ = DestroyWindow(window);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            // SAFETY: ends the one message pump owned by this thread.
            unsafe {
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        _ => {
            // SAFETY: unhandled messages are delegated to the system exactly
            // once, with the original scalar payloads.
            unsafe { DefWindowProcW(window, message, wparam, lparam) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combo_mappings_cover_every_mode_suggest_binding_and_dictionary_format() {
        for mode in Mode::ALL {
            assert_eq!(mode_from_index(Some(mode_index(mode))), Ok(mode));
        }
        for binding in SuggestAccept::ALL {
            assert_eq!(
                suggest_from_index(Some(suggest_index(binding))),
                Ok(binding)
            );
        }
        for (index, format) in DictionaryFormat::ALL.into_iter().enumerate() {
            assert_eq!(dictionary_format(Some(index + 1), false), Ok(Some(format)));
        }
        assert_eq!(dictionary_format(Some(0), true), Ok(None));
        assert!(dictionary_format(Some(0), false).is_err());
    }
}
