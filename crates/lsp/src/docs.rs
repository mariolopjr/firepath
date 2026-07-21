//! The document store: what the editor has open, laid over what is on disk
//!
//! Once the user starts editing, the buffer and the file on disk differ, and
//! the server answers about the buffer. [`Documents`] holds every open buffer
//! and returns the buffer's text when a uri is open, the file's bytes when it
//! is not.
//!
//! Sync is full: a change notification carries the whole new text rather than
//! an edit to apply, so a change replaces the buffer and re-parses it. A
//! document's errors are therefore always the errors of the text the editor
//! shows. Only the changed file is re-parsed for now; reloading a whole journal
//! with its include directives is not supported yet.
//!
//! Positions are sent as a line and a count of UTF-16 code units, while a
//! [`Span`] is a byte offset into the file. [`Document::position`] and
//! [`Document::offset`] map between them in both directions, and
//! [`Document::range`] and [`Document::span`] do the same for a pair of them

use std::borrow::Cow;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::PathBuf;

use firepath_ledger::{FileId, ParseError, Span, clamp_u32, parse};
use lsp_types::{Position, Range, Uri};

/// The handle every parse is tagged with
///
/// An error carries a span, not a file, so nothing reads this back today
const SOLE_FILE: FileId = FileId::new(0);

/// One open buffer: its text, the version the client last gave it, and what
/// parsing that text found
#[derive(Debug)]
pub struct Document {
    /// The buffer as the editor last sent it
    text: Vec<u8>,
    /// The version the client stamped that text with
    version: i32,
    /// The byte offset each line starts at, ascending, the first at zero
    line_starts: Vec<u32>,
    /// Every error parsing `text` found
    errors: Vec<ParseError>,
}

impl Document {
    /// Parse a buffer into a document
    fn new(version: i32, text: String) -> Self {
        Self::from_bytes(version, text.into_bytes())
    }

    /// Parse a buffer already held as bytes
    ///
    /// A buffer from the client is always a `String`
    fn from_bytes(version: i32, text: Vec<u8>) -> Self {
        Self {
            errors: parse(SOLE_FILE, &text),
            line_starts: line_starts(&text),
            version,
            text,
        }
    }

    /// The buffer's bytes
    pub fn text(&self) -> &[u8] {
        &self.text
    }

    /// The version the client stamped the buffer with
    pub fn version(&self) -> i32 {
        self.version
    }

    /// Every error parsing the buffer found, in source order
    pub fn errors(&self) -> &[ParseError] {
        &self.errors
    }

    /// The client-facing position of a byte offset
    ///
    /// An offset past the end of the buffer resolves to the end of its last
    /// line, so a span past the text still maps to a valid position
    pub fn position(&self, offset: u32) -> Position {
        // The line is the last one starting at or before the offset. Line
        // starts ascend, so binary search finds it instead of a linear scan,
        // which matters when every error in a large file is mapped
        let line = self
            .line_starts
            .partition_point(|&start| start <= offset)
            .saturating_sub(1);
        let start = self.line_starts.get(line).copied().unwrap_or(0);
        Position {
            line: clamp_u32(line),
            character: utf16_len(self.slice(start, offset)),
        }
    }

    /// The byte offset of a client-facing position
    ///
    /// A character past the end of its line resolves to the end of that line,
    /// and a line past the end of the buffer to the end of the buffer. The
    /// protocol lets a client address past the text, and there is no byte there
    /// to return
    pub fn offset(&self, position: Position) -> u32 {
        let line = position.line as usize;
        let Some(&start) = self.line_starts.get(line) else {
            return clamp_u32(self.text.len());
        };
        let line_text = self.slice(start, self.line_end(line, start));
        start.saturating_add(utf16_prefix(line_text, position.character))
    }

    /// The client-facing range a source span covers
    pub fn range(&self, span: Span) -> Range {
        Range {
            start: self.position(span.start()),
            end: self.position(span.end()),
        }
    }

    /// The source span a client-facing range covers
    pub fn span(&self, range: Range) -> Span {
        let start = self.offset(range.start);
        // A client can send a range whose end precedes its start. Span::new
        // rejects a reversed span, so clamp the end up to the start
        Span::new(start, self.offset(range.end).max(start))
    }

    /// Where a line's text ends as the client counts it, before a `\r\n` or
    /// `\n`, or the buffer end for the last line
    ///
    /// The `\r` of a CRLF is left out on purpose. A client measures a line
    /// without its ending, and the protocol clamps a character past the end
    /// back to that length, so `character: u32::MAX`, the way a client says end
    /// of line, has to land on the `\r` and not after it. Count the `\r` and an
    /// edit written at that position overwrites the CRLF with a bare `\n`.
    ///
    /// [`Document::position`] does count the `\r`, so the two are not exact
    /// inverses: the `\n` of a CRLF maps to a position that comes back a byte
    /// early, on the `\r`. Only a parse span lands on that `\n`, and a range
    /// there is clamped off client-side, so no edit is ever written through it
    fn line_end(&self, line: usize, start: u32) -> u32 {
        let end = self.line_starts.get(line.saturating_add(1)).map_or_else(
            || clamp_u32(self.text.len()),
            |&next| next.saturating_sub(1),
        );
        let last = end.saturating_sub(1);
        if end > start && self.text.get(last as usize) == Some(&b'\r') {
            last
        } else {
            end
        }
    }

    /// The buffer's bytes from `start` to `end`, clamped to what is there
    ///
    /// Both bounds are clamped before the slice is taken, so `start <= end <=
    /// len` holds and the fallback is unreachable
    fn slice(&self, start: u32, end: u32) -> &[u8] {
        let end = (end as usize).min(self.text.len());
        let start = (start as usize).min(end);
        self.text.get(start..end).unwrap_or_default()
    }
}

/// Every buffer the editor has open, laid over the files on disk
#[derive(Debug, Default)]
pub struct Documents {
    open: HashMap<Uri, Document>,
}

impl Documents {
    /// A store with nothing open
    pub fn new() -> Self {
        Self::default()
    }

    /// Take the buffer a `didOpen` carried, parsing it, and return the document
    /// it became
    ///
    /// A uri that is already open is replaced rather than merged: a second open
    /// is the client resynchronizing, so its text replaces what was there
    pub fn open(&mut self, uri: Uri, version: i32, text: String) -> &Document {
        self.open
            .entry(uri)
            .insert_entry(Document::new(version, text))
            .into_mut()
    }

    /// Replace an open buffer with the text a `didChange` carried, re-parsing
    /// it, and return the document it became
    ///
    /// `None` if the client never opened the uri. Under full sync there is no
    /// prior text to apply a change onto, so a change to an unopened uri is a
    /// protocol violation
    pub fn change(&mut self, uri: &Uri, version: i32, text: String) -> Option<&Document> {
        let document = self.open.get_mut(uri)?;
        *document = Document::new(version, text);
        Some(document)
    }

    /// Drop an open buffer, so the uri reads from the file on disk again
    ///
    /// Returns whether it was open at all
    pub fn close(&mut self, uri: &Uri) -> bool {
        self.open.remove(uri).is_some()
    }

    /// The document for an open uri, if it is open
    pub fn get(&self, uri: &Uri) -> Option<&Document> {
        self.open.get(uri)
    }

    /// The bytes behind a uri: the open buffer if the editor has one, the file
    /// on disk if not
    ///
    /// The server reads journal files through here. Reading the path directly
    /// would return stale bytes for a file the user has unsaved edits in
    ///
    /// # Errors
    ///
    /// Returns an error if the uri is not open and names no readable file
    pub fn source(&self, uri: &Uri) -> io::Result<Cow<'_, [u8]>> {
        if let Some(document) = self.open.get(uri) {
            return Ok(Cow::Borrowed(document.text()));
        }
        let path = file_path(uri).map_err(|reason| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                format!("{} {reason}", uri.as_str()),
            )
        })?;
        fs::read(path).map(Cow::Owned)
    }
}

/// Why a uri names no file this server can read
///
/// Each reason is a distinct message appended to the error, so the log says
/// which check failed rather than giving one blanket refusal
type Refusal = &'static str;

/// The filesystem path a `file:` uri names
///
/// The checks are strict because a path that is not exactly the file the uri
/// names would read a different file, and reading the wrong journal is worse
/// than reading none.
///
/// # Errors
///
/// Returns why the uri names no readable local file: a scheme other than
/// `file`, an authority naming another host, a relative path, a Windows drive
/// path, a segment holding an escaped separator or a NUL, or a segment that
/// does not percent-decode to UTF-8
fn file_path(uri: &Uri) -> Result<PathBuf, Refusal> {
    let scheme = uri.scheme().ok_or("does not name a local file")?;
    if !scheme.eq_lowercase("file") {
        return Err("does not name a local file");
    }
    // The authority names the host holding the file. Empty and `localhost` both
    // mean this machine. Any other host names a file elsewhere, and resolving
    // its path locally would open an unrelated file of the same name
    if let Some(authority) = uri.authority() {
        let host = authority.as_str();
        if !host.is_empty() && !host.eq_ignore_ascii_case("localhost") {
            return Err("names a file on another host");
        }
    }
    let path = uri.path();
    if !path.is_absolute() {
        return Err("does not name an absolute path");
    }
    // Each segment is decoded on its own and the `/` separators put back
    // between them, so an escaped separator stays part of a name. Decoding the
    // whole path at once would let `%2F` act as a real separator and resolve to
    // a different file
    let mut decoded = String::new();
    for (index, segment) in path.segments().enumerate() {
        let segment = segment
            .decode()
            .into_string()
            .map_err(|_| "does not percent-decode to UTF-8")?;
        // A Windows uri puts the drive in the first segment, `/C:/journals`,
        // which is not a valid path here. Only the posix form is handled, so
        // the drive form is refused rather than resolved to a path that names
        // nothing
        if index == 0 && is_drive(&segment) {
            return Err("names a Windows drive path, which is not supported");
        }
        if segment.contains('/') || segment.contains('\0') {
            return Err("has an escaped separator or NUL in a path segment");
        }
        decoded.push('/');
        decoded.push_str(&segment);
    }
    Ok(PathBuf::from(decoded))
}

/// Whether a path segment is a Windows drive letter: `C:`
fn is_drive(segment: &str) -> bool {
    let mut bytes = segment.bytes();
    matches!(
        (bytes.next(), bytes.next(), bytes.next()),
        (Some(letter), Some(b':'), None) if letter.is_ascii_alphabetic()
    )
}

/// The byte offset each line of `text` starts at, the first at zero
///
/// Lines are split on `\n` alone, the way the parser splits them, so a line
/// number in a parse error and a line number in a client position mean the same
/// line. A lone carriage return does not start a new line even though a client
/// would draw one there
fn line_starts(text: &[u8]) -> Vec<u32> {
    // Count first so the vector allocates once. `memchr_iter` gives no size
    // hint, so extending from it would regrow repeatedly, and the whole file is
    // re-scanned on every keystroke
    let mut starts = Vec::with_capacity(memchr::memchr_iter(b'\n', text).count().saturating_add(1));
    starts.push(0);
    starts.extend(
        memchr::memchr_iter(b'\n', text)
            // A newline past u32::MAX cannot be a line start a span points into
            .filter_map(|newline| u32::try_from(newline).ok())
            .map(|newline| newline.saturating_add(1)),
    );
    starts
}

/// How many UTF-16 code units `bytes` encodes to
fn utf16_len(bytes: &[u8]) -> u32 {
    let mut units = 0u32;
    let mut at = 0usize;
    while let Some((&lead, rest)) = bytes.get(at..).and_then(<[u8]>::split_first) {
        let (len, count) = sequence(lead, rest);
        units = units.saturating_add(count);
        at = at.saturating_add(len);
    }
    units
}

/// The byte length of the prefix of `bytes` that is `units` UTF-16 code units
/// long, or all of `bytes` when it holds fewer
///
/// A count that lands inside a surrogate pair takes the whole pair, since half
/// a character is not a byte offset
fn utf16_prefix(bytes: &[u8], units: u32) -> u32 {
    let mut seen = 0u32;
    let mut at = 0usize;
    while seen < units {
        let Some((&lead, rest)) = bytes.get(at..).and_then(<[u8]>::split_first) else {
            break;
        };
        let (len, count) = sequence(lead, rest);
        seen = seen.saturating_add(count);
        at = at.saturating_add(len);
    }
    clamp_u32(at)
}

/// How many bytes the UTF-8 sequence starting with `lead` spans, and how many
/// UTF-16 code units it encodes to. `rest` is what follows the lead byte
///
/// Passing the lead byte separately keeps this total: there is no empty input
/// to handle, so only the callers' loops need the bounds check.
///
/// A byte that starts no well-formed sequence counts as one byte and one unit,
/// which is what a lossy decode renders it as: a journal may be Latin-1, and
/// the mapping still has to cover every byte. Overlong and surrogate encodings
/// are taken at face value, so the count is exact for valid UTF-8 and
/// approximate for anything else
fn sequence(lead: u8, rest: &[u8]) -> (usize, u32) {
    let len: usize = match lead {
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        // ASCII, a stray continuation byte, or one no sequence starts with
        _ => return (1, 1),
    };
    let wanted = len.saturating_sub(1);
    let continuations = rest.get(..wanted).unwrap_or_default();
    if continuations.len() < wanted || !continuations.iter().all(|&byte| byte & 0xC0 == 0x80) {
        // Truncated or malformed, so the lead byte stands alone and the scan
        // picks up at the byte after it
        return (1, 1);
    }
    // Only a four-byte sequence is above the BMP, and only it needs a pair
    (len, if len == 4 { 2 } else { 1 })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "unwrap keeps the fixtures terse"
)]
mod tests {
    use std::str::FromStr;

    use lsp_types::{Position, Range, Uri};

    use super::{Document, Documents};
    use firepath_ledger::Span;

    // A source with every sequence width in it: ASCII, a two-byte é, a
    // three-byte arrow, a four-byte astral clef, an empty line, a CRLF ending,
    // and no trailing newline on the last line
    const MIXED: &str = "a é→𝄞b\n\r\n𝄞\r\nx é\ntrailing";

    fn uri(text: &str) -> Uri {
        Uri::from_str(text).unwrap()
    }

    fn doc(text: &str) -> Document {
        Document::new(1, text.to_owned())
    }

    fn at(line: u32, character: u32) -> Position {
        Position { line, character }
    }

    #[test]
    fn every_char_boundary_round_trips_through_a_position() {
        // Exhaustive rather than sampled: the source is small enough to check
        // every boundary, and a random generator would hit the astral char
        // only rarely
        let document = doc(MIXED);
        for (offset, _) in MIXED.char_indices() {
            let offset = u32::try_from(offset).unwrap();
            let position = document.position(offset);
            // The `\n` of a CRLF is the one byte position() does not invert: it
            // lies past the line end the client sees, so it maps back to the
            // `\r` before it
            let expected = match MIXED.as_bytes().get(offset.wrapping_sub(1) as usize) {
                Some(b'\r') => offset.saturating_sub(1),
                _ => offset,
            };
            assert_eq!(
                document.offset(position),
                expected,
                "offset {offset} through {position:?}"
            );
        }
        // The end of the text is a boundary a span's end lands on, and it is
        // not a char index
        let end = u32::try_from(MIXED.len()).unwrap();
        assert_eq!(document.offset(document.position(end)), end);
    }

    #[test]
    fn every_addressable_position_round_trips_through_an_offset() {
        // The other direction, over every position a client could send that
        // names a character rather than pointing past one
        let document = doc(MIXED);
        for (line, text) in MIXED.split('\n').enumerate() {
            // A client counts a line without its ending, so the carriage
            // return of a CRLF is not one of the positions it can send
            let text = text.strip_suffix('\r').unwrap_or(text);
            let line = u32::try_from(line).unwrap();
            let mut character = 0u32;
            for unit in std::iter::once(0).chain(text.chars().map(char::len_utf16)) {
                character = character.saturating_add(u32::try_from(unit).unwrap());
                let position = at(line, character);
                assert_eq!(
                    document.position(document.offset(position)),
                    position,
                    "{position:?}"
                );
            }
        }
    }

    #[test]
    fn a_character_counts_utf16_code_units_not_bytes() {
        // "a é→𝄞b": a=1 byte, space=1, é=2 bytes 1 unit, →=3 bytes 1 unit,
        // 𝄞=4 bytes 2 units, b=1
        let document = doc(MIXED);
        assert_eq!(document.position(0), at(0, 0)); // a
        assert_eq!(document.position(2), at(0, 2)); // é, its lead byte
        assert_eq!(document.position(4), at(0, 3)); // →, past the two-byte é
        assert_eq!(document.position(7), at(0, 4)); // 𝄞, past the three-byte →
        assert_eq!(document.position(11), at(0, 6)); // b, past the surrogate pair
    }

    #[test]
    fn a_character_inside_a_surrogate_pair_resolves_to_the_whole_pair() {
        // Line 2 is a lone 𝄞: character 1 is the pair's low half, which is not
        // a byte offset, so it takes the pair rather than splitting it
        let document = doc(MIXED);
        assert_eq!(document.offset(at(2, 1)), document.offset(at(2, 2)));
    }

    #[test]
    fn a_character_past_the_end_of_its_line_clamps_to_the_line() {
        let document = doc(MIXED);
        // Line 3 is "x é", three characters ending before its newline
        let end = document.offset(at(3, 3));
        assert_eq!(document.offset(at(3, 99)), end);
        assert_eq!(document.text().get(end as usize), Some(&b'\n'));
    }

    #[test]
    fn a_line_past_the_end_of_the_buffer_clamps_to_the_end() {
        let document = doc(MIXED);
        let end = u32::try_from(MIXED.len()).unwrap();
        assert_eq!(document.offset(at(99, 0)), end);
    }

    #[test]
    fn an_offset_past_the_end_of_the_buffer_resolves_to_the_last_line() {
        let document = doc("ab\ncd");
        // "cd" is line 1, so anything past it lands at its end rather than
        // wrapping onto a line that is not there
        assert_eq!(document.position(99), at(1, 2));
    }

    #[test]
    fn a_carriage_return_does_not_break_a_line() {
        // Line 1 of the fixture is the empty CRLF line, so only the \n after
        // its \r starts the line holding the lone clef
        let document = doc(MIXED);
        assert_eq!(document.position(13), at(1, 0)); // the \r itself
        assert_eq!(document.position(14), at(1, 1)); // the \n after it
        assert_eq!(document.position(15), at(2, 0)); // the clef on the next line
    }

    #[test]
    fn a_character_past_a_crlf_line_clamps_before_the_carriage_return() {
        // The end-of-line idiom on a CRLF line. Clamping to the \n instead
        // would let an edit at this position overwrite the CRLF with a bare \n
        let document = doc("ab\r\ncd");
        let end = document.offset(at(0, u32::MAX));
        assert_eq!(end, 2);
        assert_eq!(document.text().get(end as usize), Some(&b'\r'));
        // The last line has no ending at all, so it still clamps to the buffer
        assert_eq!(document.offset(at(1, u32::MAX)), 6);
    }

    #[test]
    fn a_lone_carriage_return_at_the_end_of_the_buffer_is_not_addressable() {
        // Nothing follows it to make it a line ending, but a client draws one
        // there all the same, so a character past the end stops before it
        let document = doc("ab\r");
        assert_eq!(document.offset(at(0, u32::MAX)), 2);
    }

    #[test]
    fn an_empty_crlf_line_has_no_addressable_character() {
        // The \r is the whole line, so the line the client sees is empty and
        // clamping cannot reach the ending
        let document = doc(MIXED);
        assert_eq!(document.offset(at(1, u32::MAX)), 13);
    }

    #[test]
    fn an_empty_buffer_has_one_line_and_one_position() {
        let document = doc("");
        assert_eq!(document.position(0), at(0, 0));
        assert_eq!(document.offset(at(0, 0)), 0);
        assert_eq!(document.offset(at(0, 5)), 0);
    }

    #[test]
    fn a_byte_that_starts_no_sequence_counts_as_one_character() {
        // Latin-1 "é" is the single byte 0xE9, which leads a three-byte
        // sequence it does not have, so it stands alone rather than swallowing
        // the two ASCII bytes after it
        let document = Document::from_bytes(1, b"\xe9ab".to_vec());
        assert_eq!(document.position(3), at(0, 3));
        assert_eq!(document.offset(at(0, 3)), 3);
    }

    #[test]
    fn a_truncated_sequence_counts_its_lead_byte_alone() {
        // A two-byte lead at the very end of the buffer, with no continuation
        // to pair with
        let document = Document::from_bytes(1, b"a\xc3".to_vec());
        assert_eq!(document.position(2), at(0, 2));
    }

    #[test]
    fn a_span_maps_to_the_range_that_covers_it_and_back() {
        let document = doc(MIXED);
        // The é on the last-but-one line, two bytes wide and one character
        let span = Span::new(23, 25);
        let range = document.range(span);
        assert_eq!(range, Range::new(at(3, 2), at(3, 3)));
        assert_eq!(document.span(range), span);
    }

    #[test]
    fn a_reversed_range_maps_to_an_empty_span() {
        let document = doc(MIXED);
        let span = document.span(Range::new(at(3, 3), at(0, 0)));
        assert!(span.is_empty());
        assert_eq!(span.start(), document.offset(at(3, 3)));
    }

    #[test]
    fn opening_a_document_parses_it_and_keeps_its_version() {
        let mut store = Documents::new();
        let document = store.open(uri("file:///j.ledger"), 7, "; a comment\n".to_owned());
        assert_eq!(document.version(), 7);
        assert!(document.errors().is_empty());
    }

    #[test]
    fn a_parse_error_is_carried_on_the_document() {
        let mut store = Documents::new();
        // An indented line with no block above it, the parser's orphan error
        let document = store.open(
            uri("file:///j.ledger"),
            1,
            "    Assets:Cash  $5\n".to_owned(),
        );
        let error = document.errors().first().expect("the orphan error");
        assert_eq!(document.range(error.span), Range::new(at(0, 0), at(0, 19)));
    }

    #[test]
    fn a_change_replaces_the_buffer_and_reparses_it() {
        let file = uri("file:///j.ledger");
        let mut store = Documents::new();
        store.open(file.clone(), 1, "    orphan\n".to_owned());
        let document = store
            .change(&file, 2, "; fixed\n".to_owned())
            .expect("the document is open");
        assert_eq!(document.version(), 2);
        assert_eq!(document.text(), b"; fixed\n");
        assert!(document.errors().is_empty());
    }

    #[test]
    fn opening_a_uri_twice_replaces_what_was_there() {
        let file = uri("file:///j.ledger");
        let mut store = Documents::new();
        store.open(file.clone(), 1, "; first\n".to_owned());
        store.open(file.clone(), 2, "; second\n".to_owned());
        assert_eq!(store.get(&file).unwrap().text(), b"; second\n");
    }

    #[test]
    fn a_change_to_a_document_that_was_never_opened_is_refused() {
        let mut store = Documents::new();
        assert!(
            store
                .change(&uri("file:///never.ledger"), 1, "; text\n".to_owned())
                .is_none()
        );
        assert!(store.get(&uri("file:///never.ledger")).is_none());
    }

    #[test]
    fn closing_reports_whether_the_document_was_open() {
        let file = uri("file:///j.ledger");
        let mut store = Documents::new();
        store.open(file.clone(), 1, "; text\n".to_owned());
        assert!(store.close(&file));
        assert!(!store.close(&file));
        assert!(store.get(&file).is_none());
    }

    #[test]
    fn an_open_buffer_shadows_the_file_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("j.ledger");
        std::fs::write(&path, b"; on disk\n").unwrap();
        let file = uri(&format!("file://{}", path.display()));

        let mut store = Documents::new();
        // Nothing open, so the uri reads through to the file
        assert_eq!(store.source(&file).unwrap().as_ref(), b"; on disk\n");

        store.open(file.clone(), 1, "; in the editor\n".to_owned());
        assert_eq!(store.source(&file).unwrap().as_ref(), b"; in the editor\n");

        // Closing hands the uri back to the file, unedited buffer and all
        store.close(&file);
        assert_eq!(store.source(&file).unwrap().as_ref(), b"; on disk\n");
    }

    #[test]
    fn a_percent_encoded_path_reads_the_file_it_names() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("my journal.ledger");
        std::fs::write(&path, b"; spaced\n").unwrap();
        let encoded = path.display().to_string().replace(' ', "%20");

        let store = Documents::new();
        let source = store.source(&uri(&format!("file://{encoded}"))).unwrap();
        assert_eq!(source.as_ref(), b"; spaced\n");
    }

    /// The reason a uri was refused, with the uri itself stripped off the front
    fn refusal(text: &str) -> String {
        let store = Documents::new();
        let error = store.source(&uri(text)).expect_err("refused");
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
        let message = error.to_string();
        let reason = message
            .strip_prefix(text)
            .expect("the uri leads the message")
            .to_owned();
        reason.trim_start().to_owned()
    }

    #[test]
    fn a_uri_that_names_no_local_file_is_refused() {
        for text in ["untitled:Untitled-1", "https://example.com/j.ledger"] {
            assert_eq!(refusal(text), "does not name a local file");
        }
    }

    #[test]
    fn a_uri_without_a_scheme_is_refused() {
        // A relative reference parses as a uri but names no scheme to check
        assert_eq!(refusal("j.ledger"), "does not name a local file");
    }

    #[test]
    fn a_uri_naming_another_host_is_not_read_from_this_one() {
        // The path alone resolves to a real local file, so dropping the host
        // would quietly read this machine's copy instead of refusing
        assert_eq!(
            refusal("file://evil.example.com/etc/passwd"),
            "names a file on another host"
        );
    }

    #[test]
    fn the_local_host_is_still_this_machine() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("j.ledger");
        std::fs::write(&path, b"; local\n").unwrap();

        let store = Documents::new();
        for host in ["", "localhost", "LOCALHOST"] {
            let source = store
                .source(&uri(&format!("file://{host}{}", path.display())))
                .unwrap();
            assert_eq!(source.as_ref(), b"; local\n", "host {host:?}");
        }
    }

    #[test]
    fn an_escaped_separator_does_not_become_one() {
        // %2F is a byte of a name, not structure. Decoding the path whole would
        // split the segment and resolve to /a/b.ledger, a different file
        assert_eq!(
            refusal("file:///a%2Fb.ledger"),
            "has an escaped separator or NUL in a path segment"
        );
        assert_eq!(
            refusal("file:///a%00b.ledger"),
            "has an escaped separator or NUL in a path segment"
        );
    }

    #[test]
    fn a_path_that_does_not_decode_to_utf8_is_refused() {
        // A lossy decode names a different file, so there is nothing to read
        assert_eq!(
            refusal("file:///%FF.ledger"),
            "does not percent-decode to UTF-8"
        );
    }

    #[test]
    fn a_windows_drive_path_is_refused_rather_than_resolved() {
        // /C:/j.ledger is not where the file is on Windows, and only the posix
        // form is handled, so it is refused instead of read as a local path
        assert_eq!(
            refusal("file:///C:/j.ledger"),
            "names a Windows drive path, which is not supported"
        );
    }

    #[test]
    fn a_first_segment_shaped_like_a_drive_without_a_letter_is_a_name() {
        // `1:` has the shape but not the letter, so it is an ordinary directory
        // name and has to reach the filesystem rather than be refused. Reaching
        // it is what NotFound reports
        let store = Documents::new();
        let error = store
            .source(&uri("file:///1%3A/j.ledger"))
            .expect_err("no such file");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn a_drive_shaped_name_deeper_in_the_path_is_still_a_name() {
        // Only the first segment can be a drive. A directory named `C:` is a
        // legal posix name and must not be refused
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("C:");
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(nested.join("j.ledger"), b"; nested\n").unwrap();

        let store = Documents::new();
        let source = store
            .source(&uri(&format!(
                "file://{}/C%3A/j.ledger",
                dir.path().display()
            )))
            .unwrap();
        assert_eq!(source.as_ref(), b"; nested\n");
    }

    #[test]
    fn a_relative_file_uri_is_refused() {
        // A rootless path has nothing to resolve it against
        assert_eq!(refusal("file:j.ledger"), "does not name an absolute path");
    }

    #[test]
    fn a_file_uri_for_a_file_that_is_not_there_reports_why() {
        let store = Documents::new();
        let error = store
            .source(&uri("file:///nowhere/absolutely/not.ledger"))
            .expect_err("no such file");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }
}
