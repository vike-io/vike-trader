//! `cargo run -p vike-rust-strategy --bin write_sdk_marker -- <SNAPSHOT> <NUMBER> <COMMIT>`: write the marker
//! `<SNAPSHOT>/SDK` of SDK `NUMBER`, cut at `COMMIT` (decision 0296), and print it.
//!
//! The packer's door to the ONE marker writer (`vike_rust_strategy::sdk::SdkMarker`'s `of_snapshot` and `write`):
//! `scripts/package_strategy_source.sh` runs it at the tag, over the staged snapshot that ships, so the hashes are of
//! the snapshot's own bytes and the interface version is the tagged commit's ([`ABI_VERSION`] of the build this
//! binary is). Only the packer runs it; no release ships it. A bin, not an example: an example would compile the
//! crate's dev-dependencies (the simulator, the builder) on every runner that packs. It writes once: a marker already
//! there with the same content is left as it is, another one is refused (exit 1).

use std::path::Path;
use std::process::ExitCode;

use vike_rust_strategy::abi::ABI_VERSION;
use vike_rust_strategy::sdk::SdkMarker;

const USAGE: &str = "usage: write_sdk_marker <SNAPSHOT> <NUMBER> <COMMIT>";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [snapshot, number, commit] = args.as_slice() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let Ok(number) = number.parse::<u32>() else {
        eprintln!("write_sdk_marker: '{number}' is not an SDK number\n{USAGE}");
        return ExitCode::from(2);
    };
    let snapshot = Path::new(snapshot);
    let marker = SdkMarker::of_snapshot(snapshot, number, commit, ABI_VERSION);
    match marker.write(snapshot) {
        Ok(()) => {
            print!("{}", marker.to_toml());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("write_sdk_marker: {e}");
            ExitCode::FAILURE
        }
    }
}
