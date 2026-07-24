//! The upstream harness: parse ledger's own `.test` files and run firepath
//! against them
//!
//! A `.test` file is a ledger journal with `test <command>` / `end test` blocks
//! written into it. ledger runs each block's command against the file itself and
//! diffs what comes back against the block's body, so the journal under test and
//! the expectations for it are one file. ledger's parser reads `test`/`end test`
//! as a comment block, which is why the expectations do not disturb the journal;
//! firepath's line grouping does the same, so the whole file is the journal input
//! for both.
//!
//! This mirrors `read_test` in the submodule's `test/RegressTests.py`, quirks
//! included, so the harness measures firepath against what ledger's own runner
//! does rather than against a tidier reading of the format. The quirks are called
//! out where they are implemented, and every place this reads the format
//! differently on purpose is labelled `Deviation`.
//!
//! Upstream's `transform_line` is applied by the runner rather than the parse
//! (see [`transform`]): both of its values depend on where the harness runs, so
//! they cannot be baked into a parsed record. Upstream applies the substitution
//! to the command as well, but only on a command line carrying a ` -> code`,
//! which no checked-out file does

use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::io::Write as _;
use std::mem;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::thread;

// Renamed on the way in: this module has its own `parse`, for the `.test`
// format, and the two sit side by side in `measure`
use firepath_ledger::{FileId, parse as parse_journal};

/// One `test` block: a command line and what ledger is expected to produce
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Case {
    /// The command line after `test `, without any ` -> code` suffix
    pub command: String,
    /// Expected stdout, `None` when the block carries no body lines
    ///
    /// `None` and `Some("")` differ: upstream leaves stdout uncompared when a
    /// block declares nothing, rather than demanding no output
    pub output: Option<String>,
    /// Expected stderr: the body after an `__ERROR__` line, `None` when there is
    /// none, which upstream reads as expecting stderr to stay empty
    pub error: Option<String>,
    /// Expected exit status, 0 unless a ` -> code` declared one
    ///
    /// Not necessarily declared by [`command`](Self::command): a `test` line that
    /// replaces the command carries a code only when it writes one, so a code an
    /// earlier command declared stays in force
    pub exit_code: i32,
}

impl Case {
    /// Whether the command names its own `-f`, so ledger reads a journal other
    /// than the `.test` file
    ///
    /// Upstream tests for the flag exactly this loosely, then hands ledger the
    /// test file only when the command does not already name an input
    pub fn reads_another_journal(&self) -> bool {
        self.command.contains("-f ")
    }
}

/// One parsed `.test` file
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestFile<'a> {
    /// The journal ledger reads, which is the whole file: the expectation blocks
    /// inside it parse as comment blocks
    pub journal: &'a str,
    /// Every case in the file, in the order they appear
    pub cases: Vec<Case>,
    /// Commands that never run: a `test` line opening while a block is already
    /// open replaces the command rather than starting a case, so the replaced one
    /// is lost. `regress/C927CFFE.test` carries one. Reported here so a file that
    /// asserts less than it looks like it does can be seen
    pub dropped: Vec<String>,
}

/// Why a `.test` file yielded no records
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Malformed {
    /// The file holds no bytes. Upstream warns and counts it a failure
    Empty,
    /// The file holds no `test` line, so it asserts nothing
    ///
    /// Deviation: upstream runs such a file's zero cases and reports nothing,
    /// which reads as a clean pass. A file that measures nothing is worth seeing
    NoCases,
}

impl fmt::Display for Malformed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("the file is empty"),
            Self::NoCases => f.write_str("the file holds no test block"),
        }
    }
}

impl Error for Malformed {}

/// Parse one `.test` file into the cases it declares
///
/// # Errors
///
/// Returns [`Malformed`] for a file that declares no case at all, so an input
/// the harness cannot measure is reported rather than counted as zero failures
pub fn parse(source: &str) -> Result<TestFile<'_>, Malformed> {
    if source.is_empty() {
        return Err(Malformed::Empty);
    }

    let mut cases = Vec::new();
    let mut dropped = Vec::new();
    let mut open: Option<Case> = None;
    // Sticky across a `test` line, as upstream leaves it, and cleared only when
    // the block ends
    let mut in_error = false;

    // Split keeping the terminator: upstream reads with `readline`, so a final
    // line the file never terminated stays unterminated and its expectation is
    // one byte shorter than the same text a newline followed
    for raw in source.split_inclusive('\n') {
        let (line, terminated) = match raw.strip_suffix('\n') {
            Some(line) => (line, true),
            None => (raw, false),
        };
        // Python opens the file in universal-newline mode, so a CRLF reaches
        // `read_test` as a bare LF
        let line = line.strip_suffix('\r').unwrap_or(line);

        // A `test` line opens a case wherever it lands, an open block included
        if let Some(rest) = line.strip_prefix("test ") {
            let (command, exit_code) = split_exit_code(rest);
            match open.as_mut() {
                // Upstream overwrites the command in place, keeping any body it
                // has already collected, so the replaced command never runs
                Some(case) => {
                    dropped.push(mem::replace(&mut case.command, command.to_owned()));
                    // Upstream assigns the exit code only on a line that carries
                    // one, so a replacement without ` -> code` leaves the code the
                    // replaced command declared rather than resetting it
                    if let Some(code) = exit_code {
                        case.exit_code = code;
                    }
                }
                None => {
                    open = Some(Case {
                        command: command.to_owned(),
                        output: None,
                        error: None,
                        exit_code: exit_code.unwrap_or(0),
                    });
                }
            }
            continue;
        }

        // A block closes only while one is open: outside a block this is journal
        // text that happens to read the same way
        if line.starts_with("end test") {
            if open.is_some() {
                cases.extend(open.take());
                in_error = false;
            }
            continue;
        }

        // Every other line outside a block is journal text as well
        let Some(case) = open.as_mut() else { continue };

        if !in_error && line.starts_with("__ERROR__") {
            in_error = true;
            continue;
        }

        let body = if in_error {
            &mut case.error
        } else {
            &mut case.output
        };
        // The newline goes back on: upstream keeps it on every expectation line
        // and diffs against process output read the same way
        let body = body.get_or_insert_default();
        body.push_str(line);
        if terminated {
            body.push('\n');
        }
    }

    // A block still open at the end of the file is a case upstream runs anyway,
    // which `regress/1182_3.test` relies on
    cases.extend(open);

    if cases.is_empty() {
        return Err(Malformed::NoCases);
    }
    Ok(TestFile {
        journal: source,
        cases,
        dropped,
    })
}

/// Split a trailing ` -> code` off a command line, returning the code the line
/// declares or `None` when it declares none
///
/// Upstream matches `(.*) -> ([0-9]+)` with a greedy first group, so the last
/// ` -> ` followed by a digit is the one that counts and anything after those
/// digits is dropped. An arrow with no digits after it is part of the command:
/// `test eval 'x, y -> f(x)'` stays whole.
///
/// Deviation: upstream reads the digits as a Python integer of any width, so
/// `-> 99999999999999999999` is an exit code there and nothing but a test that
/// can never pass. Here a run of digits no exit status can hold is not a code, so
/// the search carries on past it and the arrow stays in the command. No
/// checked-out file declares one
fn split_exit_code(rest: &str) -> (&str, Option<i32>) {
    let mut head = rest;
    while let Some((before, after)) = head.rsplit_once(" -> ") {
        let end = after
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(after.len());
        if let Some(digits) = after.get(..end).filter(|digits| !digits.is_empty())
            && let Ok(code) = digits.parse()
        {
            return (before, Some(code));
        }
        head = before;
    }
    (rest, None)
}

/// Where the checked-out ledger tests live, holding the `manual`, `baseline`,
/// and `regress` directories upstream runs
///
/// The submodule is optional: the directory is empty until `just fetch-upstream`
/// checks it out, so a caller reads it expecting it to be absent
pub fn tests_dir() -> PathBuf {
    // The crate sits two levels under the repository root, which is where the
    // submodule is checked out
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../upstream/ledger/test")
}

/// Every word ledger dispatches a command on
///
/// Transcribed from ledger's own lookup tables: the `symbol_t::COMMAND` and
/// `symbol_t::PRECOMMAND` arms of `report.cc`, plus the precommands `global.cc`
/// and `pyinterp.cc` add. Single letters are in it because ledger accepts them:
/// `b`, `p`, and `r` are balance, print, and register
const COMMAND_WORDS: &[&str] = &[
    "accounts",
    "args",
    "b",
    "bal",
    "balance",
    "budget",
    "cleared",
    "commodities",
    "convert",
    "csv",
    "draft",
    "echo",
    "emacs",
    "entry",
    "equity",
    "eval",
    "expr",
    "format",
    "generate",
    "lisp",
    "p",
    "parse",
    "payees",
    "period",
    "pop",
    "pricedb",
    "pricemap",
    "prices",
    "pricesdb",
    "print",
    "push",
    "python",
    "query",
    "r",
    "reg",
    "register",
    "reload",
    "script",
    "select",
    "source",
    "stat",
    "stats",
    "tags",
    "template",
    "xact",
    "xml",
];

/// The ledger command a case's command line invokes
///
/// ledger dispatches on the first argument that is neither an option nor an
/// option's value. Which options take a separate value is not modelled here, so
/// the word is found by matching whole tokens against [`COMMAND_WORDS`] instead.
/// That fails closed: a line the vocabulary does not cover yields `None` rather
/// than a token that is really a file name.
///
/// Quoting is not modelled either, so a command word inside a quoted argument
/// would be picked up as the command
pub fn command_word(command: &str) -> Option<&str> {
    command
        .split_ascii_whitespace()
        .find(|token| COMMAND_WORDS.contains(token))
}

/// What one run of the harness measured
///
/// One meter, one denominator: [`cases`](Self::cases). Parsing is not scored on
/// its own, because a journal firepath cannot read fails its cases anyway, and
/// scoring it twice would let the flattering number be quoted
///
/// The denominator is every case in every file upstream itself runs, which is
/// every `.test` file but the `*_py.test` ones (see [`test_files`]). Those need
/// an interpreter firepath does not embed, so counting them would hold the meter
/// under 100% no matter what firepath does
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    /// Cases run
    pub cases: usize,
    /// Cases firepath satisfied
    pub passing: usize,
    /// Cases that did not pass, by why. The command word is part of the
    /// category, so the breakdown doubles as the list of commands still not
    /// implemented
    pub failures: BTreeMap<String, usize>,
    /// Parse errors by message over every journal the run read, counting every
    /// error rather than the first in a file
    ///
    /// Unscored, and deliberately: this is the grammar M1 still owes, which is
    /// worth reading off a run without being a second percentage to quote
    pub grammar: BTreeMap<String, usize>,
    /// Files that declared no case at all, by why
    pub malformed: BTreeMap<String, usize>,
}

impl Report {
    /// The fraction of cases firepath satisfied, as a percentage
    pub fn conformance(&self) -> f64 {
        percent(self.passing, self.cases)
    }

    /// Render the report as JSON, the meter and every category included
    ///
    /// # Errors
    ///
    /// Fails only if the report cannot be serialized, which a map of counts
    /// keyed by string cannot do
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(&Json {
            cases: self.cases,
            passing: self.passing,
            conformance_pct: self.conformance(),
            failures: &self.failures,
            grammar: &self.grammar,
            malformed: &self.malformed,
        })
    }

    /// Render the report as a shields.io endpoint badge, `ledger compat N/M (P%)`
    ///
    /// The shape is shields' [endpoint schema]: a static object a badge URL
    /// reads and renders. Color comes from [`Report::badge_color`].
    ///
    /// Anything reading the message back must parse the leading fraction and
    /// ignore the rest, which is what the PR comparison in CI does
    ///
    /// [endpoint schema]: https://shields.io/badges/endpoint-badge
    ///
    /// # Errors
    ///
    /// Fails only if the endpoint cannot be serialized, which a struct of a
    /// number and three strings cannot do
    pub fn badge_endpoint(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(&Endpoint {
            schema_version: 1,
            label: "ledger compat",
            message: format!("{}/{} ({}%)", self.passing, self.cases, self.conformance()),
            color: self.badge_color(),
        })
    }

    /// The shields color the badge displays, from the conformance percentage
    ///
    /// A six-band ramp red through brightgreen, so the badge reddens on a low
    /// number and greens as firepath closes the gap. Full parity is the only
    /// brightgreen
    fn badge_color(&self) -> &'static str {
        match self.conformance() {
            pct if pct >= 100.0 => "brightgreen",
            pct if pct >= 90.0 => "green",
            pct if pct >= 75.0 => "yellowgreen",
            pct if pct >= 50.0 => "yellow",
            pct if pct >= 25.0 => "orange",
            _ => "red",
        }
    }
}

/// The report's wire shape, with the derived percentage alongside the counts it
/// comes from
///
/// Separate from [`Report`] so the percentage stays derived rather than stored,
/// which keeps the counts the one source of truth and `Report` comparable by
/// equality
#[derive(Debug, serde::Serialize)]
struct Json<'a> {
    cases: usize,
    passing: usize,
    conformance_pct: f64,
    // BTreeMaps, so every category lands in the JSON in sorted order and two
    // runs over the same tests produce byte-identical output
    failures: &'a BTreeMap<String, usize>,
    grammar: &'a BTreeMap<String, usize>,
    malformed: &'a BTreeMap<String, usize>,
}

/// The shields.io endpoint badge wire shape
///
/// `schemaVersion` is the field shields keys on to read the rest, and it is `1`
/// for every badge this produces. The renamed field is the one camelCase name
/// shields requires
#[derive(Debug, serde::Serialize)]
struct Endpoint<'a> {
    #[serde(rename = "schemaVersion")]
    schema_version: u8,
    label: &'a str,
    message: String,
    color: &'a str,
}

/// A count as a percentage of a total, rounded to two places, and zero when
/// there is nothing to divide by
#[expect(
    clippy::cast_precision_loss,
    reason = "the counts are test-file and case tallies, far below the 2^53 an f64 holds exactly"
)]
fn percent(part: usize, total: usize) -> f64 {
    if total == 0 {
        return 0.0;
    }
    let ratio = part as f64 / total as f64;
    // Round on the way out rather than at the print, so the number the badge
    // shows and the number a caller compares are the same one
    (ratio * 10_000.0).round() / 100.0
}

/// Substitute the two placeholders upstream expands in every expectation line
///
/// `transform_line` in `RegressTests.py` does exactly this and nothing else.
/// `$sourcepath` is the `--sourcepath` argument, which upstream's `CMakeLists.txt` sets to
/// ledger's own source root, and `$FILE` is the resolved path of the `.test`
/// file
fn transform(text: &str, source_root: &Path, file: &Path) -> String {
    text.replace("$sourcepath", &source_root.display().to_string())
        .replace("$FILE", &file.display().to_string())
}

/// What running one case produced against what it declared
///
/// Upstream compares three things: stdout when the block wrote a body,
/// stderr output, and the exit status
fn verdict(case: &Case, run: &Run, source_root: &Path, file: &Path) -> Result<(), &'static str> {
    // `RegressTests.py:132`. `None` is not `Some("")`: a block that wrote no body
    // leaves stdout unread, so anything at all satisfies it
    if let Some(want) = case.output.as_deref()
        && transform(want, source_root, file) != run.stdout
    {
        return Err("stdout");
    }
    // `RegressTests.py:156` reads `test.error is not None or process_error is
    // not None`, and `readlines` never returns None, so the arm is always taken:
    // a block with no `__ERROR__` section demands stderr stay empty
    if transform(case.error.as_deref().unwrap_or_default(), source_root, file) != run.stderr {
        return Err("stderr");
    }
    if case.exit_code != run.exit_code {
        return Err("exit code");
    }
    Ok(())
}

/// What one invocation of firepath produced
#[derive(Debug)]
struct Run {
    stdout: String,
    stderr: String,
    exit_code: i32,
}

/// Run one case's command line and report whether firepath satisfied it
///
/// The invocation mirrors `RegressTests.py:100-116`: upstream hands ledger
/// `--args-only` so no environment or init file can leak in, adds
/// `--columns=80` unless the command sets its own width, points `-f` at the
/// `.test` file unless the command already names an input, and feeds the file on
/// stdin when the command reads `-`. The working directory is ledger's source
/// root, because commands name their inputs relative to it.
///
/// The command line goes through a shell, same as upstream's, because the
/// fixture quotes arguments (`bal expr "post.all(account =~ /x/)"`)
fn run_case(
    firepath: &Path,
    source_root: &Path,
    file: &Path,
    journal: &str,
    case: &Case,
) -> io::Result<Run> {
    let mut invocation = format!("{} --args-only", shell_quote(firepath));
    if !case.command.contains("--columns") {
        invocation.push_str(" --columns=80");
    }
    let line = if case.reads_another_journal() {
        format!("{invocation} {}", case.command)
    } else {
        format!("{invocation} -f {} {}", shell_quote(file), case.command)
    };

    let mut child = Command::new("sh")
        .arg("-c")
        .arg(&line)
        .current_dir(source_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    // Upstream writes the test file to stdin whenever the command reads from it.
    // `journal` is the file's own text, which the caller already read. Stdin is
    // closed either way, by the feeder when it finishes and by
    // `wait_with_output` when there is no feeder, so a command that waits on
    // input it was not given still sees the end of it
    let journal_in = if reads_stdin(&case.command) {
        child.stdin.take()
    } else {
        None
    };

    let (out, written) = thread::scope(|scope| {
        // The journal goes in on its own thread so the output pipes drain while
        // it is written. Writing it all before reading anything wedges the run
        // as soon as a command echoes its input back: the command stops reading
        // once its output pipe fills, and this side is still blocked on the
        // input pipe rather than draining the output one
        let feeder =
            journal_in.map(|mut stdin| scope.spawn(move || stdin.write_all(journal.as_bytes())));
        let out = child.wait_with_output();
        (out, feeder.map(thread::ScopedJoinHandle::join))
    });

    match written {
        // A command that exits before reading its input breaks the pipe, which
        // is the command's behaviour and not a failure of the harness: what it
        // wrote before exiting is still the answer to compare
        Some(Ok(Err(err))) if err.kind() == io::ErrorKind::BrokenPipe => {}
        Some(Ok(other)) => other?,
        // `write_all` on a pipe has no panicking path, so a feeder that comes
        // back panicked means the harness is broken, not that a case failed
        Some(Err(_)) => return Err(io::Error::other("the journal never reached the command")),
        None => {}
    }

    let out = out?;
    Ok(Run {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        // A process killed by a signal has no code. Upstream compares against
        // python's returncode, which is negative for a signal and so can never
        // equal a declared exit status, and a missing code here cannot either
        exit_code: out.status.code().unwrap_or(-1),
    })
}

/// Whether the command reads its journal from stdin, which upstream matches as
/// `-f -` or `-f /dev/stdin` followed by whitespace or the end of the line
fn reads_stdin(command: &str) -> bool {
    let mut rest = command;
    while let Some(at) = rest.find("-f ") {
        let after = rest.get(at.saturating_add(3)..).unwrap_or_default();
        let arg = after.split_ascii_whitespace().next().unwrap_or_default();
        if arg == "-" || arg == "/dev/stdin" {
            return true;
        }
        rest = after;
    }
    false
}

/// A path as a single shell word
///
/// Single quotes, with any single quote in the path closed and re-opened around
/// an escaped one, which is the only byte `sh` treats specially inside them
fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', r"'\''"))
}

/// Run firepath against the `.test` files under a directory that upstream runs,
/// which is every one of them but the `*_py.test` ones (see [`test_files`])
///
/// For each case: run the command line and compare what came back against what
/// the block declared. Every journal is also parsed on its own to fill
/// [`Report::grammar`]
///
/// # Errors
///
/// Fails when the directory cannot be resolved or walked, one of its files
/// cannot be read, or a command cannot be spawned
pub fn measure(dir: &Path, firepath: &Path) -> io::Result<Report> {
    // Resolved first, and upstream resolves both the source root and the test
    // file for the same reason: `$sourcepath` and `$FILE` are compared against
    // paths the command resolved itself, and changing into a directory drops any
    // `..` from what the command reads back as its own location. A root spelled
    // with a `..` would expand into an expectation no output can equal
    let dir = dir.canonicalize()?;
    let mut report = Report::default();
    // Commands name their inputs relative to ledger's source root, which is the
    // submodule checkout two levels above the test directories
    let source_root = dir.parent().unwrap_or(dir.as_path());

    for (index, path) in test_files(&dir)?.iter().enumerate() {
        let source = fs::read_to_string(path)?;

        // The whole file is the journal: ledger reads the `.test` file itself
        // with `-f`, and both parsers take the expectation blocks as comments.
        // The id only tags errors for rendering
        let id = FileId::new(u32::try_from(index).unwrap_or(u32::MAX));
        for error in parse_journal(id, source.as_bytes()).errors {
            bump(&mut report.grammar, error.message);
        }

        match parse(&source) {
            Err(malformed) => bump(&mut report.malformed, malformed.to_string()),
            Ok(file) => {
                for case in &file.cases {
                    report.cases = report.cases.saturating_add(1);
                    let run = run_case(firepath, source_root, path, file.journal, case)?;
                    match verdict(case, &run, source_root, path) {
                        Ok(()) => report.passing = report.passing.saturating_add(1),
                        // The command word rides along so the breakdown says
                        // which commands are owed, not only how they failed
                        Err(why) => bump(
                            &mut report.failures,
                            match command_word(&case.command) {
                                Some(word) => format!("{why}: {word}"),
                                None => format!("{why}: no ledger command on the line"),
                            },
                        ),
                    }
                }
            }
        }
    }

    Ok(report)
}

/// Add one to a category's count
fn bump(counts: &mut BTreeMap<String, usize>, category: String) {
    let count = counts.entry(category).or_default();
    *count = count.saturating_add(1);
}

/// Whether a `.test` file is one upstream runs only when it was built with
/// Python
///
/// Both of upstream's filters key on the same name suffix: `CMakeLists.txt`
/// never registers the test unless `HAVE_BOOST_PYTHON`, and `RegressTests.py`
/// drops the name from a directory run unless `--python` was passed
fn runs_only_with_python(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.as_encoded_bytes().ends_with(b"_py.test"))
}

/// Every `.test` file under a directory that upstream runs, in path order,
/// subdirectories included
///
/// Upstream's `CMakeLists.txt` runs three directories through its harness, `manual`,
/// `baseline`, and `regress`, and those are the only ones holding `.test` files,
/// so a plain recursive walk covers exactly what upstream runs. Sorting at the
/// end rather than relying on the walk makes the order the same on every
/// filesystem
///
/// A `*_py.test` file is left out, the way an upstream build without Boost.Python
/// leaves it out. firepath embeds no interpreter and never will, so running those
/// would put cases in the denominator that nothing can ever satisfy
fn test_files(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];

    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|kind| kind == "test")
                && !runs_only_with_python(&path)
            {
                found.push(path);
            }
        }
    }

    found.sort();
    Ok(found)
}

/// Where the `firepath` binary this build produced sits
///
/// Cargo names a binary's path in an environment variable only for the crate
/// that declares it, and this is not that crate, so it is found next to the
/// running executable instead. A test binary lives one level deeper, under
/// `deps/`, so both places are tried. A caller that wants to measure some other
/// build passes the path to [`measure`] directly
pub fn firepath_binary() -> Option<PathBuf> {
    let exe = env::current_exe().ok()?;
    let dir = exe.parent()?;
    let name = format!("firepath{}", env::consts::EXE_SUFFIX);
    [dir.join(&name), dir.join("..").join(&name)]
        .into_iter()
        .find(|candidate| candidate.is_file())
}

/// Measure `dir` with `firepath` and render the report, or say why the run
/// cannot happen
///
/// Reports a missing input rather than measuring zero cases against it: a run
/// that found no tests and a run that satisfied none of them both print a zero,
/// and only one of them is a firepath result
fn measured_report(dir: &Path, firepath: Option<&Path>) -> Result<Report, String> {
    if !dir.is_dir() {
        return Err(format!(
            "{} is not checked out, run `just fetch-upstream`",
            dir.display()
        ));
    }
    let Some(firepath) = firepath else {
        return Err("no firepath binary next to this one, run `just build`".to_owned());
    };
    measure(dir, firepath).map_err(|err| err.to_string())
}

/// The full report as JSON, or why the run cannot happen
fn report_json(dir: &Path, firepath: Option<&Path>) -> Result<String, String> {
    measured_report(dir, firepath)?
        .to_json()
        .map_err(|err| format!("the report will not serialize: {err}"))
}

/// The conformance badge as a shields.io endpoint
///
/// Shares [`measured_report`] with [`report_json`]
fn badge_json(dir: &Path, firepath: Option<&Path>) -> Result<String, String> {
    measured_report(dir, firepath)?
        .badge_endpoint()
        .map_err(|err| format!("the badge will not serialize: {err}"))
}

/// The whole body of the `conformance-upstream` command: run the checked-out
/// tests and print either the full report or the badge endpoint as JSON
///
/// `--badge` prints the shields.io endpoint the CI badge reads. Without it the
/// full report prints
#[cfg_attr(coverage_nightly, coverage(off))]
pub fn cli() -> ExitCode {
    let badge = env::args().nth(1).as_deref() == Some("--badge");
    let dir = tests_dir();
    let firepath = firepath_binary();
    let rendered = if badge {
        badge_json(&dir, firepath.as_deref())
    } else {
        report_json(&dir, firepath.as_deref())
    };
    match rendered {
        Ok(json) => {
            println!("{json}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("conformance-upstream: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::fs;

    use sha2::{Digest, Sha256};

    use super::{
        Case, Malformed, Path, PathBuf, Report, TestFile, badge_json, command_word,
        firepath_binary, measure, parse, percent, reads_stdin, report_json, shell_quote,
        test_files, tests_dir, transform,
    };
    use std::fmt::Write as _;

    // A file's worth of the format, covering a plain block, an error block, and
    // the journal text the blocks sit in
    const SAMPLE: &str = "\
2012-01-01 * Opening
    Assets:Cash                 10.00 USD
    Equity:Opening

test bal
                  10.00 USD  Assets:Cash
end test

2012-01-02 * Later
    Assets:Cash                 -1.00 USD
    Expenses:Food

test -f /dev/null bal broken -> 1
__ERROR__
Error: no such file
end test
";

    fn parsed(source: &str) -> TestFile<'_> {
        parse(source).expect("the sample declares cases")
    }

    // Cases are read by position, and one that is not there is a defect in the
    // parse rather than in the test
    fn case<'a>(file: &'a TestFile<'_>, at: usize) -> &'a Case {
        file.cases.get(at).expect("the file declares this case")
    }

    #[test]
    fn a_block_records_its_command_and_body() {
        let file = parsed(SAMPLE);
        let first = file.cases.first().expect("the sample opens with a block");
        assert_eq!(first.command, "bal");
        assert_eq!(
            first.output.as_deref(),
            Some("                  10.00 USD  Assets:Cash\n")
        );
        assert_eq!(first.error, None);
        assert_eq!(first.exit_code, 0);
    }

    #[test]
    fn an_error_section_splits_stderr_from_stdout() {
        let file = parsed(SAMPLE);
        let second = file.cases.get(1).expect("the sample holds two blocks");
        assert_eq!(second.command, "-f /dev/null bal broken");
        assert_eq!(second.exit_code, 1);
        // Everything after __ERROR__ is stderr, and nothing came before it
        assert_eq!(second.output, None);
        assert_eq!(second.error.as_deref(), Some("Error: no such file\n"));
    }

    #[test]
    fn the_journal_is_the_whole_file_blocks_included() {
        let file = parsed(SAMPLE);
        assert_eq!(file.journal, SAMPLE);
        assert!(file.dropped.is_empty());
    }

    #[test]
    fn a_command_naming_its_own_input_is_flagged() {
        let file = parsed(SAMPLE);
        assert!(!case(&file, 0).reads_another_journal());
        assert!(case(&file, 1).reads_another_journal());
    }

    #[test]
    fn a_block_with_no_body_leaves_stdout_uncompared() {
        let file = parsed("2012-01-01 * A\n\ntest bal\nend test\n");
        assert_eq!(case(&file, 0).output, None);
    }

    #[test]
    fn journal_lines_outside_a_block_are_not_expectations() {
        let file = parsed("test bal\nout\nend test\n2012-01-01 * A\n    Assets  1 USD\n");
        assert_eq!(file.cases.len(), 1);
        assert_eq!(case(&file, 0).output.as_deref(), Some("out\n"));
    }

    #[test]
    fn an_arrow_with_no_digits_after_it_stays_in_the_command() {
        // A value expression, not an exit code, and upstream keeps it whole
        let file = parsed("test eval 'foo = x, y, z -> print(x, y, z); foo(1, 2, 3)'\nend test\n");
        assert_eq!(
            case(&file, 0).command,
            "eval 'foo = x, y, z -> print(x, y, z); foo(1, 2, 3)'"
        );
        assert_eq!(case(&file, 0).exit_code, 0);
    }

    #[test]
    fn the_last_arrow_followed_by_digits_is_the_exit_code() {
        let file = parsed("test eval 'x -> y' -> 14\nend test\n");
        assert_eq!(case(&file, 0).command, "eval 'x -> y'");
        assert_eq!(case(&file, 0).exit_code, 14);
    }

    #[test]
    fn a_number_too_large_for_an_exit_status_is_not_one() {
        // The one place the read deviates: upstream takes this as an exit code
        // no process can return, here it stays part of the command
        let file = parsed("test bal -> 99999999999999999999\nend test\n");
        assert_eq!(case(&file, 0).command, "bal -> 99999999999999999999");
        assert_eq!(case(&file, 0).exit_code, 0);
    }

    #[test]
    fn a_replacement_command_without_a_code_keeps_the_one_it_replaced() {
        // Upstream assigns the exit code only from a line that carries one, so
        // the replaced command's code outlives the command itself
        let file = parsed("test bal -> 3\ntest reg\nend test\n");
        assert_eq!(case(&file, 0).command, "reg");
        assert_eq!(case(&file, 0).exit_code, 3);
        // And a replacement that does carry one overwrites it
        let file = parsed("test bal -> 3\ntest reg -> 5\nend test\n");
        assert_eq!(case(&file, 0).exit_code, 5);
    }

    #[test]
    fn an_error_section_runs_past_a_replacement_command() {
        // `__ERROR__` is sticky across a `test` line, so the body after the
        // replacement is still stderr
        let file = parsed("test a\n__ERROR__\nfirst\ntest b\nsecond\nend test\n");
        assert_eq!(case(&file, 0).command, "b");
        assert_eq!(case(&file, 0).output, None);
        assert_eq!(case(&file, 0).error.as_deref(), Some("first\nsecond\n"));
    }

    #[test]
    fn an_error_section_ends_with_its_block() {
        // The next block starts on stdout again rather than inheriting stderr
        let file = parsed("test a\n__ERROR__\nboom\nend test\ntest b\nout\nend test\n");
        assert_eq!(file.cases.len(), 2);
        assert_eq!(case(&file, 1).output.as_deref(), Some("out\n"));
        assert_eq!(case(&file, 1).error, None);
    }

    #[test]
    fn a_body_line_the_file_never_terminated_keeps_no_newline() {
        // Upstream's readline hands back the last line as the file holds it, so
        // an expectation the file cut short expects no trailing newline either
        let file = parsed("test bal\nout");
        assert_eq!(case(&file, 0).output.as_deref(), Some("out"));
    }

    #[test]
    fn a_crlf_line_ending_reaches_the_body_as_a_bare_newline() {
        // Python reads in universal-newline mode, which translates before
        // `read_test` ever sees the line
        let file = parsed("test bal\r\nout\r\nend test\r\n");
        assert_eq!(case(&file, 0).command, "bal");
        assert_eq!(case(&file, 0).output.as_deref(), Some("out\n"));
    }

    #[test]
    fn a_block_left_open_at_the_end_of_the_file_still_yields_its_case() {
        let file = parsed("2012-01-01 * A\n\ntest bal Expenses -> 0\n");
        assert_eq!(file.cases.len(), 1);
        assert_eq!(case(&file, 0).command, "bal Expenses");
        assert_eq!(case(&file, 0).output, None);
    }

    #[test]
    fn a_second_test_line_replaces_the_command_and_reports_the_one_it_dropped() {
        let file = parsed("test reg\ntest -l \"date\" reg\nout\nend test\n");
        assert_eq!(file.cases.len(), 1);
        assert_eq!(case(&file, 0).command, "-l \"date\" reg");
        // The body collected either side of the replacement stays with the case
        assert_eq!(case(&file, 0).output.as_deref(), Some("out\n"));
        assert_eq!(file.dropped, vec!["reg".to_owned()]);
    }

    #[test]
    fn an_empty_file_is_malformed() {
        assert_eq!(parse(""), Err(Malformed::Empty));
        assert_eq!(parse("").unwrap_err().to_string(), "the file is empty");
    }

    #[test]
    fn a_file_with_no_block_is_malformed() {
        let journal = "2012-01-01 * A\n    Assets:Cash   1 USD\n    Equity\n";
        assert_eq!(parse(journal), Err(Malformed::NoCases));
        assert_eq!(
            Malformed::NoCases.to_string(),
            "the file holds no test block"
        );
    }

    #[test]
    fn a_bare_test_word_does_not_open_a_block() {
        // Upstream matches on `test ` with the space, so `test` alone is journal
        assert_eq!(parse("test\nend test\n"), Err(Malformed::NoCases));
    }

    /// Report that the submodule is not checked out, so a test that proves
    /// nothing says so rather than passing quietly
    ///
    /// CI checks the submodule out, so there the absence is a broken workflow
    /// rather than a local convenience and the test fails instead of skipping
    fn skipped_for_missing_submodule(test: &str, path: &Path) {
        assert!(
            std::env::var_os("CI").is_none(),
            "{} is not checked out: CI must run `just fetch-upstream` before the tests",
            path.display()
        );
        eprintln!(
            "SKIP {test}: {} is not checked out, run `just fetch-upstream`",
            path.display()
        );
    }

    fn skipped_for_missing_binary(test: &str) {
        assert!(
            std::env::var_os("CI").is_none(),
            "no firepath binary next to this one: CI runs the whole workspace and must build it"
        );
        eprintln!("SKIP {test}: no firepath binary next to this one, run `just build`");
    }

    /// What one directory of the checked-out upstream tests parses to
    #[derive(Debug, Default, PartialEq, Eq)]
    struct Counts {
        files: usize,
        cases: usize,
        nonzero_exit: usize,
        no_output: usize,
        with_error: usize,
        /// sha256 over every field of every case, in path order, so this pins the
        /// parse byte for byte and not just its shape
        digest: String,
    }

    // Digested exactly as the reference implementation is: file name, command,
    // exit code, then the two bodies with an absent one counting as empty.
    // Each field ends with a NUL, which no field can hold, so a byte crossing a
    // field boundary changes the digest rather than moving inside it
    fn counts_for(dir: &Path) -> Counts {
        let mut paths: Vec<_> = fs::read_dir(dir)
            .expect("the upstream tests directory is readable")
            .map(|entry| entry.expect("the directory entry is readable").path())
            .filter(|path| path.extension().is_some_and(|kind| kind == "test"))
            .collect();
        paths.sort();

        let mut counts = Counts::default();
        let mut digest = Sha256::new();
        for path in &paths {
            let source = fs::read_to_string(path).expect("every test file is UTF-8");
            let file = parse(&source)
                .unwrap_or_else(|err| panic!("{} is malformed: {err}", path.display()));
            counts.files = counts.files.saturating_add(1);
            for case in &file.cases {
                counts.cases = counts.cases.saturating_add(1);
                counts.nonzero_exit = counts
                    .nonzero_exit
                    .saturating_add(usize::from(case.exit_code != 0));
                counts.no_output = counts
                    .no_output
                    .saturating_add(usize::from(case.output.is_none()));
                counts.with_error = counts
                    .with_error
                    .saturating_add(usize::from(case.error.is_some()));
                for field in [
                    path.file_name().unwrap_or_default().as_encoded_bytes(),
                    case.command.as_bytes(),
                    case.exit_code.to_string().as_bytes(),
                    case.output.as_deref().unwrap_or_default().as_bytes(),
                    case.error.as_deref().unwrap_or_default().as_bytes(),
                ] {
                    digest.update(field);
                    digest.update(b"\0");
                }
            }
        }
        counts.digest = format!("{:x}", digest.finalize());
        counts
    }

    #[test]
    fn the_checked_out_upstream_tests_parses_to_its_pinned_counts() {
        let dir = tests_dir();
        if !dir.join("baseline").is_dir() {
            skipped_for_missing_submodule(
                "the_checked_out_upstream_tests_parses_to_its_pinned_counts",
                &dir,
            );
            return;
        }

        // Pinned against ledger v3.4.1. The digests were taken from upstream's
        // own read_test run over the same files, so they pin agreement with the
        // reference implementation and not merely against a previous run of this
        // parser. Bumping the submodule moves them, which is why that bump is a
        // decision and not a chore.
        //
        // Every `.test` file counts here, the ten `_py.test` ones the runner
        // leaves out included (see `test_files`). This pins the parser against
        // the reference implementation over the whole format, which is a
        // separate question from which files the meter is entitled to score
        assert_eq!(
            counts_for(&dir.join("manual")),
            Counts {
                files: 10,
                cases: 11,
                nonzero_exit: 0,
                no_output: 0,
                with_error: 0,
                digest: "432fd7678313f964a3b462b9a297d9b38c14da9527b5ddb5f50686bc15df3f39".into(),
            }
        );
        assert_eq!(
            counts_for(&dir.join("baseline")),
            Counts {
                files: 216,
                cases: 422,
                nonzero_exit: 13,
                no_output: 19,
                with_error: 23,
                digest: "f29cf66bb5b547e44772c5fe454aecd15ee5bdc90ce05da7463250c89423eaa4".into(),
            }
        );
        assert_eq!(
            counts_for(&dir.join("regress")),
            Counts {
                files: 219,
                cases: 352,
                nonzero_exit: 33,
                no_output: 54,
                with_error: 36,
                digest: "669c3ceee1fbd62528467a61fefd78021f8bc50b1e0a49fb4ca2fb30e34df0ca".into(),
            }
        );
    }

    #[test]
    fn the_upstream_tests_carries_one_known_dropped_command() {
        let path = tests_dir().join("regress").join("C927CFFE.test");
        let Ok(source) = fs::read_to_string(&path) else {
            skipped_for_missing_submodule(
                "the_upstream_tests_carries_one_known_dropped_command",
                &path,
            );
            return;
        };
        // A stray `test reg` on the line above the real command, in ledger's own
        // upstream tests. Upstream runs five cases here and never reports the sixth
        let file = parsed(&source);
        assert_eq!(file.cases.len(), 5);
        assert_eq!(file.dropped, vec!["reg".to_owned()]);
    }

    #[test]
    fn the_upstream_tests_carries_one_block_the_file_never_closes() {
        let path = tests_dir().join("regress").join("1182_3.test");
        let Ok(source) = fs::read_to_string(&path) else {
            skipped_for_missing_submodule(
                "the_upstream_tests_carries_one_block_the_file_never_closes",
                &path,
            );
            return;
        };
        // The only file whose last `test` line has no `end test` after it, so
        // dropping an unclosed block would silently cost a case
        let file = parsed(&source);
        assert_eq!(file.cases.len(), 1);
        assert_eq!(case(&file, 0).command, "bal Expenses:Cookies");
        assert_eq!(case(&file, 0).output, None);
    }

    #[test]
    fn the_command_word_is_found_past_the_options_that_precede_it() {
        assert_eq!(command_word("bal"), Some("bal"));
        assert_eq!(command_word("-f /dev/null reg --flat"), Some("reg"));
        // A single-letter command, which ledger accepts
        assert_eq!(command_word("--now 2012-01-01 b"), Some("b"));
    }

    #[test]
    fn a_command_line_naming_no_ledger_command_has_no_word() {
        // The one shape in the checked-out tests that names none: a global
        // option that does the work, with no command after it
        assert_eq!(command_word("--script test/baseline/opt-script.dat"), None);
        assert_eq!(command_word(""), None);
    }

    #[test]
    fn a_submodule_bump_that_adds_a_command_is_caught_here() {
        // COMMAND_WORDS is transcribed from ledger's dispatch tables, so a bump
        // that adds a command word leaves it stale. Staleness shows up as a
        // command line the vocabulary cannot read, and there is exactly one of
        // those in the pinned checkout: a global option that does the work with
        // no command after it. Anything else here is a word to add to the list
        let dir = tests_dir();
        if !dir.join("baseline").is_dir() {
            skipped_for_missing_submodule(
                "a_submodule_bump_that_adds_a_command_is_caught_here",
                &dir,
            );
            return;
        }

        let mut unreadable = Vec::new();
        for path in test_files(&dir).expect("the upstream tests are readable") {
            let source = fs::read_to_string(&path).expect("every test file is UTF-8");
            let Ok(file) = parse(&source) else { continue };
            for case in &file.cases {
                if command_word(&case.command).is_none() {
                    unreadable.push(case.command.clone());
                }
            }
        }

        assert_eq!(unreadable, ["--script test/baseline/opt-script.dat"]);
    }

    #[test]
    fn a_percentage_rounds_to_two_places_and_survives_an_empty_total() {
        assert!((percent(0, 0) - 0.0).abs() < f64::EPSILON);
        assert!((percent(1, 3) - 33.33).abs() < f64::EPSILON);
        assert!((percent(2, 3) - 66.67).abs() < f64::EPSILON);
        assert!((percent(7, 7) - 100.0).abs() < f64::EPSILON);
    }

    /// A directory of `.test` files to measure, one per name given
    fn tests_in(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        for (name, body) in files {
            let path = dir.path().join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, body).unwrap();
        }
        dir
    }

    /// A stand-in for the binary under test, so the runner can be driven
    /// through every outcome without depending on what firepath does today
    ///
    /// `body` is the shell script's whole body: whatever it writes and the
    /// status it exits with is what the harness sees come back
    #[cfg(unix)]
    fn stub(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;

        let path = dir.join("stub-firepath");
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn a_case_that_declared_no_output_passes_on_a_silent_zero_exit() {
        // The shape that makes ledger's crash regressions passable: the block
        // asserts only that the command survived, so anything on stdout does
        let dir = tests_in(&[("crash.test", "test bal foo -M\nend test\n")]);
        let bin = stub(dir.path(), "echo whatever this prints is not compared");

        let report = measure(dir.path(), &bin).unwrap();
        assert_eq!(report.cases, 1);
        assert_eq!(report.passing, 1);
        assert!((report.conformance() - 100.0).abs() < f64::EPSILON);
        assert!(report.failures.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_declared_body_is_compared_and_a_mismatch_names_the_command() {
        let file = "test bal\n  10.00 USD  Assets:Cash\nend test\n";
        let dir = tests_in(&[("bal.test", file)]);

        let matching = stub(dir.path(), "echo '  10.00 USD  Assets:Cash'");
        assert_eq!(measure(dir.path(), &matching).unwrap().passing, 1);

        let wrong = stub(dir.path(), "echo '  99.99 USD  Assets:Cash'");
        let report = measure(dir.path(), &wrong).unwrap();
        assert_eq!(report.passing, 0);
        assert_eq!(report.failures.get("stdout: bal"), Some(&1));
    }

    #[cfg(unix)]
    #[test]
    fn stderr_is_compared_even_when_the_block_declared_none() {
        // `None` for stderr is an assertion, not an absence: upstream demands it
        // stay empty. This is the arm that catches a command firepath refuses
        let dir = tests_in(&[("quiet.test", "test bal\nend test\n")]);
        let bin = stub(dir.path(), "echo 'error: unrecognized subcommand' >&2");

        let report = measure(dir.path(), &bin).unwrap();
        assert_eq!(report.failures.get("stderr: bal"), Some(&1));
    }

    #[cfg(unix)]
    #[test]
    fn an_exit_status_that_does_not_match_is_its_own_category() {
        let dir = tests_in(&[("code.test", "test bal -> 1\nend test\n")]);
        let bin = stub(dir.path(), "exit 0");

        let report = measure(dir.path(), &bin).unwrap();
        assert_eq!(report.failures.get("exit code: bal"), Some(&1));
    }

    #[cfg(unix)]
    #[test]
    fn a_command_line_naming_no_command_still_reports_a_category() {
        let dir = tests_in(&[("script.test", "test --script x.dat\nend test\n")]);
        let bin = stub(dir.path(), "echo boom >&2");

        let report = measure(dir.path(), &bin).unwrap();
        assert_eq!(
            report.failures.get("stderr: no ledger command on the line"),
            Some(&1)
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_command_that_exits_before_reading_stdin_does_not_fail_the_run() {
        // Far past any pipe buffer, so the write cannot complete into a
        // command that is already gone. That is the command's behaviour, not a
        // broken harness, and what it wrote before exiting is still its answer
        let mut journal = String::from("test -f - print\nend test\n");
        for line in 0..20_000 {
            let _ = writeln!(journal, "; padding line {line} past the pipe buffer");
        }
        let dir = tests_in(&[("big.test", &journal)]);
        let bin = stub(dir.path(), "exit 7");

        let report = measure(dir.path(), &bin).unwrap();
        assert_eq!(report.cases, 1);
        assert_eq!(report.failures.get("exit code: print"), Some(&1));
    }

    #[cfg(unix)]
    #[test]
    fn a_command_echoing_more_than_a_pipe_holds_does_not_wedge_the_run() {
        // The journal is written while the answer is read back. Written first
        // instead, this wedges: `cat` fills its output pipe, stops reading, and
        // the harness is still blocked on the input pipe with nothing draining
        // the output one. A regression hangs here rather than failing
        let mut journal = String::from("test -f - print\nend test\n");
        for line in 0..20_000 {
            let _ = writeln!(journal, "; padding line {line} past the pipe buffer");
        }
        let dir = tests_in(&[("echo.test", &journal)]);
        let bin = stub(dir.path(), "cat");

        let report = measure(dir.path(), &bin).unwrap();
        // The block declared no body, so the echoed journal is not compared: what
        // is under test is that the run finished at all
        assert_eq!(report.cases, 1);
        assert_eq!(report.passing, 1, "{:?}", report.failures);
    }

    #[cfg(unix)]
    #[test]
    fn the_source_root_a_case_expands_is_the_directory_the_command_runs_in() {
        // `$sourcepath` is compared against paths the command resolved itself,
        // and `tests_dir` names the checkout through `../..`. Changing into a
        // directory drops the `..`, so an unresolved root expands into a second
        // spelling of the same place that no output can ever equal
        let dir = tests_in(&[("cwd.test", "test bal\n$sourcepath\nend test\n")]);
        // `-P`, so the answer is the resolved directory rather than whatever
        // `PWD` this process was started with says
        let bin = stub(dir.path(), "pwd -P");
        let name = dir
            .path()
            .file_name()
            .expect("the temp directory has a name");
        let through_parent = dir.path().join("..").join(name);

        let report = measure(&through_parent, &bin).unwrap();
        assert_eq!(report.passing, 1, "{:?}", report.failures);
    }

    #[cfg(unix)]
    #[test]
    fn the_journal_reaches_the_command_on_stdin_when_it_asks_for_one() {
        // Upstream feeds the test file in whenever the command reads `-`, and
        // the journal is the whole file, expectation blocks included
        let dir = tests_in(&[("stdin.test", "test -f - print\n2012-01-01 * A\nend test\n")]);
        let bin = stub(dir.path(), "grep -c . -");

        let report = measure(dir.path(), &bin).unwrap();
        // Three non-empty lines came back, which only a fed stdin can produce
        assert_eq!(report.failures.get("stdout: print"), Some(&1));
    }

    #[test]
    fn the_placeholders_upstream_expands_are_expanded_here() {
        let expanded = transform(
            "While parsing file \"$FILE\", line 4:\n$sourcepath/test/x.dat\n",
            Path::new("/src/ledger"),
            Path::new("/src/ledger/test/regress/1036.test"),
        );
        assert_eq!(
            expanded,
            "While parsing file \"/src/ledger/test/regress/1036.test\", line 4:\n/src/ledger/test/x.dat\n"
        );
    }

    #[test]
    fn a_command_reading_stdin_is_told_apart_from_one_reading_a_file() {
        assert!(reads_stdin("-f - reg"));
        assert!(reads_stdin("-f /dev/stdin reg"));
        assert!(reads_stdin("-f x.dat -f - reg"));
        assert!(!reads_stdin("-f /dev/null reg"));
        assert!(!reads_stdin("bal"));
        // A trailing `-f` with nothing after it names no input at all
        assert!(!reads_stdin("reg -f "));
    }

    #[test]
    fn a_path_becomes_one_shell_word_however_it_is_spelled() {
        assert_eq!(shell_quote(Path::new("/a/b c.test")), "'/a/b c.test'");
        assert_eq!(shell_quote(Path::new("/a/it's.test")), r"'/a/it'\''s.test'");
    }

    #[cfg(unix)]
    #[test]
    fn a_file_that_declares_no_case_is_reported_rather_than_dropped() {
        let dir = tests_in(&[
            ("empty.test", ""),
            (
                "no-block.test",
                "2012-01-01 * A\n    Assets:Cash  1 USD\n    Equity\n",
            ),
        ]);
        let bin = stub(dir.path(), "exit 0");

        let report = measure(dir.path(), &bin).unwrap();
        assert_eq!(report.cases, 0);
        assert_eq!(report.malformed.get("the file is empty"), Some(&1));
        assert_eq!(
            report.malformed.get("the file holds no test block"),
            Some(&1)
        );
        // Nothing to divide by, so the meter reads zero rather than dividing
        assert!((report.conformance() - 0.0).abs() < f64::EPSILON);
    }

    #[cfg(unix)]
    #[test]
    fn the_grammar_breakdown_counts_errors_without_scoring_them() {
        // A journal the parser refuses still runs its cases, and the refusal is
        // reported so the M1 work left is readable off a run
        let dir = tests_in(&[("dirty.test", "account Assets:Cash\ntest bal\nend test\n")]);
        let bin = stub(dir.path(), "exit 0");

        let report = measure(dir.path(), &bin).unwrap();
        assert_eq!(
            report
                .grammar
                .get("the \"account\" directive is not supported yet"),
            Some(&1)
        );
        // Reported, not scored: the case passed on its own terms
        assert_eq!(report.cases, 1);
        assert_eq!(report.passing, 1);
    }

    #[test]
    fn a_directory_that_is_not_there_fails_the_run() {
        let missing = tests_dir().join("no-such-directory");
        assert!(measure(&missing, Path::new("/bin/true")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_file_that_needs_an_interpreter_is_not_measured() {
        // Upstream registers a `*_py.test` file only when it was built with
        // Boost.Python. firepath has no interpreter to answer one with
        let dir = tests_in(&[
            ("plain.test", "test bal\nend test\n"),
            ("dir-python_py.test", "test python\nend test\n"),
            // The suffix is the whole name, which upstream's glob matches too
            ("_py.test", "test python\nend test\n"),
        ]);
        let bin = stub(dir.path(), "exit 0");

        let report = measure(dir.path(), &bin).unwrap();
        assert_eq!(report.cases, 1);
    }

    #[cfg(unix)]
    #[test]
    fn a_case_in_a_subdirectory_is_measured_like_any_other() {
        // Upstream keeps its tests in `manual`, `baseline`, and `regress`, so the
        // walk has to descend rather than read the directory it is handed
        let dir = tests_in(&[("regress/nested.test", "test bal\nend test\n")]);
        let bin = stub(dir.path(), "exit 0");

        let report = measure(dir.path(), &bin).unwrap();
        assert_eq!(report.cases, 1);
        assert_eq!(report.passing, 1);
    }

    #[cfg(unix)]
    #[test]
    fn a_subdirectory_that_cannot_be_walked_fails_the_run() {
        // Same reason an unreadable file does: a walk that swallowed the
        // directory it could not open would report a smaller denominator and a
        // higher percentage than the tests justify
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tests_in(&[("real.test", SAMPLE)]);
        let closed = dir.path().join("closed");
        fs::create_dir(&closed).unwrap();
        fs::set_permissions(&closed, fs::Permissions::from_mode(0o000)).unwrap();

        let measured = measure(dir.path(), Path::new("/bin/true"));
        // Reopened before the assertion, or the temp directory cannot be removed
        fs::set_permissions(&closed, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(measured.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_test_file_that_cannot_be_read_fails_the_run() {
        // A measurement that skipped an unreadable input would report a smaller
        // denominator and a higher percentage than the tests justify
        let dir = tests_in(&[("real.test", SAMPLE)]);
        std::os::unix::fs::symlink(dir.path().join("gone"), dir.path().join("dangling.test"))
            .unwrap();

        assert!(measure(dir.path(), Path::new("/bin/true")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn the_json_carries_the_meter_and_every_category() {
        let dir = tests_in(&[("clean.test", SAMPLE)]);
        let bin = stub(dir.path(), "echo nope >&2");
        let json = measure(dir.path(), &bin).unwrap().to_json().unwrap();

        assert!(json.contains("\"cases\": 2"));
        assert!(json.contains("\"passing\": 0"));
        assert!(json.contains("\"conformance_pct\": 0.0"));
        // The sample's two cases fail on different comparisons: the first
        // declared a stdout body the stub does not write, the second declared an
        // `__ERROR__` body that is not what the stub writes
        assert!(json.contains("\"stdout: bal\": 1"), "{json}");
        assert!(json.contains("\"stderr: bal\": 1"), "{json}");
    }

    #[cfg(unix)]
    #[test]
    fn the_command_reports_a_missing_input_rather_than_measuring_nothing() {
        // Both of these would otherwise print a zero conformance, which reads as
        // a firepath result rather than as a harness that never ran
        let dir = tests_in(&[("clean.test", SAMPLE)]);
        let bin = stub(dir.path(), "exit 0");

        let absent = dir.path().join("no-such-directory");
        let err = report_json(&absent, Some(&bin)).expect_err("the directory is not there");
        assert!(
            err.ends_with("is not checked out, run `just fetch-upstream`"),
            "{err}"
        );

        let err = report_json(dir.path(), None).expect_err("no binary was named");
        assert_eq!(err, "no firepath binary next to this one, run `just build`");
    }

    #[cfg(unix)]
    #[test]
    fn the_command_prints_the_report_it_measured() {
        let dir = tests_in(&[("clean.test", SAMPLE)]);
        let bin = stub(dir.path(), "exit 0");

        let json = report_json(dir.path(), Some(&bin)).expect("the run completes");
        assert!(json.contains("\"cases\": 2"), "{json}");
    }

    #[test]
    fn the_badge_color_climbs_from_red_to_bright_green() {
        // Every band boundary, and the value just under it, so the ramp cannot
        // slide a threshold without a test noticing
        let color = |passing, cases| {
            Report {
                cases,
                passing,
                ..Report::default()
            }
            .badge_color()
        };
        assert_eq!(color(0, 775), "red");
        assert_eq!(color(24, 100), "red");
        assert_eq!(color(25, 100), "orange");
        assert_eq!(color(49, 100), "orange");
        assert_eq!(color(50, 100), "yellow");
        assert_eq!(color(74, 100), "yellow");
        assert_eq!(color(75, 100), "yellowgreen");
        assert_eq!(color(89, 100), "yellowgreen");
        assert_eq!(color(90, 100), "green");
        assert_eq!(color(99, 100), "green");
        assert_eq!(color(100, 100), "brightgreen");
    }

    #[test]
    fn the_badge_endpoint_is_a_shields_object() {
        // Compact JSON, so the field separators are bare colons. The message is
        // the fraction followed by the percentage it works out to
        let badge = Report {
            cases: 775,
            passing: 3,
            ..Report::default()
        }
        .badge_endpoint()
        .expect("the badge serializes");
        assert!(badge.contains("\"schemaVersion\":1"), "{badge}");
        assert!(badge.contains("\"label\":\"ledger compat\""), "{badge}");
        assert!(badge.contains("\"message\":\"3/775 (0.39%)\""), "{badge}");
        assert!(badge.contains("\"color\":\"red\""), "{badge}");

        // Full parity drops the trailing zeros, so the badge reads `100%`
        // rather than `100.00%`
        let full = Report {
            cases: 775,
            passing: 775,
            ..Report::default()
        }
        .badge_endpoint()
        .expect("the badge serializes");
        assert!(full.contains("\"message\":\"775/775 (100%)\""), "{full}");
        assert!(full.contains("\"color\":\"brightgreen\""), "{full}");

        // Nothing to divide by is 0%, never the NaN a bare division would put
        // in the message
        let empty = Report::default()
            .badge_endpoint()
            .expect("the badge serializes");
        assert!(empty.contains("\"message\":\"0/0 (0%)\""), "{empty}");
    }

    #[cfg(unix)]
    #[test]
    fn the_badge_command_measures_before_it_paints() {
        let dir = tests_in(&[("clean.test", SAMPLE)]);
        let bin = stub(dir.path(), "exit 0");

        // The badge shares the report's guard: a run that cannot happen is an
        // error, never a zero-case badge that would read as a firepath result
        let absent = dir.path().join("no-such-directory");
        let err = badge_json(&absent, Some(&bin)).expect_err("the directory is not there");
        assert!(
            err.ends_with("is not checked out, run `just fetch-upstream`"),
            "{err}"
        );

        // A real measurement paints a shields object over the cases it ran
        let badge = badge_json(dir.path(), Some(&bin)).expect("the run completes");
        assert!(badge.contains("\"schemaVersion\":1"), "{badge}");
        assert!(badge.contains("\"label\":\"ledger compat\""), "{badge}");
    }

    #[cfg(unix)]
    #[test]
    fn the_command_reports_a_measurement_that_failed_partway() {
        // The directory is there and the binary is there, and the run still
        // cannot finish. Reporting the counts it got to would understate the
        // denominator and overstate the percentage
        let dir = tests_in(&[("real.test", SAMPLE)]);
        std::os::unix::fs::symlink(dir.path().join("gone"), dir.path().join("dangling.test"))
            .unwrap();
        let bin = stub(dir.path(), "exit 0");

        // The message is the operating system's, whose wording is not this
        // crate's to pin: what is under test is that the failure comes back as
        // itself rather than as one of the two guards above it
        let err = report_json(dir.path(), Some(&bin)).expect_err("one file cannot be read");
        assert!(!err.contains("run `just"), "{err}");
    }

    #[test]
    fn the_binary_under_test_is_found_next_to_this_one() {
        // A workspace-wide `cargo test` builds every binary first, so the one
        // under test is a sibling of this test executable. A package-scoped run
        // builds only this package, and then there is nothing to find, which is
        // a narrower invocation rather than a defect. CI runs the workspace
        match firepath_binary() {
            Some(found) => assert!(found.is_file()),
            None => skipped_for_missing_binary("the_binary_under_test_is_found_next_to_this_one"),
        }
    }
}
