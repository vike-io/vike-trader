//! What this crate's tests share: [`binaries`] for the integration tests that spawn the shipped
//! binaries, the [`roster`] fixture, and the argv readers ([`try_cli`], [`refused`]) both binaries'
//! unit tests read their `clap` command lines through. Compiled ONCE here behind `test-support` (only
//! the self dev-dependency turns it on) instead of once per binary.

use serde_json::{Value, json};

use crate::eval::harness::Binaries;
use crate::locate_binary;

/// The roster shape `tools/list` answers, including the two fields the API has no slot for.
pub fn roster() -> Vec<Value> {
    vec![
        json!({
            "name": "node_snapshot",
            "description": "Read the running node's live state.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true, "openWorldHint": true }
        }),
        json!({
            "name": "submit_order",
            "description": "Submit an order. WITHOUT confirm:true this only PREVIEWS.",
            "inputSchema": {
                "type": "object",
                "properties": { "venue": { "type": "string" }, "qty": { "type": "number" } },
                "required": ["venue", "qty"]
            },
            "annotations": { "destructiveHint": true }
        }),
    ]
}

/// What `clap` would have done with a command line that did not yield a `C`.
#[derive(Debug)]
pub struct CliExit {
    pub code: i32,
    pub to_stdout: bool,
    pub text: String,
}

/// `line` (the program name left off) read by `C`, as `clap` reads a real argv.
pub fn try_cli<C: clap::Parser>(line: &[&str]) -> Result<C, CliExit> {
    let argv = std::iter::once("bin").chain(line.iter().copied());
    C::try_parse_from(argv).map_err(|e| CliExit {
        code: e.exit_code(),
        to_stdout: !e.use_stderr(),
        text: e.to_string(),
    })
}

/// The usage error the test expects (stderr, exit 2), as its text.
pub fn refused<C: clap::Parser + std::fmt::Debug>(line: &[&str]) -> String {
    let exit = try_cli::<C>(line).expect_err("this command line must be refused");
    assert_eq!((exit.code, exit.to_stdout), (2, false), "{line:?}: {exit:?}");
    exit.text
}

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
