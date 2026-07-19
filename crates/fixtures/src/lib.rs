//! `firepath-fixtures`: generate the ledger fixtures and check them against the
//! pinned hashes in the committed manifest
//!
//! Crate convention: never iterate a std `HashMap`. Output order must depend
//! only on the inputs, never on a per-process hash seed, so this crate uses
//! `BTreeMap` or sorted vecs everywhere output order could otherwise leak into a
//! file.

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

mod banking;
mod household;
pub mod manifest;
mod rng;

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::Write as _;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use sha2::{Digest, Sha256};

use crate::household::Household;
use crate::rng::Rng;

pub use crate::manifest::Manifest;
use crate::manifest::SCHEMA_VERSION;

/// Generate the ledger fixtures from the committed manifest
#[derive(Debug, Parser)]
#[command(version, about, long_about = None)]
struct Args {
    /// Record the newly generated hashes into the manifest instead of checking
    /// against them
    #[arg(long)]
    pin: bool,
}

/// Where the generator reads the manifest and writes the fixtures
#[derive(Debug)]
struct Paths {
    manifest: PathBuf,
    data_dir: PathBuf,
}

/// The whole body of the `gen-fixtures` command: parse the arguments, locate
/// the files, then pin or verify
///
/// Locates the files from the crate's own path and not the working directory,
/// so the command behaves identically however it is launched
// Tested via `just check` in CI, so no need to instrument the entry point
#[cfg_attr(coverage_nightly, coverage(off))]
pub fn cli() -> ExitCode {
    let args = Args::parse();
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    match workspace_paths(crate_dir).and_then(|paths| run(&paths, args.pin)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("gen-fixtures: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Derive the manifest and fixtures paths from the crate's own directory
///
/// `manifest.toml` sits next to the crate manifest, the fixtures land under
/// `data/fixtures` at the workspace root two levels up
fn workspace_paths(crate_dir: &Path) -> Result<Paths, Box<dyn Error>> {
    let root = crate_dir
        .parent()
        .and_then(Path::parent)
        .ok_or("cannot locate the workspace root")?;
    Ok(Paths {
        manifest: crate_dir.join("manifest.toml"),
        data_dir: root.join("data").join("fixtures"),
    })
}

/// Load the manifest, generate the fixtures, then either pin or verify them
fn run(paths: &Paths, pin: bool) -> Result<(), Box<dyn Error>> {
    // Fall back to the in-code defaults when the manifest is gone, so it comes
    // back pinned to the recorded window rather than to today
    let (mut manifest, missing) = match fs::read_to_string(&paths.manifest) {
        Ok(text) => (toml::from_str::<Manifest>(&text)?, false),
        Err(err) if err.kind() == ErrorKind::NotFound => (Manifest::default(), true),
        Err(err) => return Err(err.into()),
    };

    // This build only knows how to produce the current layout, so refuse a
    // manifest that declares a different schema rather than pinning or verifying
    // the wrong shape. A default manifest carries the current version, so this
    // only ever rejects a loaded one
    if manifest.schema_version != SCHEMA_VERSION {
        return Err(format!(
            "manifest schema version {} is not supported, this build generates version {SCHEMA_VERSION}",
            manifest.schema_version
        )
        .into());
    }

    let files = generate(&manifest)?;
    let computed = hashes_of(&files);

    if pin {
        manifest.hashes = Some(computed);
        write_manifest(&paths.manifest, &manifest)?;
        write_files(&paths.data_dir, &files)?;
        println!("pinned {} file(s)", files.len());
        return Ok(());
    }

    // If a manifest is missing, create a new one without hashes
    if missing {
        write_manifest(&paths.manifest, &manifest)?;
    }

    match manifest.hashes {
        None => Err("manifest has no pinned hashes, run `just gen-fixtures --pin`".into()),
        Some(pinned) => {
            verify(&pinned, &computed)?;
            write_files(&paths.data_dir, &files)?;
            println!("verified {} file(s)", files.len());
            Ok(())
        }
    }
}

/// Build the fixtures in memory, keyed by file name relative to the fixtures dir
///
/// A `main.ledger` carries a header comment recording the inputs plus an
/// include skeleton, one journal per calendar year in the window. Each year's
/// journal holds that year's banking activity, generated from the committed
/// household and the seeded generator.
///
/// One generator is seeded from the manifest and threaded through the years in
/// ascending order, so the whole fixture is a stable function of the seed and the
/// window.
///
/// # Errors
/// Fails when the manifest window cannot be read as a year range, a reversed
/// window included
pub fn generate(manifest: &Manifest) -> Result<BTreeMap<String, String>, Box<dyn Error>> {
    let years = manifest.years()?;

    let mut files = BTreeMap::new();

    let mut body = String::new();
    body.push_str("; firepath ledger fixtures\n");
    body.push_str("; generated by `just gen-fixtures`, do not edit by hand\n");
    // Echo the inputs into the file so any change to them changes the output,
    // and therefore the hash, catching an unpinned edit to seed or window
    let _ = writeln!(body, "; schema version {}", manifest.schema_version);
    let _ = writeln!(body, "; rng seed {}", manifest.seed);
    let _ = writeln!(
        body,
        "; window {} to {}",
        manifest.window_start, manifest.window_end
    );
    body.push_str(";\n; include skeleton, one journal per calendar year in the window\n");

    let household = Household::sample();
    let mut rng = Rng::new(manifest.seed);
    for year in years {
        let name = format!("transactions/{year}.ledger");
        let _ = writeln!(body, "include {name}");
        files.insert(name, banking::emit_year(year, &household, &mut rng));
    }

    files.insert("main.ledger".to_owned(), body);
    Ok(files)
}

/// sha256 each file's bytes, preserving the sorted key order of the input
fn hashes_of(files: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    files
        .iter()
        .map(|(name, body)| (name.clone(), sha256_hex(body.as_bytes())))
        .collect()
}

/// Lower-case hex sha256 of a byte slice
fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    // sha256 is 32 bytes, so exactly 64 hex chars
    let mut hex = String::with_capacity(64);
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Compare newly computed hashes against the pinned set, reporting the first
/// divergence in deterministic sorted order
fn verify(
    pinned: &BTreeMap<String, String>,
    computed: &BTreeMap<String, String>,
) -> Result<(), Box<dyn Error>> {
    for (name, got) in computed {
        match pinned.get(name) {
            Some(want) if want == got => {}
            Some(want) => {
                return Err(format!(
                    "hash mismatch for {name}: pinned {want}, generated {got}; run `just gen-fixtures --pin` to accept"
                )
                .into());
            }
            None => {
                return Err(format!(
                    "{name} is generated but not pinned, run `just gen-fixtures --pin`"
                )
                .into());
            }
        }
    }
    for name in pinned.keys() {
        if !computed.contains_key(name) {
            return Err(format!(
                "{name} is pinned but no longer generated, run `just gen-fixtures --pin`"
            )
            .into());
        }
    }
    Ok(())
}

/// Serialize the manifest as TOML with a single trailing newline for a clean diff
fn write_manifest(path: &Path, manifest: &Manifest) -> Result<(), Box<dyn Error>> {
    // Normalize to exactly one trailing newline whatever the serializer emits
    let mut text = toml::to_string(manifest)?;
    text.truncate(text.trim_end_matches('\n').len());
    text.push('\n');
    fs::write(path, text)?;
    Ok(())
}

/// Write each generated file under the fixtures directory, creating it if needed
fn write_files(dir: &Path, files: &BTreeMap<String, String>) -> Result<(), Box<dyn Error>> {
    for (name, body) in files {
        let path = dir.join(name);
        // A file name may carry a subdirectory, so ensure each file's parent
        // exists. join keeps dir as an ancestor, so parent never climbs above it
        fs::create_dir_all(path.parent().unwrap_or(dir))?;
        fs::write(path, body)?;
    }
    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::{
        Args, Manifest, Paths, generate, hashes_of, run, sha256_hex, verify, workspace_paths,
        write_manifest,
    };
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};

    // A new, uniquely named scratch dir per test, so parallel tests do not
    // collide
    fn temp_paths(tag: &str) -> Paths {
        let mut dir = std::env::temp_dir();
        dir.push(format!("firepath-fixtures-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Paths {
            manifest: dir.join("manifest.toml"),
            data_dir: dir.join("data"),
        }
    }

    fn cleanup(paths: &Paths) {
        if let Some(parent) = paths.manifest.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    fn data_file(paths: &Paths) -> PathBuf {
        paths.data_dir.join("main.ledger")
    }

    #[test]
    fn args_definition_is_valid() {
        use clap::CommandFactory;
        Args::command().debug_assert();
    }

    #[test]
    fn committed_manifest_hashes_match_generated_output() {
        // Guards the pin against drift under `cargo test`, which CI runs. A
        // change to the generator or the manifest inputs without a re-pin fails
        // here
        let manifest_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("manifest.toml");
        let manifest: Manifest =
            toml::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();

        let pinned = manifest
            .hashes
            .clone()
            .expect("committed manifest must carry pinned hashes, run `just gen-fixtures --pin`");
        let computed = hashes_of(&generate(&manifest).unwrap());

        assert_eq!(
            computed, pinned,
            "committed fixtures drifted from the pinned hashes, run `just gen-fixtures --pin`"
        );
    }

    #[test]
    fn an_unsupported_schema_version_is_rejected() {
        let paths = temp_paths("schema");
        run(&paths, true).unwrap();

        // Bump the schema past what this build knows how to produce
        let mut manifest: Manifest =
            toml::from_str(&fs::read_to_string(&paths.manifest).unwrap()).unwrap();
        manifest.schema_version += 1;
        write_manifest(&paths.manifest, &manifest).unwrap();

        let err = run(&paths, false).unwrap_err();
        assert!(err.to_string().contains("schema version"));

        cleanup(&paths);
    }

    #[test]
    fn generation_is_byte_idempotent() {
        let manifest = Manifest::default();
        assert_eq!(generate(&manifest).unwrap(), generate(&manifest).unwrap());
    }

    #[test]
    fn generated_ledger_has_header_and_year_includes() {
        let manifest = Manifest::default();
        let files = generate(&manifest).unwrap();
        let body = files.get("main.ledger").unwrap();
        assert!(body.starts_with("; firepath ledger fixtures\n"));
        assert!(body.contains("include transactions/2015.ledger"));
        assert!(body.contains("include transactions/2024.ledger"));
    }

    #[test]
    fn every_include_resolves_to_a_generated_file() {
        // No dangling includes: every file main.ledger pulls in is also
        // generated, so `ledger` can read the whole fixture, and each year's
        // journal opens with its header comment
        let files = generate(&Manifest::default()).unwrap();
        let main = files.get("main.ledger").unwrap();
        let mut includes = 0;
        for line in main.lines() {
            if let Some(target) = line.strip_prefix("include ") {
                let year = files
                    .get(target)
                    .unwrap_or_else(|| panic!("include target {target:?} is not generated"));
                assert!(year.starts_with("; firepath transactions for "));
                includes += 1;
            }
        }
        assert_eq!(includes, 10, "one include per year in the window");
    }

    #[test]
    fn sha256_hex_matches_known_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn verify_accepts_a_match_and_rejects_drift_in_both_directions() {
        let mut pinned = BTreeMap::new();
        pinned.insert("main.ledger".to_owned(), "aaa".to_owned());
        let mut computed = pinned.clone();
        assert!(verify(&pinned, &computed).is_ok());

        // A changed hash for a known file
        computed.insert("main.ledger".to_owned(), "bbb".to_owned());
        assert!(verify(&pinned, &computed).is_err());

        // A file generated but not pinned
        computed.insert("main.ledger".to_owned(), "aaa".to_owned());
        computed.insert("extra.ledger".to_owned(), "ccc".to_owned());
        assert!(verify(&pinned, &computed).is_err());

        // A file pinned but no longer generated. Order the names so
        // a matching entry is checked before the loop reaches the missing one
        let mut pinned_more = BTreeMap::new();
        pinned_more.insert("a.ledger".to_owned(), "x".to_owned());
        pinned_more.insert("z.ledger".to_owned(), "y".to_owned());
        let mut computed_fewer = BTreeMap::new();
        computed_fewer.insert("a.ledger".to_owned(), "x".to_owned());
        assert!(verify(&pinned_more, &computed_fewer).is_err());
    }

    #[test]
    fn workspace_paths_derive_from_the_crate_dir() {
        let paths = workspace_paths(Path::new("/repo/crates/fixtures")).unwrap();
        assert_eq!(
            paths.manifest,
            PathBuf::from("/repo/crates/fixtures/manifest.toml")
        );
        assert_eq!(paths.data_dir, PathBuf::from("/repo/data/fixtures"));
    }

    #[test]
    fn workspace_paths_reject_a_shallow_crate_dir() {
        // Fewer than two ancestors means there is no workspace root to anchor to
        assert!(workspace_paths(Path::new("fixtures")).is_err());
    }

    #[test]
    fn generate_rejects_a_reversed_window() {
        let manifest = Manifest {
            window_start: "2024-01-01".to_owned(),
            window_end: "2015-12-31".to_owned(),
            ..Manifest::default()
        };
        assert!(generate(&manifest).is_err());
    }

    #[test]
    fn a_manifest_that_is_not_a_file_surfaces_the_io_error() {
        let paths = temp_paths("ioerr");
        // A directory where the manifest should be makes the read fail with
        // something other than NotFound
        fs::create_dir_all(&paths.manifest).unwrap();
        assert!(run(&paths, false).is_err());
        cleanup(&paths);
    }

    #[test]
    fn pin_writes_hashes_then_verify_roundtrips() {
        let paths = temp_paths("roundtrip");

        // No manifest yet, so pinning creates it, records hashes, and writes output
        run(&paths, true).unwrap();
        assert!(paths.manifest.exists());
        let manifest_text = fs::read_to_string(&paths.manifest).unwrap();
        assert!(manifest_text.contains("[hashes]"));
        let first = fs::read(data_file(&paths)).unwrap();

        // Verify passes and regenerates byte-identical output
        run(&paths, false).unwrap();
        let second = fs::read(data_file(&paths)).unwrap();
        assert_eq!(first, second);

        cleanup(&paths);
    }

    #[test]
    fn deleting_the_manifest_restores_the_pinned_window() {
        let paths = temp_paths("restore");
        run(&paths, true).unwrap();
        fs::remove_file(&paths.manifest).unwrap();

        // A plain run recreates the manifest but has no pinned hashes to check
        let err = run(&paths, false).unwrap_err();
        assert!(err.to_string().contains("no pinned hashes"));

        let manifest_text = fs::read_to_string(&paths.manifest).unwrap();
        assert!(manifest_text.contains("2015-01-01"));
        assert!(manifest_text.contains("2024-12-31"));
        // Recreated without hashes, pinning stays a deliberate step
        assert!(!manifest_text.contains("[hashes]"));

        cleanup(&paths);
    }

    #[test]
    fn a_drifted_input_fails_verification_until_repinned() {
        let paths = temp_paths("drift");
        run(&paths, true).unwrap();

        // Change an input so generation drifts from the pinned hashes
        let mut manifest: Manifest =
            toml::from_str(&fs::read_to_string(&paths.manifest).unwrap()).unwrap();
        manifest.seed = 7;
        write_manifest(&paths.manifest, &manifest).unwrap();

        let err = run(&paths, false).unwrap_err();
        assert!(err.to_string().contains("hash mismatch"));

        // Only pinning clears it
        run(&paths, true).unwrap();
        run(&paths, false).unwrap();

        cleanup(&paths);
    }
}
