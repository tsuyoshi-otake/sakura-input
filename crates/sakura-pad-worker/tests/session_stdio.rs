//! Real child-process protocol tests using synthetic content only.
use std::io::{Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use sakura_pad_worker::envelope::{open_with_password_v2, open_with_recovery_v2, RecoveryKey};
use sakura_pad_worker::session_protocol::{self, Operation, Request, Response, Status};
use sakura_pad_worker::{seal, Scope};
use zeroize::Zeroizing;

const VAULT: [u8; 16] = *b"session-vault-01";
const PASSWORD: &[u8] = b"synthetic session password";

struct OwnedChild {
    child: Child,
    input: Option<ChildStdin>,
    output: ChildStdout,
}

impl OwnedChild {
    fn spawn() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_sakura_pad_session"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn session child");
        let input = child.stdin.take().unwrap();
        let output = child.stdout.take().unwrap();
        Self {
            child,
            input: Some(input),
            output,
        }
    }

    fn exchange(&mut self, request: &Request) -> Response {
        let input = self.input.as_mut().unwrap();
        session_protocol::write_request(&mut *input, request).unwrap();
        input.flush().unwrap();
        session_protocol::read_response(&mut self.output).unwrap()
    }

    fn exit_within(&mut self, deadline: Duration) {
        let started = Instant::now();
        while self.child.try_wait().unwrap().is_none() {
            assert!(started.elapsed() < deadline, "session child did not exit");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn request(id: u64, generation: u64, operation: Operation, payload: Vec<u8>) -> Request {
    Request {
        id,
        generation,
        operation,
        vault_id: [0; 16],
        memo_id: 0,
        password: Zeroizing::new(Vec::new()),
        payload: Zeroizing::new(payload),
    }
}

fn unlock_request(id: u64, generation: u64, password: &[u8], envelope: Vec<u8>) -> Request {
    let mut request = request(id, generation, Operation::Unlock, envelope);
    request.vault_id = VAULT;
    request.password = Zeroizing::new(password.to_vec());
    request
}

fn create_request(id: u64, generation: u64, plaintext: &[u8]) -> Request {
    let mut request = request(id, generation, Operation::Create, plaintext.to_vec());
    request.vault_id = VAULT;
    request.password = Zeroizing::new(PASSWORD.to_vec());
    request
}

#[test]
fn recoverable_creation_returns_exactly_one_key_and_both_routes_reseal() {
    let mut child = OwnedChild::spawn();
    let mut create = create_request(1, 1, b"recoverable body");
    create.operation = Operation::CreateRecoverable;
    let created = child.exchange(&create);
    assert_eq!(created.status, Status::Success);
    let fields = session_protocol::parse_created_recovery(&created.payload).unwrap();
    let key = RecoveryKey::decode(fields.recovery_key).unwrap();
    let first_envelope = fields.envelope.to_vec();
    assert_eq!(
        &*open_with_recovery_v2(Scope::pad(VAULT), &key, &first_envelope).unwrap(),
        b"recoverable body"
    );
    let verified = child.exchange(&request(2, 1, Operation::Verify, first_envelope.clone()));
    assert_eq!(&*verified.payload, b"recoverable body");
    let resealed = child.exchange(&request(3, 1, Operation::Reseal, b"password edit".to_vec()));
    assert_eq!(resealed.status, Status::Success);
    assert_eq!(
        &*open_with_recovery_v2(Scope::pad(VAULT), &key, &resealed.payload).unwrap(),
        b"password edit"
    );
    assert_eq!(
        child
            .exchange(&request(4, 1, Operation::Lock, vec![]))
            .status,
        Status::Success
    );
    let mut recovery = unlock_request(
        5,
        2,
        fields.recovery_key.as_bytes(),
        resealed.payload.to_vec(),
    );
    recovery.operation = Operation::UnlockRecoveryV2;
    let unlocked = child.exchange(&recovery);
    assert_eq!(&*unlocked.payload, b"password edit");
    let recovery_reseal =
        child.exchange(&request(6, 2, Operation::Reseal, b"recovery edit".to_vec()));
    assert_eq!(recovery_reseal.status, Status::Success);
    assert_eq!(
        &*open_with_password_v2(Scope::pad(VAULT), PASSWORD, &recovery_reseal.payload).unwrap(),
        b"recovery edit"
    );
    assert_eq!(
        child
            .exchange(&request(7, 2, Operation::Lock, vec![]))
            .status,
        Status::Success
    );
    let mut password = unlock_request(8, 3, PASSWORD, recovery_reseal.payload.to_vec());
    password.operation = Operation::UnlockPasswordV2;
    assert_eq!(&*child.exchange(&password).payload, b"recovery edit");
    assert_eq!(
        child
            .exchange(&request(9, 3, Operation::Shutdown, vec![]))
            .status,
        Status::Success
    );
    child.exit_within(Duration::from_secs(2));
}

#[test]
fn rejected_recovery_unlock_is_terminal_for_its_generation() {
    let mut child = OwnedChild::spawn();
    let mut create = create_request(1, 1, b"private body");
    create.operation = Operation::CreateRecoverable;
    let created = child.exchange(&create);
    let envelope = session_protocol::parse_created_recovery(&created.payload)
        .unwrap()
        .envelope
        .to_vec();
    assert_eq!(
        child
            .exchange(&request(2, 1, Operation::Lock, vec![]))
            .status,
        Status::Success
    );
    let mut wrong = unlock_request(
        3,
        2,
        b"SPRK1-00000000-00000000-00000000-00000000-00000000-00000000-00000000-00000000",
        envelope.clone(),
    );
    wrong.operation = Operation::UnlockRecoveryV2;
    let failed = child.exchange(&wrong);
    assert_eq!(failed.status, Status::Rejected);
    assert!(failed.payload.is_empty());
    assert_eq!(
        child
            .exchange(&request(4, 2, Operation::Reseal, b"blocked".to_vec()))
            .status,
        Status::Locked
    );
    let mut stale = unlock_request(5, 2, PASSWORD, envelope.clone());
    stale.operation = Operation::UnlockPasswordV2;
    assert_eq!(child.exchange(&stale).status, Status::Stale);
    stale.id = 6;
    stale.generation = 3;
    assert_eq!(&*child.exchange(&stale).payload, b"private body");
    assert_eq!(
        child
            .exchange(&request(7, 3, Operation::Shutdown, vec![]))
            .status,
        Status::Success
    );
    child.exit_within(Duration::from_secs(2));
}

#[test]
fn create_enrolls_and_keeps_an_authenticated_session() {
    let mut child = OwnedChild::spawn();
    let created = child.exchange(&create_request(1, 3, b"new protected Pad"));
    assert_eq!(created.status, Status::Success);
    assert!(!created.payload.is_empty());
    let verified = child.exchange(&request(2, 3, Operation::Verify, created.payload.to_vec()));
    assert_eq!(verified.status, Status::Success);
    assert_eq!(&*verified.payload, b"new protected Pad");
    assert_eq!(
        child
            .exchange(&create_request(3, 4, b"second creation"))
            .status,
        Status::Stale
    );
    let resealed = child.exchange(&request(4, 3, Operation::Reseal, b"later edit".to_vec()));
    assert_eq!(resealed.status, Status::Success);
    assert_eq!(
        child
            .exchange(&request(5, 3, Operation::Shutdown, Vec::new()))
            .status,
        Status::Success
    );
    child.exit_within(Duration::from_secs(2));
}

#[test]
fn authenticated_session_reseals_twice_then_locks_and_rejects_stale_generation() {
    let envelope = seal(Scope::pad(VAULT), PASSWORD, b"initial").unwrap();
    let mut child = OwnedChild::spawn();
    let response = child.exchange(&unlock_request(1, 11, PASSWORD, envelope));
    assert_eq!(response.status, Status::Success);
    assert_eq!(&*response.payload, b"initial");

    let first = child.exchange(&request(2, 11, Operation::Reseal, b"first save".to_vec()));
    assert_eq!(first.status, Status::Success);
    let second = child.exchange(&request(3, 11, Operation::Reseal, b"second save".to_vec()));
    assert_eq!(second.status, Status::Success);
    assert_ne!(*first.payload, *second.payload);
    assert_eq!(
        child
            .exchange(&request(3, 11, Operation::Reseal, b"replay".to_vec()))
            .status,
        Status::Stale
    );
    assert_eq!(
        child
            .exchange(&request(4, 11, Operation::Lock, Vec::new()))
            .status,
        Status::Success
    );
    assert_eq!(
        child
            .exchange(&request(5, 11, Operation::Reseal, b"blocked".to_vec()))
            .status,
        Status::Locked
    );
    assert_eq!(
        child
            .exchange(&request(6, 10, Operation::Reseal, b"stale".to_vec()))
            .status,
        Status::Stale
    );
    assert_eq!(
        child
            .exchange(&unlock_request(7, 11, PASSWORD, second.payload.to_vec()))
            .status,
        Status::Stale
    );

    let reopened = child.exchange(&unlock_request(8, 12, PASSWORD, second.payload.to_vec()));
    assert_eq!(reopened.status, Status::Success);
    assert_eq!(&*reopened.payload, b"second save");
    assert_eq!(
        child
            .exchange(&request(9, 12, Operation::Shutdown, Vec::new()))
            .status,
        Status::Success
    );
    child.exit_within(Duration::from_secs(2));
    assert!(child.child.try_wait().unwrap().unwrap().success());
}

#[test]
fn wrong_password_has_no_plaintext_and_failed_generation_cannot_reseal() {
    let envelope = seal(Scope::pad(VAULT), PASSWORD, b"private fixture").unwrap();
    let mut child = OwnedChild::spawn();
    let failed = child.exchange(&unlock_request(1, 1, b"wrong", envelope));
    assert_eq!(failed.status, Status::Rejected);
    assert!(failed.payload.is_empty());
    let reseal = child.exchange(&request(2, 1, Operation::Reseal, b"new".to_vec()));
    assert_eq!(reseal.status, Status::Locked);
    let shutdown = child.exchange(&request(3, 1, Operation::Shutdown, Vec::new()));
    assert_eq!(shutdown.status, Status::Success);
    child.exit_within(Duration::from_secs(2));
}

#[test]
fn malformed_oversized_and_truncated_frames_fail_once_and_exit() {
    for frame in [b"invalid".to_vec(), u32::MAX.to_le_bytes().to_vec(), {
        let mut bytes = Vec::new();
        session_protocol::write_request(&mut bytes, &request(1, 1, Operation::Lock, Vec::new()))
            .unwrap();
        bytes.pop();
        bytes
    }] {
        let mut child = OwnedChild::spawn();
        child.input.as_mut().unwrap().write_all(&frame).unwrap();
        drop(child.input.take());
        let response = session_protocol::read_response(&mut child.output).unwrap();
        assert_eq!(response.status, Status::InvalidRequest);
        assert_eq!((response.id, response.generation), (0, 0));
        assert!(response.payload.is_empty());
        child.exit_within(Duration::from_secs(2));
        let mut extra = Vec::new();
        child.output.read_to_end(&mut extra).unwrap();
        assert!(extra.is_empty());
    }
}
