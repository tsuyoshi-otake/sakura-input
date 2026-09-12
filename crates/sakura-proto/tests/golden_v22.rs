//! Golden frames for protocol v22. Keep expected bytes independent of codec helpers.

use sakura_proto::{
    decode_request, decode_response, encode_request, encode_response, AppearanceTheme, InputScope,
    KeyCode, KeyInput, Mode, Modifiers, Output, PadShortcut, Request, Response, UiState,
    FRAME_HEADER_LEN, PROTOCOL_VERSION,
};

const REQUEST_ID: u64 = 0x0102_0304_0506_0708;
const SESSION_ID: u64 = 0x1112_1314_1516_1718;

fn assert_request_golden(name: &str, request: &Request, expected: &[u8]) {
    let mut actual = Vec::new();
    encode_request(request, REQUEST_ID, &mut actual).expect("encode request");
    assert_eq!(actual, expected, "{name} encoded bytes changed");

    let (id, decoded) = decode_request(&expected[FRAME_HEADER_LEN..]).expect("decode fixture");
    assert_eq!(id, REQUEST_ID, "{name} request id");
    assert_eq!(&decoded, request, "{name} fixture value");
}

fn assert_response_golden(name: &str, response: &Response, expected: &[u8]) {
    let mut actual = Vec::new();
    encode_response(response, REQUEST_ID, &mut actual).expect("encode response");
    assert_eq!(actual, expected, "{name} encoded bytes changed");

    let (id, decoded) = decode_response(&expected[FRAME_HEADER_LEN..]).expect("decode fixture");
    assert_eq!(id, REQUEST_ID, "{name} request id");
    assert_eq!(&decoded, response, "{name} fixture value");
}

#[test]
fn hello_request_and_response_match_v22_golden_frames() {
    assert_eq!(PROTOCOL_VERSION, 22);
    assert_request_golden(
        "hello request",
        &Request::Hello { client_version: 22 },
        &[
            0x0e, 0x00, 0x00, 0x00, 0x16, 0x00, 0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01,
            0x01, 0x00, 0x16, 0x00,
        ],
    );
    assert_response_golden(
        "hello response",
        &Response::Hello {
            server_version: 22,
            engine_version: [1, 2, 3],
        },
        &[
            0x14, 0x00, 0x00, 0x00, 0x16, 0x00, 0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01,
            0x01, 0x80, 0x16, 0x00, 0x01, 0x00, 0x02, 0x00, 0x03, 0x00,
        ],
    );
}

#[test]
fn representative_output_matches_v22_golden_frame() {
    assert_response_golden(
        "output response",
        &Response::Output(Output {
            consumed: true,
            beep: false,
            mode: Some(Mode::Hiragana),
            preedit: None,
            commit: Some("確定".to_owned()),
            delete_before: "x".to_owned(),
            candidates: None,
            candidate_detail: None,
        }),
        &[
            0x1f, 0x00, 0x00, 0x00, 0x16, 0x00, 0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01,
            0x03, 0x80, 0x01, 0x00, 0x01, 0x01, 0x00, 0x01, 0x06, 0x00, 0xe7, 0xa2, 0xba, 0xe5,
            0xae, 0x9a, 0x01, 0x00, 0x78, 0x00, 0x00,
        ],
    );
}

#[test]
fn representative_key_inputs_match_v22_golden_frames() {
    let cases = [
        (
            "ascii char",
            KeyInput {
                code: KeyCode::Char,
                ch: Some('a'),
                modifiers: Modifiers::NONE,
                repeat: false,
                test_only: false,
            },
            &[
                0x1e, 0x00, 0x00, 0x00, 0x16, 0x00, 0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01,
                0x03, 0x00, 0x18, 0x17, 0x16, 0x15, 0x14, 0x13, 0x12, 0x11, 0x01, 0x00, 0x01, 0x61,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            ][..],
        ),
        (
            "non-character repeat",
            KeyInput {
                code: KeyCode::Muhenkan,
                ch: None,
                modifiers: Modifiers(Modifiers::SHIFT.0 | Modifiers::CTRL.0),
                repeat: true,
                test_only: false,
            },
            &[
                0x1a, 0x00, 0x00, 0x00, 0x16, 0x00, 0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01,
                0x03, 0x00, 0x18, 0x17, 0x16, 0x15, 0x14, 0x13, 0x12, 0x11, 0x11, 0x00, 0x00, 0x03,
                0x01, 0x00,
            ][..],
        ),
        (
            "non-bmp test key",
            KeyInput {
                code: KeyCode::F12,
                ch: Some('\u{1f980}'),
                modifiers: Modifiers(0x1f),
                repeat: true,
                test_only: true,
            },
            &[
                0x1e, 0x00, 0x00, 0x00, 0x16, 0x00, 0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01,
                0x03, 0x00, 0x18, 0x17, 0x16, 0x15, 0x14, 0x13, 0x12, 0x11, 0x2b, 0x00, 0x01, 0x80,
                0xf9, 0x01, 0x00, 0x1f, 0x01, 0x01,
            ][..],
        ),
    ];

    for (name, key, expected) in cases {
        assert_request_golden(
            name,
            &Request::SendKey {
                session: SESSION_ID,
                key,
            },
            expected,
        );
    }
}

#[test]
fn every_mode_matches_its_v22_golden_frame() {
    let cases = [
        (Mode::Direct, 0x00),
        (Mode::Hiragana, 0x01),
        (Mode::Katakana, 0x02),
        (Mode::HalfKatakana, 0x03),
        (Mode::FullAlnum, 0x04),
        (Mode::HalfAlnum, 0x05),
    ];
    assert_eq!(Mode::ALL, cases.map(|(mode, _)| mode));

    for (mode, wire_value) in cases {
        let mut expected = vec![
            0x15, 0x00, 0x00, 0x00, 0x16, 0x00, 0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01,
            0x13, 0x00, 0x18, 0x17, 0x16, 0x15, 0x14, 0x13, 0x12, 0x11,
        ];
        expected.push(wire_value);
        assert_request_golden(
            "set mode",
            &Request::SetMode {
                session: SESSION_ID,
                mode,
            },
            &expected,
        );
    }
}

#[test]
fn every_input_scope_matches_its_v22_golden_frame() {
    let cases = [
        (InputScope::Normal, 0x00),
        (InputScope::Password, 0x01),
        (InputScope::Url, 0x02),
        (InputScope::Email, 0x03),
        (InputScope::Digits, 0x04),
        (InputScope::Unclassified, 0x05),
    ];
    assert_eq!(InputScope::ALL, cases.map(|(scope, _)| scope));

    for (scope, wire_value) in cases {
        let mut expected = vec![
            0x15, 0x00, 0x00, 0x00, 0x16, 0x00, 0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01,
            0x06, 0x00, 0x18, 0x17, 0x16, 0x15, 0x14, 0x13, 0x12, 0x11,
        ];
        expected.push(wire_value);
        assert_request_golden(
            "set input scope",
            &Request::SetInputScope {
                session: SESSION_ID,
                scope,
            },
            &expected,
        );
    }
}

#[test]
fn representative_appearance_and_pad_values_match_v22_golden_frames() {
    let cases = [
        (
            AppearanceTheme::Light,
            PadShortcut::Disabled,
            &[
                0x1d, 0x00, 0x00, 0x00, 0x16, 0x00, 0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01,
                0x06, 0x80, 0x18, 0x17, 0x16, 0x15, 0x14, 0x13, 0x12, 0x11, 0x01, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x01, 0x00,
            ][..],
        ),
        (
            AppearanceTheme::Dark,
            PadShortcut::DoubleCtrl,
            &[
                0x1d, 0x00, 0x00, 0x00, 0x16, 0x00, 0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01,
                0x06, 0x80, 0x18, 0x17, 0x16, 0x15, 0x14, 0x13, 0x12, 0x11, 0x02, 0x01, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x01, 0x00,
            ][..],
        ),
    ];

    for (appearance_theme, pad_shortcut, expected) in cases {
        let response = Response::Ui(UiState {
            revision: SESSION_ID,
            appearance_theme,
            pad_shortcut,
            mode: None,
            candidates: None,
            candidate_detail: None,
            anchor: None,
            document: None,
            renderer_visible: true,
            stopping: false,
        });
        assert_response_golden("renderer preferences", &response, expected);
    }
}
