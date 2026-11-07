use std::{
    io::{Read, Write},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

const REQUEST_MAGIC: u32 = 0x524e_4b53;
const RESPONSE_MAGIC: u32 = 0x534e_4b53;

fn push_candidate(payload: &mut Vec<u8>, fingerprint: u64, local_cost: i32, text: &str) {
    payload.extend_from_slice(&fingerprint.to_le_bytes());
    payload.extend_from_slice(&local_cost.to_le_bytes());
    payload.extend_from_slice(&(text.len() as u32).to_le_bytes());
    payload.extend_from_slice(text.as_bytes());
}

fn request() -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&REQUEST_MAGIC.to_le_bytes());
    payload.extend_from_slice(&1u16.to_le_bytes());
    payload.extend_from_slice(&0u16.to_le_bytes());
    payload.extend_from_slice(&77u64.to_le_bytes());
    payload.extend_from_slice(&0u32.to_le_bytes());
    payload.extend_from_slice(&2u32.to_le_bytes());
    push_candidate(&mut payload, 101, 0, "日本語");
    push_candidate(&mut payload, 202, 0, "日本人");
    let mut frame = Vec::with_capacity(payload.len() + 4);
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(&payload);
    frame
}

fn take<const N: usize>(input: &[u8], cursor: &mut usize) -> [u8; N] {
    let end = *cursor + N;
    let value = input[*cursor..end].try_into().unwrap();
    *cursor = end;
    value
}

#[test]
#[ignore = "requires SAKURA_NEURAL_WORKER and SAKURA_NEURAL_MODEL_DIR real artifacts"]
fn real_ort_model_scores_a_bounded_ipc_request() {
    let worker = std::env::var_os("SAKURA_NEURAL_WORKER").expect("worker path environment");
    let model = std::env::var_os("SAKURA_NEURAL_MODEL_DIR").expect("model path environment");
    let mut child = Command::new(worker)
        .arg("--stdio")
        .arg("--model-dir")
        .arg(model)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn real worker");
    let mut input = child.stdin.take().unwrap();
    let mut output = child.stdout.take().unwrap();
    input.write_all(&request()).unwrap();
    input.flush().unwrap();
    drop(input);

    let (send, receive) = mpsc::channel();
    let reader = thread::spawn(move || {
        let result = (|| {
            let mut length = [0u8; 4];
            output.read_exact(&mut length)?;
            let length = u32::from_le_bytes(length) as usize;
            if length > 32 * 1024 {
                return Err(std::io::Error::other("oversized response"));
            }
            let mut payload = vec![0u8; length];
            output.read_exact(&mut payload)?;
            Ok::<_, std::io::Error>(payload)
        })();
        let _ = send.send(result);
    });
    let payload = match receive.recv_timeout(Duration::from_secs(10)) {
        Ok(payload) => payload,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            panic!("real worker response timed out: {error}");
        }
    };
    // The reader owns stdout and must terminate before child cleanup.
    reader.join().unwrap();
    let payload = payload.unwrap();
    if child.try_wait().unwrap().is_none() {
        child.kill().unwrap();
    }
    child.wait().unwrap();

    let mut cursor = 0usize;
    assert_eq!(
        u32::from_le_bytes(take(&payload, &mut cursor)),
        RESPONSE_MAGIC
    );
    assert_eq!(u16::from_le_bytes(take(&payload, &mut cursor)), 1);
    assert_eq!(u16::from_le_bytes(take(&payload, &mut cursor)), 0);
    assert_eq!(u64::from_le_bytes(take(&payload, &mut cursor)), 77);
    assert_eq!(u16::from_le_bytes(take(&payload, &mut cursor)), 0);
    assert_eq!(u16::from_le_bytes(take(&payload, &mut cursor)), 0);
    assert_eq!(u32::from_le_bytes(take(&payload, &mut cursor)), 2);
    for expected_fingerprint in [101u64, 202] {
        assert_eq!(
            u64::from_le_bytes(take(&payload, &mut cursor)),
            expected_fingerprint
        );
        assert!(f32::from_bits(u32::from_le_bytes(take(&payload, &mut cursor))).is_finite());
    }
    assert_eq!(cursor, payload.len());
}

struct ParsedResponse {
    id: u64,
    status: u16,
    scores: Vec<(u64, f32)>,
}

fn parse_response(payload: &[u8]) -> ParsedResponse {
    let mut cursor = 0usize;
    assert_eq!(
        u32::from_le_bytes(take(payload, &mut cursor)),
        RESPONSE_MAGIC
    );
    assert_eq!(u16::from_le_bytes(take(payload, &mut cursor)), 1);
    let status = u16::from_le_bytes(take(payload, &mut cursor));
    let id = u64::from_le_bytes(take(payload, &mut cursor));
    let _legacy_tier = take::<2>(payload, &mut cursor);
    let _reserved = take::<2>(payload, &mut cursor);
    let count = u32::from_le_bytes(take(payload, &mut cursor)) as usize;
    let mut scores = Vec::with_capacity(count);
    for _ in 0..count {
        let fingerprint = u64::from_le_bytes(take(payload, &mut cursor));
        scores.push((
            fingerprint,
            f32::from_bits(u32::from_le_bytes(take(payload, &mut cursor))),
        ));
    }
    assert_eq!(cursor, payload.len(), "trailing response bytes");
    ParsedResponse { id, status, scores }
}

fn fixture_request(id: u64, case: &serde_json::Value) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&REQUEST_MAGIC.to_le_bytes());
    payload.extend_from_slice(&1u16.to_le_bytes());
    payload.extend_from_slice(&0u16.to_le_bytes());
    payload.extend_from_slice(&id.to_le_bytes());
    payload.extend_from_slice(&0u32.to_le_bytes());
    let candidates = case["candidates"].as_array().unwrap();
    payload.extend_from_slice(&(candidates.len() as u32).to_le_bytes());
    for candidate in candidates {
        push_candidate(
            &mut payload,
            candidate["fingerprint"].as_u64().unwrap(),
            i32::try_from(candidate["local_cost"].as_i64().unwrap()).unwrap(),
            candidate["text"].as_str().unwrap(),
        );
    }
    let mut frame = Vec::with_capacity(payload.len() + 4);
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(&payload);
    frame
}

#[test]
#[ignore = "requires SAKURA_NEURAL_WORKER and SAKURA_NEURAL_MODEL_DIR real artifacts"]
fn real_ort_model_matches_issue440_parity_fixture() {
    let worker = std::env::var_os("SAKURA_NEURAL_WORKER").expect("worker path environment");
    let model = std::env::var_os("SAKURA_NEURAL_MODEL_DIR").expect("model path environment");
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/issue440-parity.json"))
            .expect("issue440 parity fixture");
    assert_eq!(
        fixture["gate"]["score"].as_str().unwrap(),
        sakura_rerank_proto::Selection::RULE
    );
    assert_eq!(
        fixture["gate"]["tau"].as_f64().unwrap(),
        sakura_rerank_proto::Selection::LOCAL_OVERRIDE_MARGIN
    );
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len() as u64, fixture["case_count"].as_u64().unwrap());

    let mut child = Command::new(worker)
        .arg("--stdio")
        .arg("--model-dir")
        .arg(model)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn real worker");
    let mut input = child.stdin.take().unwrap();
    let mut output = child.stdout.take().unwrap();

    let (send, receive) = mpsc::channel();
    let reader = thread::spawn(move || loop {
        let result = (|| {
            let mut length = [0u8; 4];
            output.read_exact(&mut length)?;
            let length = u32::from_le_bytes(length) as usize;
            if length > 32 * 1024 {
                return Err(std::io::Error::other("oversized response"));
            }
            let mut payload = vec![0u8; length];
            output.read_exact(&mut payload)?;
            Ok::<_, std::io::Error>(payload)
        })();
        let terminal = result.is_err();
        if send.send(result).is_err() || terminal {
            break;
        }
    });

    // One worker process scores every case on sequential request ids; the
    // whole sweep stays bounded by a shared 30 s budget on top of the
    // per-request 10 s cap.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut failure = None;
    for (index, case) in cases.iter().enumerate() {
        let outcome = (|| -> Result<(), String> {
            let request_id = index as u64 + 1;
            input
                .write_all(&fixture_request(request_id, case))
                .and_then(|()| input.flush())
                .map_err(|error| format!("write request: {error}"))?;
            let budget = deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_secs(10));
            let payload = receive
                .recv_timeout(budget)
                .map_err(|error| format!("worker response timed out: {error}"))?
                .map_err(|error| format!("read worker response: {error}"))?;
            let response = parse_response(&payload);
            if response.id != request_id {
                return Err(format!("response id {} != {request_id}", response.id));
            }
            if response.status != 0 {
                return Err(format!("worker status {}", response.status));
            }
            let candidates = case["candidates"].as_array().unwrap();
            if response.scores.len() != candidates.len() {
                return Err(format!(
                    "score count {} != {} candidates",
                    response.scores.len(),
                    candidates.len()
                ));
            }
            let expected_scores = case["scores"].as_array().unwrap();
            let scores: Vec<f64> = candidates
                .iter()
                .map(|candidate| {
                    let fingerprint = candidate["fingerprint"].as_u64().unwrap();
                    response
                        .scores
                        .iter()
                        .find_map(|(key, value)| (*key == fingerprint).then_some(*value))
                        .map(f64::from)
                        .ok_or_else(|| format!("missing score for {fingerprint}"))
                })
                .collect::<Result<_, String>>()?;
            for (position, (actual, expected)) in scores
                .iter()
                .zip(expected_scores.iter().map(|value| value.as_f64().unwrap()))
                .enumerate()
            {
                if (*actual - expected).abs() > 1e-4 {
                    return Err(format!(
                        "score[{position}] {actual} differs from fixture {expected}"
                    ));
                }
            }
            let mut argmax = 0usize;
            for position in 1..scores.len() {
                if scores[position] > scores[argmax] {
                    argmax = position;
                }
            }
            let expected_argmax = case["argmax"].as_u64().unwrap() as usize;
            if argmax != expected_argmax {
                return Err(format!("argmax {argmax} != fixture {expected_argmax}"));
            }
            let gated = if argmax != 0
                && scores[argmax] - scores[0]
                    > sakura_rerank_proto::Selection::LOCAL_OVERRIDE_MARGIN
            {
                argmax
            } else {
                0
            };
            let expected_gated = case["gated_choice_with_local_first"].as_u64().unwrap() as usize;
            if gated != expected_gated {
                return Err(format!("gated choice {gated} != fixture {expected_gated}"));
            }
            Ok(())
        })();
        if let Err(error) = outcome {
            failure = Some(format!("case {index}: {error}"));
            break;
        }
        if Instant::now() >= deadline {
            failure = Some(format!("case {index}: 30 s fixture budget exhausted"));
            break;
        }
    }

    drop(input);
    if child.try_wait().unwrap().is_none() {
        let _ = child.kill();
    }
    let _ = child.wait();
    let _ = reader.join();
    if let Some(failure) = failure {
        panic!("{failure}");
    }
}
