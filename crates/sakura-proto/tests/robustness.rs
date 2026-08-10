//! Hand-rolled, deterministic fuzzing for the decode paths.
//!
//! No external crates: a fixed-seed xorshift64* PRNG stands in for a fuzz
//! harness. The only property under test is "never panics" — a malformed
//! frame must decode to `Err`, never crash the engine that trusts this
//! codec on a hostile pipe (DESIGN.md §7). Iteration count is controlled
//! by the `SAKURA_FUZZ_ITERS` env var (default 200_000, chosen to stay
//! well under ~10s; a longer campaign is a Phase 5 concern per the spec).

use sakura_proto::{
    decode_request, decode_response, encode_request, encode_response, payload_len, peek_header,
    Error, InputScope, KeyCode, KeyInput, Mode, Modifiers, Request, Response, ScreenRect,
    UndoCommitOutcome, FRAME_HEADER_LEN, MAX_COMMIT_BYTES, MAX_PAYLOAD,
};

/// A minimal xorshift64* PRNG. Deterministic given a seed, so a failing
/// run is always reproducible from the seed printed on panic (were one to
/// ever happen -- the whole point of this test is that it can't).
struct Xorshift64Star {
    state: u64,
}

impl Xorshift64Star {
    fn new(seed: u64) -> Self {
        // xorshift64* requires a non-zero state.
        Xorshift64Star {
            state: if seed == 0 {
                0xdead_beef_cafe_babe
            } else {
                seed
            },
        }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }

    /// Returns a value in `0..bound` (`bound` must be > 0).
    fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            return 0;
        }
        (self.next_u64() % bound as u64) as usize
    }

    fn fill_bytes(&mut self, dst: &mut [u8]) {
        let mut i = 0;
        while i < dst.len() {
            let word = self.next_u64().to_le_bytes();
            let n = (dst.len() - i).min(8);
            dst[i..i + n].copy_from_slice(&word[..n]);
            i += n;
        }
    }
}

fn iters() -> usize {
    std::env::var("SAKURA_FUZZ_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(200_000)
}

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok()?.parse().ok()
}

fn campaign_seed(domain: u64) -> u64 {
    let shard = env_u64("SAKURA_FUZZ_SHARD").unwrap_or(0);
    let seed = env_u64("SAKURA_FUZZ_SEED").unwrap_or(0);
    domain ^ shard.rotate_left(17) ^ seed.rotate_left(31)
}

/// Random byte strings of random lengths must never make any decoder
/// panic; they may of course fail to decode.
#[test]
fn random_bytes_never_panic_any_decoder() {
    let mut rng = Xorshift64Star::new(0x5A6B_0001);
    let n = iters();
    for _ in 0..n {
        let len = rng.below(300);
        let mut buf = vec![0u8; len];
        rng.fill_bytes(&mut buf);

        // None of these may panic, regardless of what `buf` contains.
        let _ = decode_request(&buf);
        let _ = decode_response(&buf);
        let _ = peek_header(&buf);
        if buf.len() >= 4 {
            let header = [buf[0], buf[1], buf[2], buf[3]];
            let _ = payload_len(&header);
        }
    }
}

/// Builds one valid encoded `Request` frame per call, cycling through a
/// handful of shapes so the mutation tests below exercise strings,
/// options, and sequences, not just fixed-size fields.
fn sample_valid_request_frames(rng: &mut Xorshift64Star) -> Vec<Vec<u8>> {
    let key = KeyInput {
        code: KeyCode::Char,
        ch: Some('a'),
        modifiers: Modifiers::SHIFT,
        repeat: true,
        test_only: false,
    };
    let requests = [
        Request::Hello { client_version: 1 },
        Request::CreateSession {
            process_name: "notepad.exe".to_string(),
        },
        Request::SendKey { session: 42, key },
        Request::ProbeKey {
            session: 42,
            scope: InputScope::Normal,
            fresh_context: false,
            key,
        },
        Request::ProbeKey {
            session: 42,
            scope: InputScope::Password,
            fresh_context: true,
            key,
        },
        Request::Commit { session: 42 },
        Request::Revert { session: 42 },
        Request::UndoCommit {
            session: 42,
            outcome: UndoCommitOutcome::Unknown,
        },
        Request::Reconvert {
            session: 42,
            text: "仮名".to_owned(),
            preview: false,
        },
        Request::ClearLearning,
        Request::ClearInputHistory,
        Request::FlushInputHistory,
        Request::InputHistoryStats,
        Request::SetInputScope {
            session: 42,
            scope: InputScope::Password,
        },
        Request::SetMode {
            session: 42,
            mode: Mode::HalfAlnum,
        },
        Request::DeleteSession { session: 42 },
        Request::Ping,
        Request::Shutdown,
        Request::SetUiPlacement {
            session: 42,
            anchor: Some(ScreenRect {
                left: -50,
                top: 10,
                right: 25,
                bottom: 30,
            }),
            renderer_visible: true,
        },
    ];
    requests
        .iter()
        .map(|req| {
            let mut dst = Vec::new();
            let id = rng.next_u64();
            encode_request(req, id, &mut dst).expect("encode a valid sample request");
            dst
        })
        .collect()
}

#[test]
fn probe_key_fresh_context_bool_rejects_nonzero_nonone_wire_values() {
    let request = Request::ProbeKey {
        session: 42,
        scope: InputScope::Normal,
        fresh_context: false,
        key: KeyInput {
            code: KeyCode::Char,
            ch: Some('a'),
            modifiers: Modifiers::NONE,
            repeat: false,
            test_only: true,
        },
    };
    let mut frame = Vec::new();
    encode_request(&request, 7, &mut frame).expect("encode ProbeKey request");

    // Frame header + version + request id + message type + session + scope.
    const FRESH_CONTEXT_OFFSET: usize = FRAME_HEADER_LEN + 2 + 8 + 2 + 8 + 1;
    assert_eq!(frame[FRESH_CONTEXT_OFFSET], 0);
    for invalid in [2u8, u8::MAX] {
        let mut mutated = frame.clone();
        mutated[FRESH_CONTEXT_OFFSET] = invalid;
        assert_eq!(
            decode_request(&mutated[FRAME_HEADER_LEN..]),
            Err(Error::BadBool),
            "fresh_context wire byte {invalid} must be rejected"
        );
    }
}

#[test]
fn undo_commit_outcomes_are_strict_and_exact_text_is_bounded() {
    let request = Request::UndoCommit {
        session: 42,
        outcome: UndoCommitOutcome::Unknown,
    };
    let mut frame = Vec::new();
    encode_request(&request, 7, &mut frame).expect("encode undo request");

    // The outcome follows version (2), request id (8), message type (2), and
    // session (8) in the payload. Unknown values must not be accepted as a
    // terminal engine state.
    let outcome_offset = FRAME_HEADER_LEN + 2 + 8 + 2 + 8;
    frame[outcome_offset..outcome_offset + 2].copy_from_slice(&0u16.to_le_bytes());
    assert_eq!(
        decode_request(&frame[FRAME_HEADER_LEN..]),
        Err(Error::BadEnum)
    );

    let too_long = Response::Output(sakura_proto::Output {
        consumed: true,
        beep: false,
        mode: None,
        preedit: None,
        commit: None,
        delete_before: "a".repeat(MAX_COMMIT_BYTES + 1),
        candidates: None,
        candidate_detail: None,
    });
    let mut output_frame = Vec::new();
    assert_eq!(
        encode_response(&too_long, 8, &mut output_frame),
        Err(Error::TooLarge)
    );

    // `Output::decode` must reject the exact-delete field before allocating
    // an owned string. Make a structurally complete frame whose borrowed
    // field is over the commit cap, so the expected result is TooLarge rather
    // than Truncated.
    let valid = Response::Output(sakura_proto::Output {
        consumed: true,
        beep: false,
        mode: None,
        preedit: None,
        commit: None,
        delete_before: String::new(),
        candidates: None,
        candidate_detail: None,
    });
    let mut oversized_decode_frame = Vec::new();
    encode_response(&valid, 9, &mut oversized_decode_frame).expect("encode valid output");
    let candidate_tag = oversized_decode_frame.pop().expect("candidate tag");
    oversized_decode_frame.extend(std::iter::repeat_n(b'a', MAX_COMMIT_BYTES + 1));
    oversized_decode_frame.push(candidate_tag);
    let payload_len = u32::try_from(oversized_decode_frame.len() - FRAME_HEADER_LEN)
        .expect("payload length fits");
    oversized_decode_frame[..FRAME_HEADER_LEN].copy_from_slice(&payload_len.to_le_bytes());
    let delete_len_offset = FRAME_HEADER_LEN + 2 + 8 + 2 + 1 + 1 + 1 + 1 + 1;
    oversized_decode_frame[delete_len_offset..delete_len_offset + 2]
        .copy_from_slice(&u16::try_from(MAX_COMMIT_BYTES + 1).unwrap().to_le_bytes());
    assert_eq!(
        decode_response(&oversized_decode_frame[FRAME_HEADER_LEN..]),
        Err(Error::TooLarge)
    );
}

fn sample_valid_response_frames(rng: &mut Xorshift64Star) -> Vec<Vec<u8>> {
    use sakura_proto::types::CandidatePresentation;
    use sakura_proto::{
        Candidate, CandidateDetail, CandidateKind, CandidateList, ErrorCode, Mode, Output, Preedit,
        Response, Segment, UnderlineKind,
    };
    let responses = [
        Response::Hello {
            server_version: 1,
            engine_version: [0, 1, 0],
        },
        Response::SessionCreated {
            session: 42,
            mode: Mode::Hiragana,
        },
        Response::InputMode {
            mode: Mode::HalfAlnum,
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
        Response::Output(Output {
            consumed: true,
            beep: false,
            mode: None,
            preedit: None,
            commit: None,
            delete_before: String::new(),
            candidates: Some(CandidateList {
                kind: CandidateKind::Conversion,
                presentation: CandidatePresentation::Compact,
                items: vec![Candidate {
                    text: "Rust".to_owned(),
                    annotation: "language".to_owned(),
                }],
                selected: 0,
                page_size: 9,
            }),
            candidate_detail: Some(CandidateDetail {
                reading: "らすと".to_owned(),
                definition: "安全性と速度を重視するプログラミング言語。".to_owned(),
                definition_truncated: false,
                aliases: vec!["Rust language".to_owned()],
                related: vec!["Cargo".to_owned()],
                similar: vec!["C++".to_owned()],
                antonyms: vec!["unsafe".to_owned()],
            }),
        }),
        Response::Pong,
        Response::Ok,
        Response::Error(ErrorCode::Malformed),
    ];
    responses
        .iter()
        .map(|res| {
            let mut dst = Vec::new();
            let id = rng.next_u64();
            encode_response(res, id, &mut dst).expect("encode a valid sample response");
            dst
        })
        .collect()
}

/// Single-bit flips, truncation at every prefix length, and length-field
/// corruption applied to otherwise-valid frames: none of it may panic
/// either decoder, whether the mutated bytes are fed as a `Request` or a
/// `Response` payload (a fuzzer does not know which direction bytes were
/// meant for; the same guarantee must hold both ways).
#[test]
fn structured_mutation_of_valid_frames_never_panics() {
    let mut rng = Xorshift64Star::new(0x5A6B_0002);
    let mut frames = sample_valid_request_frames(&mut rng);
    frames.extend(sample_valid_response_frames(&mut rng));

    let n = iters();
    let mut done = 0usize;
    'outer: for base in &frames {
        if base.len() < FRAME_HEADER_LEN {
            continue;
        }
        let payload = &base[FRAME_HEADER_LEN..];

        // Truncation at every prefix length, including zero and the full
        // length.
        for cut in 0..=payload.len() {
            let truncated = &payload[..cut];
            let _ = decode_request(truncated);
            let _ = decode_response(truncated);
            done += 1;
            if done >= n {
                break 'outer;
            }
        }

        // Single-bit flips at every bit position.
        for byte_idx in 0..payload.len() {
            for bit in 0..8u8 {
                let mut mutated = payload.to_vec();
                mutated[byte_idx] ^= 1 << bit;
                let _ = decode_request(&mutated);
                let _ = decode_response(&mutated);
                done += 1;
                if done >= n {
                    break 'outer;
                }
            }
        }

        // Length-field corruption: patch the 4-byte frame length prefix
        // with random garbage and re-check `payload_len`, then decode the
        // (unmodified) payload directly too.
        for _ in 0..64 {
            let mut header = [0u8; 4];
            rng.fill_bytes(&mut header);
            let _ = payload_len(&header);
            done += 1;
            if done >= n {
                break 'outer;
            }
        }

        // Random-byte mutation at random positions, several rounds.
        for _ in 0..64 {
            if payload.is_empty() {
                break;
            }
            let mut mutated = payload.to_vec();
            let idx = rng.below(mutated.len());
            mutated[idx] = rng.next_u32() as u8;
            let _ = decode_request(&mutated);
            let _ = decode_response(&mutated);
            done += 1;
            if done >= n {
                break 'outer;
            }
        }
    }
}

/// `payload_len` must reject any declared length above `MAX_PAYLOAD`,
/// and accept anything at or below it.
#[test]
fn payload_len_rejects_declared_lengths_above_max_payload() {
    let mut rng = Xorshift64Star::new(0x5A6B_0003);
    let n = iters().min(50_000);
    for _ in 0..n {
        // Bias half the samples to be near the boundary, half fully random,
        // so both the edge and the general case get exercised.
        let len: u32 = if rng.below(2) == 0 {
            let offset = (rng.below(8) as i64) - 4; // -4..=3
            (MAX_PAYLOAD as i64 + offset).max(0) as u32
        } else {
            rng.next_u32()
        };
        let header = len.to_le_bytes();
        let result = payload_len(&header);
        if len as usize > MAX_PAYLOAD {
            assert_eq!(result, Err(sakura_proto::Error::TooLarge));
        } else {
            assert_eq!(result, Ok(len as usize));
        }
    }
}

/// The Phase 5 soak entry point. Unlike the short regression tests above, this
/// target is ignored by default and accepts both a shard and a per-slice seed.
/// A resumable runner changes the seed after every successful slice, so
/// restarting a campaign advances coverage instead of replaying the same bytes.
#[test]
#[ignore = "long deterministic campaign; set SAKURA_FUZZ_ITERS, SAKURA_FUZZ_SHARD, and SAKURA_FUZZ_SEED"]
fn sharded_protocol_campaign() {
    let iterations = iters();
    let shard = env_u64("SAKURA_FUZZ_SHARD").unwrap_or(0);
    let slice_seed = env_u64("SAKURA_FUZZ_SEED").unwrap_or(0);
    let mut random = Xorshift64Star::new(campaign_seed(0x5A6B_72F0_0000_0001));

    for iteration in 0..iterations {
        let seed = random.state;
        let len = match iteration % 8 {
            0 => random.below(4),
            1 => FRAME_HEADER_LEN + random.below(64),
            2 => 255 + random.below(4),
            3 => 4_096 + random.below(64),
            _ => random.below(1_024),
        };
        let mut bytes = vec![0u8; len];
        random.fill_bytes(&mut bytes);

        let outcome = std::panic::catch_unwind(|| {
            let _ = decode_request(&bytes);
            let _ = decode_response(&bytes);
            let _ = peek_header(&bytes);
            if bytes.len() >= FRAME_HEADER_LEN {
                let header = [bytes[0], bytes[1], bytes[2], bytes[3]];
                let _ = payload_len(&header);
            }
        });
        assert!(
            outcome.is_ok(),
            "protocol decoder panicked at shard {shard}, slice seed {slice_seed}, iteration {iteration}, PRNG seed {seed:#018x}, bytes {}",
            bytes.len()
        );
    }
}
