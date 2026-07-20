//! The dispatch loop and the handler seam it drives
//!
//! One message is read, dispatched, and answered before the next is read. The
//! loop owns message order, the shutdown sequence, and the panic boundary, and
//! hands everything else to a [`Handler`].

use std::any::Any;
use std::panic::{self, AssertUnwindSafe};
use std::time::Duration;

use lsp_server::{Connection, ErrorCode, Message, Notification, Request, RequestId, Response};

use crate::Error;

/// The request that asks the server to prepare to stop
const SHUTDOWN: &str = "shutdown";

/// The notification that ends the session
const EXIT: &str = "exit";

/// How long the loop waits for the `exit` that should follow a `shutdown`
const EXIT_TIMEOUT: Duration = Duration::from_secs(30);

/// How a session ended
///
/// The protocol asks for a different process exit code depending on whether the
/// client shut the server down before exiting, so the loop reports which of
/// them happened rather than only that the session is over
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// The client asked to shut down and then exited
    Clean,
    /// The client sent `exit` without asking to shut down first
    WithoutShutdown,
    /// The client stopped talking without finishing the sequence, which is what
    /// an editor that was killed rather than closed looks like
    Disconnected,
}

impl Exit {
    /// The process exit code the protocol asks for
    ///
    /// An exit that no shutdown preceded gets 1, so a client can tell an
    /// normal teardown from a server that came down under it.
    pub fn code(self) -> u8 {
        match self {
            Self::Clean | Self::Disconnected => 0,
            Self::WithoutShutdown => 1,
        }
    }
}

/// What the loop hands each message to
///
/// It sees whole messages, method name included since dispatching on the method
/// is the handler's job
pub trait Handler {
    /// Answer a request. The returned response is sent to the client as-is, so
    /// an implementation reports a per-request failure by returning an error
    /// response rather than by unwinding
    fn request(&mut self, request: Request) -> Response;

    /// React to a notification. Notifications carry no reply, so a failure here
    /// is only ever reported out of band
    fn notification(&mut self, notification: Notification);
}

/// A handler that refuses every request
///
/// The server declares no capabilities, so a conforming client never sends a
/// request that lands here
#[derive(Debug)]
pub struct MethodNotFound;

impl Handler for MethodNotFound {
    fn request(&mut self, request: Request) -> Response {
        Response::new_err(
            request.id,
            ErrorCode::MethodNotFound as i32,
            format!("{} is not supported", request.method),
        )
    }

    fn notification(&mut self, _notification: Notification) {}
}

/// Read messages until the session ends, dispatching each to `handler`
///
/// Returns how the session ended: the shutdown sequence, a bare exit, or a
/// client that stopped talking.
///
/// `shutdown` and `exit` are answered here rather than by the handler as they
/// belong to the protocol.
///
/// A handler that panics does not crash the server. The panic becomes an
/// [`ErrorCode::InternalError`] reply to the request that caused it and the
/// loop reads the next message.
///
/// # Errors
///
/// Returns an error if the client sends something other than `exit` after its
/// shutdown request, or if the connection drops while a reply is being sent
pub fn main_loop<H: Handler>(connection: &Connection, handler: &mut H) -> Result<Exit, Error> {
    for message in &connection.receiver {
        match message {
            // The reply goes out before the exit that should follow is waited
            // on, so a client blocked on it can proceed to send that exit
            Message::Request(request) if request.method == SHUTDOWN => {
                connection
                    .sender
                    .send(Message::Response(Response::new_ok(request.id, ())))?;
                return await_exit(connection);
            }
            Message::Request(request) => {
                // Kept because the request itself is moved into the handler and
                // a panic reply still has to name the request it answers
                let id = request.id.clone();
                let response = catch_panic(id, || handler.request(request));
                connection.sender.send(Message::Response(response))?;
            }
            // The stdio and socket readers also close the channel on an exit,
            // which would end the loop a message later
            Message::Notification(notification) if notification.method == EXIT => {
                return Ok(Exit::WithoutShutdown);
            }
            // The panic is caught only to avoid crashing the loop. The default
            // panic hook has already printed it to stderr
            Message::Notification(notification) => {
                let _ =
                    panic::catch_unwind(AssertUnwindSafe(|| handler.notification(notification)));
            }
            // The server sends no requests yet
            Message::Response(_) => {}
        }
    }
    Ok(Exit::Disconnected)
}

/// Wait for the exit that should follow the shutdown the loop just answered
///
/// # Errors
///
/// Returns an error if the client sends any other message, which is a broken
/// sequence rather than an ending
fn await_exit(connection: &Connection) -> Result<Exit, Error> {
    match connection.receiver.recv_timeout(EXIT_TIMEOUT) {
        Ok(Message::Notification(notification)) if notification.method == EXIT => Ok(Exit::Clean),
        Ok(message) => Err(format!("expected exit after shutdown, got {message:?}").into()),
        // The client either crashed or never got around to the exit. It holds its
        // shutdown reply either way, so this is the session ending rather than
        // the session failing
        Err(_) => Ok(Exit::Disconnected),
    }
}

/// Run `handle`, turning a panic into an error response for `id`
///
/// [`AssertUnwindSafe`] waives the unwind-safety check rather than satisfying
/// it, so nothing here keeps a handler's own state consistent across a panic.
/// The loop calls the handler again regardless, which holds only while the
/// handler is stateless. A handler that owns document state has to restore its
/// own invariants, because a panic can leave it mid-update
fn catch_panic(id: RequestId, handle: impl FnOnce() -> Response) -> Response {
    match panic::catch_unwind(AssertUnwindSafe(handle)) {
        Ok(response) => response,
        Err(payload) => Response::new_err(
            id,
            ErrorCode::InternalError as i32,
            format!("handler panicked: {}", panic_message(&*payload)),
        ),
    }
}

/// The message a panic carried, if it is one of the two shapes that survive
///
/// `panic!` boxes a `&'static str` for a literal and a `String` for a formatted
/// message. Anything else came from `panic_any` and has no text to recover
fn panic_message(payload: &(dyn Any + Send)) -> &str {
    if let Some(message) = payload.downcast_ref::<&'static str>() {
        message
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message
    } else {
        "no message"
    }
}
