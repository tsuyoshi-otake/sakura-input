#![allow(clippy::expect_used, clippy::panic)]

use sakura_ipc::{Descriptor, PipeInstance};
use sakura_proto::{
    decode_request, encode_response, Mode, Output, Request, Response, UndoCommitOutcome,
};

fn empty_output() -> Output {
    Output {
        consumed: true,
        beep: false,
        mode: None,
        preedit: None,
        commit: None,
        delete_before: String::new(),
        candidates: None,
        candidate_detail: None,
    }
}

pub(crate) fn fake_engine_for_unknown_undo(tag: &str) -> (String, std::thread::JoinHandle<()>) {
    let name = format!(
        r"\\.\pipe\sakura_tsf_unknown_undo_{tag}_{}",
        std::process::id()
    );
    let security = Descriptor::for_pipe().expect("descriptor");
    let server = PipeInstance::create(&name, &security, true).expect("create");
    let handle = std::thread::spawn(move || {
        server.wait_for_client().expect("client");
        let mut buffer = Vec::new();

        let payload = server.read_frame(&mut buffer).expect("Hello request");
        let (id, request) = decode_request(payload).expect("decode Hello");
        assert!(matches!(request, Request::Hello { .. }));
        let mut reply = Vec::new();
        encode_response(
            &Response::Hello {
                server_version: sakura_proto::PROTOCOL_VERSION,
                engine_version: [0, 1, 0],
            },
            id,
            &mut reply,
        )
        .expect("encode Hello");
        server.write_all(&reply).expect("write Hello");

        let payload = server
            .read_frame(&mut buffer)
            .expect("CreateSession request");
        let (id, request) = decode_request(payload).expect("decode CreateSession");
        assert!(matches!(request, Request::CreateSession { .. }));
        reply.clear();
        encode_response(
            &Response::SessionCreated {
                session: 1,
                mode: Mode::Hiragana,
            },
            id,
            &mut reply,
        )
        .expect("encode CreateSession");
        server.write_all(&reply).expect("write CreateSession");

        let payload = server.read_frame(&mut buffer).expect("UndoCommit request");
        let (id, request) = decode_request(payload).expect("decode UndoCommit");
        assert!(matches!(
            request,
            Request::UndoCommit {
                outcome: UndoCommitOutcome::Unknown,
                ..
            }
        ));
        reply.clear();
        encode_response(&Response::Ok, id, &mut reply).expect("encode UndoCommit");
        server.write_all(&reply).expect("write UndoCommit");
    });
    (name, handle)
}

pub(crate) fn fake_engine_for_undo_timeout(tag: &str) -> (String, std::thread::JoinHandle<()>) {
    let name = format!(
        r"\\.\pipe\sakura_tsf_undo_timeout_{tag}_{}",
        std::process::id()
    );
    let security = Descriptor::for_pipe().expect("descriptor");
    let server = PipeInstance::create(&name, &security, true).expect("create");
    let handle = std::thread::spawn(move || {
        server.wait_for_client().expect("client");
        let mut buffer = Vec::new();

        let payload = server.read_frame(&mut buffer).expect("Hello request");
        let (id, request) = decode_request(payload).expect("decode Hello");
        assert!(matches!(request, Request::Hello { .. }));
        let mut reply = Vec::new();
        encode_response(
            &Response::Hello {
                server_version: sakura_proto::PROTOCOL_VERSION,
                engine_version: [0, 1, 0],
            },
            id,
            &mut reply,
        )
        .expect("encode Hello");
        server.write_all(&reply).expect("write Hello");

        let payload = server
            .read_frame(&mut buffer)
            .expect("CreateSession request");
        let (id, request) = decode_request(payload).expect("decode CreateSession");
        assert!(matches!(request, Request::CreateSession { .. }));
        reply.clear();
        encode_response(
            &Response::SessionCreated {
                session: 1,
                mode: Mode::Hiragana,
            },
            id,
            &mut reply,
        )
        .expect("encode CreateSession");
        server.write_all(&reply).expect("write CreateSession");

        let payload = server.read_frame(&mut buffer).expect("UndoCommit request");
        let (_id, request) = decode_request(payload).expect("decode UndoCommit");
        assert!(matches!(
            request,
            Request::UndoCommit {
                outcome: UndoCommitOutcome::Rejected,
                ..
            }
        ));
        // Let the fixed 50 ms engine budget expire. This is the one engine
        // failure mode that returns false while retaining a desynchronized
        // transport, which the early terminal helper must explicitly drop.
        std::thread::sleep(std::time::Duration::from_millis(100));
    });
    (name, handle)
}

pub(crate) fn fake_engine_with_no_further_requests(
    tag: &str,
) -> (String, std::thread::JoinHandle<()>) {
    let name = format!(r"\\.\pipe\sakura_tsf_recovery_{tag}_{}", std::process::id());
    let security = Descriptor::for_pipe().expect("descriptor");
    let server = PipeInstance::create(&name, &security, true).expect("create");
    let handle = std::thread::spawn(move || {
        server.wait_for_client().expect("client");
        let mut buffer = Vec::new();

        let payload = server.read_frame(&mut buffer).expect("Hello request");
        let (id, request) = decode_request(payload).expect("decode Hello");
        assert!(matches!(request, Request::Hello { .. }));
        let mut reply = Vec::new();
        encode_response(
            &Response::Hello {
                server_version: sakura_proto::PROTOCOL_VERSION,
                engine_version: [0, 1, 0],
            },
            id,
            &mut reply,
        )
        .expect("encode Hello");
        server.write_all(&reply).expect("write Hello");

        let payload = server
            .read_frame(&mut buffer)
            .expect("CreateSession request");
        let (id, request) = decode_request(payload).expect("decode CreateSession");
        assert!(matches!(request, Request::CreateSession { .. }));
        reply.clear();
        encode_response(
            &Response::SessionCreated {
                session: 1,
                mode: Mode::Hiragana,
            },
            id,
            &mut reply,
        )
        .expect("encode CreateSession");
        server.write_all(&reply).expect("write CreateSession");

        // A local encode failure never reaches the wire (`client.rs`
        // rejects it before the first `write_all`), so a healthy peer
        // modeling that scenario has nothing further to read or answer.
    });
    (name, handle)
}

pub(crate) fn fake_engine_for_reject_then_key(tag: &str) -> (String, std::thread::JoinHandle<()>) {
    let name = format!(
        r"\\.\pipe\sakura_tsf_reject_then_key_{tag}_{}",
        std::process::id()
    );
    let security = Descriptor::for_pipe().expect("descriptor");
    let server = PipeInstance::create(&name, &security, true).expect("create");
    let handle = std::thread::spawn(move || {
        server.wait_for_client().expect("client");
        let mut buffer = Vec::new();

        let payload = server.read_frame(&mut buffer).expect("Hello request");
        let (id, request) = decode_request(payload).expect("decode Hello");
        assert!(matches!(request, Request::Hello { .. }));
        let mut reply = Vec::new();
        encode_response(
            &Response::Hello {
                server_version: sakura_proto::PROTOCOL_VERSION,
                engine_version: [0, 1, 0],
            },
            id,
            &mut reply,
        )
        .expect("encode Hello");
        server.write_all(&reply).expect("write Hello");

        let payload = server
            .read_frame(&mut buffer)
            .expect("CreateSession request");
        let (id, request) = decode_request(payload).expect("decode CreateSession");
        assert!(matches!(request, Request::CreateSession { .. }));
        reply.clear();
        encode_response(
            &Response::SessionCreated {
                session: 1,
                mode: Mode::Hiragana,
            },
            id,
            &mut reply,
        )
        .expect("encode CreateSession");
        server.write_all(&reply).expect("write CreateSession");

        // The oversized `Reconvert` never reaches the wire -- encoding
        // fails inside `Client::call`, before any byte is written to
        // this pipe (`client.rs`). So the next, and only remaining,
        // request this peer ever sees is the verification key sent
        // right after it.
        let payload = server.read_frame(&mut buffer).expect("SendKey request");
        let (id, request) = decode_request(payload).expect("decode SendKey");
        assert!(matches!(request, Request::SendKey { .. }));
        reply.clear();
        encode_response(&Response::Output(empty_output()), id, &mut reply).expect("encode SendKey");
        server.write_all(&reply).expect("write SendKey");
    });
    (name, handle)
}
