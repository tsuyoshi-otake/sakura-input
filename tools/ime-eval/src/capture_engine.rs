use std::path::Path;
use std::time::Duration;

use crate::capture::MAX_CANDIDATES_PER_SYSTEM;
use crate::quality::QUALITY_CANDIDATE_LIMIT;
use crate::types::{err, Error, SemanticCase, SystemOutput};

const MAX_CANDIDATE_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureInputMethod {
    Romaji,
    Kana,
}

/// Romaji capture of one engine artifact, ready for a schema-v1 capture file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateCapture {
    /// One output per requested case, in request order. Each holds at most
    /// `MAX_CANDIDATES_PER_SYSTEM` candidates in engine order.
    pub outputs: Vec<SystemOutput>,
    /// Cases whose engine list was longer than `MAX_CANDIDATES_PER_SYSTEM`
    /// and was cut to its leading candidates, in request order.
    pub truncated_case_ids: Vec<String>,
}

/// One captured case after its lane's candidate bound was applied.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CapturedCase {
    output: SystemOutput,
    truncated: bool,
}

/// Captures conversion candidates from one owned engine artifact.
///
/// The Windows implementation launches the supplied engine on a private test
/// pipe and private `LOCALAPPDATA` tree. Other platforms fail closed because
/// the shipping engine and its named-pipe contract are Windows-only.
///
/// A case whose engine list is longer than the capture-file bound keeps its
/// leading `MAX_CANDIDATES_PER_SYSTEM` candidates and is named in
/// `truncated_case_ids`; it does not fail the capture (Issue #297).
///
/// A `literal_token` case is not converted: its single candidate is the
/// composition the engine shows after typing (Issue #303).
pub fn capture_candidates(
    engine: &Path,
    dictionary: &Path,
    cases: &[SemanticCase],
    temp_root: &Path,
    timeout: Duration,
) -> Result<CandidateCapture, Error> {
    let captured = capture_candidates_with_input_method(
        engine,
        dictionary,
        cases,
        temp_root,
        timeout,
        CaptureInputMethod::Romaji,
    )?;
    let truncated_case_ids = cases
        .iter()
        .zip(&captured)
        .filter(|(_, captured)| captured.truncated)
        .map(|(case, _)| case.case_id.clone())
        .collect();
    Ok(CandidateCapture {
        outputs: captured.into_iter().map(|case| case.output).collect(),
        truncated_case_ids,
    })
}

/// Capture direct kana cases in an isolated profile. This is used by the
/// deterministic quality fixture because its source contract supplies kana
/// readings, not a user-specific romaji spelling. The profile is temporary,
/// so it cannot touch learning or user-dictionary state.
///
/// A list longer than the quality lane's 18-candidate production contract
/// (`QUALITY_CANDIDATE_LIMIT`) fails the capture; it is never cut.
pub fn capture_kana_candidates(
    engine: &Path,
    dictionary: &Path,
    cases: &[SemanticCase],
    temp_root: &Path,
    timeout: Duration,
) -> Result<Vec<SystemOutput>, Error> {
    let captured = capture_candidates_with_input_method(
        engine,
        dictionary,
        cases,
        temp_root,
        timeout,
        CaptureInputMethod::Kana,
    )?;
    Ok(captured.into_iter().map(|case| case.output).collect())
}

fn capture_candidates_with_input_method(
    engine: &Path,
    dictionary: &Path,
    cases: &[SemanticCase],
    temp_root: &Path,
    timeout: Duration,
    input_method: CaptureInputMethod,
) -> Result<Vec<CapturedCase>, Error> {
    #[cfg(windows)]
    {
        windows::capture_candidates(engine, dictionary, cases, temp_root, timeout, input_method)
    }

    #[cfg(not(windows))]
    {
        let _ = (engine, dictionary, cases, temp_root, timeout, input_method);
        Err(err(
            "real engine candidate capture is only supported on Windows",
        ))
    }
}

/// Applies one lane's candidate bound to an engine list, kept in engine order.
///
/// The wire allows up to `sakura_proto::MAX_CANDIDATES` candidates, and real
/// readings often return more than 64. Romaji capture writes schema-v1 capture
/// files, which accept at most `MAX_CANDIDATES_PER_SYSTEM` candidates per
/// system, so a longer list keeps its leading candidates and reports the cut.
/// Kana capture feeds the quality lane, whose production contract rejects
/// lists longer than `QUALITY_CANDIDATE_LIMIT` instead. In both lanes an empty
/// list, or an empty or oversized candidate anywhere in the list, fails.
fn bound_candidates(
    case_id: &str,
    mut candidates: Vec<String>,
    input_method: CaptureInputMethod,
) -> Result<CapturedCase, Error> {
    if candidates.is_empty() {
        return Err(err(format!("case {case_id} produced no candidates")));
    }
    let outside_bounds = || {
        err(format!(
            "case {case_id} produced candidates outside capture bounds"
        ))
    };
    if candidates
        .iter()
        .any(|candidate| candidate.is_empty() || candidate.len() > MAX_CANDIDATE_BYTES)
    {
        return Err(outside_bounds());
    }
    let truncated = match input_method {
        CaptureInputMethod::Romaji => {
            let truncated = candidates.len() > MAX_CANDIDATES_PER_SYSTEM;
            candidates.truncate(MAX_CANDIDATES_PER_SYSTEM);
            truncated
        }
        CaptureInputMethod::Kana if candidates.len() > QUALITY_CANDIDATE_LIMIT => {
            return Err(outside_bounds());
        }
        CaptureInputMethod::Kana => false,
    };
    Ok(CapturedCase {
        output: SystemOutput { candidates },
        truncated,
    })
}

/// The candidate list of one `literal_token` case (Issue #303).
///
/// The engine keeps a typed ASCII token as a literal composition, where Space
/// inserts a space instead of opening a candidate list. The case's single
/// candidate is therefore the composition left after typing, which is exactly
/// what the literal oracle compares with the reading. Text committed while
/// typing, or no composition at all, fails the case rather than recording
/// only part of the token.
fn literal_candidates(
    case_id: &str,
    composition: &str,
    committed: &str,
) -> Result<Vec<String>, Error> {
    if !committed.is_empty() {
        return Err(err(format!(
            "case {case_id} (literal_token) committed {committed:?} while typing"
        )));
    }
    if composition.is_empty() {
        return Err(err(format!(
            "case {case_id} (literal_token) left no composition after typing"
        )));
    }
    Ok(vec![composition.to_owned()])
}

#[cfg(windows)]
mod windows {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread::sleep;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use sakura_ipc::{Client, Fault, PATIENT_CONNECT};
    use sakura_proto::{
        InputScope, KeyCode, KeyInput, Mode, Modifiers, Preedit, Request, Response, SessionId,
        PROTOCOL_VERSION,
    };

    use super::{bound_candidates, literal_candidates, CaptureInputMethod, CapturedCase};
    use crate::types::{err, Error, SemanticCase};

    const PIPE_PREFIX: &str = r"\\.\pipe\SakuraInputEngineTest-";
    const CONNECT_SLICE: Duration = Duration::from_millis(100);
    const MAX_TYPING_BYTES: usize = 4096;
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    pub(super) fn capture_candidates(
        engine: &Path,
        dictionary: &Path,
        cases: &[SemanticCase],
        temp_root: &Path,
        timeout: Duration,
        input_method: CaptureInputMethod,
    ) -> Result<Vec<CapturedCase>, Error> {
        if cases.is_empty() {
            return Err(err("candidate capture has no semantic cases"));
        }
        if !engine.is_file() {
            return Err(err(format!(
                "engine executable does not exist: {}",
                engine.display()
            )));
        }
        if !dictionary.is_file() {
            return Err(err(format!(
                "dictionary image does not exist: {}",
                dictionary.display()
            )));
        }

        let mut owned = OwnedEngine::spawn(engine, dictionary, temp_root, input_method)?;
        let result = (|| {
            let mut client = owned.connect(timeout)?;
            handshake(&mut client, timeout)?;
            let mut captured = Vec::with_capacity(cases.len());
            for case in cases {
                captured.push(capture_case(&mut client, case, timeout, input_method)?);
            }
            Ok(captured)
        })();
        drop_client_before_cleanup(result, &mut owned)
    }

    fn drop_client_before_cleanup(
        result: Result<Vec<CapturedCase>, Error>,
        owned: &mut OwnedEngine,
    ) -> Result<Vec<CapturedCase>, Error> {
        let cleanup = owned.cleanup();
        match (result, cleanup) {
            (Ok(captured), Ok(())) => Ok(captured),
            (Err(error), Ok(())) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Err(error), Err(_cleanup_error)) => Err(error),
        }
    }

    fn handshake(client: &mut Client, timeout: Duration) -> Result<(), Error> {
        match client
            .call(
                &Request::Hello {
                    client_version: PROTOCOL_VERSION,
                },
                timeout,
            )
            .map_err(|fault| err(format!("candidate capture Hello failed: {fault}")))?
        {
            Response::Hello { server_version, .. } if server_version == PROTOCOL_VERSION => Ok(()),
            Response::Hello { server_version, .. } => Err(err(format!(
                "candidate capture protocol mismatch: engine={server_version}, runner={PROTOCOL_VERSION}"
            ))),
            other => Err(err(format!(
                "candidate capture expected Hello, got {other:?}"
            ))),
        }
    }

    fn capture_case(
        client: &mut Client,
        case: &SemanticCase,
        timeout: Duration,
        input_method: CaptureInputMethod,
    ) -> Result<CapturedCase, Error> {
        let typing = case.input.typing.as_deref().ok_or_else(|| {
            err(format!(
                "case {} has no input.typing capture sequence",
                case.case_id
            ))
        })?;
        if typing.is_empty() {
            return Err(err(format!(
                "case {} has an empty input.typing capture sequence",
                case.case_id
            )));
        }
        if typing.len() > MAX_TYPING_BYTES {
            return Err(err(format!(
                "case {} input.typing exceeds {MAX_TYPING_BYTES} bytes",
                case.case_id
            )));
        }
        if case.task != "conversion" {
            return Err(err(format!(
                "case {} has unsupported capture task {:?}",
                case.case_id, case.task
            )));
        }

        let session = create_session(client, timeout)?;
        let result = (|| {
            expect_ok(
                client,
                Request::SetInputScope {
                    session,
                    scope: InputScope::Normal,
                },
                timeout,
                "SetInputScope",
            )?;
            expect_input_mode(client, session, timeout)?;

            let mut composition = String::new();
            let mut committed = String::new();
            for character in typing.chars() {
                let key = character_key(character)?;
                match client
                    .call(&Request::SendKey { session, key }, timeout)
                    .map_err(|fault| {
                        err(format!(
                            "case {} key {:?} failed: {fault}",
                            case.case_id, character
                        ))
                    })? {
                    Response::Output(output) if output.consumed => {
                        composition = preedit_text(output.preedit.as_ref());
                        if let Some(commit) = output.commit {
                            committed.push_str(&commit);
                        }
                    }
                    Response::Output(output) => {
                        return Err(err(format!(
                            "case {} key {:?} was not consumed: {output:?}",
                            case.case_id, character
                        )));
                    }
                    other => {
                        return Err(err(format!(
                            "case {} key {:?} expected Output, got {other:?}",
                            case.case_id, character
                        )));
                    }
                }
            }

            if case.constraints.literal_token {
                let candidates = literal_candidates(&case.case_id, &composition, &committed)?;
                return bound_candidates(&case.case_id, candidates, input_method);
            }

            let output = match client
                .call(
                    &Request::SendKey {
                        session,
                        key: named_key(KeyCode::Space),
                    },
                    timeout,
                )
                .map_err(|fault| err(format!("case {} conversion failed: {fault}", case.case_id)))?
            {
                Response::Output(output) if output.consumed => output,
                Response::Output(output) => {
                    return Err(err(format!(
                        "case {} conversion key was not consumed: {output:?}",
                        case.case_id
                    )));
                }
                other => {
                    return Err(err(format!(
                        "case {} conversion expected Output, got {other:?}",
                        case.case_id
                    )));
                }
            };
            let candidates = output.candidates.ok_or_else(|| {
                let preedit = preedit_text(output.preedit.as_ref());
                err(format!(
                    "case {} produced no candidate list (consumed={}, beep={}, preedit={preedit:?}, commit={:?})",
                    case.case_id, output.consumed, output.beep, output.commit
                ))
            })?;
            bound_candidates(
                &case.case_id,
                candidates
                    .items
                    .into_iter()
                    .map(|candidate| candidate.text)
                    .collect(),
                input_method,
            )
        })();

        let _ = client.call(&Request::Revert { session }, timeout);
        let _ = client.call(&Request::DeleteSession { session }, timeout);
        result
    }

    /// The composition text an engine output displays, or empty without one.
    fn preedit_text(preedit: Option<&Preedit>) -> String {
        preedit
            .map(|preedit| {
                preedit
                    .segments
                    .iter()
                    .map(|segment| segment.text.as_str())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn create_session(client: &mut Client, timeout: Duration) -> Result<SessionId, Error> {
        match client
            .call(
                &Request::CreateSession {
                    process_name: "sakura-ime-eval.exe".to_owned(),
                },
                timeout,
            )
            .map_err(|fault| err(format!("candidate capture CreateSession failed: {fault}")))?
        {
            Response::SessionCreated { session, .. } => Ok(session),
            other => Err(err(format!(
                "candidate capture expected SessionCreated, got {other:?}"
            ))),
        }
    }

    fn expect_ok(
        client: &mut Client,
        request: Request,
        timeout: Duration,
        operation: &str,
    ) -> Result<(), Error> {
        match client
            .call(&request, timeout)
            .map_err(|fault| err(format!("{operation} failed: {fault}")))?
        {
            Response::Ok => Ok(()),
            other => Err(err(format!("{operation} expected Ok, got {other:?}"))),
        }
    }

    fn expect_input_mode(
        client: &mut Client,
        session: SessionId,
        timeout: Duration,
    ) -> Result<(), Error> {
        match client
            .call(
                &Request::SetMode {
                    session,
                    mode: Mode::Hiragana,
                },
                timeout,
            )
            .map_err(|fault| err(format!("SetMode failed: {fault}")))?
        {
            Response::InputMode {
                mode: Mode::Hiragana,
            } => Ok(()),
            other => Err(err(format!(
                "candidate capture expected Hiragana InputMode, got {other:?}"
            ))),
        }
    }

    fn character_key(character: char) -> Result<KeyInput, Error> {
        if character == '\0' || character.is_control() {
            return Err(err(format!(
                "capture typing contains unsupported control character U+{:04X}",
                character as u32
            )));
        }
        Ok(KeyInput {
            code: KeyCode::Char,
            ch: Some(character),
            modifiers: if character.is_ascii_uppercase() {
                Modifiers::SHIFT
            } else {
                Modifiers::NONE
            },
            repeat: false,
            test_only: false,
        })
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

    #[derive(Debug)]
    struct OwnedEngine {
        child: Option<Child>,
        pipe_name: String,
        profile: Option<PathBuf>,
    }

    impl OwnedEngine {
        fn spawn(
            engine: &Path,
            dictionary: &Path,
            temp_root: &Path,
            input_method: CaptureInputMethod,
        ) -> Result<Self, Error> {
            fs::create_dir_all(temp_root)
                .map_err(|error| err(format!("create {}: {error}", temp_root.display())))?;
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|error| err(format!("read system clock: {error}")))?
                .as_nanos();
            let sequence = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let token = format!("{}-{nonce:x}-{sequence:x}", std::process::id());
            let profile = temp_root.join(format!("sakura-ime-eval-{token}"));
            fs::create_dir(&profile)
                .map_err(|error| err(format!("create {}: {error}", profile.display())))?;
            if input_method == CaptureInputMethod::Kana {
                let config_path = profile
                    .join("SakuraInput")
                    .join("config")
                    .join("config.toml");
                if let Some(parent) = config_path.parent() {
                    fs::create_dir_all(parent)
                        .map_err(|error| err(format!("create {}: {error}", parent.display())))?;
                }
                fs::write(&config_path, crate::quality::QUALITY_CAPTURE_CONFIG)
                    .map_err(|error| err(format!("write {}: {error}", config_path.display())))?;
            }
            let pipe_name = format!("{PIPE_PREFIX}eval-{token}");
            let mut command = Command::new(engine);
            command
                .arg("--test-pipe")
                .arg(&pipe_name)
                .env("SAKURA_DICTIONARY", dictionary)
                .env("LOCALAPPDATA", &profile)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            let child = match command.spawn() {
                Ok(child) => child,
                Err(error) => {
                    let _ = fs::remove_dir_all(&profile);
                    return Err(err(format!("spawn evaluation engine: {error}")));
                }
            };
            Ok(Self {
                child: Some(child),
                pipe_name,
                profile: Some(profile),
            })
        }

        fn connect(&mut self, timeout: Duration) -> Result<Client, Error> {
            let deadline = Instant::now() + timeout;
            loop {
                let child = self
                    .child
                    .as_mut()
                    .ok_or_else(|| err("evaluation engine is no longer owned"))?;
                if let Some(status) = child
                    .try_wait()
                    .map_err(|error| err(format!("inspect evaluation engine: {error}")))?
                {
                    return Err(err(format!(
                        "evaluation engine exited before pipe connection: {status}"
                    )));
                }
                let left = deadline
                    .checked_duration_since(Instant::now())
                    .unwrap_or_default();
                if left.is_zero() {
                    return Err(err("evaluation engine pipe connection timed out"));
                }
                match Client::connect_to(&self.pipe_name, left.min(CONNECT_SLICE)) {
                    Ok(client) => {
                        let expected = child.id();
                        let actual = client.server_process_id().map_err(|fault| {
                            err(format!("identify evaluation pipe server: {fault}"))
                        })?;
                        if actual != expected {
                            return Err(err(format!(
                                "evaluation pipe served by pid {actual}, expected owned pid {expected}"
                            )));
                        }
                        return Ok(client);
                    }
                    Err(Fault::Timeout) => sleep(Duration::from_millis(10)),
                    Err(_) if Instant::now() < deadline => sleep(Duration::from_millis(10)),
                    Err(fault) => {
                        return Err(err(format!("connect evaluation engine pipe: {fault}")));
                    }
                }
            }
        }

        fn cleanup(&mut self) -> Result<(), Error> {
            let result = self.cleanup_child();
            let profile_result = self.profile.take().map_or(Ok(()), |profile| {
                fs::remove_dir_all(&profile)
                    .map_err(|error| err(format!("remove {}: {error}", profile.display())))
            });
            match (result, profile_result) {
                (Ok(()), Ok(())) => Ok(()),
                (Err(error), _) => Err(error),
                (Ok(()), Err(error)) => Err(error),
            }
        }

        fn cleanup_child(&mut self) -> Result<(), Error> {
            let Some(mut child) = self.child.take() else {
                return Ok(());
            };
            let pid = child.id();
            if let Ok(client) = Client::connect_to(&self.pipe_name, PATIENT_CONNECT) {
                if client.server_process_id().ok() == Some(pid) {
                    let mut client = client;
                    let _ = client.call(&Request::Shutdown, PATIENT_CONNECT);
                }
            }
            let deadline = Instant::now() + PATIENT_CONNECT;
            loop {
                match child
                    .try_wait()
                    .map_err(|error| err(format!("wait evaluation engine pid {pid}: {error}")))?
                {
                    Some(_) => return Ok(()),
                    None if Instant::now() < deadline => sleep(Duration::from_millis(20)),
                    None => {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(err(format!(
                            "evaluation engine pid {pid} did not exit after shutdown"
                        )));
                    }
                }
            }
        }
    }

    impl Drop for OwnedEngine {
        fn drop(&mut self) {
            if self.child.is_some() {
                let _ = self.cleanup();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbered(count: usize) -> Vec<String> {
        (0..count).map(|index| format!("候補{index:03}")).collect()
    }

    #[test]
    fn romaji_list_over_the_file_bound_keeps_leading_candidates_and_flags_the_cut() {
        let captured = bound_candidates("long", numbered(106), CaptureInputMethod::Romaji)
            .expect("a long romaji list is cut, not rejected");
        assert!(captured.truncated);
        assert_eq!(
            captured.output.candidates,
            numbered(MAX_CANDIDATES_PER_SYSTEM)
        );
    }

    #[test]
    fn romaji_list_at_the_file_bound_is_kept_whole_and_not_flagged() {
        let captured = bound_candidates(
            "exact",
            numbered(MAX_CANDIDATES_PER_SYSTEM),
            CaptureInputMethod::Romaji,
        )
        .expect("a list at the bound is valid");
        assert!(!captured.truncated);
        assert_eq!(
            captured.output.candidates,
            numbered(MAX_CANDIDATES_PER_SYSTEM)
        );
    }

    #[test]
    fn kana_list_keeps_the_quality_production_contract() {
        let captured = bound_candidates(
            "quality",
            numbered(QUALITY_CANDIDATE_LIMIT),
            CaptureInputMethod::Kana,
        )
        .expect("a list at the quality limit is valid");
        assert!(!captured.truncated);
        assert_eq!(captured.output.candidates.len(), QUALITY_CANDIDATE_LIMIT);

        let error = bound_candidates(
            "quality-long",
            numbered(QUALITY_CANDIDATE_LIMIT + 1),
            CaptureInputMethod::Kana,
        )
        .expect_err("the quality lane never cuts a list");
        assert!(error.0.contains("outside capture bounds"), "{error:?}");
    }

    #[test]
    fn malformed_lists_fail_in_both_lanes() {
        for method in [CaptureInputMethod::Romaji, CaptureInputMethod::Kana] {
            let error = bound_candidates("none", Vec::new(), method)
                .expect_err("an empty list has nothing to score");
            assert!(error.0.contains("produced no candidates"), "{error:?}");

            let error = bound_candidates("blank", vec!["今日".to_owned(), String::new()], method)
                .expect_err("an empty candidate is malformed");
            assert!(error.0.contains("outside capture bounds"), "{error:?}");
        }
        // An oversized candidate beyond the cut still fails: truncation never
        // hides malformed engine output.
        let mut long = numbered(MAX_CANDIDATES_PER_SYSTEM + 1);
        long.push("x".repeat(MAX_CANDIDATE_BYTES + 1));
        let error = bound_candidates("oversized", long, CaptureInputMethod::Romaji)
            .expect_err("an oversized candidate is malformed");
        assert!(error.0.contains("outside capture bounds"), "{error:?}");
    }

    #[test]
    fn literal_case_records_its_composition_as_the_single_candidate() {
        assert_eq!(
            literal_candidates("spaced", "Claude Code", "").expect("a literal composition"),
            ["Claude Code"]
        );
    }

    #[test]
    fn literal_case_without_its_whole_composition_fails() {
        let error = literal_candidates("none", "", "")
            .expect_err("no composition leaves nothing to compare");
        assert!(
            error.0.contains("none (literal_token) left no composition"),
            "{error:?}"
        );

        let error = literal_candidates("split", "-512", "AVX")
            .expect_err("a commit while typing would record only part of the token");
        assert!(
            error
                .0
                .contains("split (literal_token) committed \"AVX\" while typing"),
            "{error:?}"
        );
    }
}
