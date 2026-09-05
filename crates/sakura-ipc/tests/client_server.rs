//! The two ends of the pipe, against each other.
//!
//! The unit tests in `transport` drive the server with a hand-rolled
//! client because they are testing the framing. These drive the real
//! [`Client`], because the behaviour worth testing is the part the server
//! knows nothing about: the deadline, and what happens to a reply that
//! misses it.

#![cfg(windows)]

use std::time::{Duration, Instant};

use sakura_ipc::{Client, Descriptor, Fault, PipeInstance, ServerTrustPolicy, PATIENT_CONNECT};
use sakura_proto::{decode_request, encode_response, peek_header, Request, Response};

/// A pipe name nobody else is using, derived from this process so two test
/// binaries running at once cannot collide.
fn scratch_name(tag: &str) -> String {
    format!(r"\\.\pipe\sakura_input_test_{tag}_{}", std::process::id())
}

/// Stands up one server instance and hands it to `serve` on its own
/// thread, so the test body can be the client.
fn with_server<F>(tag: &str, serve: F) -> (String, std::thread::JoinHandle<()>)
where
    F: FnOnce(PipeInstance) + Send + 'static,
{
    let name = scratch_name(tag);
    let security = Descriptor::for_pipe().expect("descriptor");
    let server = PipeInstance::create(&name, &security, true).expect("create");
    let handle = std::thread::spawn(move || {
        server.wait_for_client().expect("a client");
        serve(server);
    });
    (name, handle)
}

#[test]
fn a_request_gets_its_reply() {
    let (name, server) = with_server("call", |pipe| {
        let mut buf = Vec::new();
        let payload = pipe.read_frame(&mut buf).expect("a request");
        let (id, request) = decode_request(payload).expect("a decodable request");
        assert_eq!(request, Request::Ping);

        let mut reply = Vec::new();
        encode_response(&Response::Pong, id, &mut reply).expect("encode");
        pipe.write_all(&reply).expect("write");
    });

    let mut client = Client::connect_to(&name, PATIENT_CONNECT).expect("connect");
    let response = client
        .call(&Request::Ping, Duration::from_secs(5))
        .expect("a reply");
    assert_eq!(response, Response::Pong);

    drop(client);
    server.join().expect("the server thread");
}

#[test]
fn a_client_reports_the_process_serving_its_exact_pipe_connection() {
    let (accepted, ready) = std::sync::mpsc::channel();
    let (name, server) = with_server("server-process-id", move |pipe| {
        accepted.send(()).expect("tell test the pipe was accepted");
        let mut buffer = Vec::new();
        // The test client sends no protocol frame. Holding this accepted
        // instance open until the client drops makes the server PID query
        // describe the exact live connection, not a name lookup.
        let _ = pipe.read_frame(&mut buffer);
    });

    let client = Client::connect_to(&name, PATIENT_CONNECT).expect("connect");
    ready
        .recv_timeout(Duration::from_secs(5))
        .expect("server accepted the exact client connection");
    assert_eq!(
        client.server_process_id().expect("query exact server PID"),
        std::process::id()
    );

    drop(client);
    server.join().expect("the server thread");
}

#[test]
fn verified_connect_rejects_a_wrong_image_without_sending_hello() {
    let (name, server) = with_server("verified-wrong-image", |pipe| {
        let mut buffer = Vec::new();
        assert!(
            pipe.read_frame(&mut buffer).is_err(),
            "no protocol frame was sent"
        );
    });
    let policy = ServerTrustPolicy::Exact(
        std::env::current_exe()
            .expect("test image path")
            .with_file_name("not-the-engine.exe"),
    );
    assert!(matches!(
        Client::connect_verified_to(&name, &policy, PATIENT_CONNECT),
        Err(Fault::UntrustedServer { .. })
    ));
    server.join().expect("the server thread");
}

/// The property the whole overlapped-I/O apparatus exists for: an engine
/// that stops answering must cost the caller its budget, not its thread.
#[test]
fn partial_reply_deadline_cannot_leave_a_reusable_stream() {
    for prefix in [1, 4, 7] {
        let (name, server) = with_server(&format!("partial-deadline-{prefix}"), move |pipe| {
            let mut buf = Vec::new();
            let request = pipe.read_frame(&mut buf).expect("request");
            let id = peek_header(request).expect("header").request_id;
            let mut reply = Vec::new();
            encode_response(&Response::Pong, id, &mut reply).expect("reply");
            pipe.write_all(reply.get(..prefix).expect("frame prefix"))
                .expect("partial reply");
            std::thread::sleep(Duration::from_millis(120));
        });
        let mut client = Client::connect_to(&name, PATIENT_CONNECT).expect("connect");
        let result = client.call_until(&Request::Ping, Instant::now() + Duration::from_millis(30));
        drop(client);
        server.join().expect("scripted peer joined");
        assert!(
            matches!(result, Err(Fault::Desynchronized)),
            "partial prefix {prefix} must retire stream, got {result:?}"
        );
    }
}

#[test]
fn a_silent_engine_costs_the_budget_and_no_more() {
    let (name, server) = with_server("timeout", |pipe| {
        let mut buf = Vec::new();
        let _ = pipe.read_frame(&mut buf);
        // Read the request and never answer, then hold the connection open
        // long enough for the client to have given up on its own.
        std::thread::sleep(Duration::from_millis(600));
    });

    let mut client = Client::connect_to(&name, PATIENT_CONNECT).expect("connect");
    let budget = Duration::from_millis(50);
    let started = Instant::now();
    let result = client.call(&Request::Ping, budget);
    let waited = started.elapsed();

    assert!(
        matches!(result, Err(Fault::Timeout)),
        "expected a timeout, got {result:?}"
    );
    // Generous, because this asserts "the deadline was honoured at all",
    // not scheduler precision on a loaded machine. A blocking read would
    // have taken the server's full 600 ms.
    assert!(
        waited < Duration::from_millis(400),
        "waited {waited:?}, which means the deadline was not honoured"
    );

    drop(client);
    server.join().expect("the server thread");
}

/// The failure this protocol's request ids exist to prevent: a reply the
/// client stopped waiting for must never be handed back as the answer to
/// the next request. Applied to a composition, that is old text landing on
/// top of what the user has since typed.
#[test]
fn a_late_reply_is_discarded_rather_than_answered_with() {
    let (name, server) = with_server("stale", |pipe| {
        let mut buf = Vec::new();

        // The request the client will give up on.
        let first = pipe.read_frame(&mut buf).expect("first request");
        let first_id = peek_header(first).expect("header").request_id;

        // The one it actually waits for.
        let second = pipe.read_frame(&mut buf).expect("second request");
        let second_id = peek_header(second).expect("header").request_id;
        assert!(second_id > first_id);

        // Answered in order, so the stale reply is physically in front of
        // the one the client wants.
        let mut reply = Vec::new();
        encode_response(&Response::Ok, first_id, &mut reply).expect("encode");
        pipe.write_all(&reply).expect("write");
        encode_response(&Response::Pong, second_id, &mut reply).expect("encode");
        pipe.write_all(&reply).expect("write");
    });

    let mut client = Client::connect_to(&name, PATIENT_CONNECT).expect("connect");

    let abandoned = client.call(&Request::Ping, Duration::from_millis(30));
    assert!(
        matches!(abandoned, Err(Fault::Timeout)),
        "the first call must time out for this test to mean anything, got {abandoned:?}"
    );

    let response = client
        .call(&Request::Ping, Duration::from_secs(5))
        .expect("a reply");
    assert_eq!(
        response,
        Response::Pong,
        "the client returned the reply to the abandoned request"
    );

    drop(client);
    server.join().expect("the server thread");
}

/// A connection that outlives the engine reports the engine's death, and
/// does it as a disconnect rather than as an unexplained OS error, because
/// the DLL's recovery path keys off exactly that distinction.
#[test]
fn an_engine_that_vanishes_reads_as_a_disconnect() {
    let (name, server) = with_server("vanish", |pipe| {
        let mut buf = Vec::new();
        let _ = pipe.read_frame(&mut buf);
        drop(pipe);
    });

    let mut client = Client::connect_to(&name, PATIENT_CONNECT).expect("connect");
    let result = client.call(&Request::Ping, Duration::from_secs(5));
    assert!(
        matches!(result, Err(Fault::Disconnected)),
        "expected a disconnect, got {result:?}"
    );

    drop(client);
    server.join().expect("the server thread");
}
