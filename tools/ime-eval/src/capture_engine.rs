use std::path::Path;
use std::time::Duration;

use crate::types::{Error, SemanticCase, SystemOutput};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureInputMethod {
    Romaji,
    Kana,
}

/// Captures conversion candidates from one owned engine artifact.
///
/// The Windows implementation launches the supplied engine on a private test
/// pipe and private `LOCALAPPDATA` tree. Other platforms fail closed because
/// the shipping engine and its named-pipe contract are Windows-only.
pub fn capture_candidates(
    engine: &Path,
    dictionary: &Path,
    cases: &[SemanticCase],
    temp_root: &Path,
    timeout: Duration,
) -> Result<Vec<SystemOutput>, Error> {
    capture_candidates_with_input_method(
        engine,
        dictionary,
        cases,
        temp_root,
        timeout,
        CaptureInputMethod::Romaji,
    )
}

/// Capture direct kana cases in an isolated profile. This is used by the
/// deterministic quality fixture because its source contract supplies kana
/// readings, not a user-specific romaji spelling. The profile is temporary,
/// so it cannot touch learning or user-dictionary state.
pub fn capture_kana_candidates(
    engine: &Path,
    dictionary: &Path,
    cases: &[SemanticCase],
    temp_root: &Path,
    timeout: Duration,
) -> Result<Vec<SystemOutput>, Error> {
    capture_candidates_with_input_method(
        engine,
        dictionary,
        cases,
        temp_root,
        timeout,
        CaptureInputMethod::Kana,
    )
}

fn capture_candidates_with_input_method(
    engine: &Path,
    dictionary: &Path,
    cases: &[SemanticCase],
    temp_root: &Path,
    timeout: Duration,
    input_method: CaptureInputMethod,
) -> Result<Vec<SystemOutput>, Error> {
    #[cfg(windows)]
    {
        windows::capture_candidates(engine, dictionary, cases, temp_root, timeout, input_method)
    }

    #[cfg(not(windows))]
    {
        let _ = (engine, dictionary, cases, temp_root, timeout, input_method);
        Err(crate::types::err(
            "real engine candidate capture is only supported on Windows",
        ))
    }
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
        InputScope, KeyCode, KeyInput, Mode, Modifiers, Request, Response, SessionId,
        PROTOCOL_VERSION,
    };

    use super::CaptureInputMethod;
    use crate::types::{err, Error, SemanticCase, SystemOutput};

    const PIPE_PREFIX: &str = r"\\.\pipe\SakuraInputEngineTest-";
    const CONNECT_SLICE: Duration = Duration::from_millis(100);
    const MAX_TYPING_BYTES: usize = 4096;
    // Generic semantic capture retains its historical bounded-file limit;
    // the production wire decoder itself is capped at 18. Quality capture
    // uses the explicit Stage 1/production limit below.
    const MAX_GENERIC_CANDIDATES: usize = 64;
    const MAX_QUALITY_CANDIDATES: usize = 18;
    const MAX_CANDIDATE_BYTES: usize = 4096;
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    pub(super) fn capture_candidates(
        engine: &Path,
        dictionary: &Path,
        cases: &[SemanticCase],
        temp_root: &Path,
        timeout: Duration,
        input_method: CaptureInputMethod,
    ) -> Result<Vec<SystemOutput>, Error> {
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
        result: Result<Vec<SystemOutput>, Error>,
        owned: &mut OwnedEngine,
    ) -> Result<Vec<SystemOutput>, Error> {
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
    ) -> Result<SystemOutput, Error> {
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
                    Response::Output(output) if output.consumed => {}
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
                let preedit = output
                    .preedit
                    .as_ref()
                    .map(|preedit| {
                        preedit
                            .segments
                            .iter()
                            .map(|segment| segment.text.as_str())
                            .collect::<String>()
                    })
                    .unwrap_or_default();
                err(format!(
                    "case {} produced no candidate list (consumed={}, beep={}, preedit={preedit:?}, commit={:?})",
                    case.case_id, output.consumed, output.beep, output.commit
                ))
            })?;
            if candidates.items.is_empty() {
                return Err(err(format!("case {} produced no candidates", case.case_id)));
            }
            let candidate_limit = match input_method {
                CaptureInputMethod::Romaji => MAX_GENERIC_CANDIDATES,
                CaptureInputMethod::Kana => MAX_QUALITY_CANDIDATES,
            };
            if candidates.items.len() > candidate_limit
                || candidates.items.iter().any(|candidate| {
                    candidate.text.is_empty() || candidate.text.len() > MAX_CANDIDATE_BYTES
                })
            {
                return Err(err(format!(
                    "case {} produced candidates outside capture bounds",
                    case.case_id
                )));
            }
            Ok(SystemOutput {
                candidates: candidates
                    .items
                    .into_iter()
                    .map(|candidate| candidate.text)
                    .collect(),
            })
        })();

        let _ = client.call(&Request::Revert { session }, timeout);
        let _ = client.call(&Request::DeleteSession { session }, timeout);
        result
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
