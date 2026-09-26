//! Real child-process evidence; only synthetic bytes, no user profile/files.
use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use sakura_pad_worker::protocol::{self, Operation, Request, Response, Status};
use zeroize::Zeroizing;

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn invoke(bytes: &[u8]) -> Response {
    let mut child = OwnedChild(
        Command::new(env!("CARGO_BIN_EXE_sakura_pad_worker"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn owned worker"),
    );
    child.0.stdin.take().unwrap().write_all(bytes).unwrap(); // closes stdin
    let started = Instant::now();
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(status.success(), "worker terminal: {status}");
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(40),
            "worker deadline"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let response = protocol::read_response(child.0.stdout.take().unwrap()).unwrap();
    let mut stderr = String::new();
    std::io::Read::read_to_string(&mut child.0.stderr.take().unwrap(), &mut stderr).unwrap();
    assert!(
        stderr.is_empty(),
        "worker diagnostics must be empty on handled failure"
    );
    response
}

fn send(request: &Request) -> Response {
    let mut bytes = Zeroizing::new(Vec::new());
    protocol::write_request(&mut *bytes, request).unwrap();
    invoke(&bytes)
}

#[test]
fn subprocess_round_trip_does_not_accept_other_password_or_memo() {
    let mut request = Request {
        id: 91,
        operation: Operation::Seal,
        vault_id: [7; 16],
        memo_id: 19,
        password: Zeroizing::new(b"synthetic fixture password".to_vec()),
        payload: Zeroizing::new("テスト専用タイトル\n本文".as_bytes().to_vec()),
    };
    let sealed = send(&request);
    assert_eq!(sealed.id, 91);
    assert_eq!(sealed.status, Status::Success);
    assert!(!sealed
        .payload
        .windows(12)
        .any(|bytes| bytes == b"synthetic fi"));
    request.operation = Operation::Open;
    request.payload = sealed.payload;
    let opened = send(&request);
    assert_eq!(opened.status, Status::Success);
    assert_eq!(*opened.payload, "テスト専用タイトル\n本文".as_bytes());
    request.password = Zeroizing::new(b"different fixture password".to_vec());
    let refused = send(&request);
    assert_eq!(refused.id, 91);
    assert_eq!(refused.status, Status::Rejected);
    assert!(refused.payload.is_empty());
    request.memo_id = 20;
    let refused = send(&request);
    assert_eq!(refused.status, Status::Rejected);
    assert!(refused.payload.is_empty());
}

#[test]
fn malformed_input_has_exact_terminal_response_and_no_diagnostic_echo() {
    let response = invoke(b"invalid private synthetic input");
    assert_eq!(response.id, 0);
    assert_eq!(response.status, Status::InvalidRequest);
    assert!(response.payload.is_empty());
}

#[test]
fn parent_cancellation_reaps_a_worker_waiting_for_input() {
    let mut child = OwnedChild(
        Command::new(env!("CARGO_BIN_EXE_sakura_pad_worker"))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    child.0.stdin.as_mut().unwrap().write_all(b"SKR").unwrap();
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    assert!(child.0.try_wait().unwrap().is_some());
}
