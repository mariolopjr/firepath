//! A scripted client driving a whole server session
//!
//! [`Connection::memory`] serves the two ends of a real lsp-server connection
//! without the pipes, so the server runs on a thread and the test is the
//! client: it does the handshake, sends requests, reads replies, and shuts the
//! server down.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::panic;
use std::thread;

use firepath_lsp::{Exit, Handler, MethodNotFound, serve};
use lsp_server::{Connection, ErrorCode, Message, Notification, Request, RequestId, Response};
use serde_json::{Value, json};

const REPLY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

// The outcome of a session once the server thread has been joined
type Ended = Result<Exit, String>;

// A client end plus the handle of the server thread serving it
struct Session {
    client: Connection,
    server: thread::JoinHandle<Ended>,
}

// Starts the server on a thread without doing the handshake, so a test can
// script one that breaks.
//
// The error is flattened to a String so that a joined result can be compared
// and matched on
fn start_raw<H: Handler + Send + 'static>(
    mut handler: H,
) -> (Connection, thread::JoinHandle<Ended>) {
    let (server_end, client) = Connection::memory();
    let server =
        thread::spawn(move || serve(&server_end, &mut handler).map_err(|err| err.to_string()));
    (client, server)
}

// The initialize request every handshake opens with
fn initialize_request() -> Message {
    Message::Request(Request {
        id: RequestId::from(1),
        method: "initialize".to_owned(),
        params: json!({ "capabilities": {} }),
    })
}

impl Session {
    // Starts the server on a thread and completes the initialize handshake,
    // returning the session and what the server answered initialize with
    fn start<H: Handler + Send + 'static>(handler: H) -> (Self, Value) {
        let (client, server) = start_raw(handler);

        client.sender.send(initialize_request()).unwrap();
        let result = match client.receiver.recv_timeout(REPLY_TIMEOUT).unwrap() {
            Message::Response(response) => response.response_result.expect("initialize succeeded"),
            other => panic!("expected the initialize response, got {other:?}"),
        };
        client
            .sender
            .send(Message::Notification(Notification {
                method: "initialized".to_owned(),
                params: json!({}),
            }))
            .unwrap();

        (Self { client, server }, result)
    }

    fn request(&self, id: i32, method: &str) -> Response {
        self.client
            .sender
            .send(Message::Request(Request {
                id: RequestId::from(id),
                method: method.to_owned(),
                params: json!({}),
            }))
            .unwrap();
        match self.client.receiver.recv_timeout(REPLY_TIMEOUT).unwrap() {
            Message::Response(response) => response,
            other => panic!("expected a response, got {other:?}"),
        }
    }

    fn notify(&self, method: &str) {
        self.client
            .sender
            .send(Message::Notification(Notification {
                method: method.to_owned(),
                params: json!({}),
            }))
            .unwrap();
    }

    // The protocol's own exit: shutdown is answered, then exit ends the loop
    fn shutdown(self) {
        let response = self.request(9999, "shutdown");
        assert!(
            response.response_result.is_ok(),
            "shutdown failed: {response:?}"
        );
        self.notify("exit");
        let ended = self
            .server
            .join()
            .expect("the server thread did not panic")
            .expect("the session ended cleanly");
        // Only this order earns a zero exit code
        assert_eq!(ended, Exit::Clean);
    }
}

#[test]
fn initialize_handshake_reports_the_server_and_its_capabilities() {
    let (session, result) = Session::start(MethodNotFound);

    assert_eq!(
        result,
        json!({
            "capabilities": {},
            "serverInfo": {
                "name": "firepath-lsp",
                "version": env!("CARGO_PKG_VERSION"),
            },
        })
    );

    session.shutdown();
}

#[test]
fn an_unsupported_request_is_refused_rather_than_dropped() {
    let (session, _) = Session::start(MethodNotFound);

    let response = session.request(2, "textDocument/hover");
    let error = response.response_result.expect_err("an error reply");
    assert_eq!(error.code, ErrorCode::MethodNotFound as i32);
    assert!(error.message.contains("textDocument/hover"), "{error:?}");

    session.shutdown();
}

#[test]
fn an_unsupported_notification_is_ignored_rather_than_ending_the_session() {
    let (session, _) = Session::start(MethodNotFound);

    // Nothing comes back from a notification, so the only observable is that
    // the session is still there afterwards
    session.notify("textDocument/didSave");

    session.shutdown();
}

#[test]
fn a_response_the_server_never_asked_for_is_ignored() {
    let (session, _) = Session::start(MethodNotFound);

    // The server sends no requests yet, so nothing the client answers can match
    // one
    session
        .client
        .sender
        .send(Message::Response(Response::new_ok(
            RequestId::from(404),
            json!(null),
        )))
        .unwrap();

    session.shutdown();
}

// Panics on the methods it is asked to, in each of the shapes a panic payload
// can take, and answers anything else so a test can show the loop survived
struct Panicky;

impl Handler for Panicky {
    fn request(&mut self, request: Request) -> Response {
        match request.method.as_str() {
            // `panic!` with no arguments to format boxes the `&'static str`
            "panic/literal" => panic!("a literal panic"),
            // With arguments it formats and boxes a `String` instead
            "panic/formatted" => panic!("a formatted panic about {}", request.method),
            // Neither, so there is no message to recover
            "panic/opaque" => panic::panic_any(0_u8),
            _ => Response::new_ok(request.id, json!("alive")),
        }
    }

    fn notification(&mut self, notification: Notification) {
        assert_ne!(
            notification.method, "panic/now",
            "the notification handler exploded"
        );
    }
}

// The error reply a panicking request produced, asserted to be an internal error
fn panic_reply(session: &Session, id: i32, method: &str) -> String {
    let error = session
        .request(id, method)
        .response_result
        .expect_err("the panic became an error reply");
    assert_eq!(error.code, ErrorCode::InternalError as i32);
    error.message
}

#[test]
fn a_panicking_handler_yields_an_error_reply_and_the_server_keeps_serving() {
    let (session, _) = Session::start(Panicky);

    // The panic's own message is carried through, so a client log says what
    // broke rather than only that something did
    let message = panic_reply(&session, 2, "panic/literal");
    assert!(message.contains("a literal panic"), "{message}");

    let message = panic_reply(&session, 3, "panic/formatted");
    assert!(
        message.contains("a formatted panic about panic/formatted"),
        "{message}"
    );

    // A payload that is neither still has to produce a well-formed reply, since
    // the alternative is a request the client waits on forever
    let message = panic_reply(&session, 4, "panic/opaque");
    assert!(message.contains("no message"), "{message}");

    // After three panics the next request is still answered
    let response = session.request(5, "still/there");
    assert_eq!(response.response_result.ok(), Some(json!("alive")));

    session.shutdown();
}

#[test]
fn a_panicking_notification_handler_does_not_end_the_session() {
    let (session, _) = Session::start(Panicky);

    // Nothing comes back from a notification, so the only observable is that
    // the request after it is still answered
    session.notify("panic/now");

    let response = session.request(2, "still/there");
    assert_eq!(response.response_result.ok(), Some(json!("alive")));

    session.shutdown();
}

#[test]
fn a_client_that_disappears_ends_the_session_without_an_error() {
    let (session, _) = Session::start(MethodNotFound);
    let Session { client, server } = session;

    // An editor that crashed rather than closed: the pipes just close. That
    // is the end of the session, not a failure to report
    drop(client);

    assert_eq!(
        server.join().expect("the server thread did not panic"),
        Ok(Exit::Disconnected)
    );
}

// The error a session ended with, asserted to be a failure rather than a clean
// exit
fn session_error(server: thread::JoinHandle<Ended>) -> String {
    server
        .join()
        .expect("the server thread did not panic")
        .expect_err("the session ended with an error")
}

#[test]
fn a_first_message_that_is_not_a_request_fails_the_handshake() {
    let (client, server) = start_raw(MethodNotFound);

    // A request that is not initialize is refused and the handshake keeps
    // waiting, but a response cannot open a session at all
    client
        .sender
        .send(Message::Response(Response::new_ok(
            RequestId::from(1),
            json!(null),
        )))
        .unwrap();

    let error = session_error(server);
    assert!(error.contains("expected initialize request"), "{error}");
}

#[test]
fn a_handshake_that_skips_the_initialized_notification_fails() {
    let (client, server) = start_raw(MethodNotFound);

    client.sender.send(initialize_request()).unwrap();
    client.receiver.recv_timeout(REPLY_TIMEOUT).unwrap();

    // `initialized` has to be the next message. Anything else is a broken
    // handshake rather than something queued until it arrives, so a client that
    // opens a document too early loses both the document and the session
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didOpen".to_owned(),
            params: json!({}),
        }))
        .unwrap();

    let error = session_error(server);
    assert!(
        error.contains("expected initialized notification"),
        "{error}"
    );
}

#[test]
fn a_shutdown_that_is_not_followed_by_exit_ends_the_session_with_an_error() {
    let (session, _) = Session::start(MethodNotFound);
    let Session { client, server } = session;

    // Shutdown is answered before the rest of the sequence is checked, so the
    // reply lands even though the session is about to fail
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(2),
            method: "shutdown".to_owned(),
            params: json!({}),
        }))
        .unwrap();
    let response = client.receiver.recv_timeout(REPLY_TIMEOUT).unwrap();
    assert!(matches!(response, Message::Response(_)), "{response:?}");

    // Only exit may follow. This request is consumed by the shutdown sequence
    // and reported rather than answered, so a client that sends it waits on a
    // reply that never comes
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(3),
            method: "textDocument/hover".to_owned(),
            params: json!({}),
        }))
        .unwrap();

    let error = session_error(server);
    assert!(error.contains("expected exit after shutdown"), "{error}");
}

#[test]
fn a_notification_other_than_exit_after_shutdown_is_refused() {
    let (session, _) = Session::start(MethodNotFound);
    let Session { client, server } = session;

    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(2),
            method: "shutdown".to_owned(),
            params: json!({}),
        }))
        .unwrap();
    client.receiver.recv_timeout(REPLY_TIMEOUT).unwrap();

    // A notification gets this far on shape where a request does not, so this
    // is the case that decides the sequence on the method rather than on the
    // message being the wrong kind. A client still logging its way out is the
    // realistic version
    client
        .sender
        .send(Message::Notification(Notification {
            method: "$/logTrace".to_owned(),
            params: json!({}),
        }))
        .unwrap();

    let error = session_error(server);
    assert!(error.contains("expected exit after shutdown"), "{error}");
}

#[test]
fn a_client_that_disappears_after_shutdown_is_not_a_failure() {
    let (session, _) = Session::start(MethodNotFound);
    let Session { client, server } = session;

    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(2),
            method: "shutdown".to_owned(),
            params: json!({}),
        }))
        .unwrap();
    client.receiver.recv_timeout(REPLY_TIMEOUT).unwrap();

    // An editor that asked to shut down and then went away before sending exit,
    // which is what a force quit between the two looks like. It holds its
    // shutdown reply, so this is the session ending rather than a fault to
    // report against it
    drop(client);

    assert_eq!(
        server.join().expect("the server thread did not panic"),
        Ok(Exit::Disconnected)
    );
}

#[test]
fn a_bare_exit_notification_ends_the_loop_without_the_transport() {
    let (session, _) = Session::start(Panicky);
    let Session { client, server } = session;

    // A memory connection has no reader thread, so nothing closes the channel
    // on an exit the way the stdio and socket readers do. The loop still has to
    // stop here, and the handler must not see the notification: `Panicky`
    // answers anything it is asked, so a loop that kept going would hang the
    // join below rather than fail it
    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_owned(),
            params: json!({}),
        }))
        .unwrap();

    // An exit no shutdown preceded, which the protocol gives a nonzero code
    let ended = server.join().expect("the server thread did not panic");
    assert_eq!(ended, Ok(Exit::WithoutShutdown));
    assert_eq!(ended.unwrap().code(), 1);
}

#[test]
fn only_a_shutdown_before_the_exit_earns_a_zero_exit_code() {
    // The codes the CLI hands the editor, kept together so the split is one
    // place to read. `shutdown` then `exit` is asserted by `Session::shutdown`
    assert_eq!(Exit::Clean.code(), 0);
    assert_eq!(Exit::Disconnected.code(), 0);
    assert_eq!(Exit::WithoutShutdown.code(), 1);
}
