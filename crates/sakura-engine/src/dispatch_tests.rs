use super::*;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use crate::input_history::{HistoryScope, InputHistoryRecord, InputHistoryService};
use sakura_core::keymap::{KeyMap, State};
use sakura_core::width::{
    BracketStyle, CommaMark, PeriodMark, PunctuationStyle, Width, WidthPolicy,
};
use sakura_core::UserDictionary;
use sakura_proto::types::CandidatePresentation;
use sakura_proto::{CandidateKind, KeyCode, Modifiers, CANDIDATE_PAGE_SIZE};

#[test]
fn runtime_configuration_changes_the_production_keymap_and_policies() {
    let mut dispatcher = Dispatcher::new().expect("built-in dispatcher");
    let preferences = Preferences {
        keymap_preset: Preset::Atok,
        prediction_enabled: false,
        association_enabled: false,
        neural_reranker_scope: NeuralRerankerScope::AllNormalConversions,
        ..Preferences::default()
    };
    dispatcher
        .apply_runtime_configuration(preferences, Arc::from([]))
        .expect("valid runtime configuration");

    assert_eq!(dispatcher.keymap_preset, Some(Preset::Atok));
    assert_eq!(dispatcher.normalizer, preferences.normalizer);
    assert!(!dispatcher.prediction_enabled);
    assert_eq!(
        dispatcher.neural_reranker_scope,
        NeuralRerankerScope::AllNormalConversions
    );
    assert!(dispatcher.app_profiles.is_empty());
}

#[test]
fn identical_runtime_snapshots_preserve_prediction_navigation_state() {
    let (mut dispatcher, runtime) = prediction_dispatcher();
    let mut out = OutputBuf::new();
    let preferences = Preferences::default();
    let profiles: Arc<[AppProfile]> = Arc::from(default_app_profiles(preferences));
    dispatcher
        .apply_runtime_configuration(preferences, Arc::clone(&profiles))
        .expect("initial runtime snapshot");
    let session = create_session(&mut dispatcher, &mut out, "snapshot-navigation.exe");
    type_word(&mut dispatcher, session, "kana", &mut out);
    assert_eq!(out.candidate_kind(), Some(CandidateKind::Suggestion));

    for expected in [0, 1] {
        let prediction = dispatcher.prediction.clone();
        dispatcher.set_prediction(prediction);
        dispatcher
            .apply_runtime_configuration(preferences, Arc::clone(&profiles))
            .expect("unchanged runtime snapshot");
        dispatcher.dispatch(
            &Request::ProbeKey {
                session,
                scope: InputScope::Normal,
                fresh_context: false,
                key: named_key(KeyCode::Tab),
            },
            &mut out,
        );
        dispatcher
            .apply_runtime_configuration(preferences, Arc::clone(&profiles))
            .expect("scope request runtime snapshot");
        dispatcher.dispatch(
            &Request::SetInputScope {
                session,
                scope: InputScope::Normal,
            },
            &mut out,
        );
        dispatcher
            .apply_runtime_configuration(preferences, Arc::clone(&profiles))
            .expect("real-key runtime snapshot");
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Tab),
            },
            &mut out,
        );
        assert_eq!(out.selected_candidate(), Some(expected));
    }

    runtime.stop().expect("prediction worker joins");
}

#[test]
fn changed_runtime_prediction_policy_invalidates_cached_candidates() {
    let (mut dispatcher, runtime) = prediction_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "snapshot-policy.exe");
    type_word(&mut dispatcher, session, "kana", &mut out);
    let generation = dispatcher
        .sessions
        .get(session)
        .expect("live session")
        .prediction_generation;
    assert!(dispatcher
        .prediction_cache
        .candidates(session, generation)
        .is_some());

    dispatcher
        .apply_runtime_configuration(
            Preferences {
                suggest_accept: SuggestAccept::ShiftEnter,
                ..Preferences::default()
            },
            Arc::from([]),
        )
        .expect("changed runtime snapshot");
    assert!(dispatcher
        .prediction_cache
        .candidates(session, generation)
        .is_none());

    runtime.stop().expect("prediction worker joins");
}

fn builtin_dispatcher() -> Dispatcher {
    Dispatcher::new().expect("the shipped defaults must compile")
}

fn conversion_fixture() -> Arc<ConversionService> {
    let mut source = String::from(
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\nかな\t仮名\t0\t0\t100\t100\tit\tIT用語\nかな\t加奈\t0\t0\t200\t200\t\t人名\n",
        );
    for index in 3..=14 {
        writeln!(
            source,
            "かな\t候補{index:02}\t0\t0\t{}\t{}\t\tfixture",
            index * 100,
            index * 100
        )
        .expect("write fixture entry");
    }
    let entries = dictc::parse_entries("conversion.tsv", &source).expect("entries");
    let matrix = dictc::parse_connection(
        "matrix.tsv",
        "# license: MIT\nclasses\t1\ndefault\t0\n",
        false,
    )
    .expect("matrix");
    let image = Box::leak(
        dictc::compile(&entries, &matrix)
            .expect("image")
            .into_boxed_slice(),
    );
    Arc::new(ConversionService::from_static_bytes(image).expect("conversion service fixture"))
}

fn conversion_dispatcher() -> Dispatcher {
    Dispatcher::new_with_conversion(conversion_fixture()).expect("shipped defaults")
}

fn numeric_focus_conversion_dispatcher() -> Dispatcher {
    let source = concat!(
        "# license: MIT\n",
        "reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n",
        "じょうい\t上位\t0\t0\t100\t100\t\tprimary\n",
        "じょうい\t上位候補二\t0\t0\t200\t200\t\tfixture\n",
        "じょうい\t上位候補三\t0\t0\t300\t300\t\tfixture\n",
        "ちょっきん\t直近\t0\t0\t100\t100\t\tprimary\n",
    );
    let entries = dictc::parse_entries("numeric-focus.tsv", source).expect("entries");
    let matrix = dictc::parse_connection(
        "matrix.tsv",
        "# license: MIT\nclasses\t1\ndefault\t0\n",
        false,
    )
    .expect("matrix");
    let image = Box::leak(
        dictc::compile(&entries, &matrix)
            .expect("image")
            .into_boxed_slice(),
    );
    Dispatcher::new_with_conversion(Arc::new(
        ConversionService::from_static_bytes(image).expect("conversion service fixture"),
    ))
    .expect("shipped defaults")
}

fn visible_projection_conversion_dispatcher() -> Dispatcher {
    let source = concat!(
        "# license: MIT\n",
        "reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n",
        "だいさんばん\t第3番\t0\t0\t100\t100\t\tfirst raw representative\n",
        "だいさんばん\t第３番\t0\t0\t200\t200\t\twidth duplicate\n",
        "だいさんばん\t別候補\t0\t0\t300\t300\t\tsecond visible candidate\n",
    );
    let entries = dictc::parse_entries("visible-projection.tsv", source).expect("entries");
    let matrix = dictc::parse_connection(
        "visible-projection-matrix.tsv",
        "# license: MIT\nclasses\t1\ndefault\t0\n",
        false,
    )
    .expect("matrix");
    let details = [
        dictc::SourceDetail {
            reading: "だいさんばん".into(),
            surface: "第3番".into(),
            left_id: 0,
            right_id: 0,
            description: "Stable first representative detail.".into(),
            relations: vec![],
        },
        dictc::SourceDetail {
            reading: "だいさんばん".into(),
            surface: "別候補".into(),
            left_id: 0,
            right_id: 0,
            description: "Second visible candidate detail.".into(),
            relations: vec![],
        },
    ];
    let image = Box::leak(
        dictc::compile_with_details(&entries, &matrix, &details)
            .expect("image")
            .into_boxed_slice(),
    );
    Dispatcher::new_with_conversion(Arc::new(
        ConversionService::from_static_bytes(image).expect("conversion service fixture"),
    ))
    .expect("shipped defaults")
}

fn raw_repair_conversion_dispatcher() -> Dispatcher {
    let source = concat!(
        "# license: MIT\n",
        "reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n",
        "なぜか\tなぜか\t0\t0\t100\t100\t\tlocal completion\n",
        "ないか\t内科\t0\t0\t120\t120\t\tcompound segment\n",
        "にいく\tに行く\t0\t0\t120\t120\t\tcompound segment\n",
    );
    let entries = dictc::parse_entries("raw-repair-dispatch.tsv", source).expect("entries");
    let matrix = dictc::parse_connection(
        "raw-repair-dispatch-matrix.tsv",
        "# license: MIT\nclasses\t1\ndefault\t0\n",
        false,
    )
    .expect("matrix");
    let image = Box::leak(
        dictc::compile(&entries, &matrix)
            .expect("image")
            .into_boxed_slice(),
    );
    Dispatcher::new_with_conversion(Arc::new(
        ConversionService::from_static_bytes(image).expect("conversion fixture"),
    ))
    .expect("shipped defaults")
}

#[test]
fn phase1_session_and_dispatcher_layout_stay_small_for_worker_stack() {
    let session_bytes = std::mem::size_of::<Session>();
    let dispatcher_bytes = std::mem::size_of::<Dispatcher>();
    println!("Session={session_bytes} Dispatcher={dispatcher_bytes}");
    assert!(
        session_bytes <= 16 * 1024,
        "Session grew to {session_bytes} bytes"
    );
    assert!(
        dispatcher_bytes <= 4 * 1024,
        "Dispatcher grew to {dispatcher_bytes} bytes"
    );
}

#[test]
fn local_raw_completion_candidates_stay_direct_first_and_commit_repair() {
    let mut dispatcher = raw_repair_conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "raw-repair.exe");
    dispatcher.dispatch(
        &Request::SetInputScope {
            session,
            scope: InputScope::Normal,
        },
        &mut out,
    );
    type_word(&mut dispatcher, session, "nazka", &mut out);
    assert_eq!(out.preedit_text(), "なzか");

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    assert_eq!(out.candidate_kind(), Some(CandidateKind::Conversion));
    assert_eq!(out.selected_candidate(), Some(0));
    assert_eq!(out.candidate(0).map(|(text, _)| text), Some("なzか"));
    let live = dispatcher.sessions.get(session).expect("live conversion");
    assert_eq!(
        live.conversion_input_class(),
        ConversionInputClass::MixedUnresolvedLatin
    );
    assert_eq!(live.literal_policy(), LiteralPolicy::ExactOnly);
    assert!((0..out.candidate_count())
        .filter_map(|index| out.candidate(index).map(|(text, _)| text))
        .any(|text| text == "なぜか"));

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Down),
        },
        &mut out,
    );
    assert_eq!(out.selected_candidate(), Some(1));
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Enter),
        },
        &mut out,
    );
    assert_eq!(out.commit_text(), Some("なぜか"));
    assert!(dispatcher
        .sessions
        .get(session)
        .expect("session")
        .cached_surface_fingerprint("なzか")
        .is_none());

    type_word(&mut dispatcher, session, "naikniiku", &mut out);
    assert_eq!(out.preedit_text(), "ないkにいく");
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    assert_eq!(out.candidate_kind(), Some(CandidateKind::Conversion));
    assert_eq!(out.candidate(0).map(|(text, _)| text), Some("ないkにいく"));
    assert!((0..out.candidate_count())
        .filter_map(|index| out.candidate(index).map(|(text, _)| text))
        .any(|text| text.contains("内科")));
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Down),
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Enter),
        },
        &mut out,
    );
    assert_eq!(out.commit_text(), Some("内科に行く"));
}

#[test]
fn identical_runtime_snapshot_before_each_key_preserves_raw_admission() {
    let mut dispatcher = raw_repair_conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "raw-repair-runtime.exe");
    let preferences = Preferences::default();
    let profiles = Arc::<[AppProfile]>::from([]);
    dispatcher.dispatch(
        &Request::SetInputScope {
            session,
            scope: InputScope::Normal,
        },
        &mut out,
    );

    for character in "nazka".chars() {
        dispatcher
            .apply_runtime_configuration(preferences, Arc::clone(&profiles))
            .expect("identical runtime snapshot");
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key(character),
            },
            &mut out,
        );
    }
    assert_eq!(
        dispatcher
            .sessions
            .get(session)
            .expect("live session")
            .raw_provenance(),
        RawProvenanceState::AppendOnly
    );
    dispatcher
        .apply_runtime_configuration(preferences, Arc::clone(&profiles))
        .expect("identical runtime snapshot before conversion");
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    assert!((0..out.candidate_count())
        .filter_map(|index| out.candidate(index).map(|(text, _)| text))
        .any(|text| text == "なぜか"));
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Down),
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Enter),
        },
        &mut out,
    );

    for character in "naikniiku".chars() {
        dispatcher
            .apply_runtime_configuration(preferences, Arc::clone(&profiles))
            .expect("identical runtime snapshot");
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key(character),
            },
            &mut out,
        );
    }
    dispatcher
        .apply_runtime_configuration(preferences, Arc::clone(&profiles))
        .expect("identical runtime snapshot before conversion");
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    assert!((0..out.candidate_count())
        .filter_map(|index| out.candidate(index).map(|(text, _)| text))
        .any(|text| text.contains("内科")));
}

#[test]
fn changed_runtime_input_support_suppresses_raw_repair() {
    let mut dispatcher = raw_repair_conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "raw-repair-runtime-change.exe");
    let preferences = Preferences::default();
    let profiles = Arc::<[AppProfile]>::from([]);
    dispatcher.dispatch(
        &Request::SetInputScope {
            session,
            scope: InputScope::Normal,
        },
        &mut out,
    );
    type_word(&mut dispatcher, session, "nazka", &mut out);
    assert_eq!(
        dispatcher
            .sessions
            .get(session)
            .expect("live session")
            .raw_provenance(),
        RawProvenanceState::AppendOnly
    );

    let changed = Preferences {
        input_support: sakura_core::InputSupport {
            enabled: false,
            ..preferences.input_support
        },
        ..preferences
    };
    dispatcher
        .apply_runtime_configuration(changed, profiles)
        .expect("changed runtime snapshot");
    assert_eq!(
        dispatcher
            .sessions
            .get(session)
            .expect("live session")
            .raw_provenance(),
        RawProvenanceState::Suppressed
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    assert!((0..out.candidate_count())
        .filter_map(|index| out.candidate(index).map(|(text, _)| text))
        .all(|text| text != "なぜか"));
}

#[test]
fn short_reading_history_does_not_change_candidate_identity_or_auto_commit() {
    let conversion = prediction_conversion_from_source(
            "short-reading-history.tsv",
            concat!(
                "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n",
                "う\t宇\t0\t0\t100\t-\t\texact\n",
                "う\t羽\t0\t0\t200\t-\t\texact\n",
                "い\tい\t0\t0\t10\t-\t\tother reading\n",
            ),
        );
    for scope in [
        NeuralRerankerScope::Off,
        NeuralRerankerScope::LongTextOnly,
        NeuralRerankerScope::AllNormalConversions,
    ] {
        let mut baseline = None;
        for learned in [false, true] {
            let learning = Arc::new(LearningService::memory());
            if learned {
                learning.learn("い", "い", 0, 0);
            }
            let signature = with_session_candidates(
                &conversion,
                Some(&learning),
                "う",
                ConversionOptions::default(),
                |candidates| {
                    candidates
                        .iter()
                        .map(|candidate| {
                            (
                                candidate.text().to_owned(),
                                candidate.cost,
                                candidate.path_evidence(),
                            )
                        })
                        .collect::<Vec<_>>()
                },
            )
            .expect("bounded candidate construction");
            let preferences = Preferences {
                neural_reranker_scope: scope,
                prediction_enabled: false,
                ..Preferences::default()
            };
            let mut dispatcher =
                Dispatcher::new_with_configuration(Arc::clone(&conversion), learning, preferences)
                    .expect("dispatcher");
            let mut out = OutputBuf::new();
            let session = create_session(&mut dispatcher, &mut out, "synthetic-quality.exe");
            dispatcher.dispatch(
                &Request::SetInputScope {
                    session,
                    scope: InputScope::Normal,
                },
                &mut out,
            );
            type_word(&mut dispatcher, session, "u", &mut out);
            dispatcher.dispatch(
                &Request::SendKey {
                    session,
                    key: named_key(KeyCode::Space),
                },
                &mut out,
            );
            assert_eq!(
                out.preedit_text(),
                "宇",
                "scope={scope:?} learned={learned}"
            );
            dispatcher.dispatch(
                &Request::SendKey {
                    session,
                    key: named_key(KeyCode::Enter),
                },
                &mut out,
            );
            assert_eq!(out.commit_text(), Some("宇"));
            if let Some(expected) = baseline.as_ref() {
                assert_eq!(
                    &signature, expected,
                    "prior い commit changed the identity/order for う"
                );
            } else {
                baseline = Some(signature);
            }
        }
    }
}

#[test]
fn ranked_commit_repair_hints_survive_alongside_raw_plans() {
    let dispatcher = raw_repair_conversion_dispatcher();
    let learning = LearningService::memory();
    learning.learn("なぜか", "なぜか", 0, 0);
    let hints =
        learning.collect_commit_repair_readings("なせか", sakura_core::InputSupport::default());
    assert!(hints.iter().any(|hint| hint.as_str() == "なぜか"));

    let original = "なせか";
    let corrected = "なぜか";
    let runs = build_correction_runs(original, corrected).expect("replacement map runs");
    let map = CorrectionMap::new(original, corrected, &runs).expect("replacement map");
    let plan = RawRepairPlan::new(0, corrected, map, RepairTier::LocalCompletion)
        .expect("raw repair plan");
    let options = ConversionOptions::default();
    let result = with_session_raw_conversion(
        dispatcher
            .conversion
            .as_deref()
            .expect("conversion service"),
        Some(&learning),
        ConversionInput::ordinary(original),
        &[plan],
        options,
        None,
        |candidates, diagnostics| {
            assert!(candidates.iter().any(|candidate| {
                candidate.origin() == CandidateOrigin::Direct && candidate.text() == "なぜか"
            }));
            assert_eq!(diagnostics.raw_repair_passes, 1);
        },
    );
    assert!(result.is_ok());
}

#[test]
fn caret_and_backspace_edits_suppress_raw_repair_admission() {
    let mut dispatcher = raw_repair_conversion_dispatcher();
    let mut out = OutputBuf::new();
    let caret_session = create_session(&mut dispatcher, &mut out, "raw-repair-caret.exe");
    dispatcher.dispatch(
        &Request::SetInputScope {
            session: caret_session,
            scope: InputScope::Normal,
        },
        &mut out,
    );
    type_word(&mut dispatcher, caret_session, "nazka", &mut out);
    assert_eq!(
        dispatcher
            .sessions
            .get(caret_session)
            .expect("caret session")
            .raw_provenance(),
        RawProvenanceState::AppendOnly
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session: caret_session,
            key: named_key(KeyCode::Left),
        },
        &mut out,
    );
    assert_eq!(
        dispatcher
            .sessions
            .get(caret_session)
            .expect("caret session")
            .raw_provenance(),
        RawProvenanceState::Suppressed
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session: caret_session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    assert!(
        (0..out.candidate_count())
            .filter_map(|index| out.candidate(index).map(|(text, _)| text))
            .all(|text| text != "なぜか"),
        "caret movement must not revive a local completion"
    );

    let backspace_session = create_session(&mut dispatcher, &mut out, "raw-repair-backspace.exe");
    dispatcher.dispatch(
        &Request::SetInputScope {
            session: backspace_session,
            scope: InputScope::Normal,
        },
        &mut out,
    );
    type_word(&mut dispatcher, backspace_session, "nazka", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session: backspace_session,
            key: named_key(KeyCode::Backspace),
        },
        &mut out,
    );
    assert_eq!(
        dispatcher
            .sessions
            .get(backspace_session)
            .expect("backspace session")
            .raw_provenance(),
        RawProvenanceState::Suppressed
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session: backspace_session,
            key: char_key('a'),
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session: backspace_session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    assert!(
        (0..out.candidate_count())
            .filter_map(|index| out.candidate(index).map(|(text, _)| text))
            .all(|text| text != "なぜか"),
        "backspace/carry edits must remain direct-only"
    );
}

#[test]
fn reconversion_and_segment_resize_suppress_raw_repair_state() {
    let mut dispatcher = raw_repair_conversion_dispatcher();
    let mut out = OutputBuf::new();
    let reconvert_session = create_session(&mut dispatcher, &mut out, "raw-repair-reconvert.exe");
    dispatcher.dispatch(
        &Request::SetInputScope {
            session: reconvert_session,
            scope: InputScope::Normal,
        },
        &mut out,
    );
    type_word(&mut dispatcher, reconvert_session, "nazka", &mut out);
    assert_eq!(
        dispatcher
            .sessions
            .get(reconvert_session)
            .expect("reconversion session")
            .raw_provenance(),
        RawProvenanceState::AppendOnly
    );
    assert_eq!(
        dispatcher.dispatch(
            &Request::Reconvert {
                session: reconvert_session,
                text: "なぜか".to_owned(),
                preview: false,
            },
            &mut out,
        ),
        Reply::Output
    );
    assert_eq!(
        dispatcher
            .sessions
            .get(reconvert_session)
            .expect("reconversion session")
            .raw_provenance(),
        RawProvenanceState::Suppressed
    );

    let mut segmented = segmented_conversion_dispatcher();
    let segment_session = create_session(&mut segmented, &mut out, "raw-repair-segment.exe");
    type_word(&mut segmented, segment_session, "kyoudesu", &mut out);
    segmented.dispatch(
        &Request::SendKey {
            session: segment_session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    let selected = segmented
        .sessions
        .get_mut(segment_session)
        .expect("selected segment");
    assert!(selected.segment_count() >= 2);
    assert!(selected.stage_selected_raw_repair(0, "きょうです", &[9, 15]));
    selected.mark_append_only_raw_feed(false);
    assert!(selected.has_selected_raw_repair());
    assert_eq!(selected.raw_provenance(), RawProvenanceState::AppendOnly);
    segmented.dispatch(
        &Request::SendKey {
            session: segment_session,
            key: modified_named_key(KeyCode::Left, Modifiers::SHIFT),
        },
        &mut out,
    );
    let live = segmented
        .sessions
        .get(segment_session)
        .expect("segment session");
    assert_eq!(live.raw_provenance(), RawProvenanceState::Suppressed);
    assert!(!live.has_selected_raw_repair());
}

#[test]
fn probe_planning_is_pure_and_matches_apply_raw_repair_consumption() {
    let mut dispatcher = raw_repair_conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "raw-repair-probe.exe");
    dispatcher.dispatch(
        &Request::SetInputScope {
            session,
            scope: InputScope::Normal,
        },
        &mut out,
    );
    type_word(&mut dispatcher, session, "nazka", &mut out);
    let before = dispatcher
        .sessions
        .get(session)
        .expect("probe session")
        .clone();
    let expected_plans = build_local_completion_plans(&before, &dispatcher.table);
    assert!(!expected_plans.is_empty());
    dispatcher.dispatch(
        &Request::ProbeKey {
            session,
            scope: InputScope::Normal,
            fresh_context: false,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    let after_probe = dispatcher.sessions.get(session).expect("probe session");
    assert_eq!(after_probe.raw_input, before.raw_input);
    assert_eq!(after_probe.preedit, before.preedit);
    assert_eq!(after_probe.raw_provenance(), RawProvenanceState::AppendOnly);
    assert_eq!(
        build_local_completion_plans(after_probe, &dispatcher.table),
        expected_plans,
        "Probe must use the same pure plan as Apply"
    );

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    assert!((0..out.candidate_count())
        .filter_map(|index| out.candidate(index).map(|(text, _)| text))
        .any(|text| text == "なぜか"));
}

#[test]
fn raw_repair_mapping_failure_rejects_only_the_repair_candidate() {
    let dispatcher = raw_repair_conversion_dispatcher();
    let original = "なzか";
    let corrected = "なぜか";
    let runs = build_correction_runs(original, corrected).expect("replacement map runs");
    let map = CorrectionMap::new(original, corrected, &runs).expect("replacement map");
    let plan = RawRepairPlan::new(0, corrected, map, RepairTier::LocalCompletion)
        .expect("raw repair plan");
    let mut direct = None;
    let mut repaired = None;
    with_session_raw_conversion(
        dispatcher
            .conversion
            .as_deref()
            .expect("conversion service"),
        None,
        ConversionInput::ordinary(original),
        std::slice::from_ref(&plan),
        ConversionOptions::default(),
        None,
        |candidates, _| {
            direct = candidates
                .iter()
                .find(|candidate| candidate.origin() == CandidateOrigin::Direct)
                .cloned();
            repaired = candidates
                .iter()
                .find(|candidate| candidate.origin() != CandidateOrigin::Direct)
                .cloned();
        },
    )
    .expect("raw conversion");
    let direct = direct.expect("direct fallback");
    let repaired = repaired.expect("repaired candidate");
    assert!(raw_candidate_mapping_is_valid(&[plan], original, &repaired));
    assert!(!raw_candidate_mapping_is_valid(&[], original, &repaired));
    assert!(raw_candidate_mapping_is_valid(&[], original, &direct));
}

#[test]
fn selected_raw_repair_uses_original_reading_for_f9_and_f10() {
    let mut dispatcher = raw_repair_conversion_dispatcher();
    let mut out = OutputBuf::new();
    for (name, key, expected) in [
        ("raw-repair-f10.exe", KeyCode::F10, "nazka"),
        ("raw-repair-f9.exe", KeyCode::F9, "ｎａｚｋａ"),
    ] {
        let session = create_session(&mut dispatcher, &mut out, name);
        dispatcher.dispatch(
            &Request::SetInputScope {
                session,
                scope: InputScope::Normal,
            },
            &mut out,
        );
        type_word(&mut dispatcher, session, "nazka", &mut out);
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Space),
            },
            &mut out,
        );
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Down),
            },
            &mut out,
        );
        assert!(dispatcher
            .sessions
            .get(session)
            .expect("selected raw repair")
            .has_selected_raw_repair());
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(key),
            },
            &mut out,
        );
        assert_eq!(
            out.preedit_text(),
            expected,
            "{name} must use original raw input"
        );
    }
}

#[test]
fn explicit_raw_repair_commit_does_not_advance_learning_generation() {
    let learning = Arc::new(LearningService::memory());
    let mut dispatcher = raw_repair_conversion_dispatcher();
    dispatcher.observed_learning_generation = learning.generation();
    dispatcher.learning = Some(Arc::clone(&learning));
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "raw-repair-learning.exe");
    dispatcher.dispatch(
        &Request::SetInputScope {
            session,
            scope: InputScope::Normal,
        },
        &mut out,
    );
    type_word(&mut dispatcher, session, "nazka", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Down),
        },
        &mut out,
    );
    let generation_before = learning.generation();
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Enter),
        },
        &mut out,
    );
    assert_eq!(out.commit_text(), Some("なぜか"));
    assert_eq!(learning.generation(), generation_before);
    assert!(dispatcher
        .sessions
        .get(session)
        .expect("raw repair session")
        .cached_surface_fingerprint("なzか")
        .is_none());
}

/// Issue #99.  Typing one comma and converting has to offer all four marks
/// as four visibly different rows, with the configured one first.
///
/// This has to be an end-to-end test rather than a converter-level one.
/// The four candidates differ only in a character the width choke point
/// claims, so a list that is correct inside the converter still renders as
/// the same glyph four times unless `append_candidate_surface` lets the
/// `synthetic_exact` rows past `normalize_into`.  Only a dispatch-level
/// test sees the strings the reader sees.
#[test]
fn a_converted_comma_offers_every_mark_and_commits_the_chosen_one_verbatim() {
    let mut dispatcher = conversion_dispatcher();
    let normalizer = Normalizer {
        punctuation: PunctuationStyle::new(CommaMark::FullWidth, PeriodMark::FullWidth),
        ..Normalizer::default()
    };
    dispatcher
        .apply_runtime_configuration(
            Preferences {
                normalizer,
                ..Preferences::default()
            },
            Arc::from([]),
        )
        .expect("valid runtime configuration");
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "punctuation-family.exe");
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: char_key(','),
        },
        &mut out,
    );
    // The configured mark is what the preedit already showed, so replacing
    // TOP-1 with it changes nothing on screen.
    assert_eq!(out.preedit_text(), "\u{FF0C}");
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Henkan),
        },
        &mut out,
    );
    let offered = (0..4)
        .map(|index| {
            out.candidate(index)
                .map(|(text, annotation)| (text.to_owned(), annotation.to_owned()))
                .unwrap_or_else(|| panic!("candidate {index} is missing"))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        offered,
        vec![
            ("\u{FF0C}".to_owned(), "全角コンマ".to_owned()),
            ("\u{3001}".to_owned(), "全角読点".to_owned()),
            ("\u{FF64}".to_owned(), "半角読点".to_owned()),
            (",".to_owned(), "半角コンマ".to_owned()),
        ],
        "the four marks have to reach the page as four different characters"
    );
    assert_eq!(out.selected_candidate(), Some(0));

    // Committing the second row must put a touten in the document even
    // though the reader configured the full-width comma.  Re-styling here
    // is exactly the bug: it would make the extra rows unreachable.
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Henkan),
        },
        &mut out,
    );
    assert_eq!(out.selected_candidate(), Some(1));
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Enter),
        },
        &mut out,
    );
    assert_eq!(out.commit_text(), Some("\u{3001}"));
}

/// The period role is configured separately, so it has to be reachable
/// separately.  A reader on the technical-paper style gets ASCII first and
/// still reaches the kuten.
#[test]
fn a_converted_period_follows_its_own_role_setting() {
    let mut dispatcher = conversion_dispatcher();
    let normalizer = Normalizer {
        punctuation: PunctuationStyle::new(CommaMark::Touten, PeriodMark::HalfWidth),
        ..Normalizer::default()
    };
    dispatcher
        .apply_runtime_configuration(
            Preferences {
                normalizer,
                ..Preferences::default()
            },
            Arc::from([]),
        )
        .expect("valid runtime configuration");
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "punctuation-period.exe");
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: char_key('.'),
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Henkan),
        },
        &mut out,
    );
    let offered = (0..4)
        .filter_map(|index| out.candidate(index).map(|(text, _)| text.to_owned()))
        .collect::<Vec<_>>();
    assert_eq!(
        offered,
        vec![
            ".".to_owned(),
            "\u{3002}".to_owned(),
            "\u{FF61}".to_owned(),
            "\u{FF0E}".to_owned(),
        ],
        "the period role must order its own family, not the comma one"
    );
}

/// Issue #99.  The shipped style is the one where the configured mark is
/// also the reading, and that is exactly the case the ranker moves off:
/// `preferred_candidate_index` refuses a row whose text equals the reading,
/// so a reader on 、 lands on ､ instead.  Verified on the installed 1.0.28
/// build before this test existed -- typing one comma and converting
/// selected the third row.  The setting has to win, or the feature reads as
/// the bug it was meant to fix.
#[test]
fn the_shipped_punctuation_style_stays_selected_when_it_equals_the_reading() {
    let mut dispatcher = conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "punctuation-default.exe");
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: char_key(','),
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Henkan),
        },
        &mut out,
    );
    let offered = (0..4)
        .filter_map(|index| out.candidate(index).map(|(text, _)| text.to_owned()))
        .collect::<Vec<_>>();
    assert_eq!(
        offered,
        vec![
            "\u{3001}".to_owned(),
            "\u{FF64}".to_owned(),
            "\u{FF0C}".to_owned(),
            ",".to_owned(),
        ],
        "the shipped style orders the comma family touten first"
    );
    assert_eq!(
        out.selected_candidate(),
        Some(0),
        "the configured mark must be the default even though it is the reading"
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Enter),
        },
        &mut out,
    );
    assert_eq!(out.commit_text(), Some("\u{3001}"));
}

/// A style the reader changed leaves learned surfaces behind: every commit
/// made while ，． was configured taught the ranker that the touten reading
/// resolves to ，.  The installed build had 150 such rows and converted one
/// comma straight to ，, with the settings window still saying 、.  These
/// candidates are kept out of learning on the way in, so a stale entry must
/// not steer them on the way out either.
#[test]
fn a_stale_learned_mark_cannot_override_the_configured_punctuation() {
    let learning = Arc::new(LearningService::memory());
    // Four commits, because one is only weak evidence and weak evidence
    // cannot reach past the second row.  The installed build had 150.
    for _ in 0..4 {
        learning.learn("\u{3001}", "\u{FF0C}", 0, 0);
        learning.learn("\u{3002}", "\u{FF0E}", 0, 0);
    }
    let mut dispatcher = conversion_dispatcher();
    dispatcher.observed_learning_generation = learning.generation();
    dispatcher.learning = Some(Arc::clone(&learning));
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "punctuation-stale.exe");
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: char_key(','),
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Henkan),
        },
        &mut out,
    );
    assert_eq!(
        out.selected_candidate(),
        Some(0),
        "a learned surface must not outrank the configured mark"
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Enter),
        },
        &mut out,
    );
    assert_eq!(out.commit_text(), Some("\u{3001}"));
    // The other three rows stay reachable: this pins the default, not the
    // list.
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: char_key(','),
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Henkan),
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Henkan),
        },
        &mut out,
    );
    assert_eq!(out.selected_candidate(), Some(1));
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Enter),
        },
        &mut out,
    );
    assert_eq!(out.commit_text(), Some("\u{FF64}"));
}

#[test]
fn opaque_shift_ascii_exact_top1_keeps_literal_zero_and_never_learns() {
    let learning = Arc::new(LearningService::memory());
    let mut dispatcher = shifted_ascii_english_conversion_dispatcher();
    dispatcher.observed_learning_generation = learning.generation();
    dispatcher.learning = Some(Arc::clone(&learning));
    let mut out = OutputBuf::new();
    let commit_session = create_session(&mut dispatcher, &mut out, "exact-top1-commit.exe");
    for character in "ESP32".chars() {
        dispatcher.dispatch(
            &Request::SendKey {
                session: commit_session,
                key: shifted_char_key(character),
            },
            &mut out,
        );
    }
    dispatcher.dispatch(
        &Request::SendKey {
            session: commit_session,
            key: named_key(KeyCode::Henkan),
        },
        &mut out,
    );
    assert_eq!(out.selected_candidate(), Some(0));
    assert_eq!(out.candidate(0).map(|(text, _)| text), Some("ESP32"));
    let generation_before = learning.generation();
    dispatcher.dispatch(
        &Request::SendKey {
            session: commit_session,
            key: named_key(KeyCode::Enter),
        },
        &mut out,
    );
    assert_eq!(out.commit_text(), Some("ESP32"));
    assert_eq!(learning.generation(), generation_before);
    assert!(dispatcher
        .sessions
        .get(commit_session)
        .expect("exact session")
        .cached_surface_fingerprint("ESP32")
        .is_none());

    let cancel_session = create_session(&mut dispatcher, &mut out, "exact-top1-cancel.exe");
    for character in "ESP32".chars() {
        dispatcher.dispatch(
            &Request::SendKey {
                session: cancel_session,
                key: shifted_char_key(character),
            },
            &mut out,
        );
    }
    dispatcher.dispatch(
        &Request::SendKey {
            session: cancel_session,
            key: named_key(KeyCode::Henkan),
        },
        &mut out,
    );
    assert_eq!(out.candidate(0).map(|(text, _)| text), Some("ESP32"));
    dispatcher.dispatch(
        &Request::SendKey {
            session: cancel_session,
            key: named_key(KeyCode::Escape),
        },
        &mut out,
    );
    assert_eq!(out.preedit_text(), "ESP32");
    assert_eq!(learning.generation(), generation_before);
}

#[test]
fn local_raw_completion_rejects_normal_or_edited_inputs() {
    let mut dispatcher = raw_repair_conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "raw-repair-negative.exe");
    dispatcher.dispatch(
        &Request::SetInputScope {
            session,
            scope: InputScope::Normal,
        },
        &mut out,
    );
    for typed in ["nazeka", "naikaniiku", "naeka", "nazea"] {
        type_word(&mut dispatcher, session, typed, &mut out);
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Space),
            },
            &mut out,
        );
        assert!(
            (0..out.candidate_count())
                .filter_map(|index| out.candidate(index).map(|(text, _)| text))
                .all(|text| !text.contains("内科")),
            "unexpected repair for {typed}"
        );
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Escape),
            },
            &mut out,
        );
    }

    let session_state = dispatcher.sessions.get(session).expect("session");
    assert_eq!(session_state.raw_provenance(), RawProvenanceState::Unset);
}

#[test]
fn raw_repair_escape_restores_the_original_observed_reading() {
    let mut dispatcher = raw_repair_conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "raw-repair-escape.exe");
    dispatcher.dispatch(
        &Request::SetInputScope {
            session,
            scope: InputScope::Normal,
        },
        &mut out,
    );
    type_word(&mut dispatcher, session, "nazka", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    assert!((0..out.candidate_count())
        .filter_map(|index| out.candidate(index).map(|(text, _)| text))
        .any(|text| text == "なぜか"));
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Escape),
        },
        &mut out,
    );
    assert_eq!(out.preedit_text(), "なzか");
    let live = dispatcher.sessions.get(session).expect("session");
    assert_eq!(live.raw_provenance(), RawProvenanceState::Unset);
}

#[test]
fn direct_kana_input_never_enters_raw_repair_admission() {
    let mut dispatcher = raw_repair_conversion_dispatcher();
    dispatcher
        .apply_runtime_configuration(
            Preferences {
                input_method: InputMethod::Kana,
                ..Preferences::default()
            },
            Arc::from([]),
        )
        .expect("kana configuration");
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "raw-repair-kana.exe");
    dispatcher.dispatch(
        &Request::SetInputScope {
            session,
            scope: InputScope::Normal,
        },
        &mut out,
    );
    for character in ['な', 'z', 'か'] {
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key(character),
            },
            &mut out,
        );
    }
    let live = dispatcher.sessions.get(session).expect("session");
    assert_eq!(live.raw_provenance(), RawProvenanceState::Suppressed);
}

#[test]
fn selected_raw_repair_uses_corrected_reading_for_kana_transform() {
    let mut dispatcher = raw_repair_conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "raw-repair-transform.exe");
    dispatcher.dispatch(
        &Request::SetInputScope {
            session,
            scope: InputScope::Normal,
        },
        &mut out,
    );
    type_word(&mut dispatcher, session, "nazka", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Down),
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::F7),
        },
        &mut out,
    );
    assert_eq!(out.preedit_text(), "ナゼカ");
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Enter),
        },
        &mut out,
    );
    assert_eq!(out.commit_text(), Some("ナゼカ"));

    type_word(&mut dispatcher, session, "naikniiku", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Down),
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::F7),
        },
        &mut out,
    );
    assert_eq!(out.preedit_text(), "ナイカニイク");
}

fn detail_conversion_fixture() -> Arc<ConversionService> {
    let source = concat!(
        "# license: MIT\n",
        "reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n",
        "kana\tKana\t0\t0\t100\t100\t\tfixture\n",
        "a\tA\t0\t0\t100\t100\t\tfixture\n",
        "b\tB\t0\t0\t100\t100\t\tfixture\n",
    );
    let entries = dictc::parse_entries("details-conversion.tsv", source).expect("entries");
    let matrix = dictc::parse_connection(
        "details-conversion-matrix.tsv",
        "# license: MIT\nclasses\t1\ndefault\t0\n",
        false,
    )
    .expect("matrix");
    let image = Box::leak(
        dictc::compile_with_details(
            &entries,
            &matrix,
            &[dictc::SourceDetail {
                reading: "kana".into(),
                surface: "Kana".into(),
                left_id: 0,
                right_id: 0,
                description: "A source-backed test definition.".into(),
                relations: vec![],
            }],
        )
        .expect("detail image")
        .into_boxed_slice(),
    );
    Arc::new(ConversionService::from_static_bytes(image).expect("detail conversion service"))
}

#[test]
fn only_one_exact_system_edge_publishes_its_source_backed_detail() {
    let conversion = detail_conversion_fixture();
    conversion
        .with_candidates("kana", ConversionOptions::default(), |candidates| {
            let candidate = candidates
                .iter()
                .find(|candidate| candidate.text() == "Kana")
                .expect("exact dictionary candidate");
            let mut output = OutputBuf::new();
            output.begin_candidates(0, 9).expect("candidate list");
            output
                .push_candidate(candidate.text(), candidate.annotation())
                .expect("candidate");
            publish_system_candidate_detail(
                conversion.as_ref(),
                candidate.system_entry_index().expect("exact entry ordinal"),
                "kana",
                &mut output,
            );
            let detail = output.to_output().candidate_detail.expect("exact detail");
            assert_eq!(detail.reading, "kana");
            assert_eq!(detail.definition, "A source-backed test definition.");
        })
        .expect("conversion");

    conversion
        .with_candidates("ab", ConversionOptions::default(), |candidates| {
            assert!(
                candidates
                    .iter()
                    .all(|candidate| candidate.system_entry_index().is_none()),
                "a Latin run with no exact whole-token entry must not claim a system ordinal"
            );
        })
        .expect("compound conversion");
}

fn shifted_ascii_english_conversion_dispatcher() -> Dispatcher {
    let source = concat!(
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n",
            "\u{3042}\u{3044}\u{3042}\u{3080}\tIAM\t0\t0\t100\t100\t\tfixture\n",
            "claude\tClaude\t0\t0\t100\t100\tit\tfixture\n",
            "claude\tClaude Code\t0\t0\t150\t150\tit\tfixture\n",
            "openai\tOpenAI\t0\t0\t100\t100\tit\tfixture\n",
        );
    let mut entries = dictc::parse_entries("shifted-ascii.tsv", source).expect("entries");
    let mut curated = dictc::parse_entries(
        "data/curated-terms.tsv",
        include_str!("../../../data/curated-terms.tsv"),
    )
    .expect("curated Shift terms");
    for entry in &mut curated {
        entry.left_id = 0;
        entry.right_id = 0;
    }
    entries.extend(curated);
    let matrix = dictc::parse_connection(
        "matrix.tsv",
        "# license: MIT\nclasses\t1\ndefault\t0\n",
        false,
    )
    .expect("matrix");
    let image = Box::leak(
        dictc::compile(&entries, &matrix)
            .expect("image")
            .into_boxed_slice(),
    );
    let conversion = Arc::new(
        ConversionService::from_static_bytes(image).expect("shifted ASCII conversion fixture"),
    );
    Dispatcher::new_with_conversion(conversion).expect("shipped defaults")
}

fn prediction_dispatcher() -> (Dispatcher, crate::prediction::PredictionRuntime) {
    let source = "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\nかな\t仮名\t0\t1\t100\t100\tpredict\tcommon\nかなた\t彼方\t0\t2\t200\t200\tpredict\tdirection\nかながわ\t神奈川\t0\t3\t300\t300\tpredict\tprefecture\n";
    let entries = dictc::parse_entries("prediction.tsv", source).expect("entries");
    let matrix = dictc::parse_connection(
        "matrix.tsv",
        "# license: MIT\nclasses\t4\ndefault\t0\n",
        false,
    )
    .expect("matrix");
    let image = Box::leak(
        dictc::compile(&entries, &matrix)
            .expect("image")
            .into_boxed_slice(),
    );
    let conversion = Arc::new(
        ConversionService::from_static_bytes(image).expect("prediction conversion fixture"),
    );
    let learning = Arc::new(LearningService::memory());
    let runtime = crate::prediction::PredictionRuntime::start(Arc::clone(&conversion))
        .expect("prediction runtime");
    let dispatcher = Dispatcher::new_with_runtime_configuration(
        conversion,
        learning,
        runtime.service(),
        Preferences::default(),
    )
    .expect("shipped defaults");
    (dispatcher, runtime)
}

fn phase_one_prediction_conversion() -> Arc<ConversionService> {
    let source = concat!(
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n",
            "\u{304b}\u{306a}\tsystem-first\t0\t0\t100\t100\tpredict\tsystem\n",
            "\u{304b}\u{306a}\u{306b}\tsystem-second\t0\t0\t200\t200\tpredict\tsystem\n",
        );
    prediction_conversion_from_source("phase-one-prediction.tsv", source)
}

fn prediction_conversion_from_source(file_name: &str, source: &str) -> Arc<ConversionService> {
    let entries = dictc::parse_entries(file_name, source).expect("entries");
    let matrix = dictc::parse_connection(
        "matrix.tsv",
        "# license: MIT\nclasses\t1\ndefault\t0\n",
        false,
    )
    .expect("matrix");
    let image = Box::leak(
        dictc::compile(&entries, &matrix)
            .expect("image")
            .into_boxed_slice(),
    );
    Arc::new(ConversionService::from_static_bytes(image).expect("conversion service"))
}

fn equal_reading_prediction_conversion() -> Arc<ConversionService> {
    prediction_conversion_from_source(
            "equal-reading-prediction.tsv",
            concat!(
                "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n",
                "\u{304b}\u{306a}\t\u{304b}\u{306a}\t0\t0\t100\t100\tpredict\tequal\n",
                "\u{304b}\u{306a}\u{306b}\t\u{4eee}\u{540d}\u{5165}\u{529b}\t0\t0\t200\t200\tpredict\tdistinct\n",
            ),
        )
}

fn nihongo_history_conversion() -> Arc<ConversionService> {
    prediction_conversion_from_source(
            "nihongo-history.tsv",
            concat!(
                "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n",
                "\u{306b}\t\u{306b}\t0\t0\t500\t500\t\tkana\n",
                "\u{306b}\t\u{4e8c}\t0\t0\t400\t400\t\tnum\n",
                "\u{306b}\u{307b}\u{3093}\u{3054}\t\u{65e5}\u{672c}\u{8a9e}\t0\t0\t100\t100\tpredict\tlang\n",
                "\u{306b}\u{307b}\u{3093}\u{3054}\t\u{306b}\u{307b}\u{3093}\u{3054}\t0\t0\t800\t800\t\tkana\n",
                "\u{306b}\u{307b}\u{3093}\u{3054}\u{306b}\u{3085}\u{3046}\u{308a}\u{3087}\u{304f}\t\u{65e5}\u{672c}\u{8a9e}\u{5165}\u{529b}\t0\t0\t90\t90\tpredict\tinput\n",
            ),
        )
}

fn identity_first_nihongo_conversion() -> Arc<ConversionService> {
    prediction_conversion_from_source(
            "identity-first-nihongo.tsv",
            concat!(
                "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n",
                "\u{306b}\t\u{306b}\t0\t0\t500\t500\t\tkana\n",
                "\u{306b}\u{307b}\u{3093}\u{304c}\t\u{306b}\u{307b}\u{3093}\u{304c}\t0\t0\t100\t100\t\tidentity\n",
                "\u{306b}\u{307b}\u{3093}\u{304c}\t\u{65e5}\u{672c}\u{8a9e}\t0\t0\t200\t200\t\tword\n",
            ),
        )
}

fn empty_prediction_conversion() -> Arc<ConversionService> {
    prediction_conversion_from_source(
            "empty-prediction.tsv",
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n",
        )
}

fn phase_one_prediction_dispatcher(
    conversion: Arc<ConversionService>,
    learning: Arc<LearningService>,
) -> (Dispatcher, crate::prediction::PredictionRuntime) {
    let runtime = crate::prediction::PredictionRuntime::start_with_learning(
        Arc::clone(&conversion),
        Arc::clone(&learning),
    )
    .expect("prediction runtime");
    let dispatcher = Dispatcher::new_with_runtime_configuration(
        conversion,
        learning,
        runtime.service(),
        Preferences::default(),
    )
    .expect("shipped defaults");
    (dispatcher, runtime)
}

fn phase_one_learning_path(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "sakura-phase-one-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).expect("create learning test directory");
    root.join("learning.log")
}

fn segmented_conversion_dispatcher() -> Dispatcher {
    let source = "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\nきょう\t今日\t0\t0\t100\t100\t\tcommon\nきょう\t京\t0\t0\t200\t200\t\talternative\nです\tです\t0\t0\t100\t100\t\tcommon\nです\tDESU\t0\t0\t200\t200\tit\tIT\n";
    let entries = dictc::parse_entries("segments.tsv", source).expect("entries");
    let matrix = dictc::parse_connection(
        "matrix.tsv",
        "# license: MIT\nclasses\t1\ndefault\t0\n",
        false,
    )
    .expect("matrix");
    let image = Box::leak(
        dictc::compile(&entries, &matrix)
            .expect("image")
            .into_boxed_slice(),
    );
    let conversion =
        Arc::new(ConversionService::from_static_bytes(image).expect("conversion service fixture"));
    Dispatcher::new_with_conversion(conversion).expect("shipped defaults")
}

fn contextual_conversion_dispatcher() -> Dispatcher {
    let source = "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\nいしゃ\t医者\t3\t3\t100\t100\t\tcontext source\nに\tに\t3\t3\t100\t100\t\tparticle\nいった\t言った\t1\t1\t50\t50\t\tgeneric\nいった\t行った\t2\t2\t100\t100\t\tcontextual\nおわり\t終わり。\t3\t3\t100\t100\t\tboundary\n";
    let entries = dictc::parse_entries("context.tsv", source).expect("entries");
    let matrix = dictc::parse_connection(
        "matrix.tsv",
        "# license: MIT\nclasses\t4\ndefault\t0\ncost\t3\t1\t1000\ncost\t3\t2\t0\n",
        false,
    )
    .expect("matrix");
    let image = Box::leak(
        dictc::compile(&entries, &matrix)
            .expect("image")
            .into_boxed_slice(),
    );
    let conversion =
        Arc::new(ConversionService::from_static_bytes(image).expect("conversion service fixture"));
    Dispatcher::new_with_conversion(conversion).expect("shipped defaults")
}

fn char_key(c: char) -> KeyInput {
    KeyInput {
        code: KeyCode::Char,
        ch: Some(c),
        modifiers: Modifiers::NONE,
        repeat: false,
        test_only: false,
    }
}

fn shifted_char_key(c: char) -> KeyInput {
    KeyInput {
        modifiers: Modifiers::SHIFT,
        ..char_key(c)
    }
}

fn test_only_char_key(c: char) -> KeyInput {
    KeyInput {
        test_only: true,
        ..char_key(c)
    }
}

fn named_key(code: KeyCode) -> KeyInput {
    KeyInput {
        code,
        ch: None,
        modifiers: Modifiers::NONE,
        repeat: false,
        test_only: false,
    }
}

fn modified_named_key(code: KeyCode, modifiers: Modifiers) -> KeyInput {
    KeyInput {
        modifiers,
        ..named_key(code)
    }
}

fn create_session(dispatcher: &mut Dispatcher, out: &mut OutputBuf, name: &str) -> SessionId {
    match dispatcher.dispatch(
        &Request::CreateSession {
            process_name: name.to_string(),
        },
        out,
    ) {
        Reply::Message(Response::SessionCreated { session, .. }) => session,
        other => panic!("expected SessionCreated, got {other:?}"),
    }
}

fn type_word(dispatcher: &mut Dispatcher, session: SessionId, word: &str, out: &mut OutputBuf) {
    for c in word.chars() {
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key(c),
            },
            out,
        );
    }
}

fn raw_preedit(dispatcher: &mut Dispatcher, session: SessionId) -> String {
    let mut session = dispatcher
        .sessions
        .get(session)
        .expect("raw preedit session")
        .clone();
    let mut out = OutputBuf::new();
    render_preedit(
        &mut session,
        &dispatcher.table,
        &dispatcher.normalizer,
        dispatcher.conversion.as_deref(),
        dispatcher.learning.as_deref(),
        &mut dispatcher.scratch,
        &mut out,
    )
    .expect("raw preedit renders");
    out.preedit_text().to_owned()
}

#[test]
fn kana_input_method_accepts_layout_characters_without_romaji_conversion() {
    let mut dispatcher = builtin_dispatcher();
    let preferences = Preferences {
        input_method: InputMethod::Kana,
        ..Preferences::default()
    };
    dispatcher
        .apply_runtime_configuration(preferences, Arc::from([]))
        .expect("valid kana input configuration");
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

    for character in ['か', 'な'] {
        let reply = dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key(character),
            },
            &mut out,
        );
        assert!(matches!(reply, Reply::Output));
    }
    assert_eq!(out.preedit_text(), "かな");
    assert!(dispatcher
        .sessions
        .get(session)
        .expect("live kana session")
        .romaji
        .is_empty());

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Backspace),
        },
        &mut out,
    );
    assert_eq!(out.preedit_text(), "か");

    dispatcher.dispatch(&Request::Commit { session }, &mut out);
    assert_eq!(out.commit_text(), Some("か"));
}

#[test]
fn developer_history_captures_real_keys_but_not_test_unclassified_or_sensitive_keys() {
    let path = std::env::temp_dir().join(format!(
        "sakura-dispatch-history-{}-{}.bin",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let history = InputHistoryService::open(&path).expect("history");
    let mut dispatcher = builtin_dispatcher();
    dispatcher.set_input_history(Some(Arc::clone(&history)));
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "editor.exe");

    dispatcher.dispatch(
        &Request::SetInputScope {
            session,
            scope: InputScope::Unclassified,
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: char_key('u'),
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SetInputScope {
            session,
            scope: InputScope::Normal,
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: char_key('k'),
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: char_key('a'),
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: test_only_char_key('x'),
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SetInputScope {
            session,
            scope: InputScope::Password,
        },
        &mut out,
    );
    for (scope, character) in [
        (InputScope::Password, 's'),
        (InputScope::Url, 'u'),
        (InputScope::Email, 'e'),
        (InputScope::Digits, 'd'),
    ] {
        dispatcher.dispatch(&Request::SetInputScope { session, scope }, &mut out);
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key(character),
            },
            &mut out,
        );
    }
    history.flush().expect("flush");

    let snapshot = crate::input_history::read_snapshot(&path).expect("snapshot");
    let keys: Vec<_> = snapshot
        .records
        .iter()
        .filter_map(|record| match record {
            InputHistoryRecord::Key(record) => Some(record),
            _ => None,
        })
        .collect();
    assert_eq!(keys.len(), 2);
    let stats = history.stats().snapshot();
    assert_eq!(stats.excluded_unclassified_events, 1);
    assert_eq!(stats.excluded_sensitive_events, 4);
    // Probe evaluation is deliberately side-effect free. The history
    // service still exposes a defensive test-only admission counter, but
    // a real Probe never reaches that admission boundary.
    assert_eq!(stats.excluded_test_only_events, 0);
    let record = keys[0];
    assert_eq!(record.character, Some('k'));
    assert_eq!(record.scope, HistoryScope::Normal);
    assert_eq!(record.session, 1);
    let next = keys[1];
    assert_eq!(next.character, Some('a'));
    assert_eq!(next.preedit_before, record.preedit_after);
    assert_ne!(next.preedit_before, "ka");
    history.stop().expect("stop");
    let _ = std::fs::remove_file(path);
}

#[test]
fn renderer_host_policy_keeps_normal_conversion_but_rejects_history_and_learning() {
    let learning = Arc::new(LearningService::memory());
    let generation_before = learning.generation();
    let history_seed = phase_one_learning_path("renderer-private-policy");
    let history_path = history_seed.with_file_name("input-history.bin");
    let history_root = history_seed
        .parent()
        .expect("history test directory")
        .to_owned();
    let history = InputHistoryService::open(&history_path).expect("history");
    history.clear().expect("clear setup history");
    history.flush().expect("flush setup history");

    let mut dispatcher = Dispatcher::new_with_services(conversion_fixture(), Arc::clone(&learning))
        .expect("dispatcher");
    dispatcher.set_input_history(Some(Arc::clone(&history)));
    let mut out = OutputBuf::new();
    let session = create_session(
        &mut dispatcher,
        &mut out,
        crate::session::SAKURA_RENDERER_PROCESS_NAME,
    );
    assert_eq!(
        dispatcher
            .sessions
            .get(session)
            .expect("session")
            .host_policy(),
        crate::session::HostPolicy::PrivateRendererUi
    );
    assert_eq!(
        dispatcher.dispatch(
            &Request::SetInputScope {
                session,
                scope: InputScope::Unclassified,
            },
            &mut out,
        ),
        Reply::Message(Response::Ok)
    );
    assert_eq!(
        dispatcher.dispatch(
            &Request::SetInputScope {
                session,
                scope: InputScope::Normal,
            },
            &mut out,
        ),
        Reply::Message(Response::Ok)
    );
    assert_eq!(
        dispatcher
            .sessions
            .get(session)
            .expect("session")
            .host_policy(),
        crate::session::HostPolicy::PrivateRendererUi,
        "Normal scope must not relax the host policy"
    );

    // The local dictionary path remains usable for ordinary Japanese
    // conversion; only durable and external sinks are denied.
    type_word(&mut dispatcher, session, "kana", &mut out);
    assert_eq!(out.preedit_text(), "かな");
    assert_eq!(
        dispatcher.dispatch(&Request::Commit { session }, &mut out),
        Reply::Output
    );
    assert!(out.commit_text().is_some(), "local conversion must commit");
    assert_eq!(
        learning.generation(),
        generation_before,
        "Pad commits must not write learning"
    );

    history.flush().expect("flush renderer history");
    let records = crate::input_history::read_snapshot(&history_path)
        .expect("renderer history snapshot")
        .records;
    assert!(
        records.is_empty(),
        "Pad key and commit records must never reach developer history"
    );
    history.stop().expect("stop history");
    std::fs::remove_dir_all(history_root).expect("remove history test directory");
}

#[test]
fn renderer_host_policy_rejects_ai_start_apply_poll_cancel_and_record() {
    let history_seed = phase_one_learning_path("renderer-private-ai");
    let history_path = history_seed.with_file_name("input-history.bin");
    let history_root = history_seed
        .parent()
        .expect("history test directory")
        .to_owned();
    let history = InputHistoryService::open(&history_path).expect("history");
    history.clear().expect("clear setup history");
    history.flush().expect("flush setup history");

    let mut dispatcher = builtin_dispatcher();
    // A nonexistent worker makes an accidental start observable during
    // development, while the private policy must reject before spawn.
    dispatcher.set_ai_text(Arc::new(AiTextService::new(PathBuf::from(
        "sakura-private-policy-worker-do-not-start.exe",
    ))));
    dispatcher.set_input_history(Some(Arc::clone(&history)));
    let mut out = OutputBuf::new();
    let session = create_session(
        &mut dispatcher,
        &mut out,
        crate::session::SAKURA_RENDERER_PROCESS_NAME,
    );
    dispatcher.dispatch(
        &Request::SetInputScope {
            session,
            scope: InputScope::Normal,
        },
        &mut out,
    );
    type_word(&mut dispatcher, session, "kana", &mut out);
    assert_eq!(out.preedit_text(), "かな");

    let busy = |reply| Reply::Message(Response::Error(ErrorCode::Busy)) == reply;
    assert!(busy(dispatcher.dispatch(
        &Request::StartAiText {
            session,
            operation: AiTextOperation::Transform,
            text: "Pad本文".to_owned(),
        },
        &mut out,
    )));
    assert!(busy(dispatcher.dispatch(
        &Request::ApplyAiComposition {
            session,
            result: "外部結果".to_owned(),
        },
        &mut out,
    )));
    assert!(busy(
        dispatcher.dispatch(&Request::PollAiText { session, job: 1 }, &mut out,)
    ));
    assert!(busy(
        dispatcher.dispatch(&Request::CancelAiText { session, job: 1 }, &mut out,)
    ));
    assert!(busy(dispatcher.dispatch(
        &Request::RecordAiText {
            session,
            operation: AiTextOperation::Transform,
            status: AiTextStatus::Rejected,
            source: "Pad本文".to_owned(),
            result: "外部結果".to_owned(),
            model: "gpt-5.6-luna".to_owned(),
            provider: "custom".to_owned(),
            style: "plain".to_owned(),
            error_code: "private-renderer".to_owned(),
            latency_ms: 0,
            input_tokens: 0,
            output_tokens: 0,
            cached_tokens: 0,
            attempts: 0,
            test_only: false,
        },
        &mut out,
    )));

    let live = dispatcher.sessions.get(session).expect("session");
    assert!(
        live.is_composing(),
        "AI rejection must not clear composition"
    );
    history.flush().expect("flush renderer AI history");
    let snapshot =
        crate::input_history::read_snapshot(&history_path).expect("renderer AI history snapshot");
    assert!(
        snapshot.records.is_empty(),
        "AI source/result must not reach developer history"
    );
    assert_eq!(history.stats().snapshot().ai_requests, 0);
    history.stop().expect("stop history");
    std::fs::remove_dir_all(history_root).expect("remove history test directory");
}

#[test]
fn renderer_host_policy_keeps_local_prediction_worker_requests() {
    let learning = Arc::new(LearningService::memory());
    let (mut dispatcher, runtime) =
        phase_one_prediction_dispatcher(phase_one_prediction_conversion(), learning);
    let prediction = runtime.service();
    let mut out = OutputBuf::new();
    let session = create_session(
        &mut dispatcher,
        &mut out,
        crate::session::SAKURA_RENDERER_PROCESS_NAME,
    );
    dispatcher.dispatch(
        &Request::SetInputScope {
            session,
            scope: InputScope::Normal,
        },
        &mut out,
    );
    let before = prediction.request_count();
    type_word(&mut dispatcher, session, "kana", &mut out);
    assert_eq!(out.preedit_text(), "かな");
    assert_eq!(
        out.candidate_kind(),
        Some(CandidateKind::Suggestion),
        "Pad keeps local prediction UI"
    );
    assert!(
        prediction.request_count() > before,
        "Pad may use the local bounded prediction worker"
    );
    runtime.stop().expect("prediction worker joins");
}

#[test]
fn counter_exhaustion_unavailable_history_keeps_normal_input() {
    let path = std::env::temp_dir().join(format!(
        "sakura-unavailable-history-{}-{}.bin",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let history = InputHistoryService::open(&path).expect("history");
    let mut dispatcher = builtin_dispatcher();
    dispatcher.set_input_history(Some(Arc::clone(&history)));
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "synthetic.exe");
    dispatcher
        .sessions
        .get_mut(session)
        .expect("session")
        .set_history_session_id(0);
    dispatcher.dispatch(
        &Request::SetInputScope {
            session,
            scope: InputScope::Normal,
        },
        &mut out,
    );
    type_word(&mut dispatcher, session, "kana", &mut out);
    let preedit = out.preedit_text().to_owned();
    history.stop().expect("history stop");
    let snapshot = crate::input_history::read_snapshot(&path);
    let removed = std::fs::remove_file(&path);
    assert_eq!(preedit, "かな");
    assert_eq!(
        snapshot.expect("snapshot").records.len(),
        1,
        "only engine marker persists"
    );
    assert!(history.stats().dropped_events() > 0);
    removed.expect("fixture removed");
}

#[test]
fn hot_attach_and_detach_of_developer_history_matches_stats_active() {
    let path = std::env::temp_dir().join(format!(
        "sakura-dispatch-history-hot-{}-{}.bin",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let history = InputHistoryService::open(&path).expect("history");
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "hot-history.exe");
    dispatcher.dispatch(
        &Request::SetInputScope {
            session,
            scope: InputScope::Normal,
        },
        &mut out,
    );

    match dispatcher.dispatch(&Request::InputHistoryStats, &mut out) {
        Reply::Message(Response::InputHistoryStats { active: false, .. }) => {}
        other => panic!("expected inactive stats before attach, got {other:?}"),
    }

    dispatcher.set_input_history(Some(Arc::clone(&history)));
    match dispatcher.dispatch(&Request::InputHistoryStats, &mut out) {
        Reply::Message(Response::InputHistoryStats { active: true, .. }) => {}
        other => panic!("expected active stats after attach, got {other:?}"),
    }
    let history_session_after_attach = dispatcher
        .sessions
        .get(session)
        .expect("session")
        .history_session_id();

    let replacement_path = path.with_extension("replacement.bin");
    let replacement = InputHistoryService::open(&replacement_path).expect("replacement history");
    dispatcher.set_input_history(Some(Arc::clone(&replacement)));
    assert_eq!(
        dispatcher
            .sessions
            .get(session)
            .expect("session")
            .history_session_id(),
        history_session_after_attach,
        "swapping history owners while already attached must not reallocate session ids"
    );
    dispatcher.set_input_history(Some(Arc::clone(&history)));
    assert_eq!(
        dispatcher
            .sessions
            .get(session)
            .expect("session")
            .history_session_id(),
        history_session_after_attach,
        "returning to the original history owner must keep the allocated session id"
    );

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: char_key('a'),
        },
        &mut out,
    );
    history.flush().expect("flush after attach");
    let attached = crate::input_history::read_snapshot(&path).expect("snapshot");
    assert_eq!(
        attached
            .records
            .iter()
            .filter(|record| matches!(record, InputHistoryRecord::Key(_)))
            .count(),
        1
    );

    dispatcher.set_input_history(None);
    match dispatcher.dispatch(&Request::InputHistoryStats, &mut out) {
        Reply::Message(Response::InputHistoryStats { active: false, .. }) => {}
        other => panic!("expected inactive stats after detach, got {other:?}"),
    }
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: char_key('b'),
        },
        &mut out,
    );
    history.flush().expect("flush after detach");
    let detached = crate::input_history::read_snapshot(&path).expect("snapshot after detach");
    assert_eq!(
        detached
            .records
            .iter()
            .filter(|record| matches!(record, InputHistoryRecord::Key(_)))
            .count(),
        1,
        "detached dispatcher must not append new durable keys"
    );
    assert_eq!(
        detached.records.len(),
        attached.records.len(),
        "detach must not grow the history file"
    );

    history.stop().expect("stop");
    replacement.stop().expect("stop replacement");
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(replacement_path);
}

#[test]
fn test_only_enter_preserves_learning_preference_and_input_history_before_real_enter() {
    let reading = "\u{304b}\u{306a}";
    let learning = Arc::new(LearningService::memory());
    learning.learn(reading, "preferred", 0, 0);
    let preference_before = learning.preference(reading, 0, [("preferred", 0)]);
    let generation_before = learning.generation();

    let history_root = phase_one_learning_path("test-only-enter-history");
    let history_path = history_root.with_file_name("input-history.bin");
    let history = InputHistoryService::open(&history_path).expect("history");
    let mut dispatcher = Dispatcher::new_with_services(conversion_fixture(), Arc::clone(&learning))
        .expect("dispatcher");
    dispatcher.set_input_history(Some(Arc::clone(&history)));
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "test-only-enter.exe");
    dispatcher.dispatch(
        &Request::SetInputScope {
            session,
            scope: InputScope::Normal,
        },
        &mut out,
    );
    type_word(&mut dispatcher, session, "kana", &mut out);
    history.clear().expect("clear setup history");
    history.flush().expect("flush setup history");
    let session_before = dispatcher.sessions.get(session).expect("session").clone();
    assert!(crate::input_history::read_snapshot(&history_path)
        .expect("empty setup history")
        .records
        .is_empty());

    let probe_enter = KeyInput {
        test_only: true,
        ..named_key(KeyCode::Enter)
    };
    assert_eq!(
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: probe_enter,
            },
            &mut out,
        ),
        Reply::Output
    );
    let probe_output = out.to_output();
    assert!(probe_output.consumed);
    assert_eq!(learning.generation(), generation_before);
    assert_eq!(
        learning.preference(reading, 0, [("preferred", 0)]),
        preference_before,
        "Probe must not mutate the learning preference"
    );
    history.flush().expect("flush test-only Enter");
    assert!(
        crate::input_history::read_snapshot(&history_path)
            .expect("test-only Enter history")
            .records
            .is_empty(),
        "test-only Enter must emit neither a key nor a commit record"
    );
    assert_eq!(
        dispatcher.sessions.get(session).expect("session"),
        &session_before,
        "test-only Enter must leave the live session unchanged"
    );

    dispatcher
        .sessions
        .get_mut(session)
        .expect("session")
        .clone_from(&session_before);
    let real_enter = named_key(KeyCode::Enter);
    assert_eq!(
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: real_enter,
            },
            &mut out,
        ),
        Reply::Output
    );
    let real_output = out.to_output();
    assert_eq!(real_output.consumed, probe_output.consumed);
    assert_eq!(real_output.commit, probe_output.commit);
    assert!(real_output.commit.is_some());
    history.flush().expect("flush real Enter");
    let records = crate::input_history::read_snapshot(&history_path)
        .expect("real Enter history")
        .records;
    assert_eq!(
        records
            .iter()
            .filter(|record| matches!(record, InputHistoryRecord::Commit(_)))
            .count(),
        1,
        "the real Enter must emit one commit record"
    );
    assert_eq!(
        records
            .iter()
            .filter(|record| matches!(record, InputHistoryRecord::Key(_)))
            .count(),
        1,
        "the real Enter must emit one key record"
    );
    let commit = records.iter().find_map(|record| match record {
        InputHistoryRecord::Commit(record) => Some(record),
        InputHistoryRecord::Key(_)
        | InputHistoryRecord::AiText(_)
        | InputHistoryRecord::Engine(_) => None,
    });
    assert_eq!(commit.map(|record| record.reading.as_str()), Some(reading));
    assert_eq!(learning.generation(), generation_before + 1);

    history.stop().expect("stop history");
    std::fs::remove_dir_all(history_root.parent().expect("history root"))
        .expect("remove history test directory");
}

#[test]
fn test_only_scope_sensitive_transition_is_pure_and_matches_real_scope_key() {
    let mut probe_dispatcher = builtin_dispatcher();
    let mut apply_dispatcher = builtin_dispatcher();
    let mut probe_out = OutputBuf::new();
    let mut apply_out = OutputBuf::new();
    let probe_session = create_session(&mut probe_dispatcher, &mut probe_out, "probe-scope.exe");
    let apply_session = create_session(&mut apply_dispatcher, &mut apply_out, "apply-scope.exe");

    for (dispatcher, session, out) in [
        (&mut probe_dispatcher, probe_session, &mut probe_out),
        (&mut apply_dispatcher, apply_session, &mut apply_out),
    ] {
        assert_eq!(
            dispatcher.dispatch(
                &Request::SetInputScope {
                    session,
                    scope: InputScope::Normal,
                },
                out,
            ),
            Reply::Message(Response::Ok)
        );
        type_word(dispatcher, session, "ka", out);
    }

    let session_before = probe_dispatcher
        .sessions
        .get(probe_session)
        .expect("probe session")
        .clone();
    let cache_before = (*probe_dispatcher.prediction_cache).clone();
    assert_eq!(
        probe_dispatcher.dispatch(
            &Request::ProbeKey {
                session: probe_session,
                scope: InputScope::Password,
                fresh_context: false,
                key: test_only_char_key('x'),
            },
            &mut probe_out,
        ),
        Reply::Output
    );
    let probe_output = probe_out.to_output();
    assert_eq!(
        probe_dispatcher
            .sessions
            .get(probe_session)
            .expect("unchanged probe session"),
        &session_before,
        "a sensitive Probe transition must not reset the live composition"
    );
    assert_eq!(
        *probe_dispatcher.prediction_cache, cache_before,
        "a sensitive Probe transition must not clear the live cache"
    );

    assert_eq!(
        apply_dispatcher.dispatch(
            &Request::SetInputScope {
                session: apply_session,
                scope: InputScope::Password,
            },
            &mut apply_out,
        ),
        Reply::Message(Response::Ok)
    );
    assert_eq!(
        apply_dispatcher.dispatch(
            &Request::SendKey {
                session: apply_session,
                key: char_key('x'),
            },
            &mut apply_out,
        ),
        Reply::Output
    );
    assert_eq!(
        probe_output,
        apply_out.to_output(),
        "Probe must match the subsequent real scope publication and key"
    );
}

#[test]
fn test_only_scope_from_sensitive_restores_mode_purely_and_matches_real_scope_key() {
    let mut probe_dispatcher = builtin_dispatcher();
    let mut apply_dispatcher = builtin_dispatcher();
    let mut probe_out = OutputBuf::new();
    let mut apply_out = OutputBuf::new();
    let probe_session = create_session(&mut probe_dispatcher, &mut probe_out, "probe-restore.exe");
    let apply_session = create_session(&mut apply_dispatcher, &mut apply_out, "apply-restore.exe");

    for (dispatcher, session, out) in [
        (&mut probe_dispatcher, probe_session, &mut probe_out),
        (&mut apply_dispatcher, apply_session, &mut apply_out),
    ] {
        dispatcher.sessions.get_mut(session).expect("session").mode = Mode::Katakana;
        assert_eq!(
            dispatcher.dispatch(
                &Request::SetInputScope {
                    session,
                    scope: InputScope::Password,
                },
                out,
            ),
            Reply::Message(Response::Ok)
        );
    }

    let session_before = probe_dispatcher
        .sessions
        .get(probe_session)
        .expect("sensitive probe session")
        .clone();
    assert_eq!(session_before.mode(), Mode::Direct);
    let cache_before = (*probe_dispatcher.prediction_cache).clone();
    assert_eq!(
        probe_dispatcher.dispatch(
            &Request::ProbeKey {
                session: probe_session,
                scope: InputScope::Normal,
                fresh_context: false,
                key: test_only_char_key('k'),
            },
            &mut probe_out,
        ),
        Reply::Output
    );
    let probe_output = probe_out.to_output();
    assert_eq!(
        probe_dispatcher
            .sessions
            .get(probe_session)
            .expect("unchanged sensitive session"),
        &session_before
    );
    assert_eq!(*probe_dispatcher.prediction_cache, cache_before);

    assert_eq!(
        apply_dispatcher.dispatch(
            &Request::SetInputScope {
                session: apply_session,
                scope: InputScope::Normal,
            },
            &mut apply_out,
        ),
        Reply::Message(Response::Ok)
    );
    assert_eq!(
        apply_dispatcher.dispatch(
            &Request::SendKey {
                session: apply_session,
                key: char_key('k'),
            },
            &mut apply_out,
        ),
        Reply::Output
    );
    assert_eq!(
        probe_output,
        apply_out.to_output(),
        "Probe must match the real sensitive-to-Normal transition"
    );
}

#[test]
fn test_only_context_replacement_probe_uses_fresh_session_for_first_character() {
    let mut probe_dispatcher = builtin_dispatcher();
    let mut apply_dispatcher = builtin_dispatcher();
    let mut probe_out = OutputBuf::new();
    let mut apply_out = OutputBuf::new();
    let probe_session = create_session(&mut probe_dispatcher, &mut probe_out, "notepad.exe");
    let apply_session = create_session(&mut apply_dispatcher, &mut apply_out, "notepad.exe");

    // Make the old context observably non-fresh. A replacement Probe must
    // not carry this composition or its user-selected mode into the new
    // document; the live old session must remain untouched.
    probe_dispatcher
        .sessions
        .get_mut(probe_session)
        .expect("session")
        .mode = Mode::Katakana;
    type_word(&mut probe_dispatcher, probe_session, "ka", &mut probe_out);
    let old_session = probe_dispatcher
        .sessions
        .get(probe_session)
        .expect("old session")
        .clone();

    assert_eq!(
        probe_dispatcher.dispatch(
            &Request::ProbeKey {
                session: probe_session,
                scope: InputScope::Normal,
                fresh_context: true,
                key: test_only_char_key('k'),
            },
            &mut probe_out,
        ),
        Reply::Output
    );
    let probe_output = probe_out.to_output();
    assert_eq!(
        probe_dispatcher
            .sessions
            .get(probe_session)
            .expect("unchanged old session"),
        &old_session,
        "fresh-context Probe must not mutate the old session"
    );

    assert_eq!(
        apply_dispatcher.dispatch(
            &Request::SetInputScope {
                session: apply_session,
                scope: InputScope::Normal,
            },
            &mut apply_out,
        ),
        Reply::Message(Response::Ok)
    );
    assert_eq!(
        apply_dispatcher.dispatch(
            &Request::SendKey {
                session: apply_session,
                key: char_key('k'),
            },
            &mut apply_out,
        ),
        Reply::Output
    );
    assert_eq!(
        probe_output,
        apply_out.to_output(),
        "replacement Probe and fresh-context Apply must consume the first character identically"
    );
}

#[test]
fn test_only_context_replacement_probe_preserves_half_full_key_parity() {
    let mut probe_dispatcher = builtin_dispatcher();
    let mut apply_dispatcher = builtin_dispatcher();
    let mut probe_out = OutputBuf::new();
    let mut apply_out = OutputBuf::new();
    let probe_session = create_session(&mut probe_dispatcher, &mut probe_out, "notepad.exe");
    let apply_session = create_session(&mut apply_dispatcher, &mut apply_out, "notepad.exe");

    probe_dispatcher
        .sessions
        .get_mut(probe_session)
        .expect("session")
        .mode = Mode::Katakana;
    type_word(&mut probe_dispatcher, probe_session, "ka", &mut probe_out);
    let old_session = probe_dispatcher
        .sessions
        .get(probe_session)
        .expect("old session")
        .clone();
    let key = named_key(KeyCode::HankakuZenkaku);

    assert_eq!(
        probe_dispatcher.dispatch(
            &Request::ProbeKey {
                session: probe_session,
                scope: InputScope::Normal,
                fresh_context: true,
                key: KeyInput {
                    test_only: true,
                    ..key
                },
            },
            &mut probe_out,
        ),
        Reply::Output
    );
    let probe_output = probe_out.to_output();
    assert_eq!(
        probe_dispatcher
            .sessions
            .get(probe_session)
            .expect("unchanged old session"),
        &old_session
    );

    assert_eq!(
        apply_dispatcher.dispatch(
            &Request::SetInputScope {
                session: apply_session,
                scope: InputScope::Normal,
            },
            &mut apply_out,
        ),
        Reply::Message(Response::Ok)
    );
    assert_eq!(
        apply_dispatcher.dispatch(
            &Request::SendKey {
                session: apply_session,
                key,
            },
            &mut apply_out,
        ),
        Reply::Output
    );
    assert_eq!(
        probe_output,
        apply_out.to_output(),
        "replacement Probe and fresh-context Apply must preserve half/full consumed parity"
    );
}

#[test]
fn test_only_unclassified_scope_preserves_real_composition_without_persistence_or_probe_work() {
    let learning = Arc::new(LearningService::memory());
    let history_root = phase_one_learning_path("test-only-unclassified-scope");
    let history_path = history_root.with_file_name("input-history.bin");
    let history = InputHistoryService::open(&history_path).expect("history");
    let (mut dispatcher, runtime) =
        phase_one_prediction_dispatcher(phase_one_prediction_conversion(), Arc::clone(&learning));
    dispatcher.set_input_history(Some(Arc::clone(&history)));
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "probe-unclassified.exe");
    assert_eq!(
        dispatcher.dispatch(
            &Request::SetInputScope {
                session,
                scope: InputScope::Normal,
            },
            &mut out,
        ),
        Reply::Message(Response::Ok)
    );
    type_word(&mut dispatcher, session, "ka", &mut out);
    history.clear().expect("clear setup history");
    history.flush().expect("flush setup history");

    let session_before = dispatcher.sessions.get(session).expect("session").clone();
    let cache_before = (*dispatcher.prediction_cache).clone();
    let generation_before = learning.generation();
    let request_count_before = runtime.service().request_count();
    assert!(crate::input_history::read_snapshot(&history_path)
        .expect("empty setup history")
        .records
        .is_empty());

    assert_eq!(
        dispatcher.dispatch(
            &Request::ProbeKey {
                session,
                scope: InputScope::Unclassified,
                fresh_context: false,
                key: test_only_char_key('x'),
            },
            &mut out,
        ),
        Reply::Output
    );
    let probe_output = out.to_output();
    assert_eq!(
        dispatcher.sessions.get(session).expect("unchanged session"),
        &session_before
    );
    assert_eq!(*dispatcher.prediction_cache, cache_before);
    assert_eq!(learning.generation(), generation_before);
    assert_eq!(runtime.service().request_count(), request_count_before);
    history.flush().expect("flush test-only unclassified");
    assert!(
        crate::input_history::read_snapshot(&history_path)
            .expect("test-only unclassified history")
            .records
            .is_empty(),
        "Unclassified Probe must not persist input history"
    );

    dispatcher
        .sessions
        .get_mut(session)
        .expect("restore session")
        .clone_from(&session_before);
    dispatcher
        .prediction_cache
        .as_mut()
        .clone_from(&cache_before);
    assert_eq!(
        dispatcher.dispatch(
            &Request::SetInputScope {
                session,
                scope: InputScope::Unclassified,
            },
            &mut out,
        ),
        Reply::Message(Response::Ok)
    );
    assert_eq!(
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key('x'),
            },
            &mut out,
        ),
        Reply::Output
    );
    let real_output = out.to_output();
    assert_eq!(probe_output.consumed, real_output.consumed);
    assert_eq!(probe_output.beep, real_output.beep);
    assert_eq!(probe_output.mode, real_output.mode);
    assert_eq!(probe_output.preedit, real_output.preedit);
    assert_eq!(probe_output.commit, real_output.commit);
    assert_eq!(probe_output.delete_before, real_output.delete_before);
    history.flush().expect("flush real unclassified");
    assert!(
        crate::input_history::read_snapshot(&history_path)
            .expect("real unclassified history")
            .records
            .is_empty(),
        "Unclassified Apply must remain unrecorded"
    );

    runtime.stop().expect("prediction worker joins");
    history.stop().expect("stop history");
    drop(dispatcher);
    drop(learning);
    std::fs::remove_dir_all(history_root.parent().expect("history root"))
        .expect("remove unclassified test directory");
}

#[test]
fn test_only_delete_prediction_history_preserves_durable_state_and_cache_before_real_control_delete(
) {
    let learning_path = phase_one_learning_path("test-only-delete-history");
    let history_path = learning_path.with_file_name("input-history.bin");
    let reading = "\u{304b}\u{306a}";
    let learning = Arc::new(LearningService::open(&learning_path).expect("durable learning"));
    learning.learn(reading, "history-only", 0, 7);
    learning.maintain().expect("flush durable learning");
    let learning_bytes_before = std::fs::read(&learning_path).expect("learning bytes");
    let learning_generation_before = learning.generation();
    let history = InputHistoryService::open(&history_path).expect("history");
    let (mut dispatcher, runtime) =
        phase_one_prediction_dispatcher(phase_one_prediction_conversion(), Arc::clone(&learning));
    dispatcher.set_input_history(Some(Arc::clone(&history)));
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "test-only-delete.exe");
    dispatcher.dispatch(
        &Request::SetInputScope {
            session,
            scope: InputScope::Normal,
        },
        &mut out,
    );
    type_word(&mut dispatcher, session, "kana", &mut out);
    assert_eq!(
        dispatcher
            .prediction_cache
            .candidates(
                session,
                dispatcher
                    .sessions
                    .get(session)
                    .expect("session")
                    .prediction_generation,
            )
            .expect("history prediction")[0]
            .source(),
        PredictionSource::History
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Tab),
        },
        &mut out,
    );
    assert!(
        dispatcher
            .sessions
            .get(session)
            .expect("session")
            .suggestion_focused
    );
    history.clear().expect("clear setup history");
    history.flush().expect("flush setup history");
    let session_before = dispatcher.sessions.get(session).expect("session").clone();
    let cache_before = (*dispatcher.prediction_cache).clone();
    let history_before = crate::input_history::read_snapshot(&history_path)
        .expect("empty setup history")
        .records;

    let probe_key = KeyInput {
        test_only: true,
        ..modified_named_key(KeyCode::Delete, Modifiers::CTRL)
    };
    assert_eq!(
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: probe_key,
            },
            &mut out,
        ),
        Reply::Output
    );
    let probe_output = out.to_output();
    assert!(probe_output.consumed);
    assert!(!probe_output.beep);
    assert_eq!(
        dispatcher.sessions.get(session).expect("session"),
        &session_before
    );
    assert_eq!(*dispatcher.prediction_cache, cache_before);
    assert_eq!(learning.generation(), learning_generation_before);
    assert_eq!(
        std::fs::read(&learning_path).expect("unchanged learning bytes"),
        learning_bytes_before
    );
    history.flush().expect("flush test-only delete");
    assert_eq!(
        crate::input_history::read_snapshot(&history_path)
            .expect("unchanged history")
            .records,
        history_before,
        "test-only DeletePredictionHistory must not write input history"
    );

    dispatcher
        .sessions
        .get_mut(session)
        .expect("session")
        .clone_from(&session_before);
    dispatcher
        .prediction_cache
        .as_mut()
        .clone_from(&cache_before);
    assert_eq!(
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: modified_named_key(KeyCode::Delete, Modifiers::CTRL),
            },
            &mut out,
        ),
        Reply::Output
    );
    let real_output = out.to_output();
    assert_eq!(real_output.consumed, probe_output.consumed);
    assert_eq!(real_output.beep, probe_output.beep);
    assert!(
        learning.generation() > learning_generation_before,
        "real DeletePredictionHistory must advance learning generation"
    );
    let mut found = false;
    learning.visit_prediction_history(reading, |candidate_reading, surface, _, _| {
        found |= candidate_reading == reading && surface == "history-only";
        true
    });
    assert!(
        !found,
        "real control delete must remove the durable history pair"
    );
    history.flush().expect("flush real delete");
    let real_history = crate::input_history::read_snapshot(&history_path)
        .expect("real delete history")
        .records;
    assert_eq!(
        real_history
            .iter()
            .filter(|record| matches!(record, InputHistoryRecord::Key(_)))
            .count(),
        1,
        "the real control delete must emit one key record"
    );
    assert!(dispatcher
        .prediction_cache
        .candidates(
            session,
            dispatcher
                .sessions
                .get(session)
                .expect("session")
                .prediction_generation,
        )
        .into_iter()
        .flatten()
        .all(|candidate| candidate.surface() != "history-only"));

    runtime.stop().expect("prediction worker joins");
    history.stop().expect("stop history");
    drop(dispatcher);
    drop(learning);
    std::fs::remove_dir_all(learning_path.parent().expect("learning root"))
        .expect("remove learning test directory");
}

#[test]
fn test_only_probe_does_not_enqueue_prediction_work_and_real_key_keeps_parity() {
    let learning = Arc::new(LearningService::memory());
    let (mut dispatcher, runtime) =
        phase_one_prediction_dispatcher(phase_one_prediction_conversion(), Arc::clone(&learning));
    let prediction = runtime.service();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "test-only-prediction.exe");
    type_word(&mut dispatcher, session, "kana", &mut out);
    dispatcher.prediction_cache.clear();
    let cache_before = (*dispatcher.prediction_cache).clone();
    let request_count_before = prediction.request_count();
    let probe_key = KeyInput {
        test_only: true,
        ..named_key(KeyCode::Tab)
    };
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: probe_key,
        },
        &mut out,
    );
    let probe_output = out.to_output();
    assert_eq!(prediction.request_count(), request_count_before);
    assert_eq!(*dispatcher.prediction_cache, cache_before);

    let real_key = named_key(KeyCode::Tab);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: real_key,
        },
        &mut out,
    );
    let real_output = out.to_output();
    assert_eq!(real_output.consumed, probe_output.consumed);
    assert!(
        prediction.request_count() > request_count_before,
        "Apply must enqueue the equivalent eligible prediction request"
    );
    runtime.stop().expect("prediction worker joins");
}

#[test]
fn test_only_probe_uses_ephemeral_stale_cache_and_keeps_live_cache_unchanged() {
    let learning = Arc::new(LearningService::memory());
    let conversion = phase_one_prediction_conversion();
    let (mut probe_dispatcher, probe_runtime) =
        phase_one_prediction_dispatcher(Arc::clone(&conversion), Arc::clone(&learning));
    let (mut apply_dispatcher, apply_runtime) =
        phase_one_prediction_dispatcher(conversion, Arc::clone(&learning));
    let mut probe_out = OutputBuf::new();
    let mut apply_out = OutputBuf::new();
    let probe_session = create_session(&mut probe_dispatcher, &mut probe_out, "probe.exe");
    let apply_session = create_session(&mut apply_dispatcher, &mut apply_out, "apply.exe");
    type_word(&mut probe_dispatcher, probe_session, "kana", &mut probe_out);
    type_word(&mut apply_dispatcher, apply_session, "kana", &mut apply_out);
    probe_dispatcher.dispatch(
        &Request::SendKey {
            session: probe_session,
            key: named_key(KeyCode::Tab),
        },
        &mut probe_out,
    );
    apply_dispatcher.dispatch(
        &Request::SendKey {
            session: apply_session,
            key: named_key(KeyCode::Tab),
        },
        &mut apply_out,
    );
    let raw_reading = probe_dispatcher
        .sessions
        .get(probe_session)
        .expect("probe session")
        .preedit
        .as_str()
        .to_owned();
    let cache_before = (*probe_dispatcher.prediction_cache).clone();

    learning.learn(&raw_reading, "stale-generation", 0, 0);
    let probe_key = KeyInput {
        test_only: true,
        ..named_key(KeyCode::Tab)
    };
    probe_dispatcher.dispatch(
        &Request::SendKey {
            session: probe_session,
            key: probe_key,
        },
        &mut probe_out,
    );
    let probe_preedit = probe_out.preedit_text().to_owned();
    let probe_output = probe_out.to_output();
    assert_eq!(*probe_dispatcher.prediction_cache, cache_before);
    assert_eq!(probe_preedit, raw_reading);

    apply_dispatcher.dispatch(
        &Request::SendKey {
            session: apply_session,
            key: named_key(KeyCode::Tab),
        },
        &mut apply_out,
    );
    let apply_output = apply_out.to_output();
    assert_eq!(apply_output.consumed, probe_output.consumed);

    probe_runtime.stop().expect("probe prediction worker joins");
    apply_runtime.stop().expect("apply prediction worker joins");
}

#[test]
fn app_profile_is_copied_once_when_the_context_is_created() {
    let profile = AppProfile {
        process_name: "custom.exe".to_owned(),
        default_mode: Mode::HalfAlnum,
        normalizer: Normalizer {
            width: WidthPolicy {
                alnum: Width::Full,
                number: Width::Half,
                symbol: Width::FollowMode,
            },
            punctuation: PunctuationStyle::COMMA_PERIOD,
            brackets: BracketStyle::default(),
        },
        prediction_enabled: false,
        suggest_accept: SuggestAccept::Disabled,
    };
    let mut dispatcher = Dispatcher::new_with_configuration_and_profiles(
        conversion_fixture(),
        Arc::new(LearningService::memory()),
        Preferences::default(),
        Arc::from(vec![profile]),
    )
    .expect("dispatcher");
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "CUSTOM.EXE");
    let configured = dispatcher.sessions.get(session).expect("session");
    assert_eq!(configured.mode, Mode::HalfAlnum);
    assert_eq!(configured.normalizer.width.alnum, Width::Full);
    assert!(!configured.prediction_enabled);
    assert_eq!(configured.suggest_accept, SuggestAccept::Disabled);

    dispatcher.sessions.get_mut(session).expect("session").mode = Mode::Katakana;
    assert!(matches!(
        dispatcher.dispatch(
            &Request::SetInputScope {
                session,
                scope: InputScope::Normal,
            },
            &mut out,
        ),
        Reply::Message(Response::Ok)
    ));
    assert_eq!(
        dispatcher.sessions.get(session).expect("session").mode,
        Mode::Katakana,
        "focus/input-scope updates must not reapply a profile"
    );
}

#[test]
fn reconversion_preview_returns_conversion_candidates_without_mutating_the_session() {
    let mut dispatcher = conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "editor.exe");

    let reply = dispatcher.dispatch(
        &Request::Reconvert {
            session,
            text: "仮名".to_owned(),
            preview: true,
        },
        &mut out,
    );

    assert_eq!(reply, Reply::Output);
    assert_eq!(out.preedit_text(), "仮名");
    assert_eq!(out.candidate_kind(), Some(CandidateKind::Conversion));
    assert_eq!(
        dispatcher.sessions.get(session).expect("session").state(),
        State::Idle,
        "GetReconversion must be observational"
    );
}

#[test]
fn actual_reconversion_clears_cross_commit_bridge_but_preview_does_not() {
    let mut dispatcher = conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "editor.exe");
    {
        let state = dispatcher.sessions.get_mut(session).expect("session");
        state.apply_input_scope(InputScope::Normal);
        state.preedit.push_str("こうりょもれ").expect("fits");
        state.record_current_commit_with_bridge(
            "考慮漏れ",
            1949,
            0,
            2,
            SessionCrossCommitBridge::new("もれ", "漏れ", 1841, 4_000),
        );
        state.reset();
    }

    let preview = dispatcher.dispatch(
        &Request::Reconvert {
            session,
            text: "仮名".to_owned(),
            preview: true,
        },
        &mut out,
    );
    assert_eq!(preview, Reply::Output);
    assert!(dispatcher
        .sessions
        .get(session)
        .expect("session")
        .cross_commit_bridge()
        .is_some());

    let actual = dispatcher.dispatch(
        &Request::Reconvert {
            session,
            text: "仮名".to_owned(),
            preview: false,
        },
        &mut out,
    );
    assert_eq!(actual, Reply::Output);
    assert!(dispatcher
        .sessions
        .get(session)
        .expect("session")
        .cross_commit_bridge()
        .is_none());
}

#[test]
fn actual_reconversion_enters_conversion_over_the_recovered_reading() {
    let mut dispatcher = conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "editor.exe");

    let reply = dispatcher.dispatch(
        &Request::Reconvert {
            session,
            text: "仮名".to_owned(),
            preview: false,
        },
        &mut out,
    );

    assert_eq!(reply, Reply::Output);
    assert!(out.consumed);
    assert_eq!(out.preedit_text(), "仮名");
    assert_eq!(out.candidate_kind(), Some(CandidateKind::Conversion));
    let live = dispatcher.sessions.get(session).expect("session");
    assert_eq!(live.state(), State::Converting);
    assert_eq!(live.preedit.as_str(), "かな");
}

#[test]
fn a_failed_reconversion_restores_the_pre_request_input_mode() {
    // `build_reconversion` switches the persistent mode to Hiragana for
    // the recovered composition, and `Session::reset` deliberately keeps
    // `mode`. A reconversion that fails after that switch used to leave a
    // Katakana typist silently stuck in Hiragana. The fixture's 600-byte
    // ASCII surface recovers the reading です, whose sole candidate
    // overflows the render scratch under the profile's full-width alnum
    // policy, so the request always errors after entering conversion.
    let mut dispatcher = oversized_render_segment_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "editor.exe");
    dispatcher.sessions.get_mut(session).expect("session").mode = Mode::Katakana;

    let reply = dispatcher.dispatch(
        &Request::Reconvert {
            session,
            text: "x".repeat(600),
            preview: false,
        },
        &mut out,
    );

    assert_eq!(reply, Reply::Message(Response::Error(ErrorCode::TooLarge)));
    let live = dispatcher.sessions.get(session).expect("session");
    assert_eq!(live.state(), State::Idle);
    assert_eq!(
        live.mode,
        Mode::Katakana,
        "a failed reconversion must not leave the session in Hiragana"
    );
}

#[test]
fn committing_a_reconversion_puts_the_pre_request_input_mode_back() {
    // Reconversion has to force Hiragana: commit-time normalization reads
    // `session.mode`, so a Katakana session would katakana-ise the
    // okurigana the reverse scan just recovered. The forced mode is the
    // composition's, not the user's, and must not outlive the composition.
    let mut dispatcher = conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "editor.exe");
    dispatcher.sessions.get_mut(session).expect("session").mode = Mode::Katakana;

    assert_eq!(
        dispatcher.dispatch(
            &Request::Reconvert {
                session,
                text: "仮名".to_owned(),
                preview: false,
            },
            &mut out,
        ),
        Reply::Output
    );
    assert_eq!(out.mode, Some(Mode::Hiragana), "the composition's mode");
    assert_eq!(
        dispatcher.sessions.get(session).expect("session").mode,
        Mode::Hiragana
    );

    assert_eq!(
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Enter),
            },
            &mut out,
        ),
        Reply::Output
    );

    let live = dispatcher.sessions.get(session).expect("session");
    assert_eq!(live.state(), State::Idle);
    assert_eq!(
        live.mode,
        Mode::Katakana,
        "committing a reconversion must not leave the user in Hiragana"
    );
    assert_eq!(
        out.mode,
        Some(Mode::Katakana),
        "the host only learns a mode it is told about"
    );
}

#[test]
fn cancelling_a_reconversion_puts_the_pre_request_input_mode_back() {
    let mut dispatcher = conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "editor.exe");
    dispatcher.sessions.get_mut(session).expect("session").mode = Mode::HalfKatakana;

    assert_eq!(
        dispatcher.dispatch(
            &Request::Reconvert {
                session,
                text: "仮名".to_owned(),
                preview: false,
            },
            &mut out,
        ),
        Reply::Output
    );
    // Conversion swallows the first Escape to fall back to the reading;
    // the second ends the composition for good.
    for _ in 0..2 {
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Escape),
            },
            &mut out,
        );
    }

    let live = dispatcher.sessions.get(session).expect("session");
    assert_eq!(live.state(), State::Idle);
    assert_eq!(live.mode, Mode::HalfKatakana);
    assert_eq!(out.mode, Some(Mode::HalfKatakana));
}

#[test]
fn reverting_a_reconversion_answers_with_the_restored_input_mode() {
    // `Revert` carries no `Output`, so the reply itself is the only place
    // the restored mode can reach the host.
    let mut dispatcher = conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "editor.exe");
    dispatcher.sessions.get_mut(session).expect("session").mode = Mode::Katakana;

    assert_eq!(
        dispatcher.dispatch(
            &Request::Reconvert {
                session,
                text: "仮名".to_owned(),
                preview: false,
            },
            &mut out,
        ),
        Reply::Output
    );

    assert_eq!(
        dispatcher.dispatch(&Request::Revert { session }, &mut out),
        Reply::Message(Response::InputMode {
            mode: Mode::Katakana
        })
    );
    let live = dispatcher.sessions.get(session).expect("session");
    assert_eq!(live.state(), State::Idle);
    assert_eq!(live.mode, Mode::Katakana);
}

#[test]
fn reverting_an_ordinary_composition_still_answers_plain_ok() {
    let mut dispatcher = conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "editor.exe");
    type_word(&mut dispatcher, session, "kana", &mut out);

    assert_eq!(
        dispatcher.dispatch(&Request::Revert { session }, &mut out),
        Reply::Message(Response::Ok),
        "only a reconversion has a mode to give back"
    );
}

#[test]
fn password_scope_reconversion_is_refused_and_finishes_idle() {
    let mut dispatcher = conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "editor.exe");
    assert_eq!(
        dispatcher.dispatch(
            &Request::SetInputScope {
                session,
                scope: InputScope::Password,
            },
            &mut out,
        ),
        Reply::Message(Response::Ok)
    );

    assert_eq!(
        dispatcher.dispatch(
            &Request::Reconvert {
                session,
                text: "secret".to_owned(),
                preview: false,
            },
            &mut out,
        ),
        Reply::Message(Response::Error(ErrorCode::Malformed))
    );
    assert_eq!(
        dispatcher.sessions.get(session).expect("session").state(),
        State::Idle
    );
    assert!(out.to_output().preedit.is_none());
}

#[test]
fn conversion_starts_compact_and_expansion_is_idempotent() {
    let mut dispatcher = conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "editor.exe");
    type_word(&mut dispatcher, session, "kana", &mut out);

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    let compact = out.to_output().candidates.expect("compact candidates");
    assert_eq!(compact.kind, CandidateKind::Conversion);
    assert_eq!(compact.presentation, CandidatePresentation::Compact);
    assert_eq!(
        compact.visible_range(),
        usize::from(compact.selected)..usize::from(compact.selected) + 1
    );
    let selected = compact.selected;
    let count = compact.items.len();

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Tab),
        },
        &mut out,
    );
    let expanded = out.to_output().candidates.expect("expanded candidates");
    assert!(!out.beep);
    assert_eq!(expanded.kind, CandidateKind::Conversion);
    assert_eq!(expanded.presentation, CandidatePresentation::Expanded);
    assert_eq!(expanded.selected, selected);
    assert_eq!(expanded.items.len(), count);
    assert_eq!(expanded.visible_range(), expanded.current_page_range());

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Tab),
        },
        &mut out,
    );
    let repeated = out.to_output().candidates.expect("expanded candidates");
    assert!(!out.beep, "repeated expansion is an idempotent success");
    assert_eq!(repeated, expanded);
    assert_eq!(
        dispatcher.sessions.get(session).expect("session").state(),
        State::Converting
    );
}

#[test]
fn unfocused_conversion_number_commits_current_and_starts_literal_digit() {
    let mut dispatcher = numeric_focus_conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "numeric-focus.exe");
    type_word(&mut dispatcher, session, "joui", &mut out);

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    assert_eq!(out.preedit_text(), "上位");
    assert!(!dispatcher
        .sessions
        .get(session)
        .expect("unfocused conversion")
        .conversion_focused());

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: char_key('3'),
        },
        &mut out,
    );

    assert!(!out.beep);
    assert_eq!(out.commit_text(), Some("上位"));
    assert_eq!(out.preedit_text(), "3");
    assert_eq!(
        dispatcher
            .sessions
            .get(session)
            .expect("literal continuation")
            .state(),
        State::Composing
    );
}

#[test]
fn unfocused_conversion_number_sequence_stays_literal_after_first_commit() {
    let mut dispatcher = numeric_focus_conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "numeric-sequence.exe");
    type_word(&mut dispatcher, session, "chokkin", &mut out);

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    assert_eq!(out.preedit_text(), "直近");
    assert!(!dispatcher
        .sessions
        .get(session)
        .expect("unfocused conversion")
        .conversion_focused());

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: char_key('9'),
        },
        &mut out,
    );
    assert_eq!(out.commit_text(), Some("直近"));
    assert_eq!(out.preedit_text(), "9");

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: char_key('0'),
        },
        &mut out,
    );
    assert!(!out.beep);
    assert_eq!(out.commit_text(), None);
    assert_eq!(out.preedit_text(), "90");
    assert_eq!(
        dispatcher
            .sessions
            .get(session)
            .expect("literal sequence")
            .state(),
        State::Composing
    );
}

#[test]
fn focused_conversion_number_selects_visible_slot() {
    let mut dispatcher = numeric_focus_conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "focused-number.exe");
    type_word(&mut dispatcher, session, "joui", &mut out);

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Tab),
        },
        &mut out,
    );
    let focused = dispatcher
        .sessions
        .get(session)
        .expect("focused conversion");
    assert!(focused.conversion_focused());
    assert_eq!(
        out.to_output()
            .candidates
            .expect("expanded candidates")
            .presentation,
        CandidatePresentation::Expanded
    );

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: char_key('3'),
        },
        &mut out,
    );
    assert!(!out.beep);
    assert_eq!(out.commit_text(), Some("上位候補三"));
    assert_eq!(out.preedit_text(), "");
    assert_eq!(
        dispatcher
            .sessions
            .get(session)
            .expect("committed conversion")
            .state(),
        State::Idle
    );
}

#[test]
fn focused_conversion_invalid_number_beeps_and_preserves_the_list() {
    let mut dispatcher = numeric_focus_conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "invalid-number.exe");
    type_word(&mut dispatcher, session, "joui", &mut out);

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Tab),
        },
        &mut out,
    );
    let before = out.preedit_text().to_owned();
    let candidate_count = out.candidate_count();

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: char_key('9'),
        },
        &mut out,
    );

    assert!(out.beep);
    assert_eq!(out.commit_text(), None);
    assert_eq!(out.preedit_text(), before);
    assert_eq!(out.candidate_count(), candidate_count);
    let live = dispatcher
        .sessions
        .get(session)
        .expect("preserved conversion");
    assert_eq!(live.state(), State::Converting);
    assert!(live.conversion_focused());
}

#[test]
fn conversion_navigation_expands_without_changing_candidate_kind() {
    let mut dispatcher = conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "editor.exe");
    type_word(&mut dispatcher, session, "kana", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    assert_eq!(
        out.to_output()
            .candidates
            .expect("compact candidates")
            .presentation,
        CandidatePresentation::Compact
    );

    for code in [
        KeyCode::Down,
        KeyCode::Up,
        KeyCode::PageDown,
        KeyCode::PageUp,
    ] {
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(code),
            },
            &mut out,
        );
        let candidates = out.to_output().candidates.expect("navigation candidates");
        assert_eq!(candidates.kind, CandidateKind::Conversion);
        assert_eq!(candidates.presentation, CandidatePresentation::Expanded);
        assert_eq!(candidates.visible_range(), candidates.current_page_range());
    }
}

#[test]
fn custom_candidate_expand_outside_conversion_beeps_without_mutating_preedit() {
    let table = Table::builtin().expect("builtin table");
    let keymap = KeyMap::parse("[composing]\ntab = \"candidate_expand\"\n").expect("custom keymap");
    let mut dispatcher = Dispatcher::with_parts(table, keymap, Normalizer::default());
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "editor.exe");
    type_word(&mut dispatcher, session, "kana", &mut out);
    let before = out.preedit_text().to_owned();

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Tab),
        },
        &mut out,
    );

    assert!(out.consumed);
    assert!(out.beep);
    assert_eq!(out.preedit_text(), before);
    assert!(!out.has_candidates());
    assert_eq!(
        dispatcher.sessions.get(session).expect("session").state(),
        State::Composing
    );
}

#[test]
fn microsoft_numbers_type_until_the_suggestion_list_takes_focus() {
    let (mut dispatcher, runtime) = prediction_dispatcher();
    let mut out = OutputBuf::new();

    // A visible-but-unfocused suggestion list is what Microsoft IME shows
    // while the user is still typing. A number there is text, not a
    // shortcut: taking it as a shortcut made `２` + `2` commit the second
    // suggestion instead of producing `２２`.
    let typing = create_session(&mut dispatcher, &mut out, "microsoft-typing.exe");
    type_word(&mut dispatcher, typing, "kana", &mut out);
    let composed = out.preedit_text().to_owned();
    let suggestion = out.candidate(1).expect("second suggestion").0.to_owned();
    assert_eq!(out.candidate_kind(), Some(CandidateKind::Suggestion));
    assert_eq!(
        dispatcher.sessions.get(typing).expect("session").state(),
        State::Composing
    );

    dispatcher.dispatch(
        &Request::SendKey {
            session: typing,
            key: char_key('2'),
        },
        &mut out,
    );
    assert!(!out.beep);
    assert_eq!(out.commit_text(), None, "a number must not commit here");
    assert_ne!(out.preedit_text(), suggestion);
    assert!(
        out.preedit_text().starts_with(composed.as_str())
            && out.preedit_text().len() > composed.len(),
        "the number extends the composition: {composed} -> {}",
        out.preedit_text()
    );
    assert_eq!(
        dispatcher.sessions.get(typing).expect("session").state(),
        State::Composing
    );

    // Tab focuses the list, which is exactly when Microsoft IME starts
    // honouring the numbers it draws beside each suggestion.
    let focused = create_session(&mut dispatcher, &mut out, "microsoft-focused.exe");
    type_word(&mut dispatcher, focused, "kana", &mut out);
    let expected = out.candidate(1).expect("second suggestion").0.to_owned();
    dispatcher.dispatch(
        &Request::SendKey {
            session: focused,
            key: named_key(KeyCode::Tab),
        },
        &mut out,
    );
    assert_eq!(
        dispatcher.sessions.get(focused).expect("session").state(),
        State::Predicting
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session: focused,
            key: char_key('2'),
        },
        &mut out,
    );
    assert!(!out.beep);
    assert_eq!(out.commit_text(), Some(expected.as_str()));
    assert_eq!(
        dispatcher.sessions.get(focused).expect("session").state(),
        State::Idle
    );

    // A slot the focused list does not have stays a recoverable beep.
    let invalid = create_session(&mut dispatcher, &mut out, "microsoft-invalid.exe");
    type_word(&mut dispatcher, invalid, "kana", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session: invalid,
            key: named_key(KeyCode::Tab),
        },
        &mut out,
    );
    let before = out.preedit_text().to_owned();
    let candidate_count = out.candidate_count();
    dispatcher.dispatch(
        &Request::SendKey {
            session: invalid,
            key: char_key('9'),
        },
        &mut out,
    );
    assert!(out.beep);
    assert_eq!(out.commit_text(), None);
    assert_eq!(out.preedit_text(), before);
    assert_eq!(out.candidate_count(), candidate_count);
    assert_eq!(
        dispatcher.sessions.get(invalid).expect("session").state(),
        State::Predicting
    );
    runtime.stop().expect("prediction worker joins");
}

#[test]
fn prediction_history_deletion_rejects_unfocused_system_and_user_candidates() {
    let (mut dispatcher, runtime) = prediction_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
    type_word(&mut dispatcher, session, "kana", &mut out);
    let before = out.preedit_text().to_owned();

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: modified_named_key(KeyCode::Delete, Modifiers::CTRL),
        },
        &mut out,
    );
    assert!(
        out.beep,
        "deletion without a focused prediction is rejected"
    );
    assert_eq!(out.preedit_text(), before);

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Tab),
        },
        &mut out,
    );
    let focused = out.preedit_text().to_owned();
    let generation = dispatcher
        .sessions
        .get(session)
        .expect("session")
        .prediction_generation;
    assert_eq!(
        dispatcher
            .prediction_cache
            .candidates(session, generation)
            .expect("system candidates")[0]
            .source(),
        PredictionSource::System
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: modified_named_key(KeyCode::Delete, Modifiers::CTRL),
        },
        &mut out,
    );
    assert!(out.beep, "system candidates are never deletable");
    assert_eq!(out.preedit_text(), focused);
    runtime.stop().expect("prediction worker joins");

    let conversion = phase_one_prediction_conversion();
    conversion.replace_user_dictionary(
        UserDictionary::parse_tsv(
            "reading\tsurface\tpos\tcomment\n\u{304b}\u{306a}\tuser-only\tnoun\tuser\n",
        )
        .expect("user dictionary"),
    );
    let learning = Arc::new(LearningService::memory());
    let (mut dispatcher, runtime) = phase_one_prediction_dispatcher(conversion, learning);
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
    type_word(&mut dispatcher, session, "kana", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Tab),
        },
        &mut out,
    );
    let focused = out.preedit_text().to_owned();
    let generation = dispatcher
        .sessions
        .get(session)
        .expect("session")
        .prediction_generation;
    assert_eq!(
        dispatcher
            .prediction_cache
            .candidates(session, generation)
            .expect("user candidates")[0]
            .source(),
        PredictionSource::User
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: modified_named_key(KeyCode::Delete, Modifiers::CTRL),
        },
        &mut out,
    );
    assert!(out.beep, "user dictionary candidates are never deletable");
    assert_eq!(out.preedit_text(), focused);
    runtime.stop().expect("prediction worker joins");
}

#[test]
fn focused_history_deletion_committed_observation_failure_refreshes_dispatch_and_restart() {
    let path = phase_one_learning_path("forget-history-committed-observation-failure");
    let temporary = path.with_extension("forget.tmp");
    let recovery = path.with_extension("forget.recovery");
    let learning = Arc::new(LearningService::open(&path).expect("durable learning"));
    let reading = "\u{304b}\u{306a}";
    learning.learn(reading, "history-only", 0, 7);
    let learning_generation = learning.generation();
    let (mut dispatcher, runtime) =
        phase_one_prediction_dispatcher(phase_one_prediction_conversion(), Arc::clone(&learning));
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
    type_word(&mut dispatcher, session, "kana", &mut out);
    let prediction_generation = dispatcher
        .sessions
        .get(session)
        .expect("session")
        .prediction_generation;
    assert_eq!(
        dispatcher
            .prediction_cache
            .candidates(session, prediction_generation)
            .expect("history candidates")[0]
            .source(),
        PredictionSource::History
    );
    let before = out.preedit_text().to_owned();

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Tab),
        },
        &mut out,
    );
    let fault = crate::learning::ForgetPredictionCommittedObservationFault::install();
    let reply = dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: modified_named_key(KeyCode::Delete, Modifiers::CTRL),
        },
        &mut out,
    );
    drop(fault);

    assert_eq!(reply, Reply::Output, "the bound Ctrl+Delete is terminal");
    assert!(out.consumed);
    assert!(!out.beep);
    assert_eq!(out.preedit_text(), before);
    assert_eq!(learning.generation(), learning_generation + 1);
    assert_eq!(learning.maintenance_failures(), 1);
    let refreshed_generation = dispatcher
        .sessions
        .get(session)
        .expect("session")
        .prediction_generation;
    assert_ne!(refreshed_generation, prediction_generation);
    assert!(
        dispatcher
            .prediction_cache
            .candidates(session, refreshed_generation)
            .into_iter()
            .flatten()
            .all(|candidate| candidate.surface() != "history-only"),
        "the refreshed list must never replay the removed history entry"
    );
    assert!(
        out.to_output()
            .candidates
            .expect("refreshed output candidates")
            .items
            .iter()
            .all(|candidate| candidate.text != "history-only"),
        "the visible list must reflect the committed deletion"
    );
    assert!(path.exists(), "the filtered replacement is canonical");
    assert!(!temporary.exists(), "the replacement temp was consumed");
    assert!(!recovery.exists(), "the old backup cleanup completed");
    let mut live_history = false;
    learning.visit_prediction_history(reading, |candidate_reading, surface, _, _| {
        live_history |= candidate_reading == reading && surface == "history-only";
        true
    });
    assert!(!live_history, "live history commits the durable removal");

    runtime.stop().expect("prediction worker joins");
    drop(dispatcher);
    drop(learning);
    let reopened = LearningService::open(&path).expect("reopen learning");
    let mut found = false;
    reopened.visit_prediction_history(reading, |candidate_reading, surface, _, _| {
        found |= candidate_reading == reading && surface == "history-only";
        true
    });
    assert!(
        !found,
        "the removed history pair must stay absent after reopen"
    );
    drop(reopened);
    std::fs::remove_dir_all(path.parent().expect("test directory"))
        .expect("remove learning test directory");
}

#[test]
fn focused_history_deletion_after_first_rename_observation_failure_preserves_dispatch_and_restart()
{
    let path = phase_one_learning_path("forget-history-recovery-failure");
    let temporary = path.with_extension("forget.tmp");
    let recovery = path.with_extension("forget.recovery");
    let learning = Arc::new(LearningService::open(&path).expect("durable learning"));
    let reading = "\u{304b}\u{306a}";
    learning.learn(reading, "history-only", 0, 7);
    let old_bytes = std::fs::read(&path).expect("old canonical bytes");
    let learning_generation = learning.generation();
    let (mut dispatcher, runtime) =
        phase_one_prediction_dispatcher(phase_one_prediction_conversion(), Arc::clone(&learning));
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
    type_word(&mut dispatcher, session, "kana", &mut out);
    let prediction_generation = dispatcher
        .sessions
        .get(session)
        .expect("session")
        .prediction_generation;
    let cached_before = dispatcher
        .prediction_cache
        .candidates(session, prediction_generation)
        .expect("history candidates")
        .to_vec();
    assert_eq!(cached_before[0].source(), PredictionSource::History);

    assert_eq!(
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Tab),
            },
            &mut out,
        ),
        Reply::Output
    );
    assert!(
        dispatcher
            .sessions
            .get(session)
            .expect("session")
            .suggestion_focused,
        "Ctrl+Delete must target the focused history candidate"
    );
    let focused = out.preedit_text().to_owned();
    let listed_before = out
        .to_output()
        .candidates
        .expect("focused history candidate list");

    let fault = crate::learning::ForgetPredictionDeepRecoveryFault::install();
    let reply = dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: modified_named_key(KeyCode::Delete, Modifiers::CTRL),
        },
        &mut out,
    );
    drop(fault);

    assert_eq!(reply, Reply::Output, "the bound Ctrl+Delete is terminal");
    assert!(out.consumed, "the failed deletion remains consumed");
    assert!(out.beep, "the failed deletion is observable");
    assert_eq!(out.preedit_text(), focused);
    assert_eq!(out.commit_text(), None);
    assert_eq!(learning.generation(), learning_generation);
    assert_eq!(
        dispatcher
            .sessions
            .get(session)
            .expect("session")
            .prediction_generation,
        prediction_generation,
        "durable failure must not invalidate the prediction generation"
    );
    assert_eq!(
        dispatcher
            .prediction_cache
            .candidates(session, prediction_generation)
            .expect("preserved history cache"),
        cached_before.as_slice(),
        "the exact history candidate remains cached"
    );
    assert_eq!(
        out.to_output()
            .candidates
            .expect("preserved history candidate list"),
        listed_before,
        "the rendered output list remains authoritative"
    );
    let mut in_memory = false;
    learning.visit_prediction_history(reading, |candidate_reading, surface, _, _| {
        in_memory |= candidate_reading == reading && surface == "history-only";
        true
    });
    assert!(in_memory, "the old history remains in memory");
    assert!(
        !path.exists(),
        "failed immediate recovery must not recreate the canonical log"
    );
    assert_eq!(
        std::fs::read(&recovery).expect("old recovery bytes"),
        old_bytes,
        "the old canonical log is the deterministic recovery authority"
    );
    assert!(
        temporary.exists(),
        "the filtered replacement remains cleanup-only until recovery"
    );

    runtime.stop().expect("prediction worker joins");
    drop(dispatcher);
    drop(learning);

    let reopened = LearningService::open(&path).expect("restart restores old history");
    assert!(path.exists(), "startup restores the canonical log first");
    assert!(!recovery.exists(), "recovery was consumed at startup");
    assert_eq!(
        std::fs::read(&path).expect("restored canonical bytes"),
        old_bytes
    );
    let mut restored = false;
    reopened.visit_prediction_history(reading, |candidate_reading, surface, _, _| {
        restored |= candidate_reading == reading && surface == "history-only";
        true
    });
    assert!(restored, "restart restores the original history entry");
    reopened
        .maintain()
        .expect("cleanup stale filtered replacement");
    assert!(
        !temporary.exists(),
        "recovery cleanup removes the stale temp"
    );
    drop(reopened);
    std::fs::remove_dir_all(path.parent().expect("test directory"))
        .expect("remove learning test directory");
}

#[test]
fn suggestions_focus_cycle_escape_and_commit_without_becoming_conversion_candidates() {
    let (mut dispatcher, runtime) = prediction_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
    type_word(&mut dispatcher, session, "kana", &mut out);
    let raw_reading = out.preedit_text().to_owned();

    assert_eq!(out.preedit_text(), raw_reading);
    assert_eq!(out.candidate_kind(), Some(CandidateKind::Suggestion));
    assert_eq!(out.candidate_count(), 3);
    assert_eq!(out.candidate(0), Some(("仮名", "common")));
    assert_eq!(
        dispatcher.sessions.get(session).unwrap().state(),
        State::Composing
    );

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Tab),
        },
        &mut out,
    );
    assert_eq!(
        dispatcher.sessions.get(session).unwrap().state(),
        State::Predicting
    );
    assert_eq!(out.selected_candidate(), Some(0));
    let first_surface = out
        .candidate(0)
        .expect("first suggestion surface")
        .0
        .to_owned();
    assert_ne!(first_surface, raw_reading);
    assert_eq!(out.preedit_text(), first_surface);
    assert_eq!(
        dispatcher.sessions.get(session).unwrap().preedit.as_str(),
        raw_reading
    );

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Tab),
        },
        &mut out,
    );
    assert_eq!(out.selected_candidate(), Some(1));
    let second_surface = out
        .candidate(1)
        .expect("second suggestion surface")
        .0
        .to_owned();
    assert_eq!(out.preedit_text(), second_surface);

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: modified_named_key(KeyCode::Tab, Modifiers::SHIFT),
        },
        &mut out,
    );
    assert_eq!(out.selected_candidate(), Some(0));
    assert_eq!(out.preedit_text(), first_surface);

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Escape),
        },
        &mut out,
    );
    assert_eq!(
        dispatcher.sessions.get(session).unwrap().state(),
        State::Composing
    );
    assert!(!out.has_candidates());
    assert_eq!(out.preedit_text(), raw_reading);
    assert_eq!(out.preedit_text(), "かな");

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Tab),
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Enter),
        },
        &mut out,
    );
    assert_eq!(out.commit_text(), Some("仮名"));
    assert_eq!(out.commit_text(), Some(first_surface.as_str()));
    assert_eq!(
        dispatcher.sessions.get(session).unwrap().state(),
        State::Idle
    );
    runtime.stop().expect("prediction worker joins");
}

#[test]
fn prediction_input_history_uses_visible_projection() {
    let path = phase_one_learning_path("prediction-projection-history");
    let history = InputHistoryService::open(&path).expect("history");
    let learning = Arc::new(LearningService::memory());
    let (mut dispatcher, runtime) =
        phase_one_prediction_dispatcher(phase_one_prediction_conversion(), learning);
    dispatcher.set_input_history(Some(Arc::clone(&history)));
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "history-projection.exe");
    dispatcher.dispatch(
        &Request::SetInputScope {
            session,
            scope: InputScope::Normal,
        },
        &mut out,
    );
    type_word(&mut dispatcher, session, "kana", &mut out);

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Tab),
        },
        &mut out,
    );
    let first_surface = out
        .candidate(0)
        .expect("first suggestion surface")
        .0
        .to_owned();
    assert_eq!(out.preedit_text(), first_surface);

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Tab),
        },
        &mut out,
    );
    let second_surface = out
        .candidate(1)
        .expect("second suggestion surface")
        .0
        .to_owned();
    assert_eq!(out.preedit_text(), second_surface);
    history.flush().expect("flush projection history");

    let snapshot = crate::input_history::read_snapshot(&path).expect("history snapshot");
    let prediction_keys: Vec<_> = snapshot
        .records
        .iter()
        .filter_map(|record| match record {
            InputHistoryRecord::Key(record) if record.action == "predict_next" => Some(record),
            _ => None,
        })
        .collect();
    assert!(prediction_keys.len() >= 2);
    assert_eq!(prediction_keys[0].preedit_after, first_surface);
    assert_eq!(
        prediction_keys[1].preedit_before,
        prediction_keys[0].preedit_after
    );
    assert_eq!(prediction_keys[1].preedit_after, second_surface);

    history.stop().expect("stop history");
    runtime.stop().expect("prediction worker joins");
    std::fs::remove_dir_all(path.parent().expect("history root"))
        .expect("remove history directory");
}

#[test]
fn first_prediction_tab_skips_an_identity_candidate() {
    let learning = Arc::new(LearningService::memory());
    let (mut dispatcher, runtime) =
        phase_one_prediction_dispatcher(equal_reading_prediction_conversion(), learning);
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "equal-reading.exe");
    type_word(&mut dispatcher, session, "kana", &mut out);
    let reading = out.preedit_text().to_owned();

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Tab),
        },
        &mut out,
    );
    assert_eq!(
        dispatcher.sessions.get(session).unwrap().state(),
        State::Predicting
    );
    assert_eq!(out.selected_candidate(), Some(1));
    assert_ne!(out.preedit_text(), reading);
    let distinct = out.preedit_text().to_owned();

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Enter),
        },
        &mut out,
    );
    assert_eq!(out.commit_text(), Some(distinct.as_str()));
    assert_eq!(
        dispatcher.sessions.get(session).unwrap().state(),
        State::Idle
    );
    runtime.stop().expect("prediction worker joins");
}

#[test]
fn candidate_click_command_commits_the_exact_prediction_index() {
    let learning = Arc::new(LearningService::memory());
    let (mut dispatcher, runtime) =
        phase_one_prediction_dispatcher(equal_reading_prediction_conversion(), learning);
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "prediction-click.exe");
    dispatcher.dispatch(
        &Request::SetInputScope {
            session,
            scope: InputScope::Normal,
        },
        &mut out,
    );
    type_word(&mut dispatcher, session, "kana", &mut out);
    let clicked = out.candidate(1).expect("distinct prediction").0.to_owned();

    let reply = dispatcher.dispatch(
        &Request::CommitCandidate {
            session,
            revision: 41,
            candidate_index: 1,
        },
        &mut out,
    );

    assert_eq!(reply, Reply::Output);
    assert_eq!(out.commit_text(), Some(clicked.as_str()));
    assert_eq!(
        dispatcher.sessions.get(session).unwrap().state(),
        State::Idle
    );
    runtime.stop().expect("prediction worker joins");
}

#[test]
fn candidate_click_command_commits_the_exact_conversion_index() {
    let mut dispatcher = conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "conversion-click.exe");
    dispatcher.dispatch(
        &Request::SetInputScope {
            session,
            scope: InputScope::Normal,
        },
        &mut out,
    );
    type_word(&mut dispatcher, session, "kana", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    let clicked = out.candidate(1).expect("second conversion").0.to_owned();

    let reply = dispatcher.dispatch(
        &Request::CommitCandidate {
            session,
            revision: 42,
            candidate_index: 1,
        },
        &mut out,
    );

    assert_eq!(reply, Reply::Output);
    assert_eq!(out.commit_text(), Some(clicked.as_str()));
    assert_eq!(
        dispatcher.sessions.get(session).unwrap().state(),
        State::Idle
    );
}

#[test]
fn conversion_projection_deduplicates_visible_width_variants_and_navigation_changes_preedit() {
    let mut dispatcher = visible_projection_conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(
        &mut dispatcher,
        &mut out,
        "visible-projection-navigation.exe",
    );
    type_word(&mut dispatcher, session, "daisanban", &mut out);

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    assert_eq!(out.preedit_text(), "第3番");
    let visible = (0..out.candidate_count())
        .filter_map(|index| out.candidate(index).map(|(text, _)| text.to_owned()))
        .collect::<Vec<_>>();
    assert_eq!(
        visible
            .iter()
            .filter(|text| text.as_str() == "第3番")
            .count(),
        1
    );
    assert_eq!(
        visible
            .iter()
            .filter(|text| text.as_str() == "第３番")
            .count(),
        0
    );
    assert!(visible.iter().any(|text| text == "別候補"));

    let first = out.preedit_text().to_owned();
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Down),
        },
        &mut out,
    );
    assert_eq!(out.selected_candidate(), Some(1));
    assert_ne!(out.preedit_text(), first);
    assert_eq!(out.preedit_text(), "別候補");
}

#[test]
fn conversion_projection_number_and_renderer_click_commit_displayed_identity() {
    let mut dispatcher = visible_projection_conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "visible-projection-number.exe");
    type_word(&mut dispatcher, session, "daisanban", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Tab),
        },
        &mut out,
    );
    let numbered = out
        .candidate(1)
        .expect("second visible candidate")
        .0
        .to_owned();
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: char_key('2'),
        },
        &mut out,
    );
    assert_eq!(out.commit_text(), Some(numbered.as_str()));

    let mut dispatcher = visible_projection_conversion_dispatcher();
    let session = create_session(&mut dispatcher, &mut out, "visible-projection-click.exe");
    dispatcher.dispatch(
        &Request::SetInputScope {
            session,
            scope: InputScope::Normal,
        },
        &mut out,
    );
    type_word(&mut dispatcher, session, "daisanban", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Tab),
        },
        &mut out,
    );
    let clicked = out
        .candidate(1)
        .expect("second visible candidate")
        .0
        .to_owned();
    let reply = dispatcher.dispatch(
        &Request::CommitCandidate {
            session,
            revision: 43,
            candidate_index: 1,
        },
        &mut out,
    );
    assert_eq!(reply, Reply::Output);
    assert_eq!(out.commit_text(), Some(clicked.as_str()));
    assert_eq!(
        dispatcher.sessions.get(session).unwrap().state(),
        State::Idle
    );
}

#[test]
fn conversion_projection_keeps_selected_detail_on_first_raw_representative() {
    let mut dispatcher = visible_projection_conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "visible-projection-detail.exe");
    type_word(&mut dispatcher, session, "daisanban", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    let first_detail = out
        .to_output()
        .candidate_detail
        .expect("detail for first raw representative");
    assert_eq!(
        first_detail.definition,
        "Stable first representative detail."
    );
    assert_eq!(first_detail.reading, "だいさんばん");
    assert_eq!(out.preedit_text(), out.candidate(0).expect("first row").0);

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Down),
        },
        &mut out,
    );
    let second_detail = out
        .to_output()
        .candidate_detail
        .expect("detail for second visible candidate");
    assert_eq!(second_detail.definition, "Second visible candidate detail.");
    assert_eq!(second_detail.reading, "だいさんばん");
    assert_eq!(out.preedit_text(), out.candidate(1).expect("second row").0);
}

#[test]
fn prediction_explicit_retry_is_bounded_per_generation() {
    let learning = Arc::new(LearningService::memory());
    let (mut dispatcher, runtime) =
        phase_one_prediction_dispatcher(empty_prediction_conversion(), learning);
    let prediction = runtime.service();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "empty-prediction.exe");
    type_word(&mut dispatcher, session, "kana", &mut out);
    let automatic_requests = prediction.request_count();
    assert!(!out.has_candidates());

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Tab),
        },
        &mut out,
    );
    let retry_requests = prediction.request_count();
    assert_eq!(retry_requests, automatic_requests + 1);
    assert!(out.consumed);
    assert!(out.beep);
    assert!(!out.has_candidates());
    assert_eq!(
        dispatcher.sessions.get(session).unwrap().state(),
        State::Composing
    );

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Tab),
        },
        &mut out,
    );
    assert_eq!(prediction.request_count(), retry_requests);
    assert!(out.consumed);
    assert!(out.beep);
    assert_eq!(
        dispatcher.sessions.get(session).unwrap().state(),
        State::Composing
    );
    runtime.stop().expect("prediction worker joins");
}

#[test]
fn prediction_explicit_retry_success_focuses_same_key_without_second_retry() {
    let learning = Arc::new(LearningService::memory());
    let (mut dispatcher, runtime) =
        phase_one_prediction_dispatcher(empty_prediction_conversion(), learning);
    let prediction = runtime.service();
    prediction.test_script_prediction("\u{304b}", "\u{58f2}\u{4e0a}");
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "retry-success.exe");
    type_word(&mut dispatcher, session, "kana", &mut out);
    let automatic_requests = prediction.request_count();
    assert!(automatic_requests > 0);
    assert!(!out.has_candidates());
    let generation = dispatcher
        .sessions
        .get(session)
        .unwrap()
        .prediction_generation;
    assert!(dispatcher
        .prediction_cache
        .attempted_for(session, generation));

    prediction.test_set_scripted_prediction_available(true);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Tab),
        },
        &mut out,
    );
    assert_eq!(prediction.request_count(), automatic_requests + 1);
    assert!(out.consumed);
    assert!(!out.beep);
    assert_eq!(out.selected_candidate(), Some(0));
    assert_eq!(
        dispatcher.sessions.get(session).unwrap().state(),
        State::Predicting
    );
    let selected_surface = out
        .candidate(0)
        .expect("successful retry candidate")
        .0
        .to_owned();
    assert_eq!(out.preedit_text(), selected_surface);
    assert!(dispatcher.prediction_cache.explicit_retry_attempted);

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Tab),
        },
        &mut out,
    );
    assert_eq!(prediction.request_count(), automatic_requests + 1);
    assert_eq!(out.selected_candidate(), Some(0));
    assert_eq!(out.preedit_text(), selected_surface);
    runtime.stop().expect("prediction worker joins");
}

#[test]
fn prediction_live_cache_authority_loss_returns_to_raw_composing() {
    let learning = Arc::new(LearningService::memory());
    let (mut dispatcher, runtime) =
        phase_one_prediction_dispatcher(phase_one_prediction_conversion(), learning);
    let prediction = runtime.service();
    prediction.test_script_prediction("\u{304b}", "\u{58f2}\u{4e0a}");
    prediction.test_set_scripted_prediction_available(true);
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "authority-loss.exe");
    type_word(&mut dispatcher, session, "kana", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Tab),
        },
        &mut out,
    );
    let focused_surface = out.preedit_text().to_owned();
    let raw_session = dispatcher
        .sessions
        .get(session)
        .unwrap()
        .preedit
        .as_str()
        .to_owned();
    assert_eq!(
        dispatcher.sessions.get(session).unwrap().state(),
        State::Predicting
    );

    prediction.test_set_scripted_prediction_available(false);
    dispatcher.prediction_cache.clear();
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: modified_named_key(KeyCode::Delete, Modifiers::CTRL),
        },
        &mut out,
    );
    let after = dispatcher.sessions.get(session).unwrap();
    assert_eq!(after.state(), State::Composing);
    assert!(!after.suggestion_focused);
    assert!(!after.suggestions_visible);
    assert!(!out.beep, "Composing must not run Predicting-only deletion");
    assert_eq!(after.preedit.as_str(), raw_session);
    assert_eq!(out.preedit_text(), raw_preedit(&mut dispatcher, session));
    assert_ne!(out.preedit_text(), focused_surface);

    // Enter is the strongest commit boundary: after stale focus is
    // cleared, it must follow the ordinary Composing commit path rather
    // than trying to commit a missing prediction candidate.
    prediction.test_set_scripted_prediction_available(true);
    let enter_session = create_session(&mut dispatcher, &mut out, "authority-enter.exe");
    type_word(&mut dispatcher, enter_session, "kana", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session: enter_session,
            key: named_key(KeyCode::Tab),
        },
        &mut out,
    );
    let enter_raw = dispatcher
        .sessions
        .get(enter_session)
        .unwrap()
        .preedit
        .as_str()
        .to_owned();
    prediction.test_set_scripted_prediction_available(false);
    dispatcher.prediction_cache.clear();
    dispatcher.dispatch(
        &Request::SendKey {
            session: enter_session,
            key: named_key(KeyCode::Enter),
        },
        &mut out,
    );
    assert_eq!(out.commit_text(), Some(enter_raw.as_str()));
    assert_eq!(
        dispatcher.sessions.get(enter_session).unwrap().state(),
        State::Idle
    );
    runtime.stop().expect("prediction worker joins");
}

#[test]
fn prediction_focus_is_cleared_when_typing_after_projection() {
    let learning = Arc::new(LearningService::memory());
    let (mut dispatcher, runtime) =
        phase_one_prediction_dispatcher(phase_one_prediction_conversion(), learning);
    let prediction = runtime.service();
    prediction.test_script_prediction("\u{304b}", "\u{58f2}\u{4e0a}");
    prediction.test_set_scripted_prediction_available(true);
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "typing-invalidation.exe");
    type_word(&mut dispatcher, session, "kana", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Tab),
        },
        &mut out,
    );
    let focused_surface = out.preedit_text().to_owned();
    let generation_before = dispatcher
        .sessions
        .get(session)
        .unwrap()
        .prediction_generation;

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: char_key('x'),
        },
        &mut out,
    );
    let rendered_raw = raw_preedit(&mut dispatcher, session);
    let after = dispatcher.sessions.get(session).unwrap();
    assert_ne!(after.prediction_generation, generation_before);
    assert_eq!(after.state(), State::Composing);
    assert!(!after.suggestion_focused);
    assert_eq!(out.preedit_text(), rendered_raw);
    assert_ne!(out.preedit_text(), focused_surface);
    assert!(
        after.suggestions_visible,
        "fresh automatic result remains visible"
    );
    runtime.stop().expect("prediction worker joins");
}

#[test]
fn prediction_focus_is_cleared_when_backspace_after_projection() {
    let learning = Arc::new(LearningService::memory());
    let (mut dispatcher, runtime) =
        phase_one_prediction_dispatcher(phase_one_prediction_conversion(), learning);
    let prediction = runtime.service();
    prediction.test_script_prediction("\u{304b}", "\u{58f2}\u{4e0a}");
    prediction.test_set_scripted_prediction_available(true);
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "backspace-invalidation.exe");
    type_word(&mut dispatcher, session, "kana", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Tab),
        },
        &mut out,
    );
    let focused_surface = out.preedit_text().to_owned();
    let generation_before = dispatcher
        .sessions
        .get(session)
        .unwrap()
        .prediction_generation;

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Backspace),
        },
        &mut out,
    );
    let rendered_raw = raw_preedit(&mut dispatcher, session);
    let after = dispatcher.sessions.get(session).unwrap();
    assert_ne!(after.prediction_generation, generation_before);
    assert_eq!(after.state(), State::Composing);
    assert!(!after.suggestion_focused);
    assert_eq!(out.preedit_text(), rendered_raw);
    assert_ne!(out.preedit_text(), focused_surface);
    assert!(
        after.suggestions_visible,
        "fresh automatic result remains visible"
    );
    runtime.stop().expect("prediction worker joins");
}

#[test]
fn named_shift_enter_commits_top_suggestion_and_shift_space_enters_conversion() {
    let (mut dispatcher, runtime) = prediction_dispatcher();
    let mut out = OutputBuf::new();
    let first = create_session(&mut dispatcher, &mut out, "notepad.exe");
    type_word(&mut dispatcher, first, "kana", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session: first,
            key: modified_named_key(KeyCode::Enter, Modifiers::SHIFT),
        },
        &mut out,
    );
    assert_eq!(out.commit_text(), Some("仮名"));

    let second = create_session(&mut dispatcher, &mut out, "notepad.exe");
    type_word(&mut dispatcher, second, "kana", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session: second,
            key: named_key(KeyCode::Tab),
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session: second,
            key: modified_named_key(KeyCode::Space, Modifiers::SHIFT),
        },
        &mut out,
    );
    assert_eq!(
        dispatcher.sessions.get(second).unwrap().state(),
        State::Converting
    );
    assert_eq!(out.candidate_kind(), Some(CandidateKind::Conversion));
    runtime.stop().expect("prediction worker joins");
}

#[test]
fn visible_suggestions_space_converts_instead_of_committing_the_reading() {
    let (mut dispatcher, runtime) = prediction_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
    type_word(&mut dispatcher, session, "kana", &mut out);
    let reading = out.preedit_text().to_owned();
    assert_eq!(out.candidate_kind(), Some(CandidateKind::Suggestion));
    assert_eq!(
        dispatcher.sessions.get(session).unwrap().state(),
        State::Composing
    );

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );

    assert_eq!(
        out.commit_text(),
        None,
        "Space with a visible suggestion list must not commit {reading:?}"
    );
    assert_eq!(
        dispatcher.sessions.get(session).unwrap().state(),
        State::Converting
    );
    assert_eq!(out.candidate_kind(), Some(CandidateKind::Conversion));
    let candidates = out.to_output().candidates.expect("conversion candidates");
    assert_eq!(
        candidates.presentation,
        CandidatePresentation::Compact,
        "履歴一覧からの Space は、履歴なしの通常変換と同じ compact 経路"
    );
    runtime.stop().expect("prediction worker joins");
}

#[test]
fn focused_suggestions_space_converts_instead_of_committing_the_reading() {
    let (mut dispatcher, runtime) = prediction_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
    type_word(&mut dispatcher, session, "kana", &mut out);
    let reading = out.preedit_text().to_owned();
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Tab),
        },
        &mut out,
    );
    assert_eq!(
        dispatcher.sessions.get(session).unwrap().state(),
        State::Predicting
    );
    assert_eq!(out.candidate_kind(), Some(CandidateKind::Suggestion));

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );

    assert_ne!(
        out.commit_text(),
        Some(reading.as_str()),
        "Space on a focused suggestion must convert, not commit the reading"
    );
    assert_eq!(out.commit_text(), None);
    assert_eq!(
        dispatcher.sessions.get(session).unwrap().state(),
        State::Converting
    );
    assert_eq!(out.candidate_kind(), Some(CandidateKind::Conversion));
    let candidates = out.to_output().candidates.expect("conversion candidates");
    assert_eq!(
        candidates.presentation,
        CandidatePresentation::Compact,
        "Tab 選択中の Space も通常変換と同じ compact 経路"
    );
    runtime.stop().expect("prediction worker joins");
}

#[test]
fn history_prefix_space_converts_the_typed_reading_not_the_longer_history() {
    let learning = Arc::new(LearningService::memory());
    learning.learn(
        "\u{306b}\u{307b}\u{3093}\u{3054}",
        "\u{65e5}\u{672c}\u{8a9e}",
        0,
        0,
    );
    let (mut dispatcher, runtime) =
        phase_one_prediction_dispatcher(nihongo_history_conversion(), Arc::clone(&learning));
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
    type_word(&mut dispatcher, session, "ni", &mut out);
    assert_eq!(out.preedit_text(), "\u{306b}");
    assert_eq!(out.candidate_kind(), Some(CandidateKind::Suggestion));
    assert_eq!(
        out.candidate(0).map(|(text, _)| text),
        Some("\u{65e5}\u{672c}\u{8a9e}")
    );
    assert_eq!(
        dispatcher.sessions.get(session).unwrap().state(),
        State::Composing
    );

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );

    assert_eq!(out.commit_text(), None);
    assert_eq!(
        dispatcher.sessions.get(session).unwrap().state(),
        State::Converting
    );
    assert_eq!(out.candidate_kind(), Some(CandidateKind::Conversion));
    assert_eq!(
        out.preedit_text(),
        "\u{4e8c}",
        "prefix Space must convert the typed に, not adopt 履歴「日本語」"
    );
    runtime.stop().expect("prediction worker joins");
}

#[test]
fn exact_history_space_still_converts_the_typed_reading() {
    let learning = Arc::new(LearningService::memory());
    learning.learn(
        "\u{306b}\u{307b}\u{3093}\u{3054}\u{306b}\u{3085}\u{3046}\u{308a}\u{3087}\u{304f}",
        "\u{65e5}\u{672c}\u{8a9e}\u{5165}\u{529b}",
        0,
        0,
    );
    learning.learn(
        "\u{306b}\u{307b}\u{3093}\u{3054}",
        "\u{65e5}\u{672c}\u{8a9e}",
        0,
        0,
    );
    let (mut dispatcher, runtime) =
        phase_one_prediction_dispatcher(nihongo_history_conversion(), learning);
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
    type_word(&mut dispatcher, session, "nihongo", &mut out);
    assert_eq!(out.preedit_text(), "\u{306b}\u{307b}\u{3093}\u{3054}");
    assert_eq!(out.candidate_kind(), Some(CandidateKind::Suggestion));

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );

    assert_eq!(out.commit_text(), None);
    assert_eq!(
        dispatcher.sessions.get(session).unwrap().state(),
        State::Converting
    );
    assert_eq!(out.preedit_text(), "\u{65e5}\u{672c}\u{8a9e}");
    assert_eq!(
        out.candidate(0).map(|(text, _)| text),
        Some("\u{65e5}\u{672c}\u{8a9e}"),
        "typing the full にほんご must convert that reading even when a longer 履歴 exists"
    );
    runtime.stop().expect("prediction worker joins");
}

#[test]
fn identity_history_suggestion_space_still_converts_the_dictionary_surface() {
    let learning = Arc::new(LearningService::memory());
    for _ in 0..3 {
        learning.learn(
            "\u{306b}\u{307b}\u{3093}\u{3054}",
            "\u{306b}\u{307b}\u{3093}\u{3054}",
            0,
            0,
        );
    }
    let (mut dispatcher, runtime) =
        phase_one_prediction_dispatcher(nihongo_history_conversion(), Arc::clone(&learning));
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
    type_word(&mut dispatcher, session, "nihongo", &mut out);
    assert_eq!(out.preedit_text(), "\u{306b}\u{307b}\u{3093}\u{3054}");
    assert_eq!(out.candidate_kind(), Some(CandidateKind::Suggestion));
    assert_eq!(
        out.candidate(0).map(|(text, _)| text),
        Some("\u{306b}\u{307b}\u{3093}\u{3054}"),
        "履歴当性 must still appear as a suggestion"
    );

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );

    assert_eq!(out.commit_text(), None, "Space must not confirm ひらがな");
    assert_eq!(
        dispatcher.sessions.get(session).unwrap().state(),
        State::Converting
    );
    assert_eq!(out.candidate_kind(), Some(CandidateKind::Conversion));
    assert_eq!(out.preedit_text(), "\u{65e5}\u{672c}\u{8a9e}");
    assert_eq!(
        out.candidate(0).map(|(text, _)| text),
        Some("\u{65e5}\u{672c}\u{8a9e}"),
        "Space on a 履歴当性 list must convert with the dictionary, not replay にほんご"
    );
    let candidates = out.to_output().candidates.expect("conversion candidates");
    assert_eq!(
        candidates.presentation,
        CandidatePresentation::Compact,
        "履歴一覧があっても通常変換と同じ compact 経路"
    );
    runtime.stop().expect("prediction worker joins");
}

#[test]
fn identity_dictionary_candidate_is_not_selected_for_history_space() {
    let learning = Arc::new(LearningService::memory());
    for _ in 0..3 {
        learning.learn(
            "\u{306b}\u{307b}\u{3093}\u{304c}",
            "\u{306b}\u{307b}\u{3093}\u{304c}",
            0,
            0,
        );
    }
    let (mut dispatcher, runtime) =
        phase_one_prediction_dispatcher(identity_first_nihongo_conversion(), Arc::clone(&learning));
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
    type_word(&mut dispatcher, session, "nihonga", &mut out);
    assert_eq!(out.preedit_text(), "\u{306b}\u{307b}\u{3093}\u{304c}");
    assert_eq!(out.candidate_kind(), Some(CandidateKind::Suggestion));

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );

    assert_eq!(out.commit_text(), None);
    assert_eq!(out.candidate_kind(), Some(CandidateKind::Conversion));
    assert_eq!(out.preedit_text(), "\u{65e5}\u{672c}\u{8a9e}");
    assert_eq!(out.selected_candidate(), Some(1));
    assert_eq!(
        out.candidate(usize::from(out.selected_candidate().unwrap_or(0)))
            .map(|(text, _)| text),
        Some("\u{65e5}\u{672c}\u{8a9e}"),
        "identity dictionary entry must not win initial Space conversion"
    );
    runtime.stop().expect("prediction worker joins");
}

#[test]
fn typing_konnnichiha_produces_the_hiragana_preedit() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

    type_word(&mut dispatcher, session, "konnnichiha", &mut out);

    assert_eq!(out.preedit_text(), "こんにちは");
}

#[test]
fn enter_commits_the_composition_and_leaves_the_preedit_empty() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
    type_word(&mut dispatcher, session, "konnnichiha", &mut out);

    let reply = dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Enter),
        },
        &mut out,
    );

    assert_eq!(reply, Reply::Output);
    assert!(out.consumed);
    assert_eq!(out.commit_text(), Some("こんにちは"));
    assert_eq!(out.preedit_text(), "");
}

#[test]
fn decimal_period_stays_ascii_after_a_half_width_digit() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

    type_word(&mut dispatcher, session, "1.", &mut out);

    let live = dispatcher.sessions.get(session).expect("session");
    assert_eq!(live.preedit.as_str(), "1.");
    assert_eq!(live.raw_input.as_str(), "1.");
    assert_eq!(out.preedit_text(), "1.");
    assert_eq!(raw_preedit(&mut dispatcher, session), "1.");

    type_word(&mut dispatcher, session, "23", &mut out);
    assert_eq!(out.preedit_text(), "1.23");

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Enter),
        },
        &mut out,
    );
    assert_eq!(out.commit_text(), Some("1.23"));

    // Provenance replay remains correct with a literal decimal `.`:
    // deleting digits and typing another one must retain the same
    // raw/preedit alignment instead of inserting at the old `。` byte
    // boundary.
    let editing = create_session(&mut dispatcher, &mut out, "notepad.exe");
    type_word(&mut dispatcher, editing, "1.23", &mut out);
    for expected in ["1.2", "1."] {
        dispatcher.dispatch(
            &Request::SendKey {
                session: editing,
                key: named_key(KeyCode::Backspace),
            },
            &mut out,
        );
        assert_eq!(out.preedit_text(), expected);
        assert_eq!(
            dispatcher
                .sessions
                .get(editing)
                .expect("editing session")
                .raw_input
                .as_str(),
            expected
        );
    }
    dispatcher.dispatch(
        &Request::SendKey {
            session: editing,
            key: char_key('4'),
        },
        &mut out,
    );
    assert_eq!(out.preedit_text(), "1.4");
    assert_eq!(
        dispatcher
            .sessions
            .get(editing)
            .expect("editing session")
            .raw_input
            .as_str(),
        "1.4"
    );

    // The decision follows the character immediately before the caret,
    // not just the last character of the composition.
    let inserted = create_session(&mut dispatcher, &mut out, "decimal-inserted.exe");
    type_word(&mut dispatcher, inserted, "12", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session: inserted,
            key: named_key(KeyCode::Left),
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session: inserted,
            key: char_key('.'),
        },
        &mut out,
    );
    assert_eq!(out.preedit_text(), "1.2");
    assert_eq!(
        dispatcher
            .sessions
            .get(inserted)
            .expect("inserted session")
            .raw_input
            .as_str(),
        "1.2"
    );

    let ordinary = create_session(&mut dispatcher, &mut out, "decimal-ordinary.exe");
    type_word(&mut dispatcher, ordinary, "a.", &mut out);
    assert_eq!(
        out.preedit_text(),
        "あ。",
        "a period stays Japanese punctuation unless its previous input is a digit"
    );
}

#[test]
fn escape_cancels_the_composition_without_committing() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
    type_word(&mut dispatcher, session, "sakura", &mut out);
    assert!(!out.preedit_text().is_empty());

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Escape),
        },
        &mut out,
    );

    assert_eq!(out.commit_text(), None);
    assert_eq!(out.preedit_text(), "");
}

#[test]
fn backspace_removes_pending_romaji_before_emitted_kana() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
    // "ka" resolves fully to "か" (nothing pending); the trailing "k"
    // then waits on a vowel.
    type_word(&mut dispatcher, session, "kak", &mut out);
    assert_eq!(out.preedit_text(), "かk");

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Backspace),
        },
        &mut out,
    );
    assert_eq!(
        out.preedit_text(),
        "か",
        "first backspace removes the pending romaji"
    );

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Backspace),
        },
        &mut out,
    );
    assert_eq!(
        out.preedit_text(),
        "",
        "second backspace removes the emitted kana"
    );
}

/// #16 finding B: `apply_backspace` used to pop exactly one character of
/// `raw_input` no matter how many keystrokes the deleted kana actually
/// took, leaving stale keystrokes behind until the next `reset()`. Most
/// kana take more than one ASCII character (`ka`, `ki`, ...), so this is
/// the common case, not an edge case.
#[test]
fn backspace_after_a_multi_character_kana_removes_every_keystroke_behind_it() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

    type_word(&mut dispatcher, session, "ka", &mut out);
    assert_eq!(out.preedit_text(), "か");
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Backspace),
        },
        &mut out,
    );
    assert_eq!(out.preedit_text(), "", "the whole kana か is undone");
    let live = dispatcher.sessions.get(session).expect("session");
    assert_eq!(
        live.raw_input.as_str(),
        "",
        "both keystrokes that produced か are undone with it, not just the last one"
    );

    type_word(&mut dispatcher, session, "kaki", &mut out);
    assert_eq!(out.preedit_text(), "かき");
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Backspace),
        },
        &mut out,
    );
    assert_eq!(out.preedit_text(), "か", "only the trailing き is removed");
    let live = dispatcher.sessions.get(session).expect("session");
    assert_eq!(
        live.raw_input.as_str(),
        "ka",
        "raw_input keeps exactly the keystrokes behind the remaining preedit"
    );
}

/// Companion to the multi-character case above: while romaji is still
/// pending (has not resolved into a kana yet), Backspace must keep
/// removing one raw keystroke at a time, exactly as before -- there is no
/// kana to group by yet.
#[test]
fn backspace_over_pending_romaji_still_removes_one_keystroke_at_a_time() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

    type_word(&mut dispatcher, session, "kak", &mut out);
    assert_eq!(out.preedit_text(), "かk");
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Backspace),
        },
        &mut out,
    );
    assert_eq!(out.preedit_text(), "か");
    let live = dispatcher.sessions.get(session).expect("session");
    assert_eq!(
        live.raw_input.as_str(),
        "ka",
        "the pending k is undone alone, not the resolved か behind it"
    );
}

/// Characterizes (issue #16 audit, prerequisite to a B/C fix) what
/// currently happens when the caret moves while romaji is still
/// pending. Every caret motion -- Left, Right, Home, End -- shares one
/// `move_caret` implementation that calls `flush_pending`
/// unconditionally before evaluating the new cursor position. So "type
/// pending romaji, then move the caret" is not a reachable state: the
/// pending romaji is always resolved first (emitted as kana, or passed
/// through raw if it cannot resolve to anything, exactly as an explicit
/// commit would) at the *old* cursor position, and only then does the
/// cursor move. The only reachable order is "move the caret, then type
/// pending romaji" -- which is what the residual-gap tests around this
/// one exercise. Any `raw_input`/caret redesign can therefore ignore
/// "pending survives a caret move" as a case: it cannot occur.
#[test]
fn characterize_caret_movement_while_romaji_is_pending() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

    type_word(&mut dispatcher, session, "sakura", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Home),
        },
        &mut out,
    );
    type_word(&mut dispatcher, session, "k", &mut out);
    let live = dispatcher.sessions.get(session).expect("session");
    assert_eq!(
        live.romaji.pending(),
        "k",
        "k is pending before the caret moves"
    );

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Right),
        },
        &mut out,
    );
    assert_eq!(
        out.preedit_text(),
        "kさくら",
        "the pending k is flushed as a literal raw passthrough character \
             at the old cursor position before the caret moves"
    );
    let live = dispatcher.sessions.get(session).expect("session");
    assert!(
        live.romaji.is_empty(),
        "flush_pending inside move_caret already resolved the pending k \
             -- nothing is pending anymore once the caret has moved"
    );
    assert_eq!(
        live.raw_input.as_str(),
        "ksakura",
        "flush_pending never touches raw_input, only preedit/cursor/romaji"
    );
}

/// #16 finding B/C (residual, found auditing 065200b): the pending-romaji
/// branch of `apply_backspace` assumes whatever is pending sits at the
/// *end* of `raw_input` and pops `raw_input`'s last character
/// unconditionally. That assumption only holds if the pending keystroke
/// was typed with the caret at the true end of the composition. Once the
/// caret has moved (e.g. Home) before the pending keystroke lands,
/// `feed_character` correctly inserts that keystroke's raw character at
/// the caret's raw offset -- not at the end -- so the character
/// `pop_char()` removes is some unrelated, already-resolved keystroke
/// instead of the pending one.
///
/// This is invisible from `out.preedit_text()` alone: `session.preedit`
/// (the resolved kana) is untouched by either the correct or the buggy
/// path, so the on-screen text looks identical either way. Only
/// `raw_input` itself diverges, which is why this assertion has to read
/// `live.raw_input.as_str()` directly rather than trust the rendered
/// text.
#[test]
fn backspace_after_inserting_pending_romaji_at_a_moved_caret_preserves_raw_input_alignment() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

    type_word(&mut dispatcher, session, "sakura", &mut out);
    assert_eq!(out.preedit_text(), "さくら");

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Home),
        },
        &mut out,
    );

    type_word(&mut dispatcher, session, "k", &mut out);
    assert_eq!(
        out.preedit_text(),
        "kさくら",
        "the pending k renders at the caret, ahead of the untouched さくら"
    );
    let live = dispatcher.sessions.get(session).expect("session");
    assert_eq!(live.raw_input.as_str(), "ksakura");
    assert!(
        !live.romaji.is_empty(),
        "k must still be pending, not resolved"
    );

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Backspace),
        },
        &mut out,
    );
    assert_eq!(
        out.preedit_text(),
        "さくら",
        "only the pending k is undone; さくら is untouched on screen"
    );
    let live = dispatcher.sessions.get(session).expect("session");
    assert_eq!(
        live.raw_input.as_str(),
        "sakura",
        "raw_input must lose exactly the pending k, not sakura's trailing a"
    );
    assert!(
        live.romaji.is_empty(),
        "the pending k must be fully consumed by the backspace, not left dangling"
    );
}

/// #16 finding C: `feed_character` used to always *append* to
/// `raw_input` while inserting the resolved kana at the caret, so moving
/// the caret and typing produced a `raw_input` whose keystroke order no
/// longer matched what was on screen. This is the bug report's own
/// example: type "sakura", go Home, type "o" -- the preedit becomes
/// "おさくら" and `raw_input` must read "osakura", not "sakurao".
#[test]
fn typing_after_a_caret_move_inserts_the_keystroke_in_visual_order() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

    type_word(&mut dispatcher, session, "sakura", &mut out);
    assert_eq!(out.preedit_text(), "さくら");
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Home),
        },
        &mut out,
    );
    type_word(&mut dispatcher, session, "o", &mut out);
    assert_eq!(out.preedit_text(), "おさくら");
    let live = dispatcher.sessions.get(session).expect("session");
    assert_eq!(
        live.raw_input.as_str(),
        "osakura",
        "the new keystroke lands where the caret was, not at the end"
    );
}

/// #16 finding C (forward-delete half): `apply_delete_forward` never
/// touched `raw_input` at all, so it went stale on every forward-delete.
#[test]
fn delete_forward_removes_every_keystroke_behind_the_deleted_kana() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

    type_word(&mut dispatcher, session, "kaki", &mut out);
    assert_eq!(out.preedit_text(), "かき");
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Home),
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Delete),
        },
        &mut out,
    );
    assert_eq!(
        out.preedit_text(),
        "き",
        "the leading か is deleted forward"
    );
    let live = dispatcher.sessions.get(session).expect("session");
    assert_eq!(
        live.raw_input.as_str(),
        "ki",
        "both keystrokes behind the deleted か are gone, not just one"
    );
}

/// #16 finding B/C (residual, found auditing 065200b): `apply_delete_forward`
/// has no pending-romaji branch at all -- unlike `apply_backspace`, it
/// unconditionally replays `raw_input` from scratch through
/// `raw_chars_for_emitted` regardless of whether a keystroke is still
/// pending. `raw_chars_for_emitted`'s own soundness argument only covers
/// offsets `move_caret` produces (which always flushes pending first,
/// see its doc comment); an offset taken while pending is non-empty and
/// the caret sits ahead of already-resolved text is outside that
/// contract, and the replay mis-segments the buffer as a result.
///
/// Same setup as the Backspace companion above: type "sakura", Home,
/// type "k" (stays pending, inserted at the caret's raw offset by
/// finding C's fix, landing ahead of "sakura" as "ksakura"). A
/// delete-forward at that point should remove only the resolved kana
/// directly behind the pending k -- さ, produced by raw "sa" -- leaving
/// the pending k and the untouched "kura" behind くら:
/// `raw_input == "kkura"`. The rendered preedit ("kくら") looks correct
/// either way, which is exactly the false-GREEN risk this test guards
/// against by asserting `raw_input` and the pending romaji directly.
#[test]
fn delete_after_inserting_pending_romaji_at_a_moved_caret_preserves_raw_input_alignment() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

    type_word(&mut dispatcher, session, "sakura", &mut out);
    assert_eq!(out.preedit_text(), "さくら");

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Home),
        },
        &mut out,
    );

    type_word(&mut dispatcher, session, "k", &mut out);
    assert_eq!(
        out.preedit_text(),
        "kさくら",
        "the pending k renders at the caret, ahead of the untouched さくら"
    );
    let live = dispatcher.sessions.get(session).expect("session");
    assert_eq!(
        live.raw_input.as_str(),
        "ksakura",
        "setup must actually land k ahead of sakura via the moved-caret \
             insertion path, or this is not exercising finding C at all"
    );
    assert_eq!(
        live.romaji.pending(),
        "k",
        "k must still be pending, not resolved, going into the delete"
    );

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Delete),
        },
        &mut out,
    );
    assert_eq!(
        out.preedit_text(),
        "kくら",
        "delete-forward removes the first resolved kana (さ) behind the pending k"
    );
    let live = dispatcher.sessions.get(session).expect("session");
    assert_eq!(
        live.raw_input.as_str(),
        "kkura",
        "raw_input must lose exactly \"sa\" (the keystrokes behind さ), \
             keeping the pending k and the untouched \"kura\" behind くら"
    );
    assert_eq!(
        live.romaji.pending(),
        "k",
        "delete-forward must not consume the pending k -- it only removed \
             an already-resolved kana ahead of it"
    );
}

/// #16 finding B/C, carry provenance: `raw_input` holds the raw
/// provenance of the *currently visible* preedit, not an unwound
/// keystroke log. A sokuon/carry (the second "t" of "tt" resolves
/// "tt" to "っ" *and* is carried forward as the new pending) makes the
/// same raw byte simultaneously っ's source and the carried pending's
/// source. Clearing that pending on Backspace must not delete the
/// shared byte -- doing so would corrupt っ's own provenance, which is
/// still visible on screen and untouched by this Backspace. Backspace
/// only removes a raw byte when pending owns one exclusively (its
/// source range extends strictly past the already-resolved boundary);
/// a fully carried/shared pending has no byte of its own to remove.
#[test]
fn backspace_over_carried_pending_romaji_at_a_moved_caret_preserves_raw_provenance() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

    type_word(&mut dispatcher, session, "sakura", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Home),
        },
        &mut out,
    );

    type_word(&mut dispatcher, session, "tt", &mut out);
    assert_eq!(
        out.preedit_text(),
        "っtさくら",
        "っ must be emitted and the carried t must render as pending, \
             ahead of the untouched さくら"
    );
    let live = dispatcher.sessions.get(session).expect("session");
    assert_eq!(
        live.raw_input.as_str(),
        "ttsakura",
        "setup must actually produce a carry ahead of a moved caret, \
             or this is not exercising carry provenance at all"
    );
    assert_eq!(live.romaji.pending(), "t");

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Backspace),
        },
        &mut out,
    );
    assert_eq!(
        out.preedit_text(),
        "っさくら",
        "backspace clears the carried pending t; っ, which shares its \
             raw source with that pending, must remain untouched"
    );
    let live = dispatcher.sessions.get(session).expect("session");
    assert_eq!(live.romaji.pending(), "", "the pending unit is gone");
    assert_eq!(
        live.raw_input.as_str(),
        "ttsakura",
        "raw_input must stay whole -- the only raw byte the carried \
             pending could claim is the same byte っ already depends on, so \
             backspacing pending must not delete any raw byte here"
    );

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::F10),
        },
        &mut out,
    );
    assert_eq!(
        out.preedit_text(),
        "ttsakura",
        "F10's half-width alnum surface, a real downstream consumer of \
             raw_input, must reproduce the exact keystrokes behind っさくら, \
             not a corrupted leftover from a wrongly-deleted raw byte"
    );
}

/// #16 finding B/C, carry provenance: the Delete-forward analogue of
/// the Backspace test above. Unlike Backspace, deleting the *next
/// resolved kana* (さ) ahead of the carried pending must remove that
/// kana's own raw source ("sa") in full -- さ does not share its
/// source with the carry the way っ does, so nothing here should be
/// preserved. The carried pending t itself must survive untouched.
#[test]
fn delete_forward_over_carried_pending_romaji_at_a_moved_caret_preserves_raw_provenance() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

    type_word(&mut dispatcher, session, "sakura", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Home),
        },
        &mut out,
    );

    type_word(&mut dispatcher, session, "tt", &mut out);
    assert_eq!(out.preedit_text(), "っtさくら");
    let live = dispatcher.sessions.get(session).expect("session");
    assert_eq!(
        live.raw_input.as_str(),
        "ttsakura",
        "setup must actually produce a carry ahead of a moved caret, \
             or this is not exercising carry provenance at all"
    );
    assert_eq!(live.romaji.pending(), "t");

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Delete),
        },
        &mut out,
    );
    assert_eq!(
        out.preedit_text(),
        "っtくら",
        "delete-forward removes the next resolved kana (さ) behind the \
             carried pending t, leaving っ, the pending t and くら"
    );
    let live = dispatcher.sessions.get(session).expect("session");
    assert_eq!(
        live.raw_input.as_str(),
        "ttkura",
        "raw_input must lose exactly \"sa\" -- さ's own raw source -- \
             not \"a\" alone. The carried t's source (the second \"t\") is \
             also っ's source and must neither be double-deleted nor left \
             stranded"
    );
    assert_eq!(
        live.romaji.pending(),
        "t",
        "delete-forward must not consume the carried pending t"
    );
}

/// #16 finding B/C, carry provenance: the `feed_character` (typing)
/// analogue of the two tests above. Each new keystroke is spliced into
/// `raw_input` at `next_raw_boundary`, the same boundary
/// Backspace/Delete-forward read. Before that boundary was carry-aware
/// (when it was merely `pending_raw_range(...).end`, clamped but not
/// overlap-aware), it landed one byte too late -- right after っ's
/// *shared* raw source -- splicing the new keystroke into the middle of
/// さくら's own "sa" instead of ahead of it, where the caret actually
/// is. "s" cannot tell the two candidate offsets apart --
/// inserting it one byte earlier or later produces the same string by
/// coincidence, since "sakura" already starts with "s" -- so this uses
/// "y" (extends the carried "t" toward "tya"/"tyu"/"tyo", and never
/// appears in "sakura") to make the two offsets produce visibly
/// different strings.
#[test]
fn typing_after_carried_pending_romaji_at_a_moved_caret_preserves_raw_provenance() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

    type_word(&mut dispatcher, session, "sakura", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Home),
        },
        &mut out,
    );

    type_word(&mut dispatcher, session, "tt", &mut out);
    assert_eq!(out.preedit_text(), "っtさくら");
    let live = dispatcher.sessions.get(session).expect("session");
    assert_eq!(
        live.raw_input.as_str(),
        "ttsakura",
        "setup must actually produce a carry ahead of a moved caret, \
             or this is not exercising carry provenance at all"
    );
    assert_eq!(live.romaji.pending(), "t");

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: char_key('y'),
        },
        &mut out,
    );
    let live = dispatcher.sessions.get(session).expect("session");
    assert_eq!(
        live.romaji.pending(),
        "ty",
        "y extends the carried t (toward tya/tyu/tyo) rather than \
             resolving, so it must still be waiting as pending, not emitted"
    );
    assert_eq!(
        live.raw_input.as_str(),
        "ttysakura",
        "the new y must land right after \"tt\" and before \"sakura\" \
             -- at the boundary the carried t shares with っ, not one byte \
             later, inside さくら's own \"sa\""
    );
    assert_eq!(
        out.preedit_text(),
        "っtyさくら",
        "nothing resolved yet, so preedit keeps rendering っ plus the \
             now-two-character pending ty ahead of the untouched さくら"
    );
}

/// #16 finding B/C, carry provenance: no moved caret at all -- typing
/// "tt" (a sokuon that carries the second "t" forward as pending),
/// Backspace (clearing that pending without deleting the raw byte it
/// shares with っ, per the Backspace test above), then continuing to
/// type "sakura" left-to-right must not corrupt `raw_input`. This is not
/// about caret movement or Delete-forward's own raw span: it is about
/// `next_raw_boundary` itself, read by every `feed_character` insertion,
/// depending on [`raw_byte_offset_for_preedit_cursor`]'s replay of
/// `raw_input` from a fresh romaji FSM. That replay has no way to see
/// that the live FSM's pending was actually cleared by the Backspace
/// above -- `raw_input` alone does not record it -- so a naive replay
/// re-extends the discarded carry "t" into the next raw bytes ("sa"),
/// hits no matching entry, falls back to a literal "t" passthrough
/// (correct, tested FSM behavior in isolation -- see
/// `every_carrying_entry_resolves_deterministically_when_flushed_alone`
/// in romaji.rs), and that spurious literal throws off every insertion
/// point after it. Each subsequent character then lands one raw byte
/// too early, scrambling the tail into "raraku"-style corruption instead
/// of "sakura".
#[test]
fn continued_typing_after_backspacing_a_carry_does_not_corrupt_raw_input() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

    type_word(&mut dispatcher, session, "tt", &mut out);
    assert_eq!(out.preedit_text(), "っt");
    let live = dispatcher.sessions.get(session).expect("session");
    assert_eq!(live.raw_input.as_str(), "tt");
    assert_eq!(live.romaji.pending(), "t");

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Backspace),
        },
        &mut out,
    );
    assert_eq!(out.preedit_text(), "っ");
    let live = dispatcher.sessions.get(session).expect("session");
    assert_eq!(live.romaji.pending(), "", "the carried pending is gone");
    assert_eq!(
        live.raw_input.as_str(),
        "tt",
        "backspacing the fully-carried pending must not delete っ's own \
             raw source"
    );

    type_word(&mut dispatcher, session, "sakura", &mut out);
    assert_eq!(
        out.preedit_text(),
        "っさくら",
        "さくら must resolve normally -- the discarded carry must not \
             leak a literal t into freshly-typed, unrelated kana"
    );
    let live = dispatcher.sessions.get(session).expect("session");
    assert_eq!(
        live.raw_input.as_str(),
        "ttsakura",
        "each new keystroke must land at the true end of raw_input, in \
             the order it was typed -- not reordered by a replay that \
             thinks the discarded carry is still live"
    );
    assert_eq!(live.romaji.pending(), "");

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::F10),
        },
        &mut out,
    );
    assert_eq!(
        out.preedit_text(),
        "ttsakura",
        "F10's half-width alnum surface, a real downstream consumer of \
             raw_input, must reproduce the exact keystrokes behind っさくら"
    );
}

/// #16 finding B/C, downstream check: the two regression tests above
/// assert `raw_input` directly, which is exactly the kind of assertion
/// a fix could satisfy while still leaving `raw_input` wrong for every
/// *other* reader -- the false-GREEN risk `out.preedit_text()` alone
/// already demonstrated twice over in this file. F10's half-width alnum
/// surface is a real, independent consumer of `raw_input` (via
/// `segment_raw_text`, degenerating to the whole buffer for this
/// single-segment case): if the Backspace fix left `raw_input` merely
/// looking right to a direct field comparison, this would still catch
/// it.
#[test]
fn raw_input_alignment_after_moved_caret_backspace_is_observed_by_f10() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

    type_word(&mut dispatcher, session, "sakura", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Home),
        },
        &mut out,
    );
    type_word(&mut dispatcher, session, "k", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Backspace),
        },
        &mut out,
    );
    let live = dispatcher.sessions.get(session).expect("session");
    assert_eq!(
        live.raw_input.as_str(),
        "sakura",
        "setup must actually recover a clean raw_input via the Backspace fix, \
             or this is not exercising the fix at all"
    );

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::F10),
        },
        &mut out,
    );
    assert_eq!(
        out.preedit_text(),
        "sakura",
        "F10's half-width alnum surface must come from the corrected raw_input \
             (\"sakura\"), not a corrupted leftover like the old pop_char() bug's \
             \"ksakur\""
    );
}

/// #16 finding D: F6-F10 transforms rendered and committed a segment's
/// `SegmentTransform::FullAlnum`/`HalfAlnum` surface from the *whole*
/// composition's `raw_input`, not that segment's own share of it. Applied
/// to one segment of a multi-segment conversion, that leaked every other
/// segment's keystrokes into it.
#[test]
fn f10_on_one_segment_of_a_multi_segment_conversion_uses_only_its_own_keystrokes() {
    let mut dispatcher = segmented_conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "editor.exe");
    type_word(&mut dispatcher, session, "kyoudesu", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    let initial = out.to_output().preedit.expect("converted preedit");
    assert_eq!(initial.segments.len(), 2);
    assert_eq!(initial.segments[0].text, "今日");
    assert_eq!(initial.segments[1].text, "です");

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Right),
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::F10),
        },
        &mut out,
    );
    let transformed = out.to_output().preedit.expect("transformed preedit");
    assert_eq!(
        transformed.segments[0].text, "今日",
        "the unfocused first segment is untouched by a transform on the second"
    );
    assert_eq!(
        transformed.segments[1].text, "desu",
        "the second segment renders only its own keystrokes (\"desu\"), \
             not the whole composition's (\"kyoudesu\")"
    );

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Enter),
        },
        &mut out,
    );
    assert_eq!(
        out.commit_text(),
        Some("今日desu"),
        "committing mirrors what was rendered: only the second segment's \
             own keystrokes are used for its transformed surface"
    );
}

/// The DLL reports Ctrl+S as the character `s` with the Ctrl bit set,
/// so that a key map can bind chords by the letter the user sees on the
/// key. Anything the key map does not claim has to reach the
/// application untouched — an IME that turns Ctrl+S into "s" has eaten
/// a save, and mid-composition is exactly when that hurts most.
#[test]
fn an_unbound_shortcut_reaches_the_application_and_leaves_the_composition_alone() {
    for held in [
        Modifiers::CTRL,
        Modifiers::ALT,
        Modifiers(Modifiers::CTRL.0 | Modifiers::SHIFT.0),
        Modifiers(Modifiers::ALT.0 | Modifiers::SHIFT.0),
    ] {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
        type_word(&mut dispatcher, session, "sakura", &mut out);
        let before = out.preedit_text().to_owned();

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: KeyInput {
                    modifiers: held,
                    ..char_key(if held.shift() { 'S' } else { 's' })
                },
            },
            &mut out,
        );

        assert!(!out.consumed, "{held:?}+S was swallowed");
        assert_eq!(out.commit_text(), None);
        assert_eq!(
            out.preedit_text(),
            before,
            "{held:?}+S changed the composition"
        );
    }
}

// Issue #16 finding E: every named key exercised below used to fall
// through `apply_key`'s final arm and reach the host application while
// the IME owned a composition, conversion or focused suggestion list.
// The keymap now binds each one to a real action or `Action::Swallow`;
// `sakura_core::keymap`'s `ms_ime_*_is_bound_to_*` tests pin down
// exactly which action each key resolves to. The tests below pin down
// the resulting *behaviour* instead: the key must be consumed under
// both a `test_only` probe and a real dispatch, a probe must never
// mutate the live session, and a `Swallow` binding must have no
// modelled effect at all. Every case gets its own fresh dispatcher and
// session -- probe and real dispatch never share one either -- so a
// case that unexpectedly changes state cannot contaminate the next
// case, and neither dispatch can hide a mutation the other would have
// caught.

fn composing_state_dispatcher(word: &str, name: &str) -> (Dispatcher, SessionId) {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, name);
    type_word(&mut dispatcher, session, word, &mut out);
    assert_eq!(
        dispatcher.sessions.get(session).expect("session").state(),
        State::Composing,
        "setup must reach State::Composing"
    );
    (dispatcher, session)
}

fn converting_state_dispatcher(word: &str, name: &str) -> (Dispatcher, SessionId) {
    let mut dispatcher = contextual_conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, name);
    type_word(&mut dispatcher, session, word, &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    assert_eq!(
        dispatcher.sessions.get(session).expect("session").state(),
        State::Converting,
        "setup must reach State::Converting"
    );
    (dispatcher, session)
}

fn predicting_state_dispatcher(
    word: &str,
    name: &str,
) -> (Dispatcher, crate::prediction::PredictionRuntime, SessionId) {
    let (mut dispatcher, runtime) = prediction_dispatcher();
    // The state-machine assertion must not depend on the prediction worker
    // winning a 10 ms production timeout while other Cargo test binaries
    // are running.  The runtime exposes a test-only scripted response so
    // this helper exercises the same action path with a deterministic
    // terminal result; worker scheduling remains covered by prediction.rs.
    let prediction_service = runtime.service();
    prediction_service.test_script_prediction(word, "test-prediction");
    prediction_service.test_set_scripted_prediction_available(true);
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, name);
    type_word(&mut dispatcher, session, word, &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Tab),
        },
        &mut out,
    );
    assert_eq!(
        dispatcher.sessions.get(session).expect("session").state(),
        State::Predicting,
        "setup must reach State::Predicting"
    );
    (dispatcher, runtime, session)
}

#[test]
fn ms_ime_composing_named_keys_do_not_leak_to_the_host_application() {
    // Both cases bind to `Action::Swallow` (issue #16 finding E), so
    // both additionally must leave the session completely unchanged.
    let cases: [(KeyCode, Modifiers); 2] = [
        (KeyCode::PageUp, Modifiers::NONE),
        (KeyCode::PageDown, Modifiers::NONE),
    ];
    let mut failures = Vec::new();
    for (code, modifiers) in cases {
        let (mut probe_dispatcher, probe_session) =
            composing_state_dispatcher("ka", "leak-composing-probe.exe");
        let before_probe = probe_dispatcher
            .sessions
            .get(probe_session)
            .expect("session")
            .clone();
        let mut probe_out = OutputBuf::new();
        probe_dispatcher.dispatch(
            &Request::SendKey {
                session: probe_session,
                key: KeyInput {
                    test_only: true,
                    ..modified_named_key(code, modifiers)
                },
            },
            &mut probe_out,
        );
        if !probe_out.consumed {
            failures.push(format!("composing {code:?} test_only leaked to the host"));
        }
        if probe_dispatcher
            .sessions
            .get(probe_session)
            .expect("session")
            != &before_probe
        {
            failures.push(format!("composing {code:?} probe mutated the session"));
        }
        if probe_out.commit_text().is_some() {
            failures.push(format!("composing {code:?} probe produced a commit"));
        }

        let (mut real_dispatcher, real_session) =
            composing_state_dispatcher("ka", "leak-composing-real.exe");
        let before_real = real_dispatcher
            .sessions
            .get(real_session)
            .expect("session")
            .clone();
        let mut real_out = OutputBuf::new();
        real_dispatcher.dispatch(
            &Request::SendKey {
                session: real_session,
                key: modified_named_key(code, modifiers),
            },
            &mut real_out,
        );
        if !real_out.consumed {
            failures.push(format!(
                "composing {code:?} real dispatch leaked to the host"
            ));
        }
        if real_out.commit_text().is_some() {
            failures.push(format!(
                "composing {code:?} real dispatch produced a commit"
            ));
        }
        if real_dispatcher.sessions.get(real_session).expect("session") != &before_real {
            failures.push(format!(
                "composing {code:?} swallow mutated the session instead of doing nothing"
            ));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn ms_ime_converting_named_keys_do_not_leak_to_the_host_application() {
    // `swallow` marks the three cases bound to `Action::Swallow`
    // (delete, shift+tab, ctrl+delete): those must additionally leave
    // the session untouched. `home`/`end` bind to `SegmentHome`/
    // `SegmentEnd`, which legitimately move `focused_segment` -- real
    // movement across 3+ segments is proven separately, by the
    // dedicated test below, not here.
    let cases: [(KeyCode, Modifiers, bool); 5] = [
        (KeyCode::Delete, Modifiers::NONE, true),
        (KeyCode::Home, Modifiers::NONE, false),
        (KeyCode::End, Modifiers::NONE, false),
        (KeyCode::Tab, Modifiers::SHIFT, true),
        (KeyCode::Delete, Modifiers::CTRL, true),
    ];
    let mut failures = Vec::new();
    for (code, modifiers, swallow) in cases {
        let (mut probe_dispatcher, probe_session) =
            converting_state_dispatcher("ishaniittaowari", "leak-converting-probe.exe");
        let before_probe = probe_dispatcher
            .sessions
            .get(probe_session)
            .expect("session")
            .clone();
        let mut probe_out = OutputBuf::new();
        probe_dispatcher.dispatch(
            &Request::SendKey {
                session: probe_session,
                key: KeyInput {
                    test_only: true,
                    ..modified_named_key(code, modifiers)
                },
            },
            &mut probe_out,
        );
        if !probe_out.consumed {
            failures.push(format!("converting {code:?} test_only leaked to the host"));
        }
        if probe_dispatcher
            .sessions
            .get(probe_session)
            .expect("session")
            != &before_probe
        {
            failures.push(format!("converting {code:?} probe mutated the session"));
        }
        if probe_out.commit_text().is_some() {
            failures.push(format!("converting {code:?} probe produced a commit"));
        }

        let (mut real_dispatcher, real_session) =
            converting_state_dispatcher("ishaniittaowari", "leak-converting-real.exe");
        let before_real = real_dispatcher
            .sessions
            .get(real_session)
            .expect("session")
            .clone();
        let mut real_out = OutputBuf::new();
        real_dispatcher.dispatch(
            &Request::SendKey {
                session: real_session,
                key: modified_named_key(code, modifiers),
            },
            &mut real_out,
        );
        if !real_out.consumed {
            failures.push(format!(
                "converting {code:?} real dispatch leaked to the host"
            ));
        }
        if real_out.commit_text().is_some() {
            failures.push(format!(
                "converting {code:?} real dispatch produced a commit"
            ));
        }
        if swallow && real_dispatcher.sessions.get(real_session).expect("session") != &before_real {
            failures.push(format!(
                "converting {code:?} swallow mutated the session instead of doing nothing"
            ));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn ms_ime_predicting_named_keys_do_not_leak_to_the_host_application() {
    // `commits` marks the one case bound to `Action::CommitFirst`
    // (shift+enter): unlike the other ten, a commit there is correct,
    // modelled behaviour -- Microsoft IME's own "confirm the top pick"
    // shortcut -- so it is excluded from the "must not commit"
    // assertion the other ten get.
    let cases: [(KeyCode, Modifiers, bool); 11] = [
        (KeyCode::Left, Modifiers::NONE, false),
        (KeyCode::Right, Modifiers::NONE, false),
        (KeyCode::Home, Modifiers::NONE, false),
        (KeyCode::End, Modifiers::NONE, false),
        (KeyCode::Delete, Modifiers::NONE, false),
        (KeyCode::Enter, Modifiers::SHIFT, true),
        (KeyCode::F6, Modifiers::NONE, false),
        (KeyCode::F7, Modifiers::NONE, false),
        (KeyCode::F8, Modifiers::NONE, false),
        (KeyCode::F9, Modifiers::NONE, false),
        (KeyCode::F10, Modifiers::NONE, false),
    ];
    let mut failures = Vec::new();
    for (code, modifiers, commits) in cases {
        let (mut probe_dispatcher, _probe_runtime, probe_session) =
            predicting_state_dispatcher("kana", "leak-predicting-probe.exe");
        let before_probe = probe_dispatcher
            .sessions
            .get(probe_session)
            .expect("session")
            .clone();
        let mut probe_out = OutputBuf::new();
        probe_dispatcher.dispatch(
            &Request::SendKey {
                session: probe_session,
                key: KeyInput {
                    test_only: true,
                    ..modified_named_key(code, modifiers)
                },
            },
            &mut probe_out,
        );
        if !probe_out.consumed {
            failures.push(format!("predicting {code:?} test_only leaked to the host"));
        }
        if probe_dispatcher
            .sessions
            .get(probe_session)
            .expect("session")
            != &before_probe
        {
            failures.push(format!("predicting {code:?} probe mutated the session"));
        }
        if !commits && probe_out.commit_text().is_some() {
            failures.push(format!("predicting {code:?} probe produced a commit"));
        }

        let (mut real_dispatcher, _real_runtime, real_session) =
            predicting_state_dispatcher("kana", "leak-predicting-real.exe");
        let mut real_out = OutputBuf::new();
        real_dispatcher.dispatch(
            &Request::SendKey {
                session: real_session,
                key: modified_named_key(code, modifiers),
            },
            &mut real_out,
        );
        if !real_out.consumed {
            failures.push(format!(
                "predicting {code:?} real dispatch leaked to the host"
            ));
        }
        if !commits && real_out.commit_text().is_some() {
            failures.push(format!(
                "predicting {code:?} real dispatch produced an unexpected commit"
            ));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

// Issue #16 finding G-1: unlike MS-IME, ATOK restates `muhenkan` in
// every state's key-map section instead of inheriting it from
// `[global]` (see the comment above `[global]` in
// `data/keymap-atok.toml`), and `[predicting]` used to be the one
// state where that restatement was missing -- so muhenkan fell
// through to the host application while a suggestion was focused
// under the ATOK preset specifically. `sakura_core::keymap`'s
// `atok_predicting_muhenkan_is_bound_to_mode_kana_cycle` pins down
// the exact key-map binding; this test proves the ATOK preset
// actually *reaches* that action end-to-end through the engine, and
// that the resulting temporary transform behaves exactly like the
// already-proven composing-state case (issue #16 finding E's
// `ModeKanaCycle` coverage): it changes only the rendered surface,
// commits nothing, and never leaks to the host. Expected values are
// taken from an empirical dispatch of this exact sequence, not
// assumed from reading the transform code.
#[test]
fn atok_predicting_muhenkan_applies_a_temporary_katakana_transform_without_leaking() {
    let source = "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\nかな\t仮名\t0\t1\t100\t100\tpredict\tcommon\nかなた\t彼方\t0\t2\t200\t200\tpredict\tdirection\nかながわ\t神奈川\t0\t3\t300\t300\tpredict\tprefecture\n";
    let entries = dictc::parse_entries("prediction.tsv", source).expect("entries");
    let matrix = dictc::parse_connection(
        "matrix.tsv",
        "# license: MIT\nclasses\t4\ndefault\t0\n",
        false,
    )
    .expect("matrix");
    let image = Box::leak(
        dictc::compile(&entries, &matrix)
            .expect("image")
            .into_boxed_slice(),
    );
    let conversion = Arc::new(
        ConversionService::from_static_bytes(image).expect("prediction conversion fixture"),
    );
    let learning = Arc::new(LearningService::memory());
    let runtime = crate::prediction::PredictionRuntime::start(Arc::clone(&conversion))
        .expect("prediction runtime");
    let mut dispatcher = Dispatcher::new_with_runtime_configuration(
        conversion,
        learning,
        runtime.service(),
        Preferences {
            keymap_preset: Preset::Atok,
            ..Preferences::default()
        },
    )
    .expect("shipped defaults");

    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "atok-predicting-muhenkan.exe");
    type_word(&mut dispatcher, session, "kana", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Tab),
        },
        &mut out,
    );
    assert_eq!(
        dispatcher.sessions.get(session).expect("session").state(),
        State::Predicting,
        "setup must reach State::Predicting with a focused suggestion"
    );
    let mode_before = dispatcher.sessions.get(session).expect("session").mode;

    let mut out = OutputBuf::new();
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Muhenkan),
        },
        &mut out,
    );

    assert!(
        out.consumed,
        "issue #16 finding G-1: ATOK muhenkan leaked to the host from State::Predicting"
    );
    assert_eq!(
        out.commit_text(),
        None,
        "a temporary transform out of Predicting must not commit anything"
    );
    assert_eq!(
        out.preedit_text(),
        "カナ",
        "muhenkan's first cycle step must render the predicted reading as full-width katakana"
    );

    let session_ref = dispatcher.sessions.get(session).expect("session");
    assert_eq!(
        session_ref.mode, mode_before,
        "a temporary transform out of Predicting must not persist into the session's input mode"
    );
}

/// `Home`/`End` while converting jump straight to the first/last
/// segment (issue #16 finding E). A 1-segment fixture would let a
/// no-op implementation pass this test trivially, so this fixture is
/// picked to yield 4 segments, and focus is moved off both edges
/// before each assertion so neither could pass merely because focus
/// never left the edge.
#[test]
fn ms_ime_converting_home_and_end_move_focus_across_three_or_more_segments() {
    let (mut dispatcher, session) =
        converting_state_dispatcher("ishaniittaowari", "segment-home-end.exe");
    let segment_count = dispatcher
        .sessions
        .get(session)
        .expect("session")
        .segment_count();
    assert!(
        segment_count >= 3,
        "fixture must produce at least 3 segments to prove real movement, got {segment_count}"
    );
    assert_eq!(
        dispatcher
            .sessions
            .get(session)
            .expect("session")
            .focused_segment(),
        0,
        "conversion must start with the first segment focused"
    );

    let mut out = OutputBuf::new();
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Right),
        },
        &mut out,
    );
    assert_eq!(
        dispatcher
            .sessions
            .get(session)
            .expect("session")
            .focused_segment(),
        1,
        "segment_next must move focus off the first segment before End is tested"
    );

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::End),
        },
        &mut out,
    );
    assert!(out.consumed);
    assert_eq!(
        dispatcher
            .sessions
            .get(session)
            .expect("session")
            .focused_segment(),
        segment_count - 1,
        "End must move focus to the last segment"
    );

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Home),
        },
        &mut out,
    );
    assert!(out.consumed);
    assert_eq!(
        dispatcher
            .sessions
            .get(session)
            .expect("session")
            .focused_segment(),
        0,
        "Home must move focus back to the first segment"
    );
}

/// Ctrl+Space is deliberately absent from every shipped preset (see the
/// `ms-ime` preset's header comment): it is IntelliSense in every major
/// IDE. This guards against a future fix for issue #16 finding E
/// accidentally widening into claiming it too.
#[test]
fn ctrl_space_is_not_bound_and_reaches_the_application() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "code.exe");
    type_word(&mut dispatcher, session, "sakura", &mut out);
    let before = out.preedit_text().to_owned();

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: modified_named_key(KeyCode::Space, Modifiers::CTRL),
        },
        &mut out,
    );

    assert!(!out.consumed, "Ctrl+Space was swallowed");
    assert_eq!(out.commit_text(), None);
    assert_eq!(
        out.preedit_text(),
        before,
        "Ctrl+Space changed the composition"
    );
}

#[test]
fn idle_space_and_shift_space_follow_the_configured_width_policy() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    assert!(out.consumed);
    assert_eq!(out.commit_text(), Some("　"));

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: modified_named_key(KeyCode::Space, Modifiers::SHIFT),
        },
        &mut out,
    );
    assert!(out.consumed);
    assert_eq!(out.commit_text(), Some(" "));

    let preferences = Preferences {
        space_width: SpaceWidth::Half,
        shift_space_behavior: ShiftSpaceBehavior::Opposite,
        ..Preferences::default()
    };
    dispatcher
        .apply_runtime_configuration(preferences, Arc::from([]))
        .expect("runtime space settings");

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    assert_eq!(out.commit_text(), Some(" "));
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: modified_named_key(KeyCode::Space, Modifiers::SHIFT),
        },
        &mut out,
    );
    assert_eq!(out.commit_text(), Some("　"));
}

#[test]
fn idle_space_after_an_ascii_commit_stays_halfwidth() {
    let mut dispatcher = shifted_ascii_english_conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

    for (index, character) in "Claude".chars().enumerate() {
        let key = if index == 0 {
            shifted_char_key(character)
        } else {
            char_key(character)
        };
        dispatcher.dispatch(&Request::SendKey { session, key }, &mut out);
    }
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Enter),
        },
        &mut out,
    );
    assert_eq!(out.commit_text(), Some("Claude"));
    assert_eq!(
        dispatcher.sessions.get(session).expect("session").state(),
        State::Idle
    );

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    assert!(out.consumed);
    assert_eq!(out.commit_text(), Some(" "));

    for (index, character) in "Code".chars().enumerate() {
        let key = if index == 0 {
            shifted_char_key(character)
        } else {
            char_key(character)
        };
        dispatcher.dispatch(&Request::SendKey { session, key }, &mut out);
    }
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Enter),
        },
        &mut out,
    );
    assert_eq!(out.commit_text(), Some("Code"));
}

/// The issue #16 finding E keymap fix bound specific named keys in
/// `[composing]`, `[converting]` and `[predicting]` -- it must not have
/// widened into "always consume named keys". `[idle]` never binds
/// PageUp, so it must still reach the host application when there is no
/// composition to protect.
#[test]
fn an_unbound_named_key_reaches_the_application_while_idle() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
    assert_eq!(
        dispatcher.sessions.get(session).expect("session").state(),
        State::Idle
    );

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::PageUp),
        },
        &mut out,
    );

    assert!(!out.consumed, "PageUp was swallowed while idle");
    assert_eq!(out.commit_text(), None);
}

#[test]
fn a_test_only_key_reports_what_the_real_dispatch_would_without_mutating_the_session() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

    // "z" has no romaji reading and passes through raw, immediately and
    // deterministically, with no waiting -- ideal for proving a probe
    // did or did not leave a mark.
    let probe_reply = dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: test_only_char_key('z'),
        },
        &mut out,
    );
    assert_eq!(probe_reply, Reply::Output);
    let probe_consumed = out.consumed;

    // If the probe above had mutated the session, this real "z" would
    // land after a leftover "z" and produce "zz" instead of "z".
    let real_reply = dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: char_key('z'),
        },
        &mut out,
    );
    assert_eq!(real_reply, Reply::Output);
    assert_eq!(
        out.consumed, probe_consumed,
        "test_only must report the same consumed as the real dispatch"
    );
    assert_eq!(out.preedit_text(), "z");
}

#[test]
fn test_only_document_navigation_cannot_clear_the_live_cross_commit_bridge() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "bridge-probe.exe");
    {
        let state = dispatcher.sessions.get_mut(session).expect("session");
        state.apply_input_scope(InputScope::Normal);
        state.preedit.push_str("もれ").expect("fits");
        state.record_current_commit_with_bridge(
            "漏れ",
            1949,
            0,
            1,
            SessionCrossCommitBridge::new("もれ", "漏れ", 1841, 4_000),
        );
        state.reset();
        assert!(state.cross_commit_bridge().is_some());
    }

    let mut probe = named_key(KeyCode::PageUp);
    probe.test_only = true;
    assert_eq!(
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: probe
            },
            &mut out
        ),
        Reply::Output
    );
    assert!(
        dispatcher
            .sessions
            .get(session)
            .expect("live session")
            .cross_commit_bridge()
            .is_some(),
        "Probe must discard its cloned invalidation"
    );

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::PageUp),
        },
        &mut out,
    );
    assert!(
        dispatcher
            .sessions
            .get(session)
            .expect("live session")
            .cross_commit_bridge()
            .is_none(),
        "real host navigation invalidates adjacency"
    );
}

#[test]
fn test_only_stale_learning_cache_is_invalidated_ephemerally_for_probe_parity() {
    let (mut dispatcher, runtime) = prediction_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
    type_word(&mut dispatcher, session, "kana", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Tab),
        },
        &mut out,
    );
    assert!(
        dispatcher
            .prediction_cache
            .candidates(
                session,
                dispatcher
                    .sessions
                    .get(session)
                    .expect("session")
                    .prediction_generation,
            )
            .is_some(),
        "the test must start with a populated live cache"
    );

    let learning = Arc::clone(dispatcher.learning.as_ref().expect("learning service"));
    let live_cache = (*dispatcher.prediction_cache).clone();
    let session_state = dispatcher.sessions.get(session).expect("session").clone();
    let observed_generation = dispatcher.observed_learning_generation;
    learning.learn("かな", "generation bump", 0, 0);

    let history_prefix = session_state.preedit.as_str();
    let learning_generation = learning.generation();
    let mut learning_history = Vec::new();
    learning.visit_prediction_history(history_prefix, |reading, surface, right, score| {
        learning_history.push((reading.to_owned(), surface.to_owned(), right, score));
        true
    });

    let probe_reply = dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: KeyInput {
                test_only: true,
                ..modified_named_key(KeyCode::Delete, Modifiers::CTRL)
            },
        },
        &mut out,
    );
    assert_eq!(probe_reply, Reply::Output);
    let probe_output = out.to_output();
    assert!(!probe_output.consumed);
    assert!(!probe_output.beep);
    assert_eq!(
        dispatcher.sessions.get(session).expect("session"),
        &session_state,
        "Probe must not mutate the live session while clearing stale focus in its view"
    );
    assert_eq!(learning.generation(), learning_generation);
    let mut after_probe_history = Vec::new();
    learning.visit_prediction_history(history_prefix, |reading, surface, right, score| {
        after_probe_history.push((reading.to_owned(), surface.to_owned(), right, score));
        true
    });
    assert_eq!(after_probe_history, learning_history);
    assert_eq!(
        *dispatcher.prediction_cache, live_cache,
        "Probe must not mutate or clear the live stale cache"
    );

    // Restore the same session/cache snapshot and ask the real dispatcher
    // the same key. Learning remains at the newer generation, so Apply's
    // normal invalidation is the equivalent initial-state transition.
    dispatcher
        .sessions
        .get_mut(session)
        .expect("session")
        .clone_from(&session_state);
    dispatcher.prediction_cache.as_mut().clone_from(&live_cache);
    dispatcher.observed_learning_generation = observed_generation;
    let apply_reply = dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: modified_named_key(KeyCode::Delete, Modifiers::CTRL),
        },
        &mut out,
    );
    assert_eq!(apply_reply, Reply::Output);
    let apply_output = out.to_output();
    assert_eq!(
        apply_output.consumed, probe_output.consumed,
        "Probe and Apply must agree on host key consumption"
    );
    assert!(!apply_output.consumed);
    assert!(!apply_output.beep);
    assert_eq!(apply_output.delete_before, probe_output.delete_before);
    assert_eq!(
        dispatcher
            .sessions
            .get(session)
            .expect("session")
            .preedit
            .as_str(),
        "かな",
        "cache refresh must not change the composition itself"
    );
    runtime.stop().expect("prediction worker joins");
}

#[test]
fn commit_undo_render_overflow_rejects_before_tsf_and_restores_post_commit_state() {
    let mut dispatcher = Dispatcher::with_parts(
        Table::builtin().expect("romaji table"),
        KeyMap::preset(Preset::MsIme).expect("key map"),
        Normalizer {
            width: WidthPolicy {
                alnum: Width::FollowMode,
                number: Width::FollowMode,
                symbol: Width::FollowMode,
            },
            punctuation: PunctuationStyle::default(),
            brackets: BracketStyle::default(),
        },
    );
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "editor.exe");
    {
        let session_state = dispatcher.sessions.get_mut(session).expect("session");
        let reading = "a".repeat(MAX_PREEDIT_BYTES);
        session_state
            .preedit
            .push_str(&reading)
            .expect("bounded reading");
        session_state
            .raw_input
            .push_str(&reading)
            .expect("bounded raw input");
        session_state.record_current_commit("x", 42, 0, 1);
        session_state.reset();
        session_state.mode = Mode::FullAlnum;
    }

    let reply = dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: modified_named_key(KeyCode::Backspace, Modifiers::CTRL),
        },
        &mut out,
    );

    assert_eq!(reply, Reply::Message(Response::Error(ErrorCode::TooLarge)));
    assert_eq!(out.delete_before(), "", "a local failure never reaches TSF");
    let session_state = dispatcher.sessions.get_mut(session).expect("session");
    assert!(!session_state.undo_pending());
    assert!(!session_state.is_composing());
    assert_eq!(session_state.carry_right_id(), 42);
    assert!(
        session_state.undo_commit().is_some(),
        "a definite pre-mutation rejection keeps the bounded undo retryable"
    );
    assert!(session_state.reject_undo_commit());
}

#[test]
fn an_unknown_session_id_is_rejected() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();

    let reply = dispatcher.dispatch(
        &Request::SendKey {
            session: 9999,
            key: char_key('a'),
        },
        &mut out,
    );

    assert_eq!(
        reply,
        Reply::Message(Response::Error(ErrorCode::UnknownSession))
    );
}

#[test]
fn session_ids_are_never_reused_after_deletion() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    let first = create_session(&mut dispatcher, &mut out, "a.exe");

    let deleted = dispatcher.dispatch(&Request::DeleteSession { session: first }, &mut out);
    assert_eq!(deleted, Reply::Message(Response::Ok));

    let second = create_session(&mut dispatcher, &mut out, "b.exe");
    assert_ne!(first, second);
    assert!(second > first);
}

#[test]
fn reset_document_context_preserves_mode_but_clears_document_relative_state() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "editor.exe");
    assert_eq!(
        dispatcher.dispatch(
            &Request::SetInputScope {
                session,
                scope: InputScope::Normal,
            },
            &mut out,
        ),
        Reply::Message(Response::Ok)
    );
    assert_eq!(
        dispatcher.dispatch(
            &Request::SetMode {
                session,
                mode: Mode::Katakana,
            },
            &mut out,
        ),
        Reply::Message(Response::InputMode {
            mode: Mode::Katakana
        })
    );

    {
        let state = dispatcher.sessions.get_mut(session).expect("live session");
        state
            .preedit
            .push_str("こうりょもれ")
            .expect("bounded reading");
        state.record_current_commit_with_bridge(
            "考慮漏れ",
            1949,
            0,
            2,
            SessionCrossCommitBridge::new("もれ", "漏れ", 1841, 4_000),
        );
        state.reset();
        assert_eq!(state.mode(), Mode::Katakana);
        assert_eq!(state.carry_right_id(), 1949);
        assert!(state.cross_commit_bridge().is_some());
        assert!(state.cached_surface_fingerprint("こうりょもれ").is_some());
    }
    dispatcher.prediction_cache.attempted = true;
    dispatcher.prediction_cache.session = session;
    dispatcher.prediction_cache.generation = 7;

    assert_eq!(
        dispatcher.dispatch(&Request::ResetDocumentContext { session }, &mut out),
        Reply::Message(Response::Ok)
    );

    {
        let state = dispatcher.sessions.get_mut(session).expect("live session");
        assert_eq!(state.mode(), Mode::Katakana);
        assert_eq!(state.carry_right_id(), 0);
        assert!(state.cross_commit_bridge().is_none());
        assert_eq!(state.cached_surface_fingerprint("こうりょもれ"), None);
        assert_eq!(state.undo_commit(), None);
        state.preedit.push_str("か").expect("bounded composition");
    }
    assert!(!dispatcher.prediction_cache.attempted);

    assert_eq!(
        dispatcher.dispatch(&Request::ResetDocumentContext { session }, &mut out),
        Reply::Message(Response::Error(ErrorCode::Busy)),
        "a lifecycle reset must not erase a live composition"
    );
    assert_eq!(
        dispatcher
            .sessions
            .get(session)
            .expect("live session")
            .preedit
            .as_str(),
        "か"
    );
}

#[test]
fn the_session_table_reports_busy_once_full() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    for _ in 0..crate::session::MAX_SESSIONS {
        create_session(&mut dispatcher, &mut out, "app.exe");
    }

    let reply = dispatcher.dispatch(
        &Request::CreateSession {
            process_name: "one-too-many.exe".to_string(),
        },
        &mut out,
    );

    assert_eq!(reply, Reply::Message(Response::Error(ErrorCode::Busy)));
}

#[test]
fn hello_with_the_previous_v21_version_is_rejected() {
    assert_eq!(
        PROTOCOL_VERSION, 22,
        "the fault-injection status snapshot adds a v22 request and response"
    );
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();

    let reply = dispatcher.dispatch(&Request::Hello { client_version: 21 }, &mut out);

    assert_eq!(
        reply,
        Reply::Message(Response::Error(ErrorCode::UnsupportedVersion))
    );
}

#[test]
fn hello_with_v22_version_is_accepted() {
    assert_eq!(
        PROTOCOL_VERSION, 22,
        "the fault-injection status snapshot adds a v22 request and response"
    );
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();

    let reply = dispatcher.dispatch(
        &Request::Hello {
            client_version: PROTOCOL_VERSION,
        },
        &mut out,
    );

    assert_eq!(
        reply,
        Reply::Message(Response::Hello {
            server_version: PROTOCOL_VERSION,
            engine_version: ENGINE_VERSION
        })
    );
}

#[test]
fn half_width_full_width_can_restore_hiragana_from_direct_mode() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

    let off = dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::HankakuZenkaku),
        },
        &mut out,
    );
    assert_eq!(off, Reply::Output);
    assert!(out.consumed);
    assert_eq!(out.to_output().mode, Some(Mode::Direct));
    assert_eq!(
        dispatcher.sessions.get(session).expect("session").mode(),
        Mode::Direct
    );

    let ordinary_key = dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: char_key('a'),
        },
        &mut out,
    );
    assert_eq!(ordinary_key, Reply::Output);
    assert!(!out.consumed, "Direct mode must pass normal typing through");

    let on = dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::HankakuZenkaku),
        },
        &mut out,
    );
    assert_eq!(on, Reply::Output);
    assert!(out.consumed);
    assert_eq!(out.to_output().mode, Some(Mode::Hiragana));
    assert_eq!(
        dispatcher.sessions.get(session).expect("session").mode(),
        Mode::Hiragana
    );
}

#[test]
fn shifted_ascii_letters_build_an_english_composition_without_committing() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

    for character in ['K', 'A'] {
        assert_eq!(
            dispatcher.dispatch(
                &Request::SendKey {
                    session,
                    key: shifted_char_key(character),
                },
                &mut out,
            ),
            Reply::Output
        );
        assert!(out.consumed);
    }

    assert_eq!(out.commit_text(), None);
    assert_eq!(out.preedit_text(), "KA");
    let live = dispatcher.sessions.get(session).expect("session");
    assert_eq!(live.raw_input.as_str(), "KA");
    assert_eq!(live.mode(), Mode::Hiragana);

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Backspace),
        },
        &mut out,
    );
    assert_eq!(out.preedit_text(), "K");
    let live = dispatcher.sessions.get(session).expect("session");
    assert_eq!(live.raw_input.as_str(), "K");
    assert!(live.shifted_ascii);
}

/// Issue #51. Erasing the temporary English composition instead of
/// committing it used to leave the latch set with every buffer empty:
/// the session reported `Idle`, so neither Enter nor Escape reached
/// `Session::reset`, and no other key cleared it either. Everything
/// typed afterwards came out as verbatim English until the user
/// switched to another IME and back, which builds a new session.
#[test]
fn erasing_the_english_composition_returns_the_session_to_japanese() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

    for character in ['K', 'A'] {
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: shifted_char_key(character),
            },
            &mut out,
        );
    }
    assert_eq!(out.preedit_text(), "KA");

    for _ in 0..2 {
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Backspace),
            },
            &mut out,
        );
    }
    assert_eq!(out.preedit_text(), "", "the composition is fully erased");
    let live = dispatcher.sessions.get(session).expect("session");
    assert_eq!(live.state(), State::Idle);
    assert!(
        !live.shifted_ascii,
        "the temporary English mode must not outlive the composition it describes"
    );

    for character in ['k', 'a'] {
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key(character),
            },
            &mut out,
        );
    }
    assert_eq!(out.preedit_text(), "か");
}

/// The same leak through the other erase path. Forward Delete has the
/// same shape as Backspace -- remove one character and return -- so it
/// reaches an empty composition without passing `Session::reset` too.
#[test]
fn forward_deleting_the_english_composition_away_ends_the_temporary_mode() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: shifted_char_key('K'),
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: char_key('a'),
        },
        &mut out,
    );
    assert_eq!(out.preedit_text(), "Ka");

    // Visible English units are raw letters. Home then two forward
    // Deletes empties "Ka"; a single Left+Delete only removes the
    // letter after the caret.
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Home),
        },
        &mut out,
    );
    for _ in 0..2 {
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Delete),
            },
            &mut out,
        );
    }

    let live = dispatcher.sessions.get(session).expect("session");
    assert!(
        live.raw_input.is_empty() && live.preedit.is_empty() && live.romaji.is_empty(),
        "forward Delete is expected to leave nothing behind here: raw={:?} preedit={:?}",
        live.raw_input.as_str(),
        live.preedit.as_str()
    );
    assert!(!live.shifted_ascii);

    for character in ['k', 'a'] {
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key(character),
            },
            &mut out,
        );
    }
    assert_eq!(out.preedit_text(), "か");
}

/// The other polarity: releasing the latch is conditional on the
/// composition actually being gone. Erasing back to a shorter English
/// composition keeps typing English, which is what the mode is for.
#[test]
fn a_partly_erased_english_composition_stays_english() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: shifted_char_key('K'),
        },
        &mut out,
    );
    for character in ['a', 'b'] {
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key(character),
            },
            &mut out,
        );
    }
    assert_eq!(out.preedit_text(), "Kab");

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Backspace),
        },
        &mut out,
    );
    assert_eq!(out.preedit_text(), "Ka");
    assert!(
        dispatcher
            .sessions
            .get(session)
            .expect("session")
            .shifted_ascii,
        "a composition that is only partly erased is still being typed in English"
    );

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: char_key('c'),
        },
        &mut out,
    );
    assert_eq!(out.preedit_text(), "Kac");
}

/// The invariant behind #51, over generated key sequences rather than
/// the two erase paths that happened to break it: the temporary English
/// mode never survives an empty composition, whatever route emptied it.
///
/// Fixed seed, in-house generator -- no external property-testing crate
/// is on this workspace's dependency list.
#[test]
fn the_english_latch_never_survives_an_empty_composition() {
    struct Random {
        state: u64,
    }

    impl Random {
        fn next(&mut self) -> u64 {
            self.state ^= self.state << 13;
            self.state ^= self.state >> 7;
            self.state ^= self.state << 17;
            self.state.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }

        fn usize(&mut self, exclusive_end: usize) -> usize {
            usize::try_from(self.next() % exclusive_end as u64).unwrap_or(0)
        }
    }

    enum Stroke {
        Shifted(char),
        Plain(char),
        Named(KeyCode),
    }

    let strokes = [
        Stroke::Shifted('K'),
        Stroke::Shifted('S'),
        Stroke::Plain('a'),
        Stroke::Plain('b'),
        Stroke::Plain('n'),
        Stroke::Plain('o'),
        Stroke::Plain('。'),
        Stroke::Named(KeyCode::Backspace),
        Stroke::Named(KeyCode::Backspace),
        Stroke::Named(KeyCode::Delete),
        Stroke::Named(KeyCode::Left),
        Stroke::Named(KeyCode::Right),
        Stroke::Named(KeyCode::Escape),
        Stroke::Named(KeyCode::Enter),
        Stroke::Named(KeyCode::Space),
    ];

    let mut random = Random {
        state: 0x5A4B_5552_0000_0051 ^ 0x9E37_79B9_7F4A_7C15,
    };
    let mut latched = 0usize;
    let mut released_by_emptying = 0usize;

    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

    for step in 0..4_000 {
        let key = match &strokes[random.usize(strokes.len())] {
            Stroke::Shifted(character) => shifted_char_key(*character),
            Stroke::Plain(character) => char_key(*character),
            Stroke::Named(code) => named_key(*code),
        };
        let was_latched = dispatcher
            .sessions
            .get(session)
            .expect("session")
            .shifted_ascii;
        dispatcher.dispatch(&Request::SendKey { session, key }, &mut out);

        let live = dispatcher.sessions.get(session).expect("session");
        let empty = live.raw_input.is_empty() && live.preedit.is_empty() && live.romaji.is_empty();
        assert!(
            !(live.shifted_ascii && empty),
            "step {step}: the temporary English mode outlived its composition"
        );
        if live.shifted_ascii {
            latched += 1;
        }
        if was_latched && !live.shifted_ascii && empty {
            released_by_emptying += 1;
        }
    }

    assert!(
        latched > 0,
        "the campaign never entered the temporary English mode, so it proves nothing"
    );
    assert!(
        released_by_emptying > 0,
        "the campaign never emptied a latched composition, so it never exercised the fix"
    );
}

#[test]
fn shifted_first_ascii_letter_latches_english_composition_for_unshifted_ascii() {
    let mut dispatcher = shifted_ascii_english_conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

    for (index, character) in "Claude".chars().enumerate() {
        let key = if index == 0 {
            shifted_char_key(character)
        } else {
            char_key(character)
        };
        assert_eq!(
            dispatcher.dispatch(&Request::SendKey { session, key }, &mut out),
            Reply::Output,
            "character {character:?} at index {index}"
        );
        assert!(out.consumed);
    }

    assert_eq!(out.commit_text(), None);
    assert_eq!(out.preedit_text(), "Claude");
    let live = dispatcher.sessions.get(session).expect("session");
    assert_eq!(live.raw_input.as_str(), "Claude");
    assert!(live.shifted_ascii, "initial Shift must stay latched");

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    assert!(out.consumed);
    assert!(!out.beep);
    assert_eq!(out.preedit_text(), "Claude ");
    assert_eq!(out.candidate_kind(), None);
    assert!(
        !out.preedit_text().contains('\u{3000}'),
        "English word gaps must stay ASCII space"
    );
    let live = dispatcher.sessions.get(session).expect("session");
    assert_eq!(live.raw_input.as_str(), "Claude ");
    assert!(live.shifted_ascii, "Space must not end the temporary mode");

    for character in "Code".chars() {
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key(character),
            },
            &mut out,
        );
    }
    assert_eq!(out.preedit_text(), "Claude Code");
    assert_eq!(out.candidate_kind(), None);
    assert!(
        !out.preedit_text().contains('\u{3000}'),
        "typed English must not pick up an ideographic space"
    );

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Enter),
        },
        &mut out,
    );
    assert_eq!(out.commit_text(), Some("Claude Code"));
    let live = dispatcher.sessions.get(session).expect("session");
    assert_eq!(live.mode(), Mode::Hiragana);
    assert!(!live.shifted_ascii, "commit must end the temporary mode");
    assert_eq!(live.state(), State::Idle);
}

#[test]
fn shift_started_ascii_without_dictionary_hit_inserts_literal_plain_space() {
    let mut dispatcher = shifted_ascii_english_conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

    for (index, character) in "Aiamu".chars().enumerate() {
        let key = if index == 0 {
            shifted_char_key(character)
        } else {
            char_key(character)
        };
        dispatcher.dispatch(&Request::SendKey { session, key }, &mut out);
    }
    assert_eq!(out.commit_text(), None);
    assert_eq!(out.preedit_text(), "Aiamu");

    assert_eq!(
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Space),
            },
            &mut out,
        ),
        Reply::Output
    );
    assert!(out.consumed);
    assert!(!out.beep);
    assert_eq!(out.commit_text(), None);
    assert_eq!(out.preedit_text(), "Aiamu ");
    assert_eq!(out.candidate_kind(), None);
    assert_eq!(
        dispatcher.sessions.get(session).expect("session").state(),
        State::Composing
    );

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    assert!(out.consumed);
    assert!(!out.beep);
    assert_eq!(out.preedit_text(), "Aiamu  ");

    assert_eq!(
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Enter),
            },
            &mut out,
        ),
        Reply::Output
    );
    assert_eq!(out.commit_text(), Some("Aiamu  "));
}

#[test]
fn shift_space_in_shifted_ascii_is_literal_even_for_a_known_dictionary_term() {
    let mut dispatcher = shifted_ascii_english_conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

    for (index, character) in "Claude".chars().enumerate() {
        let key = if index == 0 {
            shifted_char_key(character)
        } else {
            char_key(character)
        };
        dispatcher.dispatch(&Request::SendKey { session, key }, &mut out);
    }
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: modified_named_key(KeyCode::Space, Modifiers::SHIFT),
        },
        &mut out,
    );

    assert!(out.consumed);
    assert!(!out.beep);
    assert_eq!(out.preedit_text(), "Claude ");
    assert_eq!(out.candidate_kind(), None);
    let live = dispatcher.sessions.get(session).expect("session");
    assert_eq!(live.raw_input.as_str(), "Claude ");
    assert!(live.shifted_ascii);
}

#[test]
fn shifted_ascii_terms_use_dictionary_case_and_phrase_candidates() {
    let mut dispatcher = shifted_ascii_english_conversion_dispatcher();
    let cases: &[(&str, &str, &[&str])] = &[
        ("CLAUDE", "Claude", &["Claude", "Claude Code"]),
        ("OPENAI", "OpenAI", &["OpenAI"]),
        ("GITLAB", "GitLab", &["GitLab"]),
        ("PYTORCH", "PyTorch", &["PyTorch"]),
        ("MICROSOFTTEAMS", "Microsoft Teams", &["Microsoft Teams"]),
        ("SAKURAINPUT", "Sakura Input", &["Sakura Input"]),
    ];

    for &(typed, expected, expected_candidates) in cases {
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

        for character in typed.chars() {
            assert_eq!(
                dispatcher.dispatch(
                    &Request::SendKey {
                        session,
                        key: shifted_char_key(character),
                    },
                    &mut out,
                ),
                Reply::Output,
                "Shift+{character} in {typed}"
            );
        }
        assert_eq!(out.preedit_text(), typed);

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Henkan),
            },
            &mut out,
        );
        assert!(out.consumed, "Henkan after Shift+{typed}");
        assert_eq!(out.preedit_text(), expected, "conversion for {typed}");
        assert_eq!(out.candidate_kind(), Some(CandidateKind::Conversion));
        let candidates = (0..CANDIDATE_PAGE_SIZE)
            .filter_map(|index| out.candidate(index).map(|(surface, _)| surface))
            .collect::<Vec<_>>();
        for expected_candidate in expected_candidates {
            assert!(
                candidates.contains(expected_candidate),
                "missing {expected_candidate} for {typed}: {candidates:?}"
            );
        }

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Enter),
            },
            &mut out,
        );
        assert_eq!(out.commit_text(), Some(expected), "commit for {typed}");
        assert_eq!(
            dispatcher.sessions.get(session).expect("session").mode(),
            Mode::Hiragana,
            "Shift conversion must not persist a mode change"
        );
    }
}

#[test]
fn microsoft_nonconvert_temporarily_transforms_composition_without_changing_mode() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

    type_word(&mut dispatcher, session, "ka", &mut out);
    assert_eq!(out.preedit_text(), "\u{304b}");
    assert_eq!(
        dispatcher.sessions.get(session).expect("session").mode(),
        Mode::Hiragana
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Muhenkan),
        },
        &mut out,
    );
    assert_eq!(out.to_output().mode, None);
    assert_eq!(out.commit_text(), None);
    assert_eq!(out.preedit_text(), "\u{30ab}"); // カ
    assert_eq!(
        dispatcher.sessions.get(session).expect("session").mode(),
        Mode::Hiragana
    );

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Enter),
        },
        &mut out,
    );
    assert_eq!(out.commit_text(), Some("カ"));
    assert_eq!(
        dispatcher.sessions.get(session).expect("session").mode(),
        Mode::Hiragana
    );

    type_word(&mut dispatcher, session, "ka", &mut out);
    assert_eq!(out.preedit_text(), "\u{304b}");

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Muhenkan),
        },
        &mut out,
    );
    assert_eq!(out.to_output().mode, None);
    assert_eq!(out.commit_text(), None);
    assert_eq!(out.preedit_text(), "\u{30ab}"); // カ
    assert_eq!(
        dispatcher.sessions.get(session).expect("session").mode(),
        Mode::Hiragana
    );

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Muhenkan),
        },
        &mut out,
    );
    assert_eq!(out.to_output().mode, None);
    assert_eq!(out.commit_text(), None);
    assert_eq!(out.preedit_text(), "\u{ff76}"); // ｶ
    assert_eq!(
        dispatcher.sessions.get(session).expect("session").mode(),
        Mode::Hiragana
    );

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Enter),
        },
        &mut out,
    );
    assert_eq!(out.commit_text(), Some("ｶ"));
    assert_eq!(
        dispatcher.sessions.get(session).expect("session").mode(),
        Mode::Hiragana
    );

    type_word(&mut dispatcher, session, "ka", &mut out);
    assert_eq!(out.preedit_text(), "\u{304b}"); // 次の入力はひらがな
}

#[test]
fn a_temporary_kana_transform_commits_without_teaching_the_reading_its_katakana_form() {
    let learning = Arc::new(LearningService::memory());
    let mut dispatcher = Dispatcher::new_with_services(conversion_fixture(), Arc::clone(&learning))
        .expect("dispatcher");
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

    let generation_before = learning.generation();
    type_word(&mut dispatcher, session, "ka", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Muhenkan),
        },
        &mut out,
    );
    assert_eq!(out.preedit_text(), "\u{30ab}"); // カ
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Enter),
        },
        &mut out,
    );

    assert_eq!(
        out.commit_text(),
        Some("\u{30ab}"),
        "the temporary transform still commits what the user saw"
    );
    assert_eq!(
        learning.generation(),
        generation_before,
        "a mechanical kana transform must never bias the reading towards katakana"
    );
    let preference = learning.preference("\u{304b}", 0, [("\u{30ab}", 0)]);
    assert_eq!(
        (preference.exact, preference.general),
        (None, None),
        "the transformed surface must be absent from the learning store"
    );

    // An ordinary commit of the same reading still teaches the store, so the
    // gate is specific to transforms rather than a blanket suppression.
    type_word(&mut dispatcher, session, "kana", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Enter),
        },
        &mut out,
    );
    assert_eq!(learning.generation(), generation_before + 1);
}

#[test]
fn microsoft_nonconvert_cycles_persistent_mode_when_idle() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

    for expected in [Mode::Katakana, Mode::HalfKatakana, Mode::Hiragana] {
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Muhenkan),
            },
            &mut out,
        );
        assert_eq!(out.to_output().mode, Some(expected));
        assert_eq!(
            dispatcher.sessions.get(session).expect("session").mode(),
            expected
        );
        assert_eq!(out.preedit_text(), "");
    }
}

#[test]
fn set_input_scope_password_forces_direct_mode_and_builds_no_preedit() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "login.exe");
    type_word(&mut dispatcher, session, "ka", &mut out);
    assert_eq!(
        out.preedit_text(),
        "か",
        "sanity check: normally composes before the scope change"
    );

    let reply = dispatcher.dispatch(
        &Request::SetInputScope {
            session,
            scope: InputScope::Password,
        },
        &mut out,
    );
    assert_eq!(reply, Reply::Message(Response::Ok));

    let key_reply = dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: char_key('a'),
        },
        &mut out,
    );
    assert_eq!(key_reply, Reply::Output);
    assert!(!out.consumed, "Direct mode must not intercept keys");
    assert_eq!(out.preedit_text(), "");
    assert_eq!(out.commit_text(), None);

    let toggle_reply = dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::HankakuZenkaku),
        },
        &mut out,
    );
    assert_eq!(toggle_reply, Reply::Output);
    assert!(!out.consumed, "password fields must not inspect mode keys");
    assert_eq!(
        dispatcher.sessions.get(session).expect("session").mode(),
        Mode::Direct
    );
}

#[test]
fn half_alnum_types_ascii_straight_through_and_full_alnum_widens_it() {
    // The shipped MS-IME preset binds no key to a direct mode-switch
    // action, so reaching HalfAlnum/FullAlnum needs a key map that does;
    // and the default normalizer never widens anything in any mode (by
    // design -- see `sakura_core::width`'s docs), so reaching a widened
    // FullAlnum result needs a normalizer told to follow the mode.
    let table = Table::builtin().expect("builtin table compiles");
    let keymap = KeyMap::parse("[global]\nf2 = \"mode_half_alnum\"\nf3 = \"mode_full_alnum\"\n")
        .expect("small keymap compiles");
    let normalizer = Normalizer {
        width: WidthPolicy {
            alnum: Width::FollowMode,
            number: Width::FollowMode,
            symbol: Width::FollowMode,
        },
        punctuation: PunctuationStyle::default(),
        brackets: BracketStyle::default(),
    };
    let mut dispatcher = Dispatcher::with_parts(table, keymap, normalizer);
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "cmd.exe");

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::F3),
        },
        &mut out,
    );
    let mut widened = String::new();
    for c in "docker".chars() {
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key(c),
            },
            &mut out,
        );
        assert_eq!(out.preedit_text(), "", "alnum modes never build a preedit");
        widened.push_str(out.commit_text().unwrap_or_default());
    }
    assert_eq!(widened, "ｄｏｃｋｅｒ");

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::F2),
        },
        &mut out,
    );
    let mut plain = String::new();
    for c in "docker".chars() {
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key(c),
            },
            &mut out,
        );
        plain.push_str(out.commit_text().unwrap_or_default());
    }
    assert_eq!(plain, "docker");
}

#[test]
fn an_oversized_preedit_answers_too_large_and_leaves_the_session_usable() {
    // A single entry whose output alone exceeds MAX_PREEDIT_BYTES, so
    // one keystroke overflows deterministically. This table maps
    // nothing but "a", so "usable afterward" is checked with a
    // character this same minimal table can actually resolve: "k" has
    // no entry, so it passes through raw immediately without reaching
    // the intentionally oversized "a" mapping again.
    let huge = "あ".repeat(600); // 1800 bytes > MAX_PREEDIT_BYTES (1536)
    let table = Table::parse(&format!("[kana]\na = \"{huge}\"\n")).expect("table compiles");
    let keymap = KeyMap::preset(Preset::MsIme).expect("preset compiles");
    let mut dispatcher = Dispatcher::with_parts(table, keymap, Normalizer::default());
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "app.exe");

    let reply = dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: char_key('a'),
        },
        &mut out,
    );

    assert_eq!(reply, Reply::Message(Response::Error(ErrorCode::TooLarge)));

    // The session must still be usable afterward, on the same table.
    type_word(&mut dispatcher, session, "k", &mut out);
    assert_eq!(out.preedit_text(), "k");
}

#[test]
fn a_preexisting_preedit_survives_an_overflowing_keystroke_instead_of_being_reset() {
    // Unlike the previous test, this one has real prior composition
    // content when the overflow happens. Before this fix, `Err(Overflow)`
    // reaching `send_key`'s catch site called `session.reset()`,
    // silently discarding everything the user had already typed just
    // because one more keystroke would not fit alongside it.
    let huge = "あ".repeat(600); // 1800 bytes > MAX_PREEDIT_BYTES (1536)
    let table = Table::parse(&format!("[kana]\na = \"{huge}\"\n")).expect("table compiles");
    let keymap = KeyMap::preset(Preset::MsIme).expect("preset compiles");
    let mut dispatcher = Dispatcher::with_parts(table, keymap, Normalizer::default());
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "app.exe");

    // "k" has no entry in this minimal table, so it passes through raw.
    type_word(&mut dispatcher, session, "kkk", &mut out);
    assert_eq!(out.preedit_text(), "kkk");

    let reply = dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: char_key('a'),
        },
        &mut out,
    );
    assert_eq!(reply, Reply::Message(Response::Error(ErrorCode::TooLarge)));

    assert_eq!(
        dispatcher.sessions.get(session).unwrap().preedit.as_str(),
        "kkk",
        "an overflowing keystroke must never erase the composition already typed"
    );

    // The session must still be usable afterward.
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: char_key('k'),
        },
        &mut out,
    );
    assert_eq!(out.preedit_text(), "kkkk");
}

#[test]
fn preedit_overflow_rejects_the_input_without_destroying_the_composition() {
    // Broader than `a_preexisting_preedit_survives_an_overflowing_
    // keystroke_instead_of_being_reset`: this snapshots the *entire*
    // `Session` (every field, via its derived `PartialEq`) rather than
    // only `preedit`, so a regression that partially advances
    // `raw_input`, `romaji`, `cursor`, segments, or any other field on
    // the way to the failing write cannot slip past.
    let huge = "あ".repeat(600); // 1800 bytes > MAX_PREEDIT_BYTES (1536)
    let table = Table::parse(&format!("[kana]\na = \"{huge}\"\n")).expect("table compiles");
    let keymap = KeyMap::preset(Preset::MsIme).expect("preset compiles");
    let mut dispatcher = Dispatcher::with_parts(table, keymap, Normalizer::default());
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "app.exe");

    // "k" has no entry in this minimal table, so it passes through raw,
    // giving real prior composition content -- preedit, raw_input, and
    // romaji all have something to lose if the overflowing keystroke
    // that follows is not fully atomic.
    type_word(&mut dispatcher, session, "kkk", &mut out);
    assert_eq!(out.preedit_text(), "kkk");

    let before = dispatcher.sessions.get(session).expect("session").clone();

    let reply = dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: char_key('a'),
        },
        &mut out,
    );

    assert_eq!(reply, Reply::Message(Response::Error(ErrorCode::TooLarge)));
    // `Reply::Message` promises an empty `OutputBuf`: no preedit, no
    // commit, nothing for the client to render or forward. The
    // offending key never leaves the engine's own rejection path.
    assert_eq!(out.preedit_text(), "");
    assert_eq!(out.commit_text(), None);

    let after = dispatcher.sessions.get(session).expect("session");
    assert_eq!(
        after, &before,
        "an overflowing keystroke must leave every session field \
             exactly as it was -- no commit, no partial write, no lost \
             composition"
    );

    // The session must still be usable afterward: Backspace still
    // works on the untouched composition.
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Backspace),
        },
        &mut out,
    );
    assert_eq!(
        out.preedit_text(),
        "kk",
        "Backspace must still work after a rejected overflow"
    );
}

#[test]
fn preedit_overflow_rejects_non_ascii_key_without_losing_or_leaking_it() {
    // Connects the asymmetry proven in
    // `romaji::tests::non_ascii_overflow_never_loses_the_current_character`:
    // the romaji FSM itself has nowhere to hold a non-ASCII character
    // that could not be written to its sink. If `feed_character` ever
    // wrote `session.romaji` or `session.preedit` back *before* every
    // fallible step had already succeeded, a non-ASCII keystroke
    // landing exactly at the preedit capacity boundary would vanish
    // for good -- not merely get rejected.
    // Use a single table entry to produce the exact boundary. This still
    // goes through the production `feed_character`/`table.feed` path, but
    // avoids dispatching one key for every byte while constructing the
    // fixture (which would make this test quadratic in the boundary
    // size). The output is deliberately the same plain ASCII `k` that
    // the old passthrough filler produced.
    let boundary = "k".repeat(MAX_PREEDIT_BYTES);
    let table = Table::parse(&format!("[kana]\na = \"{boundary}\"\n")).expect("table compiles");
    let keymap = KeyMap::preset(Preset::MsIme).expect("preset compiles");
    let mut dispatcher = Dispatcher::with_parts(table, keymap, Normalizer::default());
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "app.exe");

    // Fill preedit to exactly MAX_PREEDIT_BYTES with one key, leaving
    // zero bytes of room. Any further character -- ASCII or not -- must
    // now be rejected. Read the fill back from the session itself, not
    // from `out`: if the boundary dispatch happened to overflow, Apply's
    // catch site would have cleared `out` already, which would say
    // nothing about how much actually landed in `session.preedit`.
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: char_key('a'),
        },
        &mut out,
    );
    assert_eq!(
        dispatcher
            .sessions
            .get(session)
            .expect("session")
            .preedit
            .len(),
        MAX_PREEDIT_BYTES,
        "the filler must land byte-for-byte with no overflow of its own"
    );

    let before = dispatcher.sessions.get(session).expect("session").clone();

    // The offending key is a non-ASCII character. `table.feed` resolves
    // it cleanly into a scratch buffer (there is no pending romaji to
    // flush, and the scratch buffer itself is nowhere near capacity) --
    // the overflow only happens one step later, when `feed_character`
    // tries to splice that resolved text into a `preedit` that has no
    // room left.
    let reply = dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: char_key('字'),
        },
        &mut out,
    );

    assert_eq!(reply, Reply::Message(Response::Error(ErrorCode::TooLarge)));
    assert_eq!(out.preedit_text(), "");
    assert_eq!(out.commit_text(), None);

    let after = dispatcher.sessions.get(session).expect("session");
    assert_eq!(
        after, &before,
        "a non-ASCII overflow must leave the session untouched -- if \
             any field advanced past the rejected preedit write, the \
             character that could not be written would be gone from both \
             the document and the engine, with no way to retype it"
    );

    // The session must still be usable: Backspace frees room (three
    // presses -- one byte each -- to fit '字''s three UTF-8 bytes), and
    // a retry of the exact same non-ASCII key now succeeds, per
    // `table.feed`'s own documented retry contract.
    for _ in 0..3 {
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Backspace),
            },
            &mut out,
        );
    }
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: char_key('字'),
        },
        &mut out,
    );
    assert!(
        out.preedit_text().ends_with('字'),
        "retrying the exact same non-ASCII key after freeing room must \
             recover it, not lose it a second time"
    );
}

#[test]
fn probe_overflow_and_apply_overflow_agree_without_mutating_session() {
    // Mirrors `a_test_only_key_reports_what_the_real_dispatch_would_
    // without_mutating_the_session`, but for the overflow path. Probe
    // (`probe_session`) discards the `Result` of `apply_key` and always
    // answers `Reply::Output`, unlike Apply's explicit `out.clear()` +
    // `Reply::Message(Error(TooLarge))`. Full `Output` equality between
    // the two paths is not required here -- only that Probe's
    // `consumed` verdict agrees with Apply's decision to reject rather
    // than forward the key, so `OnTestKeyDown` never tells the host
    // "not mine" for a key the real path is about to swallow.
    let huge = "あ".repeat(600); // 1800 bytes > MAX_PREEDIT_BYTES (1536)
    let table_source = format!("[kana]\na = \"{huge}\"\n");

    let mut probe_dispatcher = Dispatcher::with_parts(
        Table::parse(&table_source).expect("table compiles"),
        KeyMap::preset(Preset::MsIme).expect("preset compiles"),
        Normalizer::default(),
    );
    let mut apply_dispatcher = Dispatcher::with_parts(
        Table::parse(&table_source).expect("table compiles"),
        KeyMap::preset(Preset::MsIme).expect("preset compiles"),
        Normalizer::default(),
    );
    let mut probe_out = OutputBuf::new();
    let mut apply_out = OutputBuf::new();
    let probe_session = create_session(&mut probe_dispatcher, &mut probe_out, "probe.exe");
    let apply_session = create_session(&mut apply_dispatcher, &mut apply_out, "apply.exe");

    type_word(&mut probe_dispatcher, probe_session, "kkk", &mut probe_out);
    type_word(&mut apply_dispatcher, apply_session, "kkk", &mut apply_out);

    let probe_before = probe_dispatcher
        .sessions
        .get(probe_session)
        .expect("probe session")
        .clone();
    let apply_before = apply_dispatcher
        .sessions
        .get(apply_session)
        .expect("apply session")
        .clone();

    let probe_reply = probe_dispatcher.dispatch(
        &Request::SendKey {
            session: probe_session,
            key: test_only_char_key('a'),
        },
        &mut probe_out,
    );
    assert_eq!(probe_reply, Reply::Output);
    let probe_consumed = probe_out.consumed;
    assert_eq!(
        probe_dispatcher
            .sessions
            .get(probe_session)
            .expect("probe session"),
        &probe_before,
        "Probe must never mutate the session, even on an overflowing key"
    );

    let apply_reply = apply_dispatcher.dispatch(
        &Request::SendKey {
            session: apply_session,
            key: char_key('a'),
        },
        &mut apply_out,
    );
    assert_eq!(
        apply_reply,
        Reply::Message(Response::Error(ErrorCode::TooLarge))
    );
    assert_eq!(
        apply_dispatcher
            .sessions
            .get(apply_session)
            .expect("apply session"),
        &apply_before,
        "Apply must leave the session untouched on Overflow"
    );

    assert!(
        probe_consumed,
        "Probe must report an overflowing key as consumed, agreeing \
             with Apply's decision to reject rather than forward it to the \
             host -- otherwise OnTestKeyDown tells TSF the key is free for \
             the host while the real key press is about to be swallowed by \
             a TooLarge rejection"
    );
}

#[test]
fn a_maximal_suggestion_commit_fits_exactly_at_the_preedit_boundary() {
    // `commit_suggestion_at` stages the candidate's normalized surface
    // into `scratch` (and the reading into a fresh `preedit`) *before*
    // clearing/mutating `session` at all, precisely so a fallible
    // normalization can never leave `romaji`/`raw_input`/`preedit`
    // cleared with nothing committed. That ordering is already in place
    // (see the comment at its `scratch.clear()` call).
    //
    // A prediction candidate's surface is typed as
    // `FixedStr<MAX_PREDICTION_SURFACE_BYTES>` (512 bytes) regardless of
    // source (system, user dictionary, or learned history) -- there is
    // no path that can hand `commit_suggestion_at` a larger one. The
    // widest expansion `normalize_into` can apply is 3x, ASCII (1 byte)
    // to fullwidth (3 bytes) alnum. 512 * 3 == 1536 == MAX_PREEDIT_BYTES
    // exactly, and `FixedStr::push_str` accepts a write that lands
    // exactly on capacity (`new_len > N` is the only rejection, not
    // `>=`). So the maximal legitimate suggestion commit always lands
    // precisely on the boundary and always succeeds: this specific
    // `Overflow` path is provably unreachable through any real
    // prediction candidate given today's constants, not merely untested.
    // This test locks in that exact-fit boundary instead of asserting an
    // overflow that cannot occur, so a future change narrowing either
    // constant (or widening the expansion ratio) will fail loudly here
    // rather than silently reopening the corruption `commit_suggestion_at`
    // was fixed to prevent.
    let full_surface = "x".repeat(crate::prediction::MAX_PREDICTION_SURFACE_BYTES);
    let conversion = prediction_conversion_from_source(
            "maximal-suggestion.tsv",
            &format!(
                "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\nあ\t{full_surface}\t0\t0\t100\t100\tpredict\ttest\n"
            ),
        );
    let learning = Arc::new(LearningService::memory());
    let runtime = crate::prediction::PredictionRuntime::start_with_learning(
        Arc::clone(&conversion),
        Arc::clone(&learning),
    )
    .expect("prediction runtime");
    let profile = AppProfile {
        process_name: "notepad.exe".to_owned(),
        default_mode: Mode::Hiragana,
        normalizer: Normalizer {
            width: WidthPolicy {
                alnum: Width::Full,
                number: Width::Half,
                symbol: Width::Half,
            },
            punctuation: PunctuationStyle::default(),
            brackets: BracketStyle::default(),
        },
        prediction_enabled: true,
        suggest_accept: SuggestAccept::Tab,
    };
    let mut dispatcher = Dispatcher::new_with_runtime_configuration_and_profiles(
        conversion,
        learning,
        runtime.service(),
        Preferences::default(),
        Arc::from(vec![profile]),
    )
    .expect("shipped defaults");
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
    type_word(&mut dispatcher, session, "a", &mut out);
    assert_eq!(out.preedit_text(), "あ");

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Tab),
        },
        &mut out,
    );
    assert_eq!(
        dispatcher.sessions.get(session).unwrap().state(),
        State::Predicting,
        "the 512-byte entry must be indexed and offered as a focusable suggestion"
    );

    let reply = dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Enter),
        },
        &mut out,
    );
    assert_ne!(
            reply,
            Reply::Message(Response::Error(ErrorCode::TooLarge)),
            "a maximal (512-byte) suggestion must fit exactly in the 1536-byte preedit, not overflow it"
        );
    let expected_commit = "\u{FF58}".repeat(crate::prediction::MAX_PREDICTION_SURFACE_BYTES);
    assert_eq!(out.commit_text(), Some(expected_commit.as_str()));
    assert_eq!(
        out.commit_text().unwrap().len(),
        crate::prediction::MAX_PREDICTION_SURFACE_BYTES * 3,
        "every one of the 512 ASCII bytes must have widened to a 3-byte fullwidth character"
    );

    assert_eq!(
        dispatcher.sessions.get(session).unwrap().state(),
        State::Idle
    );

    // The session must still be usable afterward.
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: char_key('k'),
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: char_key('a'),
        },
        &mut out,
    );
    assert_eq!(out.preedit_text(), "か");

    runtime.stop().expect("prediction worker joins");
}

#[test]
fn a_probed_overflowing_keystroke_never_mutates_the_live_session() {
    // The legacy `test_only` SendKey path (and `ProbeKey`, `probe_key`)
    // always runs against a throwaway clone (`probe_session`) and never
    // touches `self.sessions`, so this must hold with no change from
    // this fix -- unlike the general SendKey/Commit paths above, which
    // needed one.
    let huge = "あ".repeat(600);
    let table = Table::parse(&format!("[kana]\na = \"{huge}\"\n")).expect("table compiles");
    let keymap = KeyMap::preset(Preset::MsIme).expect("preset compiles");
    let mut dispatcher = Dispatcher::with_parts(table, keymap, Normalizer::default());
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "app.exe");
    type_word(&mut dispatcher, session, "kkk", &mut out);
    assert_eq!(out.preedit_text(), "kkk");

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: test_only_char_key('a'),
        },
        &mut out,
    );

    assert_eq!(
        dispatcher.sessions.get(session).unwrap().preedit.as_str(),
        "kkk",
        "a probed keystroke must never mutate the live session, overflowing or not"
    );
}

fn oversized_numbered_candidate_dispatcher() -> Dispatcher {
    // The first segment has a tiny dictionary surface but a 170-key raw
    // spelling. F9 expands that raw spelling to 510 bytes. The second
    // segment has a 1050-byte dictionary surface, while F6 temporarily
    // renders only the six-byte reading. Thus the transformed composition
    // fits, but a numbered pick that clears only the second transform
    // would stitch 510 + 1050 bytes and overflow MAX_PREEDIT_BYTES.
    let first_reading = "あ".repeat(170);
    let second_surface = "い".repeat(350);
    let source = format!(
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n{first_reading}\t短\t0\t0\t100\t100\t\tshort-dictionary-surface\nです\t{second_surface}\t0\t0\t100\t100\t\tlarge-default\n"
        );
    let entries =
        dictc::parse_entries("numbered-candidate-overflow.tsv", &source).expect("entries");
    let matrix = dictc::parse_connection(
        "matrix.tsv",
        "# license: MIT\nclasses\t1\ndefault\t0\n",
        false,
    )
    .expect("matrix");
    let image = Box::leak(
        dictc::compile(&entries, &matrix)
            .expect("image")
            .into_boxed_slice(),
    );
    let conversion =
        Arc::new(ConversionService::from_static_bytes(image).expect("conversion service fixture"));
    Dispatcher::new_with_conversion(conversion).expect("shipped defaults")
}

#[test]
fn an_overflowing_numbered_candidate_pick_restores_the_cleared_segment_transform() {
    // `commit_numbered_candidate` has to clear the focused segment's
    // transform *before* calling `commit_converted_segments`, or the
    // transform branch there shadows the numbered override entirely.
    // That clear used to be unconditional: if the subsequent commit
    // failed with `Overflow` (a different segment overflowing the
    // shared stitching buffer, in this case), the clear stuck around
    // even though nothing was actually committed -- silently discarding
    // an F6-F10 transform the user was still looking at.
    let mut dispatcher = oversized_numbered_candidate_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "editor.exe");

    let typed = format!("{}desu", "a".repeat(170));
    type_word(&mut dispatcher, session, &typed, &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    let converted = out.to_output().preedit.expect("converted preedit");
    assert_eq!(converted.segments.len(), 2);

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Right),
        },
        &mut out,
    );
    // The list must explicitly own number shortcuts. Focus it before
    // arming transforms; candidate navigation itself clears the focused
    // segment transform by design.
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Tab),
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::F6),
        },
        &mut out,
    );
    let before = dispatcher
        .sessions
        .get(session)
        .unwrap()
        .segment_transform(1);
    assert_ne!(
        before.0,
        SegmentTransform::None,
        "F6 must have armed a transform on the focused (です) segment"
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Left),
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::F9),
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Right),
        },
        &mut out,
    );
    let preedit_before = dispatcher
        .sessions
        .get(session)
        .unwrap()
        .preedit
        .as_str()
        .to_string();

    // "1" names the sole visible candidate. Clearing its F6 transform
    // exposes the large dictionary surface and makes the stitched commit
    // overflow while the first segment remains under F9.
    let reply = dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: char_key('1'),
        },
        &mut out,
    );
    assert_eq!(reply, Reply::Message(Response::Error(ErrorCode::TooLarge)));

    let after = dispatcher.sessions.get(session).unwrap();
    assert!(
        after.converting,
        "a failed numbered pick must not cancel the conversion"
    );
    assert_eq!(after.segment_count(), 2);
    assert_eq!(
        after.segment_transform(1),
        before,
        "the transform cleared to let the override apply must be restored \
             when the override's commit never went through"
    );
    assert_eq!(
        after.preedit.as_str(),
        preedit_before,
        "a failed numbered pick must not touch the composition being edited"
    );

    // The session must still be usable afterward. Cancelling the
    // conversion restores the raw reading rather than leaving a partial
    // commit or a poisoned candidate state.
    let reply = dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Escape),
        },
        &mut out,
    );
    assert_eq!(reply, Reply::Output);
    assert_eq!(
        dispatcher.sessions.get(session).unwrap().state(),
        State::Composing
    );
}

fn oversized_render_segment_dispatcher() -> Dispatcher {
    // きょう has four small candidates, so its selection index is
    // meaningfully normalized (`rem_euclid`) by a page-down jump that is
    // not a multiple of four. です's sole candidate is 600 bytes of
    // plain ASCII -- comfortably within dictc's own 1536-byte-per-field
    // cap, and small enough that the initial whole-string conversion at
    // Space (which stitches raw, unnormalized dictionary bytes) never
    // comes close to MAX_PREEDIT_BYTES either. It only overflows once
    // `render_converted_segments` normalizes it through this profile's
    // full-width alnum policy -- 600 ASCII bytes at up to 3 bytes each
    // fullwidth is 1800 bytes, past the 1536-byte scratch buffer -- so
    // rendering です always overflows on its own, regardless of きょう's
    // own state.
    let huge_ascii = "x".repeat(600);
    let source = format!(
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\nきょう\tA\t0\t0\t100\t100\t\tc0\nきょう\tB\t0\t0\t200\t200\t\tc1\nきょう\tC\t0\t0\t300\t300\t\tc2\nきょう\tD\t0\t0\t400\t400\t\tc3\nです\t{huge_ascii}\t0\t0\t100\t100\t\thuge-only\n"
        );
    let entries = dictc::parse_entries("render-segment-overflow.tsv", &source).expect("entries");
    let matrix = dictc::parse_connection(
        "matrix.tsv",
        "# license: MIT\nclasses\t1\ndefault\t0\n",
        false,
    )
    .expect("matrix");
    let image = Box::leak(
        dictc::compile(&entries, &matrix)
            .expect("image")
            .into_boxed_slice(),
    );
    let conversion =
        Arc::new(ConversionService::from_static_bytes(image).expect("conversion service fixture"));
    let profile = AppProfile {
        process_name: "editor.exe".to_owned(),
        default_mode: Mode::Hiragana,
        normalizer: Normalizer {
            width: WidthPolicy {
                alnum: Width::Full,
                number: Width::Half,
                symbol: Width::Half,
            },
            punctuation: PunctuationStyle::default(),
            brackets: BracketStyle::default(),
        },
        prediction_enabled: false,
        suggest_accept: SuggestAccept::Disabled,
    };
    Dispatcher::new_with_configuration_and_profiles(
        conversion,
        Arc::new(LearningService::memory()),
        Preferences::default(),
        Arc::from(vec![profile]),
    )
    .expect("shipped defaults")
}

#[test]
fn an_overflowing_render_leaves_an_earlier_segments_selection_untouched() {
    // `render_converted_segments` used to normalize (`rem_euclid`) and
    // immediately persist each segment's selection as soon as *that*
    // segment rendered successfully, one segment at a time. If a *later*
    // segment then overflowed the shared stitching buffer, the earlier
    // segment's persisted (normalized) selection stuck around even
    // though the render as a whole never completed and nothing new was
    // ever shown to the user.
    let mut dispatcher = oversized_render_segment_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "editor.exe");

    type_word(&mut dispatcher, session, "kyoudesu", &mut out);
    // です's sole candidate alone overflows, so conversion starts in an
    // already-overflowing state; that is fine here; nothing has changed
    // きょう's selection away from its initial 0 yet.
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    let started = dispatcher.sessions.get(session).unwrap();
    assert!(started.converting);
    assert_eq!(started.segment_count(), 2);
    assert_eq!(started.segment_selection(0), 0);

    // Page-down directly writes きょう's raw (unfolded) selection via
    // its own action handler, independent of rendering: 0 + 9 = 9.
    // Rendering that selection folds it to 9 % 4 = 1 -- a different
    // value, so persisting the fold is observable.
    let reply = dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::PageDown),
        },
        &mut out,
    );
    assert_eq!(reply, Reply::Message(Response::Error(ErrorCode::TooLarge)));

    let after = dispatcher.sessions.get(session).unwrap();
    assert!(
        after.converting,
        "a failed render must not cancel the conversion"
    );
    assert_eq!(after.segment_count(), 2);
    assert_eq!(
        after.segment_selection(0),
        CANDIDATE_PAGE_SIZE as i16,
        "the raw selection the page-down action wrote directly must survive \
             a render that failed on a later segment -- the render's own \
             normalized (rem_euclid) value must never have been persisted"
    );
}

#[test]
fn commit_request_flushes_pending_romaji_instead_of_dropping_it() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
    // A lone trailing "n" never resolves through `feed` alone -- it
    // needs a flush, which is exactly what a top-level Commit must do
    // rather than silently drop it.
    type_word(&mut dispatcher, session, "kon", &mut out);
    assert_eq!(out.preedit_text(), "こn");

    let reply = dispatcher.dispatch(&Request::Commit { session }, &mut out);

    assert_eq!(reply, Reply::Output);
    assert_eq!(out.commit_text(), Some("こん"));
    assert_eq!(out.preedit_text(), "");
}

#[test]
fn conversion_exposes_candidates_moves_selection_and_commits_the_focus() {
    let mut dispatcher = conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
    type_word(&mut dispatcher, session, "kana", &mut out);

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    let first = out.to_output().candidates.expect("candidate list");
    assert_eq!(first.presentation, CandidatePresentation::Compact);
    assert_eq!(first.selected, 0);
    assert!(first.items.len() > CANDIDATE_PAGE_SIZE);
    assert_eq!(first.page_size, CANDIDATE_PAGE_SIZE as u16);
    assert_eq!(first.items[0].text, "仮名");
    assert_eq!(first.items[0].annotation, "IT用語");
    assert_eq!(out.preedit_text(), "仮名");
    assert_eq!(
        dispatcher.sessions.get(session).map(Session::state),
        Some(sakura_core::keymap::State::Converting)
    );

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    let second = out.to_output().candidates.expect("candidate list");
    assert_eq!(second.presentation, CandidatePresentation::Expanded);
    assert_eq!(second.selected, 1);
    assert_eq!(out.preedit_text(), "加奈");

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Enter),
        },
        &mut out,
    );
    assert_eq!(out.commit_text(), Some("加奈"));
    assert_eq!(out.preedit_text(), "");
    assert_eq!(out.candidate_count(), 0);
    assert_eq!(
        dispatcher.sessions.get(session).map(Session::state),
        Some(sakura_core::keymap::State::Idle)
    );
}

#[test]
fn candidate_pages_and_number_shortcuts_use_the_visible_page() {
    let mut dispatcher = conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
    type_word(&mut dispatcher, session, "kana", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::PageDown),
        },
        &mut out,
    );
    let second_page = out.to_output().candidates.expect("second page");
    assert_eq!(usize::from(second_page.selected), CANDIDATE_PAGE_SIZE);
    assert_eq!(second_page.current_page(), 1);

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: char_key('9'),
        },
        &mut out,
    );
    assert!(out.beep, "a missing slot on the short final page must beep");
    assert!(
        out.has_candidates(),
        "an invalid shortcut keeps the list open"
    );

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: char_key('2'),
        },
        &mut out,
    );
    assert_eq!(out.commit_text(), Some("候補11"));
    assert_eq!(out.candidate_count(), 0);
    assert_eq!(
        dispatcher.sessions.get(session).map(Session::state),
        Some(sakura_core::keymap::State::Idle)
    );
}

#[test]
fn cancel_from_candidates_returns_to_raw_preedit_then_clears_it() {
    let mut dispatcher = conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
    type_word(&mut dispatcher, session, "kana", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Escape),
        },
        &mut out,
    );
    assert_eq!(out.preedit_text(), "かな");
    assert_eq!(out.candidate_count(), 0);
    assert_eq!(
        dispatcher.sessions.get(session).map(Session::state),
        Some(sakura_core::keymap::State::Composing)
    );

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Escape),
        },
        &mut out,
    );
    assert_eq!(out.preedit_text(), "");
    assert_eq!(
        dispatcher.sessions.get(session).map(Session::state),
        Some(sakura_core::keymap::State::Idle)
    );
}

#[test]
fn a_character_after_conversion_commits_then_starts_a_new_composition() {
    let mut dispatcher = conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
    type_word(&mut dispatcher, session, "kana", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: char_key('n'),
        },
        &mut out,
    );
    assert_eq!(out.commit_text(), Some("仮名"));
    assert_eq!(out.preedit_text(), "n");
    assert_eq!(out.candidate_count(), 0);
    assert_eq!(
        dispatcher.sessions.get(session).map(Session::state),
        Some(sakura_core::keymap::State::Composing)
    );
}

#[test]
fn caret_insertion_and_forward_delete_edit_at_the_visible_cursor() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "editor.exe");
    type_word(&mut dispatcher, session, "kana", &mut out);
    assert_eq!(out.preedit_text(), "かな");

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Left),
        },
        &mut out,
    );
    assert_eq!(
        out.to_output().preedit.expect("preedit").cursor,
        1,
        "caret must move by characters, not UTF-8 bytes"
    );

    type_word(&mut dispatcher, session, "na", &mut out);
    assert_eq!(out.preedit_text(), "かなな");
    assert_eq!(out.to_output().preedit.expect("preedit").cursor, 2);

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Delete),
        },
        &mut out,
    );
    assert_eq!(out.preedit_text(), "かな");

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Home),
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Delete),
        },
        &mut out,
    );
    assert_eq!(out.preedit_text(), "な");
    assert_eq!(out.to_output().preedit.expect("preedit").cursor, 0);
}

#[test]
fn f6_f10_transform_raw_composition_and_cycle_case_without_a_dictionary() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "editor.exe");
    type_word(&mut dispatcher, session, "gattsu", &mut out);

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::F7),
        },
        &mut out,
    );
    assert_eq!(out.preedit_text(), "ガッツ");
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::F8),
        },
        &mut out,
    );
    assert_eq!(out.preedit_text(), "ｶﾞｯﾂ");
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Enter),
        },
        &mut out,
    );
    assert_eq!(out.commit_text(), Some("ｶﾞｯﾂ"));

    type_word(&mut dispatcher, session, "docker", &mut out);
    for expected in ["docker", "DOCKER", "Docker"] {
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::F10),
            },
            &mut out,
        );
        assert_eq!(out.preedit_text(), expected);
    }
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::F9),
        },
        &mut out,
    );
    assert_eq!(out.preedit_text(), "ｄｏｃｋｅｒ");
}

#[test]
fn segment_focus_keeps_candidate_selections_independent() {
    let mut dispatcher = segmented_conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "editor.exe");
    type_word(&mut dispatcher, session, "kyoudesu", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    let initial = out.to_output().preedit.expect("converted preedit");
    assert_eq!(initial.segments.len(), 2);
    assert_eq!(initial.segments[0].text, "今日");
    assert_eq!(initial.segments[0].underline, UnderlineKind::Focused);
    assert_eq!(initial.segments[1].text, "です");

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Right),
        },
        &mut out,
    );
    let second_candidates = out
        .to_output()
        .candidates
        .expect("second segment candidates");
    let second_alternative = second_candidates.items[1].text.clone();
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Down),
        },
        &mut out,
    );
    let after_second = out.to_output().preedit.expect("converted preedit");
    assert_eq!(after_second.segments[0].text, "今日");
    assert_eq!(after_second.segments[1].text, second_alternative);

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Left),
        },
        &mut out,
    );
    let first_candidates = out
        .to_output()
        .candidates
        .expect("first segment candidates");
    let first_alternative = first_candidates.items[1].text.clone();
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Down),
        },
        &mut out,
    );
    let after_first = out.to_output().preedit.expect("converted preedit");
    assert_eq!(after_first.segments[0].text, first_alternative);
    assert_eq!(after_first.segments[1].text, second_alternative);

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: modified_named_key(KeyCode::Right, Modifiers::SHIFT),
        },
        &mut out,
    );
    assert_eq!(
        dispatcher
            .sessions
            .get(session)
            .expect("session")
            .segment_count(),
        2,
        "local resize must not merge or drop a neighbouring segment"
    );
}

#[test]
fn an_existing_dispatcher_observes_an_atomically_reloaded_user_dictionary() {
    let conversion = conversion_fixture();
    let mut dispatcher =
        Dispatcher::new_with_conversion(Arc::clone(&conversion)).expect("shipped defaults");
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "editor.exe");
    conversion.replace_user_dictionary(
        sakura_core::UserDictionary::parse_tsv(
            "reading\tsurface\tpos\tcomment\nさくら\tSakura Input\tproper-noun\tproject\n",
        )
        .expect("user dictionary"),
    );

    type_word(&mut dispatcher, session, "sakura", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );

    assert_eq!(out.preedit_text(), "Sakura Input");
    assert_eq!(
        out.to_output().candidates.expect("candidate list").items[0].annotation,
        "project"
    );
}

#[test]
fn commit_cache_reselects_a_homophone_within_the_session() {
    let mut dispatcher = conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "editor.exe");
    type_word(&mut dispatcher, session, "kana", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Down),
        },
        &mut out,
    );
    assert_eq!(out.preedit_text(), "加奈");
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Enter),
        },
        &mut out,
    );

    type_word(&mut dispatcher, session, "kana", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    assert_eq!(out.preedit_text(), "加奈");
    assert_eq!(out.selected_candidate(), Some(1));
}

#[test]
fn process_shared_learning_reselects_a_homophone_in_a_new_session() {
    let conversion = conversion_fixture();
    let learning = Arc::new(LearningService::memory());
    let mut first = Dispatcher::new_with_services(Arc::clone(&conversion), Arc::clone(&learning))
        .expect("shipped defaults");
    let mut out = OutputBuf::new();
    let first_session = create_session(&mut first, &mut out, "first.exe");
    type_word(&mut first, first_session, "kana", &mut out);
    for code in [KeyCode::Space, KeyCode::Down, KeyCode::Enter] {
        first.dispatch(
            &Request::SendKey {
                session: first_session,
                key: named_key(code),
            },
            &mut out,
        );
    }
    assert_eq!(out.commit_text(), Some("加奈"));

    let mut second =
        Dispatcher::new_with_services(conversion, Arc::clone(&learning)).expect("shipped defaults");
    let second_session = create_session(&mut second, &mut out, "second.exe");
    type_word(&mut second, second_session, "kana", &mut out);
    second.dispatch(
        &Request::SendKey {
            session: second_session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );

    assert_eq!(out.preedit_text(), "加奈");
    assert_eq!(out.selected_candidate(), Some(1));
    assert_eq!(
        learning
            .preference("かな", 0, [("仮名", 0), ("加奈", 0)])
            .exact,
        Some(1)
    );
}

#[test]
fn one_off_far_learning_does_not_override_base_conversion_but_repetition_does() {
    let conversion = conversion_fixture();
    let learning = Arc::new(LearningService::memory());
    let mut dispatcher =
        Dispatcher::new_with_services(conversion, Arc::clone(&learning)).expect("shipped defaults");
    let mut out = OutputBuf::new();

    // Candidate 6 is intentionally far below the base winner. A single
    // exact-context confirmation stays in the learning store but must not
    // make the next conversion surprising.
    learning.learn("かな", "候補07", 0, 0);
    let first = create_session(&mut dispatcher, &mut out, "first.exe");
    type_word(&mut dispatcher, first, "kana", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session: first,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    assert_eq!(out.preedit_text(), "仮名");
    assert_eq!(out.selected_candidate(), Some(0));

    // Repeated explicit evidence in the same grammatical context becomes
    // strong enough to select the user's genuine preference.
    learning.learn("かな", "候補07", 0, 0);
    learning.learn("かな", "候補07", 0, 0);
    let second = create_session(&mut dispatcher, &mut out, "second.exe");
    type_word(&mut dispatcher, second, "kana", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session: second,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    assert_eq!(out.preedit_text(), "候補07");
    assert_eq!(out.selected_candidate(), Some(6));
}

#[test]
fn explicit_learning_beats_conflicting_commit_cache_and_domain_coherence() {
    let conversion = conversion_fixture();
    let learning = Arc::new(LearningService::memory());
    let mut dispatcher =
        Dispatcher::new_with_services(conversion, Arc::clone(&learning)).expect("shipped defaults");
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "engineering-editor.exe");

    // Establish both lower layers in conflict with the eventual learned
    // choice: the session's domain ratio and commit cache prefer the IT
    // candidate `仮名`.
    for _ in 0..4 {
        type_word(&mut dispatcher, session, "kana", &mut out);
        for code in [KeyCode::Space, KeyCode::Enter] {
            dispatcher.dispatch(
                &Request::SendKey {
                    session,
                    key: named_key(code),
                },
                &mut out,
            );
        }
        assert_eq!(out.commit_text(), Some("仮名"));
    }

    learning.clear().expect("replace prior learning atomically");
    learning.learn("かな", "加奈", 0, 0);
    type_word(&mut dispatcher, session, "kana", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );

    assert_eq!(out.preedit_text(), "加奈");
    assert_eq!(out.selected_candidate(), Some(1));
}

fn capture_replay(label: &str, out: &OutputBuf, snapshot: &mut String) {
    let output = out.to_output();
    write!(
        snapshot,
        "{label}: consumed={} beep={} delete=",
        output.consumed, output.beep
    )
    .expect("snapshot write");
    if output.delete_before.is_empty() {
        snapshot.push_str("<none>");
    } else {
        write!(snapshot, "{:?}", output.delete_before).expect("snapshot write");
    }
    write!(snapshot, " mode={:?}", output.mode).expect("snapshot write");
    if let Some(preedit) = output.preedit {
        snapshot.push_str(" preedit=[");
        for (index, segment) in preedit.segments.iter().enumerate() {
            if index > 0 {
                snapshot.push(',');
            }
            write!(snapshot, "{}:{:?}", segment.text, segment.underline).expect("snapshot write");
        }
        write!(snapshot, "]@{}", preedit.cursor).expect("snapshot write");
    }
    if let Some(commit) = output.commit {
        write!(snapshot, " commit={commit}").expect("snapshot write");
    }
    if let Some(candidates) = output.candidates {
        let selected = usize::from(candidates.selected);
        write!(
            snapshot,
            " candidates={}/{}:{}|{}",
            selected,
            candidates.items.len(),
            candidates.items[selected].text,
            candidates.items[selected].annotation
        )
        .expect("snapshot write");
    }
    snapshot.push('\n');
}

#[test]
fn whole_session_editing_replay_matches_the_checked_in_snapshot() {
    let mut snapshot = String::new();
    let mut out = OutputBuf::new();
    let mut dispatcher = conversion_dispatcher();
    let session = create_session(&mut dispatcher, &mut out, "editor.exe");

    type_word(&mut dispatcher, session, "kana", &mut out);
    capture_replay("raw-kana", &out, &mut snapshot);
    for (label, key) in [
        ("convert", named_key(KeyCode::Space)),
        ("choose-second", named_key(KeyCode::Down)),
        ("commit-choice", named_key(KeyCode::Enter)),
    ] {
        dispatcher.dispatch(&Request::SendKey { session, key }, &mut out);
        capture_replay(label, &out, &mut snapshot);
    }
    type_word(&mut dispatcher, session, "kana", &mut out);
    capture_replay("raw-kana-again", &out, &mut snapshot);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    capture_replay("cache-reselect", &out, &mut snapshot);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Enter),
        },
        &mut out,
    );
    capture_replay("commit-cached", &out, &mut snapshot);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: modified_named_key(KeyCode::Backspace, Modifiers::CTRL),
        },
        &mut out,
    );
    capture_replay("undo-commit", &out, &mut snapshot);
    assert_eq!(
        dispatcher.dispatch(
            &Request::UndoCommit {
                session,
                outcome: UndoCommitOutcome::Applied,
            },
            &mut out,
        ),
        Reply::Message(Response::Ok)
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Escape),
        },
        &mut out,
    );
    capture_replay("cancel-restored-reading", &out, &mut snapshot);

    type_word(&mut dispatcher, session, "docker", &mut out);
    capture_replay("raw-identifier", &out, &mut snapshot);
    for (label, code) in [
        ("lower", KeyCode::F10),
        ("upper", KeyCode::F10),
        ("title", KeyCode::F10),
        ("full-width", KeyCode::F9),
    ] {
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(code),
            },
            &mut out,
        );
        capture_replay(label, &out, &mut snapshot);
    }

    let mut segmented = segmented_conversion_dispatcher();
    let segmented_session = create_session(&mut segmented, &mut out, "segments.exe");
    type_word(&mut segmented, segmented_session, "kyoudesu", &mut out);
    segmented.dispatch(
        &Request::SendKey {
            session: segmented_session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    capture_replay("segments", &out, &mut snapshot);
    segmented.dispatch(
        &Request::SendKey {
            session: segmented_session,
            key: modified_named_key(KeyCode::Left, Modifiers::SHIFT),
        },
        &mut out,
    );
    capture_replay("shrink-focused", &out, &mut snapshot);
    segmented.dispatch(
        &Request::SendKey {
            session: segmented_session,
            key: modified_named_key(KeyCode::Right, Modifiers::SHIFT),
        },
        &mut out,
    );
    capture_replay("restore-boundary", &out, &mut snapshot);
    segmented.dispatch(
        &Request::SendKey {
            session: segmented_session,
            key: named_key(KeyCode::Right),
        },
        &mut out,
    );
    capture_replay("focus-next", &out, &mut snapshot);

    assert_eq!(
        snapshot.trim_end(),
        include_str!("../../../corpus/session-replay/phase3-editing.snap").trim_end()
    );
}

#[test]
fn commit_undo_context_selects_the_contextual_homophone_and_rolls_it_back() {
    let mut dispatcher = contextual_conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "context.exe");
    type_word(&mut dispatcher, session, "ishani", &mut out);
    for code in [KeyCode::Space, KeyCode::Enter] {
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(code),
            },
            &mut out,
        );
    }
    assert_eq!(out.commit_text(), Some("医者に"));

    type_word(&mut dispatcher, session, "itta", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    assert_eq!(out.preedit_text(), "行った");
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Enter),
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: modified_named_key(KeyCode::Backspace, Modifiers::CTRL),
        },
        &mut out,
    );
    assert_eq!(out.delete_before(), "行った");
    assert_eq!(out.preedit_text(), "いった");

    let boundary_session = create_session(&mut dispatcher, &mut out, "boundary.exe");
    type_word(&mut dispatcher, boundary_session, "owari", &mut out);
    for code in [KeyCode::Space, KeyCode::Enter] {
        dispatcher.dispatch(
            &Request::SendKey {
                session: boundary_session,
                key: named_key(code),
            },
            &mut out,
        );
    }
    assert_eq!(out.commit_text(), Some("終わり。"));
    type_word(&mut dispatcher, boundary_session, "itta", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session: boundary_session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    assert_eq!(out.preedit_text(), "言った");
}

#[test]
fn password_scope_cannot_reach_persistent_learning() {
    let learning = LearningService::memory();
    let mut session = Session::new("password.exe");
    session.scope = InputScope::Password;
    session.preedit.push_str("かな").expect("bounded reading");

    record_learning(
        &session,
        Some(&learning),
        None,
        ExecutionPolicy::Apply,
        "加奈",
        0,
    );

    assert_eq!(
        learning.preference("かな", 0, [("仮名", 0), ("加奈", 0)]),
        LearningPreference {
            exact: None,
            general: None,
        }
    );
}

#[test]
fn ctrl_backspace_restores_the_reading_and_expires_after_another_key() {
    let mut dispatcher = conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "editor.exe");
    type_word(&mut dispatcher, session, "kana", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Down),
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Enter),
        },
        &mut out,
    );
    assert_eq!(out.commit_text(), Some("加奈"));

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: modified_named_key(KeyCode::Backspace, Modifiers::CTRL),
        },
        &mut out,
    );
    assert!(out.consumed);
    assert_eq!(out.delete_before(), "加奈");
    assert_eq!(out.preedit_text(), "かな");

    assert_eq!(
        dispatcher.dispatch(
            &Request::UndoCommit {
                session,
                outcome: UndoCommitOutcome::Applied,
            },
            &mut out,
        ),
        Reply::Message(Response::Ok)
    );

    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    assert_eq!(
        out.preedit_text(),
        "仮名",
        "undo must evict the retracted commit-cache choice"
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Enter),
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Left),
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: modified_named_key(KeyCode::Backspace, Modifiers::CTRL),
        },
        &mut out,
    );
    assert!(
        !out.consumed,
        "expired undo must reach the host application"
    );
    assert_eq!(out.delete_before(), "");
}

#[test]
fn commit_undo_pending_blocks_other_session_mutations_until_explicit_outcome() {
    let mut dispatcher = conversion_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "editor.exe");
    type_word(&mut dispatcher, session, "kana", &mut out);
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Space),
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: named_key(KeyCode::Enter),
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: modified_named_key(KeyCode::Backspace, Modifiers::CTRL),
        },
        &mut out,
    );
    assert_eq!(out.delete_before(), "仮名");
    let before = dispatcher.sessions.get(session).expect("session");
    assert!(before.undo_pending());
    assert_eq!(before.preedit.as_str(), "かな");
    assert_eq!(before.scope(), InputScope::Normal);

    // Simulate an external learning update while the host-side exact-text
    // deletion is pending. The incompatible requests below must be
    // fenced before dispatch can invalidate this live cache or advance
    // the observed epoch.
    let learning = Arc::new(LearningService::memory());
    dispatcher.observed_learning_generation = learning.generation();
    dispatcher.learning = Some(Arc::clone(&learning));
    dispatcher.prediction_cache.attempted = true;
    dispatcher.prediction_cache.session = session;
    dispatcher.prediction_cache.generation = 7;
    dispatcher.prediction_cache.has_result = true;
    let cache_before = (*dispatcher.prediction_cache).clone();
    let observed_before = dispatcher.observed_learning_generation;
    learning.learn("かな", "pending external update", 0, 0);

    let blocked = [
        Request::Commit { session },
        Request::Reconvert {
            session,
            text: "かな".to_owned(),
            preview: false,
        },
        Request::Revert { session },
        Request::SetInputScope {
            session,
            scope: InputScope::Password,
        },
        Request::SetMode {
            session,
            mode: Mode::Katakana,
        },
        Request::DeleteSession { session },
    ];
    for request in blocked {
        assert_eq!(
            dispatcher.dispatch(&request, &mut out),
            Reply::Message(Response::Error(ErrorCode::Busy)),
            "{request:?} must not escape a pending exact undo"
        );
        let current = dispatcher
            .sessions
            .get(session)
            .expect("session remains live");
        assert!(current.undo_pending());
        assert_eq!(current.preedit.as_str(), "かな");
        assert_eq!(current.scope(), InputScope::Normal);
        assert_eq!(
            *dispatcher.prediction_cache, cache_before,
            "a Busy request must not invalidate the live cache"
        );
        assert_eq!(
            dispatcher.observed_learning_generation, observed_before,
            "a Busy request must not advance the observed learning epoch"
        );
    }

    assert_eq!(
        dispatcher.dispatch(
            &Request::UndoCommit {
                session,
                outcome: UndoCommitOutcome::Rejected,
            },
            &mut out,
        ),
        Reply::Message(Response::Ok)
    );
    assert!(!dispatcher
        .sessions
        .get(session)
        .expect("session")
        .undo_pending());
}

#[test]
fn revert_request_discards_the_composition_without_committing() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
    type_word(&mut dispatcher, session, "ka", &mut out);

    let reply = dispatcher.dispatch(&Request::Revert { session }, &mut out);

    assert_eq!(reply, Reply::Message(Response::Ok));
    // Revert answers Ok directly (no Output), so a following SendKey
    // starting fresh is what proves the composition is actually gone.
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: char_key('k'),
        },
        &mut out,
    );
    assert_eq!(out.preedit_text(), "k");
}

#[test]
fn language_bar_mode_change_is_idle_scope_checked_and_never_writes_text() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

    // A fresh TSF connection has not yet classified its focused field.
    // The language bar must not guess that it is ordinary text.
    assert_eq!(
        dispatcher.dispatch(
            &Request::SetMode {
                session,
                mode: Mode::Katakana,
            },
            &mut out,
        ),
        Reply::Message(Response::Error(ErrorCode::Busy))
    );
    assert_eq!(
        dispatcher.sessions.get(session).expect("session").mode(),
        Mode::Hiragana
    );

    assert_eq!(
        dispatcher.dispatch(
            &Request::SetInputScope {
                session,
                scope: InputScope::Normal,
            },
            &mut out,
        ),
        Reply::Message(Response::Ok)
    );
    assert_eq!(
        dispatcher.dispatch(
            &Request::SetMode {
                session,
                mode: Mode::Katakana,
            },
            &mut out,
        ),
        Reply::Message(Response::InputMode {
            mode: Mode::Katakana
        })
    );
    assert_eq!(out.preedit_text(), "");
    assert_eq!(out.commit_text(), None);
    assert_eq!(
        dispatcher.sessions.get(session).expect("session").mode(),
        Mode::Katakana
    );

    // Once the user has preedit, the request is rejected rather than
    // committing, cancelling, or reinterpreting the document text.
    dispatcher.dispatch(
        &Request::SendKey {
            session,
            key: char_key('k'),
        },
        &mut out,
    );
    assert_eq!(out.preedit_text(), "k");
    assert_eq!(
        dispatcher.dispatch(
            &Request::SetMode {
                session,
                mode: Mode::Hiragana,
            },
            &mut out,
        ),
        Reply::Message(Response::Error(ErrorCode::Busy))
    );
    let current = dispatcher.sessions.get(session).expect("session");
    assert_eq!(current.mode(), Mode::Katakana);
    assert!(current.is_composing());
}

#[test]
fn ping_is_answered_with_pong() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    assert_eq!(
        dispatcher.dispatch(&Request::Ping, &mut out),
        Reply::Message(Response::Pong)
    );
}

#[test]
fn shared_learning_epoch_invalidates_every_dispatchers_prediction_cache() {
    let learning = Arc::new(LearningService::memory());
    let mut administrator = builtin_dispatcher();
    administrator.observed_learning_generation = learning.generation();
    administrator.learning = Some(Arc::clone(&learning));
    let mut other_connection = builtin_dispatcher();
    other_connection.observed_learning_generation = learning.generation();
    other_connection.learning = Some(Arc::clone(&learning));
    other_connection.prediction_cache.attempted = true;
    other_connection.prediction_cache.session = 7;
    other_connection.prediction_cache.generation = 9;

    let mut out = OutputBuf::new();
    assert_eq!(
        administrator.dispatch(&Request::ClearLearning, &mut out),
        Reply::Message(Response::Ok)
    );
    assert!(other_connection.prediction_cache.attempted);

    assert_eq!(
        other_connection.dispatch(&Request::Ping, &mut out),
        Reply::Message(Response::Pong)
    );
    assert!(!other_connection.prediction_cache.attempted);
    assert_eq!(
        other_connection.observed_learning_generation,
        learning.generation()
    );
}

#[test]
fn shutdown_is_answered_but_left_for_the_caller_to_act_on() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    assert_eq!(
        dispatcher.dispatch(&Request::Shutdown, &mut out),
        Reply::Shutdown(Response::Ok)
    );
}

#[test]
fn reset_drops_every_session_but_keeps_configuration() {
    let mut dispatcher = builtin_dispatcher();
    let mut out = OutputBuf::new();
    let first = create_session(&mut dispatcher, &mut out, "a.exe");

    dispatcher.reset();

    let reply = dispatcher.dispatch(
        &Request::SendKey {
            session: first,
            key: char_key('a'),
        },
        &mut out,
    );
    assert_eq!(
        reply,
        Reply::Message(Response::Error(ErrorCode::UnknownSession))
    );

    // Configuration survives: a new session still composes normally.
    let second = create_session(&mut dispatcher, &mut out, "b.exe");
    dispatcher.dispatch(
        &Request::SendKey {
            session: second,
            key: char_key('k'),
        },
        &mut out,
    );
    dispatcher.dispatch(
        &Request::SendKey {
            session: second,
            key: char_key('a'),
        },
        &mut out,
    );
    assert_eq!(out.preedit_text(), "か");
}
