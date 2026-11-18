//! Wire-level tests for the updater transport (#484).
//!
//! They drive the real WinHTTP calls against a scripted loopback server. The
//! URLs stay on the allowlisted names, so URL validation and redirects run as
//! in production; only the socket goes to the loopback port.

use super::*;
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

const FIRST_URL: &str = "https://github.com/first";
const REDIRECT_SAME_HOST: &str =
    "HTTP/1.1 302 Found\r\nLocation: https://github.com/next\r\nContent-Length: 0\r\n\r\n";
const REDIRECT_TO_ASSETS: &str = "HTTP/1.1 302 Found\r\nLocation: https://release-assets.githubusercontent.com/asset\r\nContent-Length: 0\r\n\r\n";
const OK_MANIFEST: &str = "HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\nmanifest";
/// Announces two read buffers of body; `Script::ResetMidBody` sends one.
const OK_TWO_BUFFERS_HEAD: &str = "HTTP/1.1 200 OK\r\nContent-Length: 131072\r\n\r\n";
const NOT_HTTP: &str = "this is not an HTTP response\r\n\r\n";
const SERVER_ERROR: &str = "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n";
const RATE_LIMITED: &str =
    "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 60\r\nContent-Length: 0\r\n\r\n";
const GENEROUS: Duration = Duration::from_secs(60);

/// What the server does with one accepted connection.
#[derive(Clone, Copy)]
enum Script {
    /// Read the request, answer, and close.
    Reply(&'static str),
    /// Read the request, answer, hold the connection open without reading for
    /// the given time, and then close it. A client that reuses the connection
    /// meanwhile gets a reset instead of an answer.
    ReplyThenLinger(&'static str, Duration),
    /// Answer with half of the announced body, wait until the client has
    /// consumed body bytes, and then reset the connection.
    ResetMidBody,
    /// Close with the request unread, which resets the connection before any
    /// response byte is sent.
    Reset,
}

/// Serves `scripts[n]` to the n-th accepted connection, one at a time.
struct Server {
    port: u16,
    stop: Arc<AtomicBool>,
    accepted: Arc<AtomicUsize>,
    /// Set by the test once the transport has handed body bytes to its
    /// consumer.
    body_seen: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Server {
    fn start(scripts: &[Script]) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback");
        let port = listener.local_addr().expect("loopback address").port();
        listener
            .set_nonblocking(true)
            .expect("non-blocking accept so the worker can stop");
        let stop = Arc::new(AtomicBool::new(false));
        let accepted = Arc::new(AtomicUsize::new(0));
        let body_seen = Arc::new(AtomicBool::new(false));
        let scripts = scripts.to_vec();
        let worker = thread::spawn({
            let stop = Arc::clone(&stop);
            let accepted = Arc::clone(&accepted);
            let body_seen = Arc::clone(&body_seen);
            move || {
                while !stop.load(Ordering::SeqCst) {
                    let stream = match listener.accept() {
                        Ok((stream, _)) => stream,
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(1));
                            continue;
                        }
                        Err(_) => return,
                    };
                    let index = accepted.fetch_add(1, Ordering::SeqCst);
                    // A connection beyond the script is reset, so a client
                    // that connects more often than a test expects fails it.
                    let script = scripts.get(index).copied().unwrap_or(Script::Reset);
                    serve(stream, script, &body_seen);
                }
            }
        });
        Self {
            port,
            stop,
            accepted,
            body_seen,
            worker: Some(worker),
        }
    }

    /// Stops the worker and returns how many connections it accepted.
    fn finish(mut self) -> usize {
        self.join();
        self.accepted.load(Ordering::SeqCst)
    }

    fn join(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            worker.join().expect("loopback server worker");
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Also reached when the transport call panics, so no worker outlives
        // its test.
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn serve(mut stream: TcpStream, script: Script, body_seen: &AtomicBool) {
    // Windows hands the listener's non-blocking mode to accepted sockets.
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    match script {
        Script::Reply(response) => {
            read_request(&mut stream);
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.shutdown(Shutdown::Write);
        }
        Script::ReplyThenLinger(response, linger) => {
            read_request(&mut stream);
            let _ = stream.write_all(response.as_bytes());
            thread::sleep(linger);
        }
        Script::ResetMidBody => {
            // As in `Reset`, the unread rest of the request turns the close
            // into a reset. A synchronous WinHTTP read returns only once its
            // buffer is full or the body ends, so the body sent here fills
            // one buffer. Waiting for the consumer then keeps the reset from
            // overtaking the response it follows.
            let _ = stream.read(&mut [0u8; 1]);
            let _ = stream.write_all(OK_TWO_BUFFERS_HEAD.as_bytes());
            let _ = stream.write_all(&[b'x'; HTTP_BUFFER_BYTES]);
            let waiting_since = Instant::now();
            while !body_seen.load(Ordering::SeqCst)
                && waiting_since.elapsed() < Duration::from_secs(5)
            {
                thread::sleep(Duration::from_millis(1));
            }
        }
        Script::Reset => {
            // One byte proves the request arrived; closing with the rest
            // unread makes Windows send a reset instead of a clean close.
            let _ = stream.read(&mut [0u8; 1]);
        }
    }
}

fn read_request(stream: &mut TcpStream) {
    let mut request = Vec::new();
    let mut chunk = [0u8; 1024];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(read) => request.extend_from_slice(&chunk[..read]),
        }
    }
}

struct Fetch {
    result: Result<Vec<u8>, String>,
    /// Every byte the transport handed to its consumer, also on failure.
    consumed: Vec<u8>,
    /// Waits the transport asked for. The test records them instead of
    /// sleeping, so no assertion depends on the wall clock.
    pauses: Vec<Duration>,
    connections: usize,
}

fn fetch(scripts: &[Script], total_timeout: Duration) -> Fetch {
    let server = Server::start(scripts);
    let mut pauses = Vec::new();
    let mut body = Vec::new();
    let outcome = stream_http_get_on(
        Wire::PlainLoopback(server.port),
        FIRST_URL,
        MAX_MANIFEST_BYTES,
        total_timeout,
        &mut |delay| pauses.push(delay),
        |chunk| {
            body.extend_from_slice(chunk);
            server.body_seen.store(true, Ordering::SeqCst);
            Ok(())
        },
    );
    Fetch {
        result: outcome.map(|_| body.clone()),
        consumed: body,
        pauses,
        connections: server.finish(),
    }
}

fn milliseconds(low: u64, high: u64) -> std::ops::RangeInclusive<Duration> {
    Duration::from_millis(low)..=Duration::from_millis(high)
}

#[test]
fn a_hop_does_not_reuse_the_connection_of_the_hop_before_it() {
    // The server closes the first connection 20 ms after answering. A second
    // hop sent on that connection would be reset; WinHTTP does not reconnect.
    let fetch = fetch(
        &[
            Script::ReplyThenLinger(REDIRECT_SAME_HOST, Duration::from_millis(20)),
            Script::Reply(OK_MANIFEST),
        ],
        GENEROUS,
    );

    assert_eq!(fetch.result.as_deref(), Ok(&b"manifest"[..]));
    assert_eq!(
        fetch.pauses,
        [],
        "the second hop must succeed without a retry"
    );
    assert_eq!(fetch.connections, 2);
}

#[test]
fn a_connection_dropped_before_the_response_is_retried_after_a_jittered_wait() {
    let fetch = fetch(&[Script::Reset, Script::Reply(OK_MANIFEST)], GENEROUS);

    assert_eq!(fetch.result.as_deref(), Ok(&b"manifest"[..]));
    assert_eq!(fetch.pauses.len(), 1);
    assert!(
        milliseconds(250, 500).contains(&fetch.pauses[0]),
        "first wait was {:?}",
        fetch.pauses[0]
    );
    assert_eq!(fetch.connections, 2);
}

#[test]
fn repeated_drops_stop_at_the_attempt_limit_and_name_the_failing_host() {
    let fetch = fetch(
        &[
            Script::Reply(REDIRECT_TO_ASSETS),
            Script::Reset,
            Script::Reset,
            Script::Reset,
        ],
        GENEROUS,
    );

    let error = fetch
        .result
        .expect_err("three dropped attempts are terminal");
    assert!(
        error.starts_with("release-assets.githubusercontent.com: WinHttp"),
        "{error}"
    );
    assert!(error.ends_with("(3 attempts)"), "{error}");
    assert_eq!(fetch.pauses.len(), 2);
    assert!(milliseconds(250, 500).contains(&fetch.pauses[0]));
    assert!(
        milliseconds(750, 1500).contains(&fetch.pauses[1]),
        "second wait was {:?}",
        fetch.pauses[1]
    );
    assert_eq!(
        fetch.connections, 4,
        "one redirect and exactly three attempts"
    );
}

#[test]
fn an_http_error_status_is_terminal_without_a_retry() {
    let fetch = fetch(&[Script::Reply(SERVER_ERROR)], GENEROUS);

    assert_eq!(
        fetch.result,
        Err("github.com returned HTTP status 500".to_owned())
    );
    assert_eq!(fetch.pauses, []);
    assert_eq!(fetch.connections, 1);
}

#[test]
fn a_rate_limit_is_terminal_without_a_retry() {
    let fetch = fetch(&[Script::Reply(RATE_LIMITED)], GENEROUS);

    let error = fetch.result.expect_err("429 is terminal");
    assert!(error.contains("Retry-After: 60"), "{error}");
    assert_eq!(fetch.pauses, []);
    assert_eq!(fetch.connections, 1);
}

#[test]
fn a_failure_that_is_not_a_dropped_connection_is_terminal_without_a_retry() {
    let fetch = fetch(&[Script::Reply(NOT_HTTP)], GENEROUS);

    let error = fetch.result.expect_err("a malformed response is terminal");
    assert!(
        error.starts_with("github.com: WinHttpReceiveResponse failed:"),
        "{error}"
    );
    assert!(!error.contains("attempt"), "{error}");
    assert_eq!(fetch.pauses, []);
    assert_eq!(fetch.connections, 1);
}

#[test]
fn a_drop_after_the_response_started_is_terminal_without_a_retry() {
    let fetch = fetch(&[Script::ResetMidBody], GENEROUS);

    let error = fetch.result.expect_err("a torn body is terminal");
    assert!(error.starts_with("WinHttpReadData failed:"), "{error}");
    assert!(
        (1..=HTTP_BUFFER_BYTES).contains(&fetch.consumed.len()),
        "the consumer saw {} bytes of the {HTTP_BUFFER_BYTES} sent once",
        fetch.consumed.len()
    );
    assert_eq!(fetch.pauses, []);
    assert_eq!(fetch.connections, 1);
}

#[test]
fn a_drop_is_not_retried_when_the_deadline_is_closer_than_the_wait() {
    // The shortest first wait is 250 ms, so a 200 ms deadline never has room.
    let fetch = fetch(&[Script::Reset], Duration::from_millis(200));

    let error = fetch.result.expect_err("no time to retry");
    assert!(error.starts_with("github.com: WinHttp"), "{error}");
    assert!(
        error.ends_with("(attempt 1 of 3; the total deadline left no time to retry)"),
        "{error}"
    );
    assert_eq!(fetch.pauses, []);
    assert_eq!(fetch.connections, 1);
}

#[test]
fn the_retry_wait_triples_and_keeps_its_random_part_in_the_upper_half() {
    for entropy in [0, 1, 249, 250, 251, 750, 751, u64::MAX] {
        let first = retry_delay(1, entropy);
        let second = retry_delay(2, entropy);
        assert!(
            milliseconds(250, 500).contains(&first),
            "{entropy}: {first:?}"
        );
        assert!(
            milliseconds(750, 1500).contains(&second),
            "{entropy}: {second:?}"
        );
    }
    assert_eq!(retry_delay(1, 0), Duration::from_millis(250));
    assert_eq!(retry_delay(1, 250), Duration::from_millis(500));
    assert_eq!(retry_delay(2, 750), Duration::from_millis(1500));
}
