//! The deploy's two shell halves of the SDK marker (`src/sdk.rs`'s `SdkMarker`, decision 0296) agree with the
//! core's one writer and reader.
//!
//! * `deploy/docker/vikectl` reads a marker in shell to install a snapshot (`add_sdk`): its number
//!   (`sdk_marker_number`) and its `[source_hash]` table (`sdk_marker_hashes`, what says two snapshots are the same
//!   SDK). Both are RUN here on `SdkMarker::to_toml`'s own text: the number is the marker's, the table carries every
//!   kind's hash, a marker that differs only outside the table (another commit, another boundary) reads the same, and
//!   one with a kind's hash moved does not.
//! * `scripts/package_strategy_source.sh` writes the marker through `src/bin/write_sdk_marker.rs` (the one writer).
//!   The `#[ignore]`d case RUNS the packer on a clone of this checkout tagged `sdk-1` and reads its archive back:
//!   `SdkMarker::read` takes the marker, `SdkMarker::verify` holds it to the snapshot's bytes, and a second packing
//!   of the same tag gives the same bytes. A second `#[ignore]`d case tags `sdk-1` and `sdk-2` and reads the step's
//!   rename table `sdk-2/RENAMES` back through the format's one reader. Lane-only: they run git, cargo and jq and
//!   compile the writer (and xtask).
//!
//! The scripts are read at run time: `deploy/docker` ships in the public mirror, `scripts/` does not, and the ignored
//! case says so and stops where the packer is absent. Scratch lives under `CARGO_TARGET_TMPDIR`, one per process.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use vike_rust_strategy::abi::ABI_VERSION;
use vike_rust_strategy::sdk::{SDK_MARKER_FILE, SdkMarker, SdkSourceHashes, TemplateKind};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// `vikectl`'s one-line marker readers lifted out of the script, then `$1` run on the marker given on stdin.
const RUN: &str = r#"eval "$(grep -E '^sdk_marker_(number|hashes)\(\) \{ .* \}$' "$0")"
declare -F sdk_marker_number > /dev/null && declare -F sdk_marker_hashes > /dev/null ||
  { echo "no sdk_marker_number/sdk_marker_hashes in $0" >&2; exit 3; }
"$1" /dev/stdin"#;

fn vikectl_reads(function: &str, marker: &SdkMarker) -> String {
    let vikectl = workspace_root().join("deploy/docker/vikectl");
    let mut child = Command::new("bash")
        .arg("-c")
        .arg(RUN)
        .arg(&vikectl)
        .arg(function)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("bash runs");
    child.stdin.take().expect("stdin").write_all(marker.to_toml().as_bytes()).expect("the marker");
    let out = child.wait_with_output().expect("bash ends");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("utf-8")
}

fn marker() -> SdkMarker {
    SdkMarker {
        number: 4,
        commit: "a3ca741107249d9f38fb18c4beddc754224438f7".to_string(),
        abi_version: 14,
        boundary_hash: "0123456789abcdef".to_string(),
        source_hash: SdkSourceHashes {
            strategy: "1111111111111111".to_string(),
            onnx: "2222222222222222".to_string(),
            study: "3333333333333333".to_string(),
        },
    }
}

#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "runs the deploy's bash functions, as the Linux boxes do"
)]
fn vikectl_reads_the_marker_the_core_writes() {
    let written = marker();
    assert_eq!(vikectl_reads("sdk_marker_number", &written).trim(), "4", "the number");

    let table = vikectl_reads("sdk_marker_hashes", &written);
    for kind in TemplateKind::ALL {
        assert!(table.contains(written.source_hash.of(kind)), "{kind:?}'s hash in {table:?}");
    }
    assert_eq!(table.lines().count(), TemplateKind::ALL.len(), "one line a kind: {table:?}");

    let elsewhere = SdkMarker {
        commit: "0d8ee58334".to_string(),
        boundary_hash: "fedcba9876543210".to_string(),
        abi_version: 15,
        ..written.clone()
    };
    assert_eq!(vikectl_reads("sdk_marker_hashes", &elsewhere), table, "the same SDK's sources");

    for kind in TemplateKind::ALL {
        let mut moved = written.clone();
        let hash = match kind {
            TemplateKind::Strategy => &mut moved.source_hash.strategy,
            TemplateKind::StrategyOnnx => &mut moved.source_hash.onnx,
            TemplateKind::Study => &mut moved.source_hash.study,
        };
        *hash = "4444444444444444".to_string();
        assert_ne!(vikectl_reads("sdk_marker_hashes", &moved), table, "{kind:?}'s hash moved");
    }
}

fn run(what: &str, command: &mut Command) -> String {
    let out = command.output().unwrap_or_else(|e| panic!("{what}: {e}"));
    assert!(
        out.status.success(),
        "{what} failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
#[ignore = "runs the packer (git, cargo, jq) on a clone of this checkout and compiles the marker writer; lane-only"]
fn the_packer_writes_a_snapshot_whose_marker_the_core_reads_and_verifies() {
    let root = workspace_root();
    if !root.join("scripts/package_strategy_source.sh").is_file() {
        eprintln!(
            "SKIPPED: no scripts/package_strategy_source.sh here (the public mirror ships no scripts/)"
        );
        return;
    }
    let scratch = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("sdk-packer-{}", std::process::id()));
    let _ = fs::remove_dir_all(&scratch);
    fs::create_dir_all(&scratch).expect("scratch");
    // A clone, so the tag this needs is never one of this checkout's.
    let clone = scratch.join("clone");
    run(
        "git clone",
        Command::new("git").args(["clone", "--quiet", "--shared"]).arg(&root).arg(&clone),
    );
    let head =
        run("git rev-parse", Command::new("git").arg("-C").arg(&clone).args(["rev-parse", "HEAD"]));
    run("tagging sdk-1", Command::new("git").arg("-C").arg(&clone).args(["tag", "sdk-1", "HEAD"]));

    // `number`: the packer's second argument, an SDK named instead of the newest (`fetch_release_tools.sh --sdk N`).
    let pack = |name: &str, number: &[&str]| {
        let archive = scratch.join(name);
        run(
            "the packer",
            Command::new("bash")
                .arg("scripts/package_strategy_source.sh")
                .arg(&archive)
                .args(number)
                .current_dir(&clone)
                .env("CARGO_TARGET_DIR", scratch.join("target")),
        );
        fs::read(&archive).expect("the archive")
    };
    let first = pack("first.tar.gz", &[]);

    let unpacked = scratch.join("unpacked");
    fs::create_dir_all(&unpacked).expect("unpacked");
    run(
        "tar",
        Command::new("tar").arg("-xzf").arg(scratch.join("first.tar.gz")).arg("-C").arg(&unpacked),
    );
    let names: Vec<String> = fs::read_dir(&unpacked)
        .expect("listed")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["sdk-1"], "ONE snapshot and nothing beside it");
    let snapshot = unpacked.join("sdk-1");
    assert!(snapshot.join(SDK_MARKER_FILE).is_file(), "the marker at sdk-1/{SDK_MARKER_FILE}");
    let marker = SdkMarker::read(&snapshot).expect("the core reads the packer's marker");
    assert_eq!(
        (marker.number, marker.commit.as_str(), marker.abi_version),
        (1, head.as_str(), ABI_VERSION)
    );
    marker.verify(&snapshot).expect("the marker describes the snapshot's own bytes");

    assert_eq!(pack("second.tar.gz", &[]), first, "one tag packs to one archive, byte for byte");
    assert_eq!(pack("named.tar.gz", &["1"]), first, "SDK 1 named packs what the newest did");
    let _ = fs::remove_dir_all(&scratch);
}

/// SDK 2's snapshot ships the step from SDK 1 beside its marker, `sdk-2/RENAMES`, written at the tag by its own
/// xtask (`sdk-renames`) and read by the format's one reader (`vike_rust_strategy::sdk::rename_table`), which is what
/// `vike-cli strategy migrate` reads. SDK 1 has no step before it, so the case above ships none.
#[test]
#[ignore = "runs the packer (git, cargo, jq) on a clone of this checkout and compiles xtask; lane-only"]
fn the_packer_ships_the_steps_rename_table_beside_the_marker() {
    use vike_rust_strategy::sdk::rename_table::{RENAME_TABLE_FILE, RenameTable};
    let root = workspace_root();
    if !root.join("scripts/package_strategy_source.sh").is_file() {
        eprintln!(
            "SKIPPED: no scripts/package_strategy_source.sh here (the public mirror ships no scripts/)"
        );
        return;
    }
    let scratch = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("sdk-packer-step-{}", std::process::id()));
    let _ = fs::remove_dir_all(&scratch);
    fs::create_dir_all(&scratch).expect("scratch");
    let clone = scratch.join("clone");
    run(
        "git clone",
        Command::new("git").args(["clone", "--quiet", "--shared"]).arg(&root).arg(&clone),
    );
    let git =
        |what: &str, args: &[&str]| run(what, Command::new("git").arg("-C").arg(&clone).args(args));
    let head = git("git rev-parse", &["rev-parse", "HEAD"]);
    git("tagging sdk-1", &["tag", "sdk-1", "HEAD~1"]);
    git("tagging sdk-2", &["tag", "sdk-2", "HEAD"]);
    let archive = scratch.join("sdk-2.tar.gz");
    run(
        "the packer",
        Command::new("bash")
            .arg("scripts/package_strategy_source.sh")
            .arg(&archive)
            .current_dir(&clone)
            .env("CARGO_TARGET_DIR", scratch.join("target")),
    );
    let unpacked = scratch.join("unpacked");
    fs::create_dir_all(&unpacked).expect("unpacked");
    run("tar", Command::new("tar").arg("-xzf").arg(&archive).arg("-C").arg(&unpacked));
    let text = fs::read_to_string(unpacked.join("sdk-2").join(RENAME_TABLE_FILE))
        .expect("sdk-2 ships its step's rename table");
    let table = RenameTable::parse(&text).expect("the format's one reader reads it");
    assert_eq!((table.from.as_str(), table.to.as_str()), ("sdk-1", head.as_str()));
    let _ = fs::remove_dir_all(&scratch);
}
