use super::*;
use sakura_proto::FixedStr;
use std::sync::atomic::{AtomicUsize, Ordering};

static OBSERVED_SCANS: AtomicUsize = AtomicUsize::new(0);

unsafe fn observed_scan(bytes: &[u8], lut: &simd::Lut) -> usize {
    OBSERVED_SCANS.fetch_add(1, Ordering::Relaxed);
    simd::passthrough_len(bytes, lut)
}

fn assert_observed_scan_count(
    normalizer: Normalizer,
    mode: Mode,
    src: &str,
    expected_scans: usize,
) {
    assert!(src.len() >= simd::MIN_VECTOR_BYTES);
    OBSERVED_SCANS.store(0, Ordering::Relaxed);
    let mut actual = String::new();
    // SAFETY: `observed_scan` has no target-feature requirement and
    // delegates to the production safe dispatcher.
    unsafe {
        normalizer
            .normalize_into_with_scan(src, mode, &mut actual, observed_scan)
            .expect("a String never overflows");
    }
    assert_eq!(actual, scalar_reference(&normalizer, src, mode));
    assert_eq!(
        OBSERVED_SCANS.load(Ordering::Relaxed),
        expected_scans,
        "{normalizer:?} in {mode:?} chose the wrong path for {src:?}"
    );
}

/// Exact pre-I2 dispatch retained only for the ignored A/B benchmark.
/// Both sides therefore share the production scanner, policy code, sink,
/// compilation unit, and optimizer visibility.
#[cfg(target_arch = "x86_64")]
fn pre_i2_normalize_into(
    normalizer: &Normalizer,
    src: &str,
    mode: Mode,
    dst: &mut impl TextSink,
) -> Result<(), Overflow> {
    if matches!(mode, Mode::Katakana | Mode::HalfKatakana) {
        return normalizer.normalize_kana(src, mode, dst);
    }
    if src.len() < simd::MIN_VECTOR_BYTES {
        for character in src.chars() {
            dst.push(normalizer.normalize_char(character, mode))?;
        }
        return Ok(());
    }
    let resolved = normalizer.resolved_widths(mode);
    normalizer.normalize_runs(src, mode, resolved, dst)
}

#[cfg(target_arch = "x86_64")]
fn width_cycles_each(mut body: impl FnMut()) -> f64 {
    use std::arch::x86_64::{_mm_lfence, _rdtsc};

    const ROUNDS: usize = 50_000;
    const REPEATS: usize = 7;
    for _ in 0..ROUNDS / 10 {
        body();
    }
    let mut best = u64::MAX;
    for _ in 0..REPEATS {
        // SAFETY: LFENCE serializes execution around RDTSC on x86-64;
        // neither intrinsic reads or writes memory.
        let start = unsafe {
            _mm_lfence();
            _rdtsc()
        };
        for _ in 0..ROUNDS {
            body();
        }
        // SAFETY: same serialized TSC read as above, after the measured
        // body has completed.
        let end = unsafe {
            _mm_lfence();
            _rdtsc()
        };
        best = best.min(end.wrapping_sub(start));
    }
    best as f64 / ROUNDS as f64
}

#[cfg(target_arch = "x86_64")]
fn measure_pre_i2_cycles(normalizer: &Normalizer, src: &str, mode: Mode) -> f64 {
    width_cycles_each(|| {
        let mut dst = FixedStr::<1024>::new();
        pre_i2_normalize_into(
            std::hint::black_box(normalizer),
            std::hint::black_box(src),
            std::hint::black_box(mode),
            &mut dst,
        )
        .expect("the benchmark sink is sized");
        std::hint::black_box(dst.len());
    })
}

#[cfg(target_arch = "x86_64")]
fn measure_i2_cycles(normalizer: &Normalizer, src: &str, mode: Mode) -> f64 {
    width_cycles_each(|| {
        let mut dst = FixedStr::<1024>::new();
        normalizer
            .normalize_into(
                std::hint::black_box(src),
                std::hint::black_box(mode),
                &mut dst,
            )
            .expect("the benchmark sink is sized");
        std::hint::black_box(normalizer);
        std::hint::black_box(dst.len());
    })
}

#[cfg(target_arch = "x86_64")]
#[test]
#[ignore = "timing, not a threshold: run with --release --ignored --nocapture and read it"]
fn issue_110_pre_i2_vs_bypass_in_cpu_cycles() {
    let default = Normalizer::default();
    let all_full = Normalizer {
        width: WidthPolicy {
            alnum: Width::Full,
            number: Width::Full,
            symbol: Width::Full,
        },
        ..Normalizer::default()
    };
    let follow_mode = Normalizer {
        width: WidthPolicy {
            alnum: Width::FollowMode,
            number: Width::FollowMode,
            symbol: Width::FollowMode,
        },
        ..Normalizer::default()
    };
    let cases = [
        ("default one-key", &default, Mode::Hiragana, "k"),
        (
            "default ASCII",
            &default,
            Mode::Hiragana,
            "docker compose up -d --build --remove-orphans",
        ),
        (
            "default Japanese",
            &default,
            Mode::Hiragana,
            "この変換候補は直前に確定した内容を考慮して並べ替えられます。",
        ),
        (
            "default mixed",
            &default,
            Mode::Hiragana,
            "Docker のビルドキャッシュは ~/.cache/docker に置く、CI でも同じ。",
        ),
        (
            "all-full ASCII",
            &all_full,
            Mode::Hiragana,
            "docker compose up -d --build --remove-orphans",
        ),
        (
            "all-full identifier",
            &all_full,
            Mode::Hiragana,
            "issue_110_width_normalizer_fast_path_identifier_2026",
        ),
        (
            "follow/full-alnum",
            &follow_mode,
            Mode::FullAlnum,
            "issue_110_width_normalizer_fast_path_identifier_2026",
        ),
        (
            "space/control-heavy",
            &all_full,
            Mode::Hiragana,
            concat!(
                " \t\n\r\0  \u{1}\u{7f} \t\n\r\0  \u{1}\u{7f} ",
                " \t\n\r\0  \u{1}\u{7f} \t\n\r\0  \u{1}\u{7f} ",
                " \t\n\r\0  \u{1}\u{7f} \t\n\r\0  \u{1}\u{7f} ",
            ),
        ),
    ];

    println!(
        "\n{:<22} {:>5} {:>14} {:>14} {:>10}",
        "case", "bytes", "old cycles", "new cycles", "delta"
    );
    for (name, normalizer, mode, src) in cases {
        let old_first = measure_pre_i2_cycles(normalizer, src, mode);
        let new_first = measure_i2_cycles(normalizer, src, mode);
        let new_cycles = new_first.min(measure_i2_cycles(normalizer, src, mode));
        let old_cycles = old_first.min(measure_pre_i2_cycles(normalizer, src, mode));
        let delta = (new_cycles / old_cycles - 1.0) * 100.0;
        println!(
            "{name:<22} {:>5} {old_cycles:>14.2} {new_cycles:>14.2} {delta:>9.2}%",
            src.len()
        );
    }
}

#[test]
fn width_offset_is_pinned_to_literal_code_points() {
    let full = Normalizer {
        width: WidthPolicy {
            alnum: Width::Full,
            number: Width::Full,
            symbol: Width::Full,
        },
        punctuation: PunctuationStyle::default(),
        brackets: BracketStyle::default(),
    };
    assert_eq!(full.normalize_char('a', Mode::Direct), 'ａ');
    assert_eq!(full.normalize_char('0', Mode::Direct), '０');
    assert_eq!(full.normalize_char('@', Mode::Direct), '＠');
}

#[test]
fn kana_modes_render_their_declared_script() {
    let normalizer = Normalizer::default();
    let source = "\u{304b}\u{304c}"; // かが
    let mut full = FixedStr::<32>::new();
    normalizer
        .normalize_into(source, Mode::Katakana, &mut full)
        .expect("full-width kana fits");
    assert_eq!(full.as_str(), "\u{30ab}\u{30ac}"); // カガ

    let mut half = FixedStr::<32>::new();
    normalizer
        .normalize_into(source, Mode::HalfKatakana, &mut half)
        .expect("half-width kana fits");
    assert_eq!(half.as_str(), "\u{ff76}\u{ff76}\u{ff9e}"); // ｶｶﾞ
}

#[test]
fn ascii_printable_half_to_full_to_half_is_identity_except_owned_punctuation_and_brackets() {
    let full = Normalizer {
        width: WidthPolicy {
            alnum: Width::Full,
            number: Width::Full,
            symbol: Width::Full,
        },
        punctuation: PunctuationStyle::default(),
        brackets: BracketStyle::default(),
    };
    let half = Normalizer::default();
    for cp in 0x21u32..=0x7E {
        let c = char::from_u32(cp).expect("valid ASCII printable code point");
        // ',' and '.' are excluded on purpose: their full-width forms,
        // ，(U+FF0C) and ．(U+FF0E), are two of the four code points the
        // punctuation choke point permanently owns (rule 4). Round-
        // tripping them through the *symbol* channel is impossible by
        // design — see `comma_and_period_full_width_forms_are_owned_by_punctuation`
        // below for the actual, documented, correct behavior.
        if matches!(c, ',' | '.' | '[' | ']') {
            continue;
        }
        let widened = full.normalize_char(c, Mode::Direct);
        assert_eq!(widened as u32, cp + 0xFEE0, "half->full offset for {c:?}");
        let narrowed = half.normalize_char(widened, Mode::Direct);
        assert_eq!(narrowed, c, "full->half round trip for {c:?}");
    }
}

#[test]
fn comma_and_period_full_width_forms_are_owned_by_punctuation() {
    // Forward: ASCII ','/'.' really are governed by `symbol` per rule
    // 4, so widening them lands on the same code points, ，/．, that
    // the punctuation choke point also emits under `CommaPeriod`.
    let full_symbol = Normalizer {
        width: WidthPolicy {
            alnum: Width::Half,
            number: Width::Half,
            symbol: Width::Full,
        },
        punctuation: PunctuationStyle::default(),
        brackets: BracketStyle::default(),
    };
    assert_eq!(full_symbol.normalize_char(',', Mode::Direct), '\u{FF0C}');
    assert_eq!(full_symbol.normalize_char('.', Mode::Direct), '\u{FF0E}');

    // Backward: feeding those same full-width forms back through a
    // Half-symbol normalizer does NOT recover ','/'.' — `punct_role`
    // claims them unconditionally and routes them through the
    // punctuation style instead (KutenTouten by default here). This is
    // exactly why the general round-trip test above excludes these two
    // characters: it is not a gap in the implementation.
    let half_symbol = Normalizer::default();
    assert_eq!(
        half_symbol.normalize_char('\u{FF0C}', Mode::Direct),
        '\u{3001}'
    );
    assert_eq!(
        half_symbol.normalize_char('\u{FF0E}', Mode::Direct),
        '\u{3002}'
    );
}

#[test]
fn ascii_space_is_not_widened_by_the_symbol_channel() {
    let full = Normalizer {
        width: WidthPolicy {
            alnum: Width::Half,
            number: Width::Half,
            symbol: Width::Full,
        },
        punctuation: PunctuationStyle::default(),
        brackets: BracketStyle::default(),
    };
    let half = Normalizer::default();
    assert_eq!(full.normalize_char(' ', Mode::Direct), ' ');
    assert_eq!(half.normalize_char('\u{3000}', Mode::Direct), ' ');
}

#[test]
fn default_policy_never_widens_docker_in_any_mode() {
    let normalizer = Normalizer::default();
    for mode in Mode::ALL {
        let mut out = String::new();
        normalizer
            .normalize_into("docker", mode, &mut out)
            .expect("fits in a growable String");
        assert_eq!(out, "docker", "mode {mode:?} widened docker");
    }
}

#[test]
fn alnum_and_number_channels_are_independent() {
    let normalizer = Normalizer {
        width: WidthPolicy {
            alnum: Width::Full,
            number: Width::Half,
            symbol: Width::Half,
        },
        punctuation: PunctuationStyle::default(),
        brackets: BracketStyle::default(),
    };
    let mut out = String::new();
    normalizer
        .normalize_into("abc123", Mode::Direct, &mut out)
        .expect("fits");
    assert_eq!(out, "ａｂｃ123");
}

#[test]
fn follow_mode_widens_only_in_full_alnum_mode() {
    let normalizer = Normalizer {
        width: WidthPolicy {
            alnum: Width::FollowMode,
            number: Width::FollowMode,
            symbol: Width::FollowMode,
        },
        punctuation: PunctuationStyle::default(),
        brackets: BracketStyle::default(),
    };
    assert_eq!(normalizer.normalize_char('a', Mode::FullAlnum), 'ａ');
    for mode in Mode::ALL {
        if mode == Mode::FullAlnum {
            continue;
        }
        assert_eq!(
            normalizer.normalize_char('a', mode),
            'a',
            "mode {mode:?} should stay half-width"
        );
    }
}

#[test]
fn punctuation_style_normalizes_from_any_of_the_four_source_forms() {
    // Every style must produce its configured pair no matter which of
    // the four source code points (、 。 ， ．) the caller hands in —
    // covering the "backward" direction too, e.g. ，-> 、 under
    // KUTEN_TOUTEN, not just the forward 、-> 、 identity. Driven off
    // `ALL` rather than a hand-written table so a tenth combination
    // cannot be added without being covered here.
    let sources = ['\u{3001}', '\u{3002}', '\u{FF0C}', '\u{FF0E}'];
    for style in PunctuationStyle::ALL {
        let expected = [
            style.comma.glyph(),
            style.period.glyph(),
            style.comma.glyph(),
            style.period.glyph(),
        ];
        let normalizer = Normalizer {
            width: WidthPolicy::default(),
            punctuation: style,
            brackets: BracketStyle::default(),
        };
        for (src, want) in sources.iter().zip(expected.iter()) {
            assert_eq!(
                normalizer.normalize_char(*src, Mode::Direct),
                *want,
                "style {style:?} src {src:?}"
            );
        }
    }
}

#[test]
fn half_width_punctuation_survives_every_symbol_width_and_mode() {
    // The whole point of the half-width marks is that they reach the
    // document as ASCII. `symbol = Full` widens every other ASCII
    // symbol, so if it ever got a say here it would widen `,` back to
    // ， and undo the setting — the same fight rule 4 already settles
    // for the full-width marks, now coming from the other side.
    for symbol in [Width::Half, Width::Full, Width::FollowMode] {
        let normalizer = Normalizer {
            width: WidthPolicy {
                alnum: Width::Half,
                number: Width::Half,
                symbol,
            },
            punctuation: PunctuationStyle::ASCII,
            brackets: BracketStyle::default(),
        };
        for mode in Mode::ALL {
            for src in ['\u{3001}', '\u{FF0C}'] {
                assert_eq!(
                    normalizer.normalize_char(src, mode),
                    ',',
                    "symbol {symbol:?} mode {mode:?} src {src:?}"
                );
            }
            for src in ['\u{3002}', '\u{FF0E}'] {
                assert_eq!(
                    normalizer.normalize_char(src, mode),
                    '.',
                    "symbol {symbol:?} mode {mode:?} src {src:?}"
                );
            }
        }
    }
}

#[test]
fn half_width_punctuation_is_emitted_but_never_reclaimed() {
    // Deliberately one-way. The punctuation channel writes ASCII `,`/`.`
    // when asked to, but `punct_role` still owns exactly four code
    // points, so ASCII `,`/`.` arriving as *input* stay ordinary symbols
    // governed by `width.symbol`. Were they claimed instead, a `.` typed
    // in direct input would come back as 。 under the default style, and
    // the `,` in `foo(a, b)` would stop being a comma.
    let ascii = Normalizer {
        width: WidthPolicy::default(),
        punctuation: PunctuationStyle::ASCII,
        brackets: BracketStyle::default(),
    };
    assert_eq!(ascii.normalize_char(',', Mode::Direct), ',');
    assert_eq!(ascii.normalize_char('.', Mode::Direct), '.');

    // Under the default style the same ASCII input is still untouched by
    // punctuation: `width.symbol` alone decides its width.
    let default_half = Normalizer::default();
    assert_eq!(default_half.normalize_char(',', Mode::Direct), ',');
    assert_eq!(default_half.normalize_char('.', Mode::Direct), '.');
    let default_full = Normalizer {
        width: WidthPolicy {
            alnum: Width::Half,
            number: Width::Half,
            symbol: Width::Full,
        },
        ..Normalizer::default()
    };
    assert_eq!(default_full.normalize_char(',', Mode::Direct), '\u{FF0C}');
    assert_eq!(default_full.normalize_char('.', Mode::Direct), '\u{FF0E}');
}

#[test]
fn half_width_punctuation_flows_through_normalize_into() {
    // `normalize_into` copies unchanged runs wholesale and only falls to
    // `normalize_char` for the rest. All four owned marks are
    // three-byte, so none of them can hide inside a single-byte
    // passthrough run — this pins that the run scanner really does hand
    // them over, and that a 3-byte -> 1-byte replacement lands intact
    // in the middle of ASCII that the scanner did copy wholesale.
    let normalizer = Normalizer {
        width: WidthPolicy::default(),
        punctuation: PunctuationStyle::ASCII,
        brackets: BracketStyle::default(),
    };
    let mut out = String::new();
    normalizer
        .normalize_into(
            "docker compose up、これで起動する。ログは journalctl で読む。",
            Mode::Hiragana,
            &mut out,
        )
        .expect("fits in a growable String");
    assert_eq!(
        out,
        "docker compose up,これで起動する.ログは journalctl で読む."
    );
}

#[test]
fn bracket_style_normalizes_all_supported_source_pairs() {
    let sources = ['[', ']', '［', '］', '「', '」', '『', '』'];
    for style in BracketStyle::ALL {
        let expected = match style {
            BracketStyle::Corner => ['「', '」', '「', '」', '「', '」', '「', '」'],
            BracketStyle::Square => ['［', '］', '［', '］', '［', '］', '［', '］'],
        };
        let normalizer = Normalizer {
            width: WidthPolicy::default(),
            punctuation: PunctuationStyle::default(),
            brackets: style,
        };
        for (source, want) in sources.into_iter().zip(expected) {
            assert_eq!(normalizer.normalize_char(source, Mode::Direct), want);
        }

        // Keep the SIMD fast path honest too: a long ASCII run must stop
        // at the bracket bytes so normalize_char owns them rather than
        // copying them through unchanged.
        let source = "[x]".repeat(16);
        let mut rendered = String::new();
        normalizer
            .normalize_into(&source, Mode::Direct, &mut rendered)
            .expect("growable output accepts the long bracket sample");
        let (open, close) = match style {
            BracketStyle::Corner => ('\u{300c}', '\u{300d}'),
            BracketStyle::Square => ('\u{ff3b}', '\u{ff3d}'),
        };
        assert!(rendered.contains(open));
        assert!(rendered.contains(close));
        assert!(!rendered.contains('['));
        assert!(!rendered.contains(']'));
    }
}

#[test]
fn punctuation_choke_point_ignores_the_symbol_width_policy() {
    // If `symbol: Half` were allowed to touch ，/．, this would shrink
    // them to ASCII ','/'.' and silently undo `punctuation:
    // CommaPeriod` (see the comment on `normalize_char`). It must not.
    let normalizer = Normalizer {
        width: WidthPolicy {
            alnum: Width::Half,
            number: Width::Half,
            symbol: Width::Half,
        },
        punctuation: PunctuationStyle::COMMA_PERIOD,
        brackets: BracketStyle::default(),
    };
    assert_eq!(
        normalizer.normalize_char('\u{3001}', Mode::Direct),
        '\u{FF0C}'
    );
    assert_eq!(
        normalizer.normalize_char('\u{FF0C}', Mode::Direct),
        '\u{FF0C}'
    );
    assert_eq!(
        normalizer.normalize_char('\u{FF0E}', Mode::Direct),
        '\u{FF0E}'
    );

    // And the converse: ASCII ',' really is governed by `symbol`, and
    // a half-width policy leaves it as ',' — it is not swept into the
    // punctuation choke point just because it looks similar.
    assert_eq!(normalizer.normalize_char(',', Mode::Direct), ',');
    assert_eq!(normalizer.normalize_char('.', Mode::Direct), '.');
}

#[test]
fn kana_kanji_and_non_bmp_characters_pass_through_untouched() {
    // A policy that widens everything it is allowed to touch, to prove
    // these characters are outside the width policy's reach entirely —
    // not just coincidentally unaffected by a Half default.
    let normalizer = Normalizer {
        width: WidthPolicy {
            alnum: Width::Full,
            number: Width::Full,
            symbol: Width::Full,
        },
        punctuation: PunctuationStyle::COMMA_PERIOD,
        brackets: BracketStyle::default(),
    };
    for c in ['あ', 'ア', '漢', '𠮷', '🍣'] {
        assert_eq!(normalizer.normalize_char(c, Mode::Direct), c);
    }
}

/// Every normalizer worth building, so the agreement tests below cover
/// all eight passthrough tables rather than the default one.
fn every_normalizer() -> Vec<Normalizer> {
    let widths = [Width::Half, Width::Full, Width::FollowMode];
    let styles = PunctuationStyle::ALL;
    let bracket_styles = BracketStyle::ALL;
    let mut all = Vec::new();
    for alnum in widths {
        for number in widths {
            for symbol in widths {
                for punctuation in styles {
                    for brackets in bracket_styles {
                        all.push(Normalizer {
                            width: WidthPolicy {
                                alnum,
                                number,
                                symbol,
                            },
                            punctuation,
                            brackets,
                        });
                    }
                }
            }
        }
    }
    all
}

/// Text chosen to straddle every kernel's block size (16, 32 and 64
/// bytes) and to mix the cases that end a run — non-ASCII, punctuation,
/// characters the policy widens — with the ones that do not.
fn corpus() -> Vec<String> {
    let mut all: Vec<String> = [
        "",
        "a",
        "docker",
        "、",
        "。",
        "，",
        "．",
        "こんにちは",
        "ａｂｃ１２３",
        "\u{3000}",
        "🍣",
        "\0\u{1}\u{7f}",
        "日本語とEnglishが混ざったテキスト、句読点。",
        "A[、，｡]\0𠮷🍣z．。］終わり",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();

    for base in [
        "abcdefghijklmnop",
        "0123456789!@#$%^",
        "あいうえお",
        "aあ1、",
        " \t\n",
    ] {
        for repeat in [1usize, 2, 3, 5, 9] {
            all.push(base.repeat(repeat));
        }
    }
    let exact_length_pattern = "a1[]!,.Z09_-+/=control".repeat(4);
    for len in [16usize, 31, 32, 63, 64, 65] {
        all.push(exact_length_pattern[..len].to_owned());
    }
    all
}

fn scalar_reference(normalizer: &Normalizer, src: &str, mode: Mode) -> String {
    src.chars()
        .map(|c| normalizer.normalize_char(c, mode))
        .collect()
}

/// The width-policy run path must be observably identical to the scalar
/// replaced — [`Normalizer::normalize_char`] over every character. This
/// is the assertion that stands between a vector kernel and the user's
/// text, so it runs over every policy, every mode, and text long enough
/// to reach the widest kernel this machine has.
#[test]
fn normalize_into_agrees_with_normalize_char_everywhere() {
    for normalizer in every_normalizer() {
        for mode in Mode::ALL {
            if matches!(mode, Mode::Katakana | Mode::HalfKatakana) {
                continue;
            }
            for src in corpus() {
                let expected: String = src
                    .chars()
                    .map(|c| normalizer.normalize_char(c, mode))
                    .collect();
                let mut actual = String::new();
                normalizer
                    .normalize_into(&src, mode, &mut actual)
                    .expect("a String never overflows");
                assert_eq!(
                    actual, expected,
                    "{normalizer:?} in {mode:?} disagreed on {src:?}"
                );
            }
        }
    }
}

#[test]
fn exact_vector_boundaries_are_identical_from_misaligned_slices() {
    let payload = "a1[]!,.Z09_-+/=control".repeat(4);
    let storage = format!("{}{payload}", "#".repeat(64));
    let offset = (1usize..=32)
        .find(|offset| !(storage.as_ptr() as usize + offset).is_multiple_of(32))
        .expect("one of 32 consecutive offsets must be misaligned");
    assert!(!(storage.as_ptr() as usize + offset).is_multiple_of(32));

    for len in [16usize, 31, 32, 63, 64, 65] {
        let src = &storage[offset..offset + len];
        assert_eq!(src.len(), len);
        for normalizer in every_normalizer() {
            for mode in Mode::ALL {
                if matches!(mode, Mode::Katakana | Mode::HalfKatakana) {
                    continue;
                }
                let expected = scalar_reference(&normalizer, src, mode);
                let mut actual = String::new();
                normalizer
                    .normalize_into(src, mode, &mut actual)
                    .expect("a String never overflows");
                assert_eq!(
                    actual, expected,
                    "{normalizer:?} in {mode:?} disagreed at offset {offset}, length {len}"
                );
            }
        }
    }
}

#[test]
fn all_full_path_selection_uses_the_first_byte_and_preserves_other_policies() {
    let src = "issue_110_width_normalizer_fast_path_identifier_2026";
    let all_full = Normalizer {
        width: WidthPolicy {
            alnum: Width::Full,
            number: Width::Full,
            symbol: Width::Full,
        },
        ..Normalizer::default()
    };
    let follow_mode = Normalizer {
        width: WidthPolicy {
            alnum: Width::FollowMode,
            number: Width::FollowMode,
            symbol: Width::FollowMode,
        },
        ..Normalizer::default()
    };

    for (normalizer, mode, input, expected_scans) in [
        // Governed ASCII starts with an unadmitted byte, so both static
        // Full and mode-resolved Full take the scalar rewrite.
        (all_full, Mode::Hiragana, src, 0),
        (follow_mode, Mode::FullAlnum, src, 0),
        // Leading unaffected bytes retain the scanner under all-full.
        (all_full, Mode::Hiragana, " issue_110_identifier_2026", 1),
        (all_full, Mode::Hiragana, "\0issue_110_identifier_2026", 1),
        // Non-all-full controls keep their pre-I2 scanner choice.
        (Normalizer::default(), Mode::Hiragana, src, 1),
        (follow_mode, Mode::Hiragana, src, 1),
    ] {
        assert_observed_scan_count(normalizer, mode, input, expected_scans);
    }

    // The gate reads bytes without assuming alignment. Exercise both
    // sides using offset slices whose starts are explicitly not 32-byte
    // aligned and whose lengths cross every shipping vector floor.
    let scalar_storage = "i".repeat(96);
    let scalar_offset = (1usize..=32)
        .find(|offset| !(scalar_storage.as_ptr() as usize + offset).is_multiple_of(32))
        .expect("one of 32 consecutive offsets must be misaligned");
    let scalar_slice = &scalar_storage[scalar_offset..scalar_offset + 65];
    assert_observed_scan_count(all_full, Mode::Hiragana, scalar_slice, 0);

    let scanner_storage = " ".repeat(96);
    let scanner_offset = (1usize..=32)
        .find(|offset| !(scanner_storage.as_ptr() as usize + offset).is_multiple_of(32))
        .expect("one of 32 consecutive offsets must be misaligned");
    let scanner_slice = &scanner_storage[scanner_offset..scanner_offset + 65];
    assert_observed_scan_count(all_full, Mode::Hiragana, scanner_slice, 1);
}

#[test]
fn all_full_identifier_and_leading_passthrough_paths_match_scalar_output() {
    let all_full = Normalizer {
        width: WidthPolicy {
            alnum: Width::Full,
            number: Width::Full,
            symbol: Width::Full,
        },
        ..Normalizer::default()
    };
    for src in [
        "issue_110_width_normalizer_fast_path_identifier_2026",
        " issue_110_width_normalizer_fast_path_identifier_2026",
        "\0\t\n\r\u{1}\u{7f} issue_110_identifier_2026",
    ] {
        let expected = scalar_reference(&all_full, src, Mode::Hiragana);
        let mut actual = String::new();
        all_full
            .normalize_into(src, Mode::Hiragana, &mut actual)
            .expect("a String never overflows");
        assert_eq!(actual, expected, "{src:?}");
    }
}

#[test]
fn all_full_overflow_is_atomic_at_every_character_boundary() {
    let normalizer = Normalizer {
        width: WidthPolicy {
            alnum: Width::Full,
            number: Width::Full,
            symbol: Width::Full,
        },
        ..Normalizer::default()
    };
    let src = "a".repeat(32);
    assert!(src.len() >= simd::MIN_VECTOR_BYTES);

    macro_rules! assert_capacity {
        ($capacity:literal, $fitting_characters:literal, $result:expr) => {{
            let mut dst = FixedStr::<$capacity>::new();
            assert_eq!(
                normalizer.normalize_into(&src, Mode::Hiragana, &mut dst),
                $result,
                "capacity {}",
                $capacity
            );
            assert_eq!(
                dst.as_str(),
                "ａ".repeat($fitting_characters),
                "capacity {}",
                $capacity
            );
        }};
    }

    assert_capacity!(0, 0, Err(Overflow));
    assert_capacity!(1, 0, Err(Overflow));
    assert_capacity!(2, 0, Err(Overflow));
    assert_capacity!(3, 1, Err(Overflow));
    assert_capacity!(92, 30, Err(Overflow));
    assert_capacity!(93, 31, Err(Overflow));
    assert_capacity!(94, 31, Err(Overflow));
    assert_capacity!(95, 31, Err(Overflow));
    assert_capacity!(96, 32, Ok(()));

    // A leading passthrough run selects the hybrid scanner branch. The
    // space and first widened character fit exactly; the next character
    // fails atomically and leaves the same normalized prefix as scalar.
    let leading_space = format!(" {}", "a".repeat(32));
    let mut scanner_dst = FixedStr::<4>::new();
    assert_eq!(
        normalizer.normalize_into(&leading_space, Mode::Hiragana, &mut scanner_dst),
        Err(Overflow)
    );
    assert_eq!(scanner_dst.as_str(), " ａ");

    // When the passthrough run itself is too large, the existing replay
    // path must retain the exact prefix that fits under the all-full gate.
    let spaces = " ".repeat(32);
    let mut run_dst = FixedStr::<20>::new();
    assert_eq!(
        normalizer.normalize_into(&spaces, Mode::Hiragana, &mut run_dst),
        Err(Overflow)
    );
    assert_eq!(run_dst.as_str(), " ".repeat(20));
}

/// The same agreement, driven by character value rather than by string
/// shape: every ASCII character in turn, buried in a long run so it is
/// classified by a vector kernel rather than by the scalar tail.
#[test]
fn every_ascii_character_survives_the_run_path_identically() {
    let policies = [
        Normalizer::default(),
        Normalizer {
            width: WidthPolicy {
                alnum: Width::Full,
                number: Width::Full,
                symbol: Width::Full,
            },
            punctuation: PunctuationStyle::COMMA_PERIOD,
            brackets: BracketStyle::default(),
        },
    ];
    for normalizer in policies {
        for cp in 0u32..0x80 {
            let c = char::from_u32(cp).expect("every ASCII code point is a character");
            let src = format!("{}{c}{}", "x".repeat(40), "y".repeat(40));
            let expected: String = src
                .chars()
                .map(|c| normalizer.normalize_char(c, Mode::Direct))
                .collect();
            let mut actual = String::new();
            normalizer
                .normalize_into(&src, Mode::Direct, &mut actual)
                .expect("a String never overflows");
            assert_eq!(actual, expected, "{normalizer:?} disagreed on {c:?}");
        }
    }
}

/// A run that does not fit must still leave the prefix that does. The
/// bulk copy is all-or-nothing, so this is the case where the fast path
/// has to fall back to reproduce the documented semantics.
#[test]
fn an_overflowing_run_still_leaves_the_prefix_that_fits() {
    let normalizer = Normalizer::default();
    // Long enough to be one passthrough run rather than a few characters.
    let mut dst = FixedStr::<20>::new();
    let result = normalizer.normalize_into("abcdefghijklmnopqrstuvwxyz", Mode::Direct, &mut dst);
    assert_eq!(result, Err(Overflow));
    assert_eq!(dst.as_str(), "abcdefghijklmnopqrst");
}

/// The same, for a run that ends mid-string because the *next* character
/// is transformed — the fallback must not lose the characters the run
/// already placed.
#[test]
fn overflow_after_a_completed_run_keeps_what_landed() {
    let normalizer = Normalizer {
        width: WidthPolicy {
            alnum: Width::Half,
            number: Width::Half,
            symbol: Width::Half,
        },
        punctuation: PunctuationStyle::KUTEN_TOUTEN,
        brackets: BracketStyle::default(),
    };
    // "abc" is a run; 、is three bytes and does not fit in the last one.
    let mut dst = FixedStr::<4>::new();
    assert_eq!(
        normalizer.normalize_into("abc、d", Mode::Direct, &mut dst),
        Err(Overflow)
    );
    assert_eq!(dst.as_str(), "abc");
}

#[test]
fn normalize_into_reports_overflow_into_a_fixed_str() {
    let normalizer = Normalizer::default();
    let mut dst = FixedStr::<2>::new();
    let result = normalizer.normalize_into("abc", Mode::Direct, &mut dst);
    assert_eq!(result, Err(Overflow));
    // Atomic per character: the two that fit landed, the third did not.
    assert_eq!(dst.as_str(), "ab");
}

#[test]
fn empty_input_produces_empty_output() {
    let normalizer = Normalizer::default();
    let mut dst = String::new();
    normalizer
        .normalize_into("", Mode::Direct, &mut dst)
        .expect("empty input always fits");
    assert_eq!(dst, "");
}

#[test]
fn defaults_match_design_defaults() {
    assert_eq!(
        WidthPolicy::default(),
        WidthPolicy {
            alnum: Width::Half,
            number: Width::Half,
            symbol: Width::Half,
        }
    );
    assert_eq!(PunctuationStyle::default(), PunctuationStyle::KUTEN_TOUTEN);
    assert_eq!(BracketStyle::default(), BracketStyle::Corner);
    assert_eq!(
        Normalizer::default(),
        Normalizer {
            width: WidthPolicy::default(),
            punctuation: PunctuationStyle::default(),
            brackets: BracketStyle::default(),
        }
    );
}

#[test]
fn punctuation_parts_cover_all_independent_combinations() {
    // `ALL` is what the settings combos and the persistence round-trips
    // iterate, so it has to be the exact cross product of the two roles
    // — no combination missing, none listed twice.
    assert_eq!(PunctuationStyle::ALL.len(), 9);
    let mut seen = Vec::new();
    for comma in CommaMark::ALL {
        for period in PeriodMark::ALL {
            let style = PunctuationStyle::new(comma, period);
            assert!(
                PunctuationStyle::ALL.contains(&style),
                "ALL is missing {style:?}"
            );
            assert_eq!(style.comma, comma);
            assert_eq!(style.period, period);
            seen.push(style);
        }
    }
    for style in PunctuationStyle::ALL {
        assert_eq!(
            seen.iter().filter(|candidate| **candidate == style).count(),
            1,
            "{style:?} is not listed exactly once"
        );
    }

    // The named conventions the settings screen and the config format
    // talk about, spelled out so reordering either role enum cannot
    // quietly repoint one of them.
    assert_eq!(PunctuationStyle::KUTEN_TOUTEN.comma.glyph(), '\u{3001}');
    assert_eq!(PunctuationStyle::KUTEN_TOUTEN.period.glyph(), '\u{3002}');
    assert_eq!(PunctuationStyle::COMMA_PERIOD.comma.glyph(), '\u{FF0C}');
    assert_eq!(PunctuationStyle::COMMA_PERIOD.period.glyph(), '\u{FF0E}');
    assert_eq!(PunctuationStyle::MIXED.comma.glyph(), '\u{3001}');
    assert_eq!(PunctuationStyle::MIXED.period.glyph(), '\u{FF0E}');
    assert_eq!(PunctuationStyle::COMMA_KUTEN.comma.glyph(), '\u{FF0C}');
    assert_eq!(PunctuationStyle::COMMA_KUTEN.period.glyph(), '\u{3002}');
    assert_eq!(PunctuationStyle::ASCII.comma.glyph(), ',');
    assert_eq!(PunctuationStyle::ASCII.period.glyph(), '.');
}

#[test]
fn every_style_orders_its_own_glyph_first_and_keeps_the_whole_family() {
    // The setting decides the first row, not which rows exist. Both
    // halves are exhaustive over the nine styles because a single style
    // getting this wrong is invisible in any other test.
    for style in PunctuationStyle::ALL {
        for (family, preferred) in [
            (COMMA_FAMILY, style.comma.glyph()),
            (PERIOD_FAMILY, style.period.glyph()),
        ] {
            for member in family {
                let ordered = style
                    .family_for(member.glyph)
                    .unwrap_or_else(|| panic!("{:?} has no family", member.glyph));
                assert_eq!(
                    ordered[0].glyph, preferred,
                    "{style:?} must offer its own glyph first for {:?}",
                    member.glyph
                );
                for expected in family {
                    assert_eq!(
                        ordered
                            .iter()
                            .filter(|variant| variant.glyph == expected.glyph)
                            .count(),
                        1,
                        "{style:?}: {:?} is not offered exactly once",
                        expected.glyph
                    );
                }
                // Everything after the first row keeps the table's own
                // order, so the list a reader learns does not reshuffle
                // when they change the setting.
                let tail: Vec<char> = ordered[1..].iter().map(|v| v.glyph).collect();
                let expected_tail: Vec<char> = family
                    .iter()
                    .map(|v| v.glyph)
                    .filter(|glyph| *glyph != preferred)
                    .collect();
                assert_eq!(tail, expected_tail, "{style:?}");
            }
        }
    }
}

#[test]
fn punctuation_families_are_disjoint_and_carry_distinct_annotations() {
    let mut glyphs = Vec::new();
    let mut annotations = Vec::new();
    for variant in COMMA_FAMILY.into_iter().chain(PERIOD_FAMILY) {
        assert!(
            !glyphs.contains(&variant.glyph),
            "{:?} appears in both families",
            variant.glyph
        );
        assert!(
            !annotations.contains(&variant.annotation),
            "`{}` annotates two glyphs",
            variant.annotation
        );
        glyphs.push(variant.glyph);
        annotations.push(variant.annotation);
    }
    assert_eq!(glyphs.len(), PUNCTUATION_FAMILY_LEN * 2);
    // A character in neither family has no family, however punctuation-
    // like it looks. `・` and `！` are the near misses worth pinning.
    for outsider in ['a', 'あ', '・', '！', '!', '｢'] {
        assert!(
            PunctuationStyle::default().family_for(outsider).is_none(),
            "{outsider:?}"
        );
    }
}

/// The dispatcher pins the configured mark as the default selection on the
/// same admission test the converter appends the family on, so a
/// disagreement between the two would put the family on the page with the
/// ranker still free to move off its top row.
#[test]
fn only_a_whole_reading_of_one_mark_opens_the_family() {
    let style = PunctuationStyle::default();
    for reading in ["\u{3001}", "\u{FF64}", "\u{FF0C}", ",", "\u{3002}", "."] {
        assert_eq!(
            style.family_reading(reading).map(|family| family.to_vec()),
            style
                .family_for(reading.chars().next().expect("one mark"))
                .map(|family| family.to_vec()),
            "{reading:?} has to answer exactly like its single character"
        );
    }
    for reading in [
        "",
        "\u{3001}\u{3001}",
        "\u{3001}\u{3072}",
        "\u{3072}\u{3001}",
        "\u{3072}",
    ] {
        assert!(
            style.family_reading(reading).is_none(),
            "{reading:?} is an ordinary reading, not a request for the family"
        );
    }
}

#[test]
fn half_width_kana_marks_are_offerable_without_being_claimed() {
    // `､` and `｡` are candidates the converter can offer but glyphs the
    // choke point does not own: `punct_role` ignores them and they sit
    // outside the U+FF01..=U+FF5E width arithmetic. That is what lets a
    // reader pick one and keep it under any setting, and it is why the
    // family table can list them without widening rule 4's four-code-
    // point set.
    assert!(punct_role('\u{FF64}').is_none());
    assert!(punct_role('\u{FF61}').is_none());
    for style in PunctuationStyle::ALL {
        for symbol in [Width::Half, Width::Full] {
            let normalizer = Normalizer {
                width: WidthPolicy {
                    alnum: symbol,
                    number: symbol,
                    symbol,
                },
                punctuation: style,
                brackets: BracketStyle::default(),
            };
            for mode in [Mode::Direct, Mode::Hiragana] {
                assert_eq!(normalizer.normalize_char('\u{FF64}', mode), '\u{FF64}');
                assert_eq!(normalizer.normalize_char('\u{FF61}', mode), '\u{FF61}');
            }
        }
    }
}
