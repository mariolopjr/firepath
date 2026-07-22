//! The handler the loop runs: the open buffers and the features answered from
//! them
//!
//! The server owns the [`Documents`] store, routes each sync notification into
//! it, and answers from the buffer the client is showing rather than from the
//! file on disk. A change re-parses that buffer and publishes its errors. A
//! semantic tokens request re-scans it for the spans an editor colors.
//!
//! The store is left consistent even if a panic occurs. An open or change
//! builds the whole [`Document`](crate::Document), parsing included, before
//! it touches the map, so a panic while parsing leaves the previous buffer in
//! place

use lsp_server::{ErrorCode, Message, Notification, Request, RequestId, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification as _,
};
use lsp_types::request::{Request as _, SemanticTokensFullRequest};
use lsp_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    SemanticTokensParams, SemanticTokensResult, TextDocumentItem, VersionedTextDocumentIdentifier,
};

use crate::diag::{clear, publish};
use crate::log;
use crate::main_loop::method_not_found;
use crate::tokens::tokens;
use crate::{Documents, Handler};

/// The language server: what the editor has open, plus what is answered from it
#[derive(Debug, Default)]
pub struct Server {
    /// What the editor has open, laid over the files on disk
    docs: Documents,
}

impl Server {
    /// A server with nothing open
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

    /// Answer a semantic tokens request from the buffer it names
    ///
    /// A uri that is not open has no buffer to color. The protocol allows a
    /// null result, which is the honest answer: what is on disk is not what the
    /// client is showing, so tokens taken from it would land on the wrong bytes
    fn semantic_tokens(&self, id: RequestId, params: SemanticTokensParams) -> Response {
        let uri = params.text_document.uri;
        let Some(document) = self.docs.get(&uri) else {
            log::dropped(format_args!(
                "a semantic tokens request for {}: it was never opened",
                uri.as_str()
            ));
            return Response::new_ok(id, Option::<SemanticTokensResult>::None);
        };
        Response::new_ok(id, SemanticTokensResult::Tokens(tokens(document)))
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

impl Handler for Server {
    fn request(&mut self, request: Request) -> Response {
        let Request { id, method, params } = request;
        match method.as_str() {
            // A request whose params do not read is a client the server cannot
            // answer, and a request always carries a reply, so it says so
            // rather than dropping the message the way a notification does
            SemanticTokensFullRequest::METHOD => match serde_json::from_value(params) {
                Ok(params) => self.semantic_tokens(id, params),
                Err(error) => {
                    Response::new_err(id, ErrorCode::InvalidParams as i32, error.to_string())
                }
            },
            // Only the capabilities the initialize result advertises are
            // answered, so anything else is a client that ignored them
            _ => method_not_found(id, &method),
        }
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
