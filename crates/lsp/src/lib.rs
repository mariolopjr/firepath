//! `firepath-lsp`: the language server for ledger journals
//!
//! The server is single-threaded and synchronous: one message is handled to
//! completion before the next is read, so a change notification is always
//! applied before the requests that follow it.
//!
//! [`serve`] runs the initialize handshake and then the dispatch loop against
//! any [`Connection`], so a test can drive a whole session over
//! [`Connection::memory`]. [`run_stdio`] is the same thing wired to the pipes
//! an editor starts the process with.
//!
//! [`Documents`] holds what the editor has open and maps between the byte
//! spans the parser reports and the UTF-16 positions the client speaks in.
//!
//! [`Diagnostics`] is the handler the loop runs: it owns the [`Documents`]
//! store, routes each sync notification into it, and publishes the parse
//! errors of the buffer that changed

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

mod diag;
mod docs;
mod log;
mod main_loop;

pub use diag::Diagnostics;
pub use docs::{Document, Documents};
pub use main_loop::{Exit, Handler, MethodNotFound, main_loop};

use std::time::{Duration, Instant};

use lsp_server::Connection;
use lsp_types::{
    InitializeResult, ServerCapabilities, ServerInfo, TextDocumentSyncCapability,
    TextDocumentSyncKind, TextDocumentSyncOptions,
};

/// Anything that ends a session other than the client asking it to
pub type Error = Box<dyn std::error::Error + Send + Sync>;

/// How long the handshake waits for the `initialized` that follows the
/// initialize reply
///
/// The client has already messaged by this point, so no response here
/// is a client that broke rather than one that has not started. If left
/// alone, a client that stop messaging without closing the pipe would
/// hold the process open forever
const INITIALIZED_TIMEOUT: Duration = Duration::from_secs(30);

/// What the server tells the client it can do
///
/// Sync is the only capability so far: the client sends the whole text on every
/// change, which is what [`Documents`] stores and re-parses.
fn capabilities() -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Options(
            TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(TextDocumentSyncKind::FULL),
                ..TextDocumentSyncOptions::default()
            },
        )),
        ..ServerCapabilities::default()
    }
}

/// Serve over stdin and stdout until the client shuts the server down
///
/// The editor starts the process and owns both pipes. Returns how the session
/// ended, which decides the process exit code
///
/// # Errors
///
/// Returns an error if the session ends any way other than the client asking
/// for it: a broken handshake, a broken shutdown sequence, or an IO thread that
/// failed on the pipes
pub fn run_stdio() -> Result<Exit, Error> {
    // Each pipe gets a thread that owns it
    let (connection, threads) = Connection::stdio();
    let result = serve(&connection, &mut Diagnostics::new());
    // Dropping the connection closes the channels the IO threads are parked on,
    // so it has to happen before the join or a failed session hangs here
    drop(connection);
    let joined = threads.join();
    // A session that failed usually takes the IO threads down with it, and of
    // the two errors the session's is the one that says what went wrong. The
    // join is only allowed to report when the session itself had nothing to say
    match (result, joined) {
        (Err(session), _) => Err(session),
        (Ok(_), Err(join)) => Err(join.into()),
        (Ok(exit), Ok(())) => Ok(exit),
    }
}

/// Run the initialize handshake and then the dispatch loop
///
/// Returns how the session ended. The initialize params are read and discarded
/// as nothing in the server is configurable by the client yet
///
/// # Errors
///
/// Returns an error if the handshake does not follow the protocol, if the
/// client leaves the handshake unfinished past [`INITIALIZED_TIMEOUT`], or if
/// the connection drops mid-session
pub fn serve<H: Handler>(connection: &Connection, handler: &mut H) -> Result<Exit, Error> {
    let (id, _params) = connection.initialize_start()?;
    let result = InitializeResult {
        capabilities: capabilities(),
        server_info: Some(ServerInfo {
            name: env!("CARGO_PKG_NAME").to_owned(),
            version: Some(env!("CARGO_PKG_VERSION").to_owned()),
        }),
    };
    // The plain `initialize_finish` waits for `initialized` without a bound.
    // `_while` polls a deadline instead, so a client that answered initialize
    // and then stops responding stops holding the process open. Measured as
    // elapsed rather than a deadline instant so the clock is never added to
    let started = Instant::now();
    connection.initialize_finish_while(id, serde_json::to_value(result)?, || {
        started.elapsed() < INITIALIZED_TIMEOUT
    })?;
    main_loop(connection, handler)
}
