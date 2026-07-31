//! Round-trip tests for the wire protocol: encode then decode must
//! reproduce the original value exactly, across every message variant and
//! a set of deliberately awkward edge cases (empty/maximal strings,
//! non-BMP characters, boundary integers, every enum value).

use sakura_proto::{
    decode_request, decode_response, encode_request, encode_response, payload_len, Error,
    ErrorCode, InputScope, KeyCode, KeyInput, Mode, Modifiers, Output, OutputBuf, Preedit, Request,
    Response, Segment, UnderlineKind, FRAME_HEADER_LEN, MAX_STRING_BYTES, PROTOCOL_VERSION,
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
        Request::Commit { session: 1 },
        Request::Revert { session: 1 },
        Request::SetInputScope {
            session: 1,
            scope: InputScope::Email,
        },
        Request::DeleteSession { session: 1 },
        Request::Ping,
        Request::Shutdown,
    ];
    for (i, req) in variants.iter().enumerate() {
        roundtrip_request(req, i as u64);
    }
}

#[test]
fn every_response_variant_roundtrips() {
    let variants = [
        Response::Hello {
            server_version: PROTOCOL_VERSION,
            engine_version: [0, 1, 2],
        },
        Response::SessionCreated { session: 1 },
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
        }),
        Response::Pong,
        Response::Ok,
        Response::Error(ErrorCode::Busy),
    ];
    for (i, res) in variants.iter().enumerate() {
        roundtrip_response(res, i as u64);
    }
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
    };
    roundtrip_response(&Response::Output(output), 1);
    roundtrip_request(&Request::Commit { session: u64::MAX }, 1);
    roundtrip_response(&Response::SessionCreated { session: u64::MAX }, 1);
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
fn wrong_protocol_version_decodes_to_unsupported_version() {
    let mut dst = Vec::new();
    encode_request(&Request::Ping, 1, &mut dst).expect("encode");
    dst[FRAME_HEADER_LEN] = 0x02;
    dst[FRAME_HEADER_LEN + 1] = 0x00;
    let result = decode_request(&dst[FRAME_HEADER_LEN..]);
    assert_eq!(result, Err(Error::UnsupportedVersion(2)));

    let mut dst = Vec::new();
    encode_response(&Response::Pong, 1, &mut dst).expect("encode");
    dst[FRAME_HEADER_LEN] = 0x02;
    dst[FRAME_HEADER_LEN + 1] = 0x00;
    let result = decode_response(&dst[FRAME_HEADER_LEN..]);
    assert_eq!(result, Err(Error::UnsupportedVersion(2)));
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
    };

    let mut frame = [0u8; 512];
    let n = buf.encode_frame(11, &mut frame).expect("encode_frame");
    let (id, response) = decode_response(&frame[FRAME_HEADER_LEN..n]).expect("decode_response");
    assert_eq!(id, 11);
    assert_eq!(response, Response::Output(expected));
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
