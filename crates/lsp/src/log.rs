//! Where the server says things that are not protocol
//!
//! stdout carries the protocol
//!
//! There is no level or filter. Everything written here is a client message the
//! server could not act on

use std::fmt::Arguments;

/// Report a client message the server dropped
///
/// A notification carries no reply, so a drop is invisible to the client and to
/// the user, who is left looking at diagnostics that no longer describe the
/// buffer. This line is the only trace it happened.
///
/// Takes `Arguments` so a caller passes `format_args!` and the message goes
/// straight to the stream without a `String` in between
pub(crate) fn dropped(what: Arguments) {
    eprintln!("firepath-lsp dropped {what}");
}
