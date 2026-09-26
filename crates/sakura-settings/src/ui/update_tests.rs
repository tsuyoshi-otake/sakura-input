use super::*;

fn version(major: u32, minor: u32, patch: u32) -> updater::Version {
    updater::Version {
        major,
        minor,
        patch,
    }
}

fn available(release_version: updater::Version) -> updater::UpdateCheckOutcome {
    updater::UpdateCheckOutcome::Available(updater::ReleaseManifest {
        trust_epoch: 1,
        release_sequence: 1,
        version: release_version,
        source_commit: "0000000000000000000000000000000000000000".to_owned(),
        installer_url: updater::installer_url_for(release_version),
        sha256: [0x5a; 32],
        size: 1,
        authenticode: updater::AuthenticodePolicy::Required,
        minimum_updater_version: version(1, 0, 0),
        expires_unix: u64::MAX,
    })
}

fn failed() -> updater::UpdateCheckOutcome {
    updater::UpdateCheckOutcome::Failed(updater::UpdateFailure {
        stage: updater::UpdateStage::ManifestDownload,
        message: "network unavailable".to_owned(),
    })
}

#[test]
fn automatic_outcomes_are_display_only_and_never_schedule_follow_up_work() {
    let current = version(2, 0, 0);
    let cases = [
        updater::UpdateCheckOutcome::Disabled,
        updater::UpdateCheckOutcome::UpToDate {
            current,
            latest: current,
        },
        available(version(2, 1, 0)),
        failed(),
    ];

    for outcome in cases {
        let presentation = App::present_update_completion(&UpdateCompletion::Check {
            origin: UpdateOperation::AutomaticCheck,
            outcome,
        });
        assert!(!presentation.message.is_empty());
        assert_eq!(presentation.modal, UpdateModal::None);
    }
}

#[test]
fn manual_check_preserves_confirmation_and_error_feedback() {
    let available = App::present_update_completion(&UpdateCompletion::Check {
        origin: UpdateOperation::ManualCheck,
        outcome: available(version(3, 4, 5)),
    });
    assert_eq!(available.modal, UpdateModal::OfferInstall(version(3, 4, 5)));
    assert!(available.message.contains("3.4.5"));

    let failure = App::present_update_completion(&UpdateCompletion::Check {
        origin: UpdateOperation::ManualCheck,
        outcome: failed(),
    });
    assert_eq!(failure.modal, UpdateModal::Error);
    assert!(failure.message.contains("network unavailable"));
}

#[test]
fn manual_install_reports_failure_without_creating_another_operation() {
    let presentation = App::present_update_completion(&UpdateCompletion::Install(
        updater::UpdateOutcome::Failed {
            version: Some(version(3, 4, 5)),
            failure: updater::UpdateFailure {
                stage: updater::UpdateStage::InstallerLaunch,
                message: "launch rejected".to_owned(),
            },
        },
    ));

    assert_eq!(presentation.modal, UpdateModal::Error);
    assert!(presentation.message.contains("launch rejected"));
}

struct UpdateFixture {
    app: App,
    _desktop: std::sync::MutexGuard<'static, ()>,
}

impl UpdateFixture {
    fn new() -> Self {
        let desktop = native_test_guard();
        register_window_class().unwrap();
        Self {
            app: App::new(create_main_window().unwrap()).unwrap(),
            _desktop: desktop,
        }
    }
}

impl Drop for UpdateFixture {
    fn drop(&mut self) {
        // SAFETY: this test owns these windows; it never starts an update worker.
        unsafe {
            let owner = GetWindow(self.app.window, GW_OWNER).ok();
            let _ = DestroyWindow(self.app.window);
            if let Some(owner) = owner {
                let _ = DestroyWindow(owner);
            }
        }
    }
}

#[test]
fn automatic_completion_keeps_native_edit_focus_values_and_page() {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetFocus, IsWindowEnabled, SetFocus};
    let current = version(2, 0, 0);
    let mut timeout = failed();
    if let updater::UpdateCheckOutcome::Failed(failure) = &mut timeout {
        failure.message = "request timed out".to_owned();
    }
    let cases = [
        failed(),
        timeout,
        available(version(3, 4, 5)),
        updater::UpdateCheckOutcome::UpToDate {
            current,
            latest: current,
        },
        updater::UpdateCheckOutcome::Disabled,
    ];
    let mut fixture = UpdateFixture::new();
    let app = &mut fixture.app;
    app.show_panel(0);
    app.show_topic_controls(INPUT_TOPIC_PROFILE);
    set_text(app.general.profile_process, "pending-profile.exe");
    // SAFETY: the fixture owns this live window and edit on the test UI thread.
    unsafe {
        let _ = ShowWindow(app.window, SW_SHOW);
        let _ = SetFocus(Some(app.general.profile_process));
    }
    for outcome in cases {
        let completion = UpdateCompletion::Check {
            origin: UpdateOperation::AutomaticCheck,
            outcome,
        };
        let presentation = App::present_update_completion(&completion);
        assert_eq!(
            presentation.modal,
            UpdateModal::None,
            "fail before opening any unexpected modal"
        );
        // SAFETY: scalar focus query on this test's UI thread.
        let focused = unsafe { GetFocus() };
        assert_eq!(focused, app.general.profile_process);
        app.update_in_flight = true;
        app.finish_update(completion);
        assert!(!app.update_in_flight);
        assert_eq!(app.selected_panel, 0);
        assert_eq!(
            window_text(app.general.profile_process),
            "pending-profile.exe"
        );
        assert_eq!(
            window_text(app.update_controls.result),
            presentation.message.replace('\n', "\r\n")
        );
        // SAFETY: all HWNDs are still owned by this fixture.
        unsafe {
            assert_eq!(GetFocus(), focused);
            assert!(IsWindowEnabled(app.window).as_bool());
            assert!(IsWindowVisible(app.general.profile_panel).as_bool());
        }
        // Repaint and navigation must not create a new update operation or
        // erase its terminal result.
        app.apply_theme();
        app.show_panel(4);
        app.show_panel(0);
        app.show_topic_controls(INPUT_TOPIC_PROFILE);
        assert!(!app.update_in_flight);
        assert_eq!(
            window_text(app.update_controls.result),
            presentation.message.replace('\n', "\r\n")
        );
        // SAFETY: restore the edit focus on our own live UI thread for the next case.
        unsafe {
            let _ = SetFocus(Some(app.general.profile_process));
        }
    }
}

#[test]
fn repeated_start_requests_cannot_replace_an_in_flight_operation() {
    let mut fixture = UpdateFixture::new();
    fixture.app.update_in_flight = true;
    set_text(fixture.app.update_controls.result, "owned operation");
    for _ in 0..10 {
        for operation in [
            UpdateOperation::AutomaticCheck,
            UpdateOperation::ManualCheck,
            UpdateOperation::ManualInstall,
        ] {
            assert!(fixture.app.start_update(operation).is_err());
            assert!(fixture.app.update_in_flight);
            assert_eq!(
                window_text(fixture.app.update_controls.result),
                "owned operation"
            );
        }
    }
    fixture.app.finish_update(UpdateCompletion::Check {
        origin: UpdateOperation::AutomaticCheck,
        outcome: failed(),
    });
    assert!(!fixture.app.update_in_flight);
    assert_eq!(
        close_decision(CloseRequest::Cancel, fixture.app.update_in_flight),
        CloseDecision::Destroy
    );
}
