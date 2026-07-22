//! The diagnostics feature: parse errors published as the buffer changes
//!
//! A buffer publishes the errors of the text the editor shows. A clean buffer
//! publishes an empty set, which clears whatever was there before, so the
//! client always shows the current text's errors and nothing stale.
//!
//! [`Server`](crate::Server) routes the sync notifications that call these

use lsp_server::{Message, Notification};
use lsp_types::notification::{Notification as _, PublishDiagnostics};
use lsp_types::{Diagnostic, DiagnosticSeverity, PublishDiagnosticsParams, Uri};

use crate::Document;

/// The source name on every diagnostic, so a client can group ours
const SOURCE: &str = "firepath";

/// The publishDiagnostics for a document's current parse errors
///
/// The version is carried so a client can drop a set it has already superseded
/// with a newer edit
pub(crate) fn publish(uri: Uri, document: &Document) -> Message {
    let diagnostics = document
        .errors()
        .iter()
        .map(|error| Diagnostic {
            range: document.range(error.span),
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some(SOURCE.to_owned()),
            message: error.message.clone(),
            ..Diagnostic::default()
        })
        .collect();
    into_message(PublishDiagnosticsParams {
        uri,
        diagnostics,
        version: Some(document.version()),
    })
}

/// A publishDiagnostics that clears a uri, sent when its buffer closes
pub(crate) fn clear(uri: Uri) -> Message {
    into_message(PublishDiagnosticsParams {
        uri,
        diagnostics: Vec::new(),
        version: None,
    })
}

/// Wrap publish params in the notification message the loop sends
fn into_message(params: PublishDiagnosticsParams) -> Message {
    Message::Notification(Notification::new(
        PublishDiagnostics::METHOD.to_owned(),
        params,
    ))
}
