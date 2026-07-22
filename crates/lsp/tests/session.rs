//! A scripted client driving a whole server session
//!
//! [`Connection::memory`] serves the two ends of a real lsp-server connection
//! without the pipes, so the server runs on a thread and the test is the
//! client: it does the handshake, sends requests, reads replies, and shuts the
//! server down.

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![cfg_attr(coverage_nightly, coverage(off))]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::panic;
use std::thread;

use firepath_lsp::{Exit, Handler, MethodNotFound, Server, serve};
use lsp_server::{Connection, ErrorCode, Message, Notification, Request, RequestId, Response};
use lsp_types::{
    DiagnosticSeverity, Position, PublishDiagnosticsParams, Range, SemanticToken, SemanticTokens,
};
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
        self.request_with(id, method, json!({}))
    }

    // Sends a request with a chosen body, for the requests whose params the
    // server reads
    fn request_with(&self, id: i32, method: &str, params: Value) -> Response {
        self.client
            .sender
            .send(Message::Request(Request {
                id: RequestId::from(id),
                method: method.to_owned(),
                params,
            }))
            .unwrap();
        match self.client.receiver.recv_timeout(REPLY_TIMEOUT).unwrap() {
            Message::Response(response) => response,
            other => panic!("expected a response, got {other:?}"),
        }
    }

    fn notify(&self, method: &str) {
        self.notify_with(method, json!({}));
    }

    // Sends a notification with a chosen body, for the sync notifications whose
    // params the diagnostics handler reads
    fn notify_with(&self, method: &str, params: Value) {
        self.client
            .sender
            .send(Message::Notification(Notification {
                method: method.to_owned(),
                params,
            }))
            .unwrap();
    }

    // The next message the server sent, which after a sync notification is the
    // publishDiagnostics it answered with
    fn recv(&self) -> Message {
        match self.client.receiver.recv_timeout(REPLY_TIMEOUT) {
            Ok(message) => message,
            Err(err) => panic!("expected a message, got {err:?}"),
        }
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

    // The legend is pinned here because a token names its type by index into
    // it: reordering it silently recolors every token the server sends
    assert_eq!(
        result,
        json!({
            "capabilities": {
                "textDocumentSync": {
                    "openClose": true,
                    "change": 1,
                },
                "semanticTokensProvider": {
                    "legend": {
                        "tokenTypes": [
                            "date", "payee", "account", "amount", "commodity", "comment",
                        ],
                        "tokenModifiers": [],
                    },
                    "full": true,
                },
            },
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

    fn notification(&mut self, notification: Notification) -> Vec<Message> {
        assert_ne!(
            notification.method, "panic/now",
            "the notification handler exploded"
        );
        Vec::new()
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

// The publishDiagnostics params the server pushed, failing if it pushed
// anything else
fn published(message: Message) -> PublishDiagnosticsParams {
    match message {
        Message::Notification(notification) => {
            assert_eq!(notification.method, "textDocument/publishDiagnostics");
            serde_json::from_value(notification.params).expect("publishDiagnostics params")
        }
        other => panic!("expected a publishDiagnostics notification, got {other:?}"),
    }
}

// A didOpen body for a document, the shape a client sends when a buffer opens
fn did_open(uri: &str, version: i32, text: &str) -> Value {
    json!({
        "textDocument": {
            "uri": uri,
            "languageId": "ledger",
            "version": version,
            "text": text,
        }
    })
}

// An indented posting with no transaction above it: the parser's separated indented error,
// spanning the whole line
const INDENTED: &str = "    Assets:Cash  $5\n";

#[test]
fn opening_a_document_with_a_parse_error_publishes_a_diagnostic_at_its_range() {
    let (session, _) = Session::start(Server::new());

    session.notify_with(
        "textDocument/didOpen",
        did_open("file:///corpus.ledger", 1, INDENTED),
    );

    let params = published(session.recv());
    assert_eq!(params.uri.as_str(), "file:///corpus.ledger");
    // The version the client stamped the buffer with, so a client can drop a
    // set a newer edit already superseded
    assert_eq!(params.version, Some(1));
    assert_eq!(params.diagnostics.len(), 1);
    let diagnostic = params.diagnostics.first().expect("one diagnostic");
    assert_eq!(
        diagnostic.range,
        Range::new(Position::new(0, 0), Position::new(0, 19))
    );
    assert_eq!(diagnostic.severity, Some(DiagnosticSeverity::ERROR));
    assert_eq!(diagnostic.source.as_deref(), Some("firepath"));
    assert!(!diagnostic.message.is_empty(), "{diagnostic:?}");

    session.shutdown();
}

#[test]
fn opening_a_clean_document_publishes_an_empty_set() {
    let (session, _) = Session::start(Server::new());

    session.notify_with(
        "textDocument/didOpen",
        did_open("file:///clean.ledger", 1, "; a comment\n"),
    );

    // A clean parse still publishes, with an empty set, so any earlier errors
    // for the uri are cleared rather than left behind
    let params = published(session.recv());
    assert!(params.diagnostics.is_empty(), "{params:?}");

    session.shutdown();
}

#[test]
fn a_change_republishes_diagnostics_for_the_new_text() {
    let (session, _) = Session::start(Server::new());

    session.notify_with(
        "textDocument/didOpen",
        did_open("file:///edit.ledger", 1, "; clean\n"),
    );
    assert!(published(session.recv()).diagnostics.is_empty());

    // Full sync: the change carries the whole new text, now with the orphan
    session.notify_with(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": "file:///edit.ledger", "version": 2 },
            "contentChanges": [{ "text": INDENTED }],
        }),
    );

    let params = published(session.recv());
    assert_eq!(params.version, Some(2));
    assert_eq!(params.diagnostics.len(), 1);

    session.shutdown();
}

#[test]
fn a_change_to_a_document_that_was_never_opened_publishes_nothing() {
    let (session, _) = Session::start(Server::new());

    // Under full sync there is no prior buffer to replace, so the change is
    // dropped without a publish
    session.notify_with(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": "file:///ghost.ledger", "version": 1 },
            "contentChanges": [{ "text": "; text\n" }],
        }),
    );

    // The next open's publish is the next message, which proves the change
    // above produced none
    session.notify_with(
        "textDocument/didOpen",
        did_open("file:///real.ledger", 1, "; clean\n"),
    );
    assert_eq!(
        published(session.recv()).uri.as_str(),
        "file:///real.ledger"
    );

    session.shutdown();
}

#[test]
fn a_change_carrying_no_content_publishes_nothing() {
    let (session, _) = Session::start(Server::new());

    session.notify_with(
        "textDocument/didOpen",
        did_open("file:///edit.ledger", 1, "; clean\n"),
    );
    assert!(published(session.recv()).diagnostics.is_empty());

    // An empty change list has no new text to apply, so it is dropped
    session.notify_with(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": "file:///edit.ledger", "version": 2 },
            "contentChanges": [],
        }),
    );

    // The next change carries text, and its publish is the next message, which
    // proves the empty one produced none
    session.notify_with(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": "file:///edit.ledger", "version": 3 },
            "contentChanges": [{ "text": INDENTED }],
        }),
    );
    let params = published(session.recv());
    assert_eq!(params.version, Some(3));
    assert_eq!(params.diagnostics.len(), 1);

    session.shutdown();
}

#[test]
fn closing_a_document_clears_its_diagnostics() {
    let (session, _) = Session::start(Server::new());

    session.notify_with(
        "textDocument/didOpen",
        did_open("file:///close.ledger", 1, INDENTED),
    );
    assert_eq!(published(session.recv()).diagnostics.len(), 1);

    // Closing reverts the uri to the file on disk, so its buffer diagnostics no
    // longer hold and are cleared with an empty set
    session.notify_with(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": "file:///close.ledger" } }),
    );
    let params = published(session.recv());
    assert_eq!(params.uri.as_str(), "file:///close.ledger");
    assert!(params.diagnostics.is_empty(), "{params:?}");

    session.shutdown();
}

#[test]
fn closing_a_document_that_was_never_opened_publishes_nothing() {
    let (session, _) = Session::start(Server::new());

    session.notify_with(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": "file:///ghost.ledger" } }),
    );

    // The next open's publish is the next message, which proves the close
    // produced none
    session.notify_with(
        "textDocument/didOpen",
        did_open("file:///real.ledger", 1, "; clean\n"),
    );
    assert_eq!(
        published(session.recv()).uri.as_str(),
        "file:///real.ledger"
    );

    session.shutdown();
}

#[test]
fn a_malformed_sync_notification_is_ignored_rather_than_ending_the_session() {
    let (session, _) = Session::start(Server::new());

    // A didOpen missing its textDocument cannot be read into params, so it is
    // dropped rather than answered and the session survives it
    session.notify_with("textDocument/didOpen", json!({}));

    session.notify_with(
        "textDocument/didOpen",
        did_open("file:///real.ledger", 1, "; clean\n"),
    );
    assert_eq!(
        published(session.recv()).uri.as_str(),
        "file:///real.ledger"
    );

    session.shutdown();
}

#[test]
fn an_unrelated_notification_is_dropped_by_the_server() {
    let (session, _) = Session::start(Server::new());

    // didSave is not a sync method the handler routes, so it is dropped and the
    // session is still there
    session.notify("textDocument/didSave");

    session.shutdown();
}

#[test]
fn a_request_the_server_has_no_capability_for_is_refused() {
    let (session, _) = Session::start(Server::new());

    // Hover is not advertised, so a request for it lands as unsupported rather
    // than being answered
    let response = session.request(2, "textDocument/hover");
    let error = response.response_result.expect_err("an error reply");
    assert_eq!(error.code, ErrorCode::MethodNotFound as i32);
    assert!(error.message.contains("textDocument/hover"), "{error:?}");

    session.shutdown();
}

// The tokens of a semanticTokens/full reply, failing if the server answered
// with anything else
fn semantic_tokens(response: Response) -> Vec<SemanticToken> {
    let result = response.response_result.expect("a semantic tokens reply");
    serde_json::from_value::<SemanticTokens>(result)
        .expect("semantic tokens")
        .data
}

// A semanticTokens/full body for a uri, the shape a client sends to color a
// buffer it has open
fn semantic_tokens_request(uri: &str) -> Value {
    json!({ "textDocument": { "uri": uri } })
}

#[test]
fn semantic_tokens_encode_the_constructs_of_the_open_buffer() {
    let (session, _) = Session::start(Server::new());

    session.notify_with(
        "textDocument/didOpen",
        did_open(
            "file:///tokens.ledger",
            1,
            "2020-01-02 * Grocery\n    Expenses:Food  $50.00\n",
        ),
    );
    published(session.recv());

    // Each token is a delta from the one before it: the line it moved down, the
    // character it starts at, its length, and its type as an index into the
    // legend the handshake published
    let tokens = semantic_tokens(session.request_with(
        2,
        "textDocument/semanticTokens/full",
        semantic_tokens_request("file:///tokens.ledger"),
    ));
    assert_eq!(
        tokens,
        vec![
            // `2020-01-02` and `Grocery` on the header line
            token(0, 0, 10, 0),
            token(0, 13, 7, 1),
            // `Expenses:Food`, then the `$` and the `50.00` it prefixes
            token(1, 4, 13, 2),
            token(0, 15, 1, 4),
            token(0, 1, 5, 3),
        ]
    );

    session.shutdown();
}

// One encoded token, in the order the protocol writes its fields
fn token(delta_line: u32, delta_start: u32, length: u32, token_type: u32) -> SemanticToken {
    SemanticToken {
        delta_line,
        delta_start,
        length,
        token_type,
        token_modifiers_bitset: 0,
    }
}

#[test]
fn semantic_tokens_follow_the_buffer_as_it_changes() {
    let (session, _) = Session::start(Server::new());

    session.notify_with(
        "textDocument/didOpen",
        did_open("file:///live.ledger", 1, "; a comment\n"),
    );
    published(session.recv());
    assert_eq!(
        semantic_tokens(session.request_with(
            2,
            "textDocument/semanticTokens/full",
            semantic_tokens_request("file:///live.ledger"),
        )),
        vec![token(0, 0, 11, 5)]
    );

    // The tokens come from the buffer, not the file, so an edit that has not
    // been saved still has to change them
    session.notify_with(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": "file:///live.ledger", "version": 2 },
            "contentChanges": [{ "text": "2020-01-02 Grocery\n" }],
        }),
    );
    published(session.recv());
    assert_eq!(
        semantic_tokens(session.request_with(
            3,
            "textDocument/semanticTokens/full",
            semantic_tokens_request("file:///live.ledger"),
        )),
        vec![token(0, 0, 10, 0), token(0, 11, 7, 1)]
    );

    session.shutdown();
}

#[test]
fn semantic_tokens_for_a_buffer_that_is_not_open_are_null() {
    let (session, _) = Session::start(Server::new());

    // Nothing is open, so there is no buffer to color. What is on disk is not
    // what the client is showing, so the protocol's null is the answer rather
    // than tokens taken from somewhere else
    let response = session.request_with(
        2,
        "textDocument/semanticTokens/full",
        semantic_tokens_request("file:///closed.ledger"),
    );
    assert_eq!(response.response_result.ok(), Some(json!(null)));

    session.shutdown();
}

#[test]
fn a_semantic_tokens_request_that_cannot_be_read_is_answered_with_an_error() {
    let (session, _) = Session::start(Server::new());

    // A request always carries a reply, so params that do not read are refused
    // rather than dropped the way a notification is: a client left waiting on a
    // reply that never comes is worse than one told what it got wrong
    let response = session.request(2, "textDocument/semanticTokens/full");
    let error = response.response_result.expect_err("an error reply");
    assert_eq!(error.code, ErrorCode::InvalidParams as i32);

    // The session survives it
    session.shutdown();
}

// The diagnostics of a publish as (range, message) pairs sorted by where they
// start, since the parse returns its errors in block order rather than source
// order and a set compared by position does not depend on which
fn by_position(params: &PublishDiagnosticsParams) -> Vec<(Range, &str)> {
    let mut located: Vec<(Range, &str)> = params
        .diagnostics
        .iter()
        .map(|diagnostic| (diagnostic.range, diagnostic.message.as_str()))
        .collect();
    located.sort_by_key(|(range, _)| (range.start.line, range.start.character));
    located
}

#[test]
fn every_parse_error_in_a_buffer_becomes_its_own_diagnostic() {
    let (session, _) = Session::start(Server::new());

    // A bad header date, a posting with no amount after its commodity, and a
    // posting whose amount is not one. The parse scans each block on its own
    // rather than stopping at the first failure, so all three have to reach the
    // client
    session.notify_with(
        "textDocument/didOpen",
        did_open(
            "file:///three.ledger",
            1,
            "2020-13-01 Grocery\n    Expenses:Food    $\n    Assets:Cash  @@@\n",
        ),
    );

    let params = published(session.recv());
    // Each lands on the line that holds it, so a diagnostic is not pinned to
    // the start of the file, and each carries its own message
    assert_eq!(
        by_position(&params),
        vec![
            (
                Range::new(Position::new(0, 0), Position::new(0, 10)),
                "2020-13-01 is not a real calendar date",
            ),
            (
                Range::new(Position::new(1, 21), Position::new(1, 22)),
                "expected a number",
            ),
            (
                Range::new(Position::new(2, 17), Position::new(2, 20)),
                "expected a commodity",
            ),
        ]
    );

    session.shutdown();
}

#[test]
fn a_diagnostic_character_counts_utf16_units_not_bytes() {
    let (session, _) = Session::start(Server::new());

    // The é before the end of the span is two bytes and one UTF-16 unit, so the
    // span ends at byte 20 and the range has to end at character 19. Handing
    // the byte offset over unmapped would underline one column past the line
    session.notify_with(
        "textDocument/didOpen",
        did_open("file:///accented.ledger", 1, "    Assets:Café  $5\n"),
    );

    let params = published(session.recv());
    assert_eq!(
        by_position(&params)
            .first()
            .map(|&(range, _)| range)
            .expect("one diagnostic"),
        Range::new(Position::new(0, 0), Position::new(0, 19))
    );

    session.shutdown();
}

#[test]
fn a_change_carrying_several_edits_publishes_the_last() {
    let (session, _) = Session::start(Server::new());

    session.notify_with(
        "textDocument/didOpen",
        did_open("file:///batched.ledger", 1, "; clean\n"),
    );
    assert!(published(session.recv()).diagnostics.is_empty());

    // Each change under full sync is a whole document, so a client that batches
    // several into one notification has each superseding the one before it.
    // Taking the first would publish the indented line error the second edit
    // already removed
    session.notify_with(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": "file:///batched.ledger", "version": 2 },
            "contentChanges": [{ "text": INDENTED }, { "text": "; clean again\n" }],
        }),
    );

    let params = published(session.recv());
    assert_eq!(params.version, Some(2));
    assert!(params.diagnostics.is_empty(), "{params:?}");

    session.shutdown();
}

#[test]
fn an_incremental_change_is_dropped_rather_than_stored_as_the_whole_buffer() {
    let (session, _) = Session::start(Server::new());

    session.notify_with(
        "textDocument/didOpen",
        did_open("file:///partial.ledger", 1, "; clean\n"),
    );
    assert!(published(session.recv()).diagnostics.is_empty());

    // A change carrying a range is one edit inside the buffer, not the buffer.
    // The server declared full sync, so this is a client that ignored the
    // capabilities, and taking its text as the whole document would leave the
    // store holding five characters the user never wrote alone on a line
    session.notify_with(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": "file:///partial.ledger", "version": 2 },
            "contentChanges": [{
                "range": {
                    "start": { "line": 0, "character": 2 },
                    "end": { "line": 0, "character": 7 },
                },
                "text": "dirty",
            }],
        }),
    );

    // The next whole-document change's publish is the next message, which
    // proves the incremental one produced none, and it lands on the version
    // that carried it rather than on anything the fragment left behind
    session.notify_with(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": "file:///partial.ledger", "version": 3 },
            "contentChanges": [{ "text": INDENTED }],
        }),
    );
    let params = published(session.recv());
    assert_eq!(params.version, Some(3));
    assert_eq!(params.diagnostics.len(), 1);

    session.shutdown();
}

#[test]
fn reopening_a_uri_publishes_from_the_text_the_second_open_carried() {
    let (session, _) = Session::start(Server::new());

    session.notify_with(
        "textDocument/didOpen",
        did_open("file:///reopened.ledger", 1, INDENTED),
    );
    assert_eq!(published(session.recv()).diagnostics.len(), 1);

    // A second open is the client resynchronizing, so its text replaces what
    // was there. Ignoring it would leave the client marking an error the buffer
    // it just sent does not have
    session.notify_with(
        "textDocument/didOpen",
        did_open("file:///reopened.ledger", 2, "; clean\n"),
    );

    let params = published(session.recv());
    assert_eq!(params.version, Some(2));
    assert!(params.diagnostics.is_empty(), "{params:?}");

    session.shutdown();
}

#[test]
fn each_open_buffer_keeps_its_own_diagnostics() {
    let (session, _) = Session::start(Server::new());

    session.notify_with(
        "textDocument/didOpen",
        did_open("file:///first.ledger", 1, INDENTED),
    );
    assert_eq!(published(session.recv()).diagnostics.len(), 1);

    session.notify_with(
        "textDocument/didOpen",
        did_open("file:///second.ledger", 1, "; clean\n"),
    );
    let params = published(session.recv());
    assert_eq!(params.uri.as_str(), "file:///second.ledger");
    assert!(params.diagnostics.is_empty(), "{params:?}");

    // Editing one buffer publishes for that uri alone
    session.notify_with(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": "file:///second.ledger", "version": 2 },
            "contentChanges": [{ "text": INDENTED }],
        }),
    );
    let params = published(session.recv());
    assert_eq!(params.uri.as_str(), "file:///second.ledger");
    assert_eq!(params.diagnostics.len(), 1);

    // The first is still open under its own uri: a change to a uri that is not
    // open publishes nothing, so this publish is what proves the second open
    // did not displace it
    session.notify_with(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": "file:///first.ledger", "version": 2 },
            "contentChanges": [{ "text": "; clean\n" }],
        }),
    );
    let params = published(session.recv());
    assert_eq!(params.uri.as_str(), "file:///first.ledger");
    assert!(params.diagnostics.is_empty(), "{params:?}");

    session.shutdown();
}

// Takes the client's receiving end away and then sends `message`, so the server
// handles it with nowhere to send what it produces. Returns how the session
// ended, the protocol's own shutdown no longer being reachable
fn send_to_a_client_that_stopped_reading(session: Session, message: Message) -> String {
    let Session { client, server } = session;
    let Connection { sender, receiver } = client;
    // Dropped before the send, so the server's sender is already disconnected
    // by the time it answers rather than racing the drop
    drop(receiver);
    sender.send(message).unwrap();
    server
        .join()
        .expect("the server thread did not panic")
        .expect_err("the session failed rather than ending")
}

// A client that stops reading is an editor that died mid-session. Every path
// that sends has to report that rather than block on a channel nobody drains

#[test]
fn a_push_to_a_client_that_stopped_reading_ends_the_session() {
    let (session, _) = Session::start(Server::new());

    let failure = send_to_a_client_that_stopped_reading(
        session,
        Message::Notification(Notification {
            method: "textDocument/didOpen".to_owned(),
            params: did_open("file:///gone.ledger", 1, INDENTED),
        }),
    );
    assert!(failure.contains("disconnected"), "{failure}");
}

#[test]
fn a_reply_to_a_client_that_stopped_reading_ends_the_session() {
    let (session, _) = Session::start(Server::new());

    let failure = send_to_a_client_that_stopped_reading(
        session,
        Message::Request(Request {
            id: RequestId::from(2),
            method: "textDocument/hover".to_owned(),
            params: json!({}),
        }),
    );
    assert!(failure.contains("disconnected"), "{failure}");
}

#[test]
fn a_shutdown_reply_to_a_client_that_stopped_reading_ends_the_session() {
    let (session, _) = Session::start(Server::new());

    // The shutdown reply goes out before the exit is waited on, so its send is
    // where a client that stopped reading is noticed
    let failure = send_to_a_client_that_stopped_reading(
        session,
        Message::Request(Request {
            id: RequestId::from(9999),
            method: "shutdown".to_owned(),
            params: json!({}),
        }),
    );
    assert!(failure.contains("disconnected"), "{failure}");
}
