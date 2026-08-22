//! Round-trip tests for the wire protocol: encode then decode must
//! reproduce the original value exactly, across every message variant and
//! a set of deliberately awkward edge cases (empty/maximal strings,
//! non-BMP characters, boundary integers, every enum value).

use sakura_proto::types::CandidatePresentation;
use sakura_proto::{
    decode_request, decode_response, encode_request, encode_response, payload_len, AppearanceTheme,
    Candidate, CandidateKind, CandidateList, Error, ErrorCode, InputScope, KeyCode, KeyInput, Mode,
    Modifiers, Output, OutputBuf, Preedit, Request, Response, ScreenRect, Segment, UiState,
    UnderlineKind, UndoCommitOutcome, FRAME_HEADER_LEN, MAX_STRING_BYTES, PROTOCOL_VERSION,
};

/// Encodes `req`, decodes it back, and asserts the id and value match.
fn roundtrip_request(req: &Request, id: u64) {
    let mut dst = Vec::new();
    encode_request(req, id, &mut dst).expect("encode_request");
    let len = payload_len(&[dst[0], dst[1], dst[2], dst[3]]).expect("payload_len");
    assert_eq!(len, dst.len() - FRAME_HEADER_LEN);
    let (decoded_id, decoded) = decode_request(&dst[FRAME_HEADER_LEN..]).expect("decode_request");
    assert_eq!(decoded_id, id);
    assert_eq!(&decoded, req);
}

/// Encodes `res`, decodes it back, and asserts the id and value match.
fn roundtrip_response(res: &Response, id: u64) {
    let mut dst = Vec::new();
    encode_response(res, id, &mut dst).expect("encode_response");
    let len = payload_len(&[dst[0], dst[1], dst[2], dst[3]]).expect("payload_len");
    assert_eq!(len, dst.len() - FRAME_HEADER_LEN);
    let (decoded_id, decoded) = decode_response(&dst[FRAME_HEADER_LEN..]).expect("decode_response");
    assert_eq!(decoded_id, id);
    assert_eq!(&decoded, res);
}

#[test]
fn every_request_variant_roundtrips() {
    let key = KeyInput {
        code: KeyCode::Char,
        ch: Some('a'),
        modifiers: Modifiers::NONE,
        repeat: false,
        test_only: false,
    };
    let variants = [
        Request::Hello {
            client_version: PROTOCOL_VERSION,
        },
        Request::CreateSession {
            process_name: "notepad.exe".to_string(),
        },
        Request::SendKey { session: 1, key },
        Request::ProbeKey {
            session: 1,
            scope: InputScope::Password,
            fresh_context: false,
            key,
        },
        Request::ProbeKey {
            session: 1,
            scope: InputScope::Normal,
            fresh_context: true,
            key,
        },
        Request::Commit { session: 1 },
        Request::Revert { session: 1 },
        Request::UndoCommit {
            session: 1,
            outcome: UndoCommitOutcome::Applied,
        },
        Request::UndoCommit {
            session: 1,
            outcome: UndoCommitOutcome::Rejected,
        },
        Request::UndoCommit {
            session: 1,
            outcome: UndoCommitOutcome::Unknown,
        },
        Request::Reconvert {
            session: 1,
            text: "仮名".to_owned(),
            preview: true,
        },
        Request::ClearLearning,
        Request::ClearInputHistory,
        Request::FlushInputHistory,
        Request::InputHistoryStats,
        Request::SetInputScope {
            session: 1,
            scope: InputScope::Email,
        },
        Request::SetMode {
            session: 1,
            mode: Mode::HalfKatakana,
        },
        Request::DeleteHistoryCandidate {
            revision: u64::MAX,
            candidate_index: u16::MAX,
        },
        Request::QueueCandidateCommit {
            revision: u64::MAX - 1,
            candidate_index: 7,
        },
        Request::PollCandidateCommit { session: 1 },
        Request::CommitCandidate {
            session: 1,
            revision: u64::MAX - 2,
            candidate_index: 8,
        },
        Request::DeleteSession { session: 1 },
        Request::Ping,
        Request::Shutdown,
        Request::WatchUi { since: 0 },
        Request::WatchUi { since: u64::MAX },
        Request::SetUiPlacement {
            session: 1,
            anchor: Some(ScreenRect {
                left: -1920,
                top: -32,
                right: -1800,
                bottom: 8,
            }),
            document: Some(ScreenRect {
                left: -1920,
                top: -64,
                right: -1600,
                bottom: 240,
            }),
            renderer_visible: true,
        },
    ];
    for (i, req) in variants.iter().enumerate() {
        roundtrip_request(req, i as u64);
    }
}

/// Fixed-seed property coverage for the complete renderer-side deletion
/// capability.  It deliberately includes values that look like wrapped UI
/// revisions and every possible `u16` index shape without adding a fuzzing
/// dependency to the production workspace.
#[test]
fn history_delete_request_preserves_random_revision_and_index_pairs() {
    let mut state = 0xD3E1_E7E0_29A5_4B17u64;
    for request_id in 0..4096u64 {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let revision = state;
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let candidate_index = (state >> 17) as u16;
        roundtrip_request(
            &Request::DeleteHistoryCandidate {
                revision,
                candidate_index,
            },
            request_id,
        );
    }
}

#[test]
fn every_response_variant_roundtrips() {
    let variants = [
        Response::Hello {
            server_version: PROTOCOL_VERSION,
            engine_version: [0, 1, 2],
        },
        Response::SessionCreated {
            session: 1,
            mode: Mode::Hiragana,
        },
        Response::InputMode {
            mode: Mode::HalfKatakana,
        },
        Response::HistoryCandidateDeleted { removed: false },
        Response::HistoryCandidateDeleted { removed: true },
        Response::CandidateCommitQueued { queued: false },
        Response::CandidateCommitQueued { queued: true },
        Response::CandidateCommitPending { request: None },
        Response::CandidateCommitPending {
            request: Some((u64::MAX, u16::MAX)),
        },
        Response::Output(Output {
            consumed: true,
            beep: false,
            mode: Some(Mode::Hiragana),
            preedit: Some(Preedit {
                segments: vec![Segment {
                    text: "あ".to_string(),
                    underline: UnderlineKind::Raw,
                }],
                cursor: 1,
            }),
            commit: Some("commit".to_string()),
            delete_before: String::new(),
            candidates: None,
            candidate_detail: None,
        }),
        Response::Pong,
        Response::Ok,
        Response::InputHistoryStats {
            active: true,
            dropped_events: 7,
            persistence_failures: 3,
            excluded_unclassified_events: 11,
            excluded_sensitive_events: 13,
            excluded_test_only_events: 17,
            ai_requests: 19,
            ai_attempts: 23,
            ai_input_tokens: 29,
            ai_output_tokens: 31,
            ai_cached_tokens: 37,
        },
        Response::Ui(UiState {
            revision: 1,
            appearance_theme: AppearanceTheme::Dark,
            mode: Some(Mode::HalfAlnum),
            candidates: Some(CandidateList {
                kind: CandidateKind::Conversion,
                presentation: CandidatePresentation::Compact,
                items: vec![Candidate {
                    text: "候補".to_owned(),
                    annotation: "注釈".to_owned(),
                    deletable_history: true,
                }],
                selected: 0,
                page_size: 9,
            }),
            candidate_detail: None,
            anchor: Some(ScreenRect {
                left: -100,
                top: 10,
                right: -40,
                bottom: 34,
            }),
            document: Some(ScreenRect {
                left: -120,
                top: 4,
                right: 200,
                bottom: 300,
            }),
            renderer_visible: true,
            stopping: false,
        }),
        // No mode means "hide the indicator", and it has to survive the
        // wire as distinctly as any mode does.
        Response::Ui(UiState {
            revision: u64::MAX,
            appearance_theme: AppearanceTheme::Auto,
            mode: None,
            candidates: None,
            candidate_detail: None,
            anchor: None,
            document: None,
            renderer_visible: false,
            stopping: false,
        }),
        // The farewell. Losing this flag on the wire would turn an
        // uninstall into a watchdog restarting the engine being removed.
        Response::Ui(UiState {
            revision: 2,
            appearance_theme: AppearanceTheme::Light,
            mode: Some(Mode::Hiragana),
            candidates: None,
            candidate_detail: None,
            anchor: None,
            document: None,
            renderer_visible: false,
            stopping: true,
        }),
        Response::Error(ErrorCode::Busy),
    ];
    for (i, res) in variants.iter().enumerate() {
        roundtrip_response(res, i as u64);
    }
}

#[test]
fn commit_undo_output_roundtrips_with_exact_utf8_text() {
    roundtrip_response(
        &Response::Output(Output {
            consumed: true,
            beep: false,
            mode: None,
            preedit: Some(Preedit {
                segments: vec![Segment {
                    text: "かな".to_owned(),
                    underline: UnderlineKind::Raw,
                }],
                cursor: 2,
            }),
            commit: None,
            delete_before: "🍣かな".to_owned(),
            candidates: None,
            candidate_detail: None,
        }),
        91,
    );
}

#[test]
fn empty_strings_roundtrip() {
    roundtrip_request(
        &Request::CreateSession {
            process_name: String::new(),
        },
        1,
    );
    roundtrip_response(
        &Response::Output(Output {
            consumed: false,
            beep: false,
            mode: None,
            preedit: Some(Preedit {
                segments: vec![Segment {
                    text: String::new(),
                    underline: UnderlineKind::Raw,
                }],
                cursor: 0,
            }),
            commit: Some(String::new()),
            delete_before: String::new(),
            candidates: None,
            candidate_detail: None,
        }),
        1,
    );
}

#[test]
fn string_of_exactly_max_string_bytes_roundtrips() {
    let s = "a".repeat(MAX_STRING_BYTES);
    assert_eq!(s.len(), MAX_STRING_BYTES);
    roundtrip_request(
        &Request::CreateSession {
            process_name: s.clone(),
        },
        1,
    );
}

#[test]
fn string_over_max_string_bytes_is_rejected_at_encode() {
    let s = "a".repeat(MAX_STRING_BYTES + 1);
    let mut dst = Vec::new();
    let result = encode_request(&Request::CreateSession { process_name: s }, 1, &mut dst);
    assert_eq!(result, Err(Error::TooLarge));
}

#[test]
fn non_bmp_characters_roundtrip_in_every_position() {
    // 𠮷 U+20BB7 and 🍣 U+1F363: both outside the Basic Multilingual Plane,
    // so both require surrogate pairs in UTF-16 -- exactly the case the
    // protocol pushes onto the DLL's TSF boundary instead of handling here.
    let non_bmp_a = '\u{20BB7}';
    let non_bmp_b = '\u{1F363}';

    // In KeyInput::ch.
    let key = KeyInput {
        code: KeyCode::Char,
        ch: Some(non_bmp_a),
        modifiers: Modifiers::NONE,
        repeat: false,
        test_only: false,
    };
    roundtrip_request(&Request::SendKey { session: 1, key }, 1);

    // In segment text and commit text.
    let mut text = String::new();
    text.push(non_bmp_a);
    text.push(non_bmp_b);
    let output = Output {
        consumed: true,
        beep: false,
        mode: None,
        preedit: Some(Preedit {
            segments: vec![Segment {
                text: text.clone(),
                underline: UnderlineKind::Focused,
            }],
            cursor: 2,
        }),
        commit: Some(text),
        delete_before: String::new(),
        candidates: None,
        candidate_detail: None,
    };
    roundtrip_response(&Response::Output(output), 1);
}

#[test]
fn cursor_and_session_at_u32_and_u64_max_roundtrip() {
    let output = Output {
        consumed: false,
        beep: false,
        mode: None,
        preedit: Some(Preedit {
            segments: Vec::new(),
            cursor: u32::MAX,
        }),
        commit: None,
        delete_before: String::new(),
        candidates: None,
        candidate_detail: None,
    };
    roundtrip_response(&Response::Output(output), 1);
    roundtrip_request(&Request::Commit { session: u64::MAX }, 1);
    roundtrip_response(
        &Response::SessionCreated {
            session: u64::MAX,
            mode: Mode::Hiragana,
        },
        1,
    );
}

#[test]
fn max_segments_roundtrips() {
    let segments = (0..sakura_proto::MAX_SEGMENTS)
        .map(|i| Segment {
            text: format!("s{i}"),
            underline: match i % 3 {
                0 => UnderlineKind::Raw,
                1 => UnderlineKind::Converted,
                _ => UnderlineKind::Focused,
            },
        })
        .collect::<Vec<_>>();
    let output = Output {
        consumed: true,
        beep: false,
        mode: None,
        preedit: Some(Preedit {
            segments,
            cursor: 0,
        }),
        commit: None,
        delete_before: String::new(),
        candidates: None,
        candidate_detail: None,
    };
    roundtrip_response(&Response::Output(output), 1);
}

#[test]
fn every_key_code_roundtrips() {
    for &code in KeyCode::ALL.iter() {
        let key = KeyInput {
            code,
            ch: None,
            modifiers: Modifiers::NONE,
            repeat: false,
            test_only: false,
        };
        roundtrip_request(&Request::SendKey { session: 1, key }, 1);
    }
}

#[test]
fn every_mode_roundtrips() {
    for &mode in Mode::ALL.iter() {
        let output = Output {
            consumed: false,
            beep: false,
            mode: Some(mode),
            preedit: None,
            commit: None,
            delete_before: String::new(),
            candidates: None,
            candidate_detail: None,
        };
        roundtrip_response(&Response::Output(output), 1);
    }
}

#[test]
fn every_input_scope_roundtrips() {
    for &scope in InputScope::ALL.iter() {
        roundtrip_request(&Request::SetInputScope { session: 1, scope }, 1);
    }
}

#[test]
fn every_underline_kind_roundtrips() {
    for &underline in UnderlineKind::ALL.iter() {
        let output = Output {
            consumed: false,
            beep: false,
            mode: None,
            preedit: Some(Preedit {
                segments: vec![Segment {
                    text: "x".to_string(),
                    underline,
                }],
                cursor: 0,
            }),
            commit: None,
            delete_before: String::new(),
            candidates: None,
            candidate_detail: None,
        };
        roundtrip_response(&Response::Output(output), 1);
    }
}

#[test]
fn every_error_code_roundtrips() {
    for &code in ErrorCode::ALL.iter() {
        roundtrip_response(&Response::Error(code), 1);
    }
}

#[test]
fn request_ids_roundtrip_exactly_including_u64_max() {
    for &id in &[0u64, 1, 12345, u64::MAX - 1, u64::MAX] {
        roundtrip_request(&Request::Ping, id);
    }
}

#[test]
fn protocol_v18_hello_roundtrips_and_v17_payloads_are_rejected() {
    const PREVIOUS_PROTOCOL_VERSION: u16 = 17;
    assert_eq!(
        PROTOCOL_VERSION, 18,
        "candidate click operations change the wire contract"
    );

    let request = Request::Hello {
        client_version: PROTOCOL_VERSION,
    };
    let mut request_frame = Vec::new();
    encode_request(&request, 18, &mut request_frame).expect("encode v18 request");
    assert_eq!(
        &request_frame[FRAME_HEADER_LEN..FRAME_HEADER_LEN + 2],
        &PROTOCOL_VERSION.to_le_bytes()
    );
    assert_eq!(
        decode_request(&request_frame[FRAME_HEADER_LEN..]),
        Ok((18, request))
    );
    request_frame[FRAME_HEADER_LEN..FRAME_HEADER_LEN + 2]
        .copy_from_slice(&PREVIOUS_PROTOCOL_VERSION.to_le_bytes());
    assert_eq!(
        decode_request(&request_frame[FRAME_HEADER_LEN..]),
        Err(Error::UnsupportedVersion(PREVIOUS_PROTOCOL_VERSION))
    );

    let response = Response::Hello {
        server_version: PROTOCOL_VERSION,
        engine_version: [1, 0, 0],
    };
    let mut response_frame = Vec::new();
    encode_response(&response, 18, &mut response_frame).expect("encode v18 response");
    assert_eq!(
        &response_frame[FRAME_HEADER_LEN..FRAME_HEADER_LEN + 2],
        &PROTOCOL_VERSION.to_le_bytes()
    );
    assert_eq!(
        decode_response(&response_frame[FRAME_HEADER_LEN..]),
        Ok((18, response))
    );
    response_frame[FRAME_HEADER_LEN..FRAME_HEADER_LEN + 2]
        .copy_from_slice(&PREVIOUS_PROTOCOL_VERSION.to_le_bytes());
    assert_eq!(
        decode_response(&response_frame[FRAME_HEADER_LEN..]),
        Err(Error::UnsupportedVersion(PREVIOUS_PROTOCOL_VERSION))
    );
}

#[test]
fn output_buf_encode_frame_matches_decode_and_to_output() {
    let mut buf = OutputBuf::new();
    buf.consumed = true;
    buf.beep = true;
    buf.mode = Some(Mode::FullAlnum);
    buf.begin_preedit();
    buf.push_segment("𠮷", UnderlineKind::Raw).expect("push");
    buf.push_segment("野家", UnderlineKind::Converted)
        .expect("push");
    buf.set_cursor(3);
    buf.set_commit("🍣定食").expect("set_commit");

    let mut frame = [0u8; 512];
    let n = buf.encode_frame(7, &mut frame).expect("encode_frame");
    let (id, response) = decode_response(&frame[FRAME_HEADER_LEN..n]).expect("decode_response");
    assert_eq!(id, 7);
    assert_eq!(response, Response::Output(buf.to_output()));
}

/// The same check, but against a hand-written expectation instead of
/// `to_output()`.
///
/// The test above cannot catch a bug in `OutputBuf`'s span arithmetic: the
/// wire bytes and the expected value are both produced by slicing the preedit
/// with the same spans, so a wrong `start` or `len` corrupts both sides
/// identically and the comparison still succeeds. Writing the expected
/// segments out by hand breaks that shared derivation.
///
/// The segment lengths are deliberately unequal in bytes (4, 6, 3, 15) and
/// unequal between bytes and characters, so an off-by-one, a swapped
/// start/end, or a byte/char confusion cannot happen to land on the right
/// answer.
#[test]
fn output_buf_segments_match_hand_written_expectation() {
    let mut buf = OutputBuf::new();
    buf.consumed = true;
    buf.mode = Some(Mode::Hiragana);
    buf.begin_preedit();
    buf.push_segment("𠮷", UnderlineKind::Raw).expect("push");
    buf.push_segment("野家", UnderlineKind::Converted)
        .expect("push");
    buf.push_segment("の", UnderlineKind::Raw).expect("push");
    buf.push_segment("ぎゅうどん", UnderlineKind::Focused)
        .expect("push");
    buf.set_cursor(9);

    let expected = Output {
        consumed: true,
        beep: false,
        mode: Some(Mode::Hiragana),
        preedit: Some(Preedit {
            segments: vec![
                Segment {
                    text: "𠮷".to_string(),
                    underline: UnderlineKind::Raw,
                },
                Segment {
                    text: "野家".to_string(),
                    underline: UnderlineKind::Converted,
                },
                Segment {
                    text: "の".to_string(),
                    underline: UnderlineKind::Raw,
                },
                Segment {
                    text: "ぎゅうどん".to_string(),
                    underline: UnderlineKind::Focused,
                },
            ],
            cursor: 9,
        }),
        commit: None,
        delete_before: String::new(),
        candidates: None,
        candidate_detail: None,
    };

    let mut frame = [0u8; 512];
    let n = buf.encode_frame(11, &mut frame).expect("encode_frame");
    let (id, response) = decode_response(&frame[FRAME_HEADER_LEN..n]).expect("decode_response");
    assert_eq!(id, 11);
    assert_eq!(response, Response::Output(expected));
}

#[test]
fn candidate_list_roundtrips_and_matches_output_buf() {
    let candidates = CandidateList {
        kind: CandidateKind::Suggestion,
        presentation: CandidatePresentation::Expanded,
        items: vec![
            Candidate {
                text: "かな".to_string(),
                annotation: "ひらがな".to_string(),
                deletable_history: false,
            },
            Candidate {
                text: "仮名".to_string(),
                annotation: "IT用語".to_string(),
                deletable_history: false,
            },
        ],
        selected: 1,
        page_size: 9,
    };
    let output = Output {
        consumed: true,
        beep: false,
        mode: None,
        preedit: None,
        commit: None,
        delete_before: String::new(),
        candidates: Some(candidates.clone()),
        candidate_detail: None,
    };
    roundtrip_response(&Response::Output(output), 23);

    let mut buf = OutputBuf::new();
    buf.consumed = true;
    buf.begin_suggestions(1, 9).expect("begin suggestions");
    for candidate in &candidates.items {
        buf.push_candidate(&candidate.text, &candidate.annotation)
            .expect("push candidate");
    }
    assert_eq!(buf.to_output().candidates, Some(candidates));
}

#[test]
fn expanded_conversion_candidate_list_roundtrips() {
    let candidates = CandidateList {
        kind: CandidateKind::Conversion,
        presentation: CandidatePresentation::Expanded,
        items: vec![
            Candidate {
                text: "first".to_string(),
                annotation: String::new(),
                deletable_history: false,
            },
            Candidate {
                text: "second".to_string(),
                annotation: String::new(),
                deletable_history: false,
            },
        ],
        selected: 1,
        page_size: 9,
    };
    assert_eq!(candidates.presentation, CandidatePresentation::Expanded);
    assert_eq!(candidates.visible_range(), 0..2);
    roundtrip_response(
        &Response::Output(Output {
            consumed: true,
            beep: false,
            mode: None,
            preedit: None,
            commit: None,
            delete_before: String::new(),
            candidates: Some(candidates),
            candidate_detail: None,
        }),
        24,
    );
}

/// The spans themselves, checked directly rather than through the encoder.
///
/// `segments()` is public, so a consumer may slice `preedit_text()` with a
/// span without going near the wire format. That path deserves its own
/// assertion against literal strings.
#[test]
fn output_buf_spans_slice_the_preedit_correctly() {
    let mut buf = OutputBuf::new();
    buf.begin_preedit();
    buf.push_segment("𠮷", UnderlineKind::Raw).expect("push");
    buf.push_segment("野家", UnderlineKind::Converted)
        .expect("push");
    buf.push_segment("の", UnderlineKind::Raw).expect("push");
    buf.push_segment("ぎゅうどん", UnderlineKind::Focused)
        .expect("push");

    assert_eq!(buf.preedit_text(), "𠮷野家のぎゅうどん");

    let text = buf.preedit_text();
    let sliced: Vec<&str> = buf
        .segments()
        .iter()
        .map(|span| {
            let start = span.start as usize;
            let end = start + span.len as usize;
            text.get(start..end)
                .expect("span must be in bounds and on a character boundary")
        })
        .collect();

    assert_eq!(sliced, ["𠮷", "野家", "の", "ぎゅうどん"]);
}
