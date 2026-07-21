//! The diagnostics feature: parse errors published as the buffer changes
//!
//! The handler owns the [`Documents`] store and answers each sync notification
//! by re-parsing the buffer it touched and publishing that buffer's errors. A
//! clean buffer publishes an empty set, which clears whatever was there before,
//! so the client always shows the current text's errors and nothing stale.
//!
//! The store is left consistent across a panic by construction: an open or a
//! change builds the whole [`Document`], parsing included, before it touches
//! the map, so a panic while parsing leaves the previous buffer in place rather
//! than a half-updated one

use lsp_server::{Message, Notification, Request, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification as _,
    PublishDiagnostics,
};
use lsp_types::{
    Diagnostic, DiagnosticSeverity, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, PublishDiagnosticsParams, TextDocumentItem, Uri,
    VersionedTextDocumentIdentifier,
};

use crate::log;
use crate::main_loop::method_not_found;
use crate::{Document, Documents, Handler};

/// The source name on every diagnostic, so a client can group ours
const SOURCE: &str = "firepath";

/// The diagnostics handler: the open buffers plus the errors parsing found
#[derive(Debug, Default)]
pub struct Diagnostics {
    /// What the editor has open, laid over the files on disk
    docs: Documents,
}

impl Diagnostics {
    /// A handler with nothing open
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse the opened buffer and publish its diagnostics
    fn opened(&mut self, params: DidOpenTextDocumentParams) -> Vec<Message> {
        let TextDocumentItem {
            uri, version, text, ..
        } = params.text_document;
        let document = self.docs.open(uri.clone(), version, text);
        vec![publish(uri, document)]
    }

    /// Re-parse the changed buffer and publish its diagnostics
    ///
    /// Full sync sends the whole new text, so the last content change is the
    /// buffer's next state: an earlier one in the same notification is text a
    /// later one already replaced. A change this server cannot install is
    /// dropped rather than guessed at, and the buffer keeps the text it had
    fn changed(&mut self, mut params: DidChangeTextDocumentParams) -> Vec<Message> {
        let VersionedTextDocumentIdentifier { uri, version } = params.text_document;
        let Some(change) = params.content_changes.pop() else {
            log::dropped(format_args!(
                "a change to {}: it carried no text",
                uri.as_str()
            ));
            return Vec::new();
        };
        // A change carrying a range is one edit inside the buffer rather than
        // the whole buffer
        if change.range.is_some() {
            log::dropped(format_args!(
                "a change to {}: it is incremental and the server declared full sync",
                uri.as_str()
            ));
            return Vec::new();
        }
        let Some(document) = self.docs.change(&uri, version, change.text) else {
            log::dropped(format_args!(
                "a change to {}: it was never opened",
                uri.as_str()
            ));
            return Vec::new();
        };
        vec![publish(uri, document)]
    }

    /// Drop the closed buffer and clear its diagnostics
    ///
    /// A closed buffer reverts to the file on disk, so the errors parsed from
    /// the buffer no longer hold and an empty set clears them. A close of a uri
    /// that was never open clears nothing
    fn closed(&mut self, params: DidCloseTextDocumentParams) -> Vec<Message> {
        let uri = params.text_document.uri;
        if self.docs.close(&uri) {
            vec![clear(uri)]
        } else {
            log::dropped(format_args!(
                "a close of {}: it was never opened",
                uri.as_str()
            ));
            Vec::new()
        }
    }
}

/// The params of a notification this server routes, reporting one it cannot read
///
/// Handed the `from_value` result rather than the raw value so no
/// `DeserializeOwned` bound is named here and the crate keeps needing no direct
/// `serde` dependency. Every routed method funnels through this one site, so
/// the drop is reported once rather than per method
fn read_params<T>(method: &str, params: Result<T, serde_json::Error>) -> Option<T> {
    match params {
        Ok(params) => Some(params),
        Err(error) => {
            log::dropped(format_args!("{method}: {error}"));
            None
        }
    }
}

impl Handler for Diagnostics {
    fn request(&mut self, request: Request) -> Response {
        // No request capability is advertised, so any request is unsupported
        method_not_found(request)
    }

    fn notification(&mut self, notification: Notification) -> Vec<Message> {
        let Notification { method, params } = notification;
        // A sync notification carries no reply, so the loop survives either way.
        // One this server does not route is not its business and goes quietly;
        // one it routes but cannot read is a message it should have acted on,
        // so that one is reported
        match method.as_str() {
            DidOpenTextDocument::METHOD => {
                read_params(&method, serde_json::from_value(params)).map(|p| self.opened(p))
            }
            DidChangeTextDocument::METHOD => {
                read_params(&method, serde_json::from_value(params)).map(|p| self.changed(p))
            }
            DidCloseTextDocument::METHOD => {
                read_params(&method, serde_json::from_value(params)).map(|p| self.closed(p))
            }
            _ => None,
        }
        .unwrap_or_default()
    }
}

/// The publishDiagnostics for a document's current parse errors
///
/// The version is carried so a client can drop a set it has already superseded
/// with a newer edit
fn publish(uri: Uri, document: &Document) -> Message {
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
fn clear(uri: Uri) -> Message {
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
