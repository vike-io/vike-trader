//! Setup shared by the integration tests that spawn the shipped binaries: [`binaries`] only.

use vike_agent_eval::eval::harness::Binaries;
use vike_agent_eval::locate_binary;

/// The two binaries the harness spawns.
///
/// In CI they are already there, because cargo builds a package's BIN targets whenever it builds
/// that package's integration tests (that is what `CARGO_BIN_EXE_<name>` names, uplifted to
/// `target/debug/<name>`) and the `test` job's `-p` list names both packages. ⚠ This read "because
/// they are workspace members" until CI run 34020320299 refuted it: the job then BUILT only the
/// affected crates, a change confined to this crate selected neither daemon, and
/// `xtask::ci::tables::roster::BINARY_DRIVER_COMPANIONS` was written to put them in the list. The job now
/// builds the WHOLE roster (`xtask::ci::plan::Plan::roster`) and only RUNS the affected crates' tests, so
/// both binaries are built on every run (measured — that table's doc carries it), and the table no
/// longer adds anything to the lane: their own tests do not run on a change confined to this crate.
///
/// ⚠ A MISSING BINARY FAILS IMMEDIATELY, NAMING THE BUILD — it does not shell out to cargo. That
/// was the first shape here and it is a trap under the runner this workspace uses: nextest KILLS a
/// test that outruns its budget, and `.config/nextest.toml` states this package's (raised for the
/// processes this suite spawns, not for a build). A cold build of those two crates outruns any
/// budget written for a test, so the convenience would have turned "you have not built the
/// binaries" into a killed test with no reason attached — on precisely the developer's first run it
/// was written to help.
pub fn binaries() -> Binaries {
    let (cli, tradehub) = (locate_binary("vike-cli", None), locate_binary("vike-tradehub", None));
    match (cli, tradehub) {
        (Ok(vike_cli), Ok(vike_tradehub)) => Binaries { vike_cli, vike_tradehub },
        (cli, tradehub) => panic!(
            "this test drives the SHIPPED binaries and neither is built in this tree.\n  \
             vike-cli:       {}\n  vike-tradehub:  {}\n\
             Build them first, then re-run:\n  \
             cargo build -p vike-cli -p vike-tradehub --bin vike-cli --bin vike-tradehub",
            cli.map_or_else(|e| e, |p| p.display().to_string()),
            tradehub.map_or_else(|e| e, |p| p.display().to_string()),
        ),
    }
}
