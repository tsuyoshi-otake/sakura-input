use std::{
    io::{Read, Write},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

const REQUEST_MAGIC: u32 = 0x524e_4b53;
const RESPONSE_MAGIC: u32 = 0x534e_4b53;

fn push_candidate(payload: &mut Vec<u8>, fingerprint: u64, text: &str) {
    payload.extend_from_slice(&fingerprint.to_le_bytes());
    payload.extend_from_slice(&0u32.to_le_bytes());
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
    push_candidate(&mut payload, 101, "日本語");
    push_candidate(&mut payload, 202, "日本人");
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
    let _tier = u16::from_le_bytes(take(&payload, &mut cursor));
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
