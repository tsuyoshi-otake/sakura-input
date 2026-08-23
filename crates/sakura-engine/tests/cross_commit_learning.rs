//! Cross-commit context and personalization authority integration tests.
//!
//! These fixtures deliberately use vocabulary unrelated to the reported
//! `ないか` case. The invariant is provenance-based: once bounded replay
//! improves at least one direct candidate, cached or learned preferences may
//! select only a candidate supported by that same replay. Without such
//! evidence, the existing learning behavior is unchanged.

use std::sync::Arc;

use dictc::{compile, parse_connection, parse_entries};
use sakura_core::Preferences;
use sakura_engine::dictionary::ConversionService;
use sakura_engine::dispatch::{Dispatcher, Reply};
use sakura_engine::learning::LearningService;
use sakura_proto::{
    InputScope, KeyCode, KeyInput, Modifiers, OutputBuf, Request, Response, SessionId,
};

fn conversion_service() -> Arc<ConversionService> {
    let entries = parse_entries(
        "cross-commit-learning.tsv",
        "# license: MIT\n\
         reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n\
         けんとう\t検討\t1\t1\t0\t-\t\t\n\
         かくにん\t確認\t1\t1\t0\t-\t\t\n\
         いたします\t致します\t1\t1\t50\t-\t\t\n\
         いたします\t板します\t1\t1\t100\t-\t\t\n\
         けんとういたします\t検討致します\t1\t1\t10\t-\t\t\n\
         かくにんいたします\t確認致します\t1\t1\t10\t-\t\t\n",
    )
    .expect("generic entries");
    let matrix = parse_connection(
        "cross-commit-learning-matrix.tsv",
        "# license: MIT\nclasses\t3\ndefault\t0\n",
        false,
    )
    .expect("generic matrix");
    let image = compile(&entries, &matrix)
        .expect("generic image")
        .into_boxed_slice();
    Arc::new(
        ConversionService::from_static_bytes(Box::leak(image)).expect("generic conversion service"),
    )
}

fn char_key(ch: char) -> KeyInput {
    KeyInput {
        code: KeyCode::Char,
        ch: Some(ch),
        modifiers: Modifiers::NONE,
        repeat: false,
        test_only: false,
    }
}

fn normal_session(
    dispatcher: &mut Dispatcher,
    out: &mut OutputBuf,
    process_name: &str,
) -> SessionId {
    let session = match dispatcher.dispatch(
        &Request::CreateSession {
            process_name: process_name.to_owned(),
        },
        out,
    ) {
        Reply::Message(Response::SessionCreated { session, .. }) => session,
        other => panic!("expected SessionCreated, got {other:?}"),
    };
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
    session
}

fn type_romaji(dispatcher: &mut Dispatcher, session: SessionId, out: &mut OutputBuf, romaji: &str) {
    for character in romaji.chars() {
        assert_eq!(
            dispatcher.dispatch(
                &Request::SendKey {
                    session,
                    key: char_key(character),
                },
                out,
            ),
            Reply::Output
        );
    }
}

fn convert_rendered(
    dispatcher: &mut Dispatcher,
    session: SessionId,
    out: &mut OutputBuf,
) -> String {
    assert_eq!(
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: KeyInput {
                    code: KeyCode::Space,
                    ch: None,
                    modifiers: Modifiers::NONE,
                    repeat: false,
                    test_only: false,
                },
            },
            out,
        ),
        Reply::Output
    );
    out.to_output()
        .preedit
        .map(|preedit| {
            preedit
                .segments
                .iter()
                .map(|segment| segment.text.as_str())
                .collect::<String>()
        })
        .unwrap_or_default()
}

#[test]
fn learned_homophone_strength_matrix_is_unchanged_without_bridge_evidence() {
    for (frequency, expected) in [
        (1, "致します"),
        (2, "板します"),
        (3, "板します"),
        (8, "板します"),
    ] {
        let learning = Arc::new(LearningService::memory());
        for _ in 0..frequency {
            learning.learn("いたします", "板します", 1, 1);
        }
        let mut dispatcher = Dispatcher::new_with_configuration(
            conversion_service(),
            learning,
            Preferences::default(),
        )
        .expect("no-context dispatcher with learned homophone");
        let mut out = OutputBuf::new();
        let session = normal_session(
            &mut dispatcher,
            &mut out,
            "generic-learning-strength-control.exe",
        );
        type_romaji(&mut dispatcher, session, &mut out, "itashimasu");
        assert_eq!(
            convert_rendered(&mut dispatcher, session, &mut out),
            expected,
            "no-context behavior changed at learned frequency {frequency}"
        );
    }
}

#[test]
fn bridge_evidence_bounds_learned_homophones_across_lexemes_and_strengths() {
    for (tail_romaji, expected_tail) in [("kenntou", "検討"), ("kakuninn", "確認")] {
        for frequency in [1, 2, 3, 8] {
            let learning = Arc::new(LearningService::memory());
            for _ in 0..frequency {
                learning.learn("いたします", "板します", 1, 1);
            }
            let mut dispatcher = Dispatcher::new_with_configuration(
                conversion_service(),
                learning,
                Preferences::default(),
            )
            .expect("dispatcher with learned homophone");
            let mut out = OutputBuf::new();
            let session = normal_session(
                &mut dispatcher,
                &mut out,
                "generic-cross-commit-learning.exe",
            );

            type_romaji(&mut dispatcher, session, &mut out, tail_romaji);
            assert_eq!(
                convert_rendered(&mut dispatcher, session, &mut out),
                expected_tail,
                "generic tail fixture must select the intended context"
            );
            assert_eq!(
                dispatcher.dispatch(
                    &Request::SendKey {
                        session,
                        key: KeyInput {
                            code: KeyCode::Enter,
                            ch: None,
                            modifiers: Modifiers::NONE,
                            repeat: false,
                            test_only: false,
                        },
                    },
                    &mut out,
                ),
                Reply::Output
            );

            type_romaji(&mut dispatcher, session, &mut out, "itashimasu");
            assert_eq!(
                convert_rendered(&mut dispatcher, session, &mut out),
                "致します",
                "tail={expected_tail}, frequency={frequency}: unrelated learning crossed proven context"
            );
        }
    }
}
