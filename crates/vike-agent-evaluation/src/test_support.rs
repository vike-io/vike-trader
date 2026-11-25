//! What this crate's tests share: [`binaries`] for the integration tests that spawn the shipped
//! binaries, the [`roster`] fixture, and the argv readers ([`try_cli`], [`refused`]) both binaries'
//! unit tests read their `clap` command lines through. Compiled ONCE here behind `test-support` (only
//! the self dev-dependency turns it on) instead of once per binary.

use serde_json::{Value, json};

use crate::evaluation::harness::Binaries;
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

/// The binaries the harness spawns.
///
/// In CI they are already there, because cargo builds a package's BIN targets whenever it builds
/// that package's integration tests (that is what `CARGO_BIN_EXE_<name>` names, uplifted to
/// `target/debug/<name>`) and the `test` job's `-p` list names every package. A test build turns on the
/// features the two price-path binaries need (each crate's self dev edge): `vike-datahub`'s `datafusion-serve`
/// (without it the bin is a stub that exits) and `vike-gatewayhub`'s `test-support` (without it
/// `vike-gatewayhub-scripted` is not built at all), so a plain `cargo build` of either is not enough. ⚠ This read "because
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
    let (cli, mcp, tradehub, datahub, gatewayhub) = (
        locate_binary("vike-cli", None),
        locate_binary("vike-mcp", None),
        locate_binary("vike-tradehub", None),
        locate_binary("vike-datahub", None),
        locate_binary("vike-gatewayhub-scripted", None),
    );
    match (cli, mcp, tradehub, datahub, gatewayhub) {
        (
            Ok(vike_cli),
            Ok(vike_mcp),
            Ok(vike_tradehub),
            Ok(vike_datahub),
            Ok(vike_gatewayhub_scripted),
        ) => Binaries { vike_cli, vike_mcp, vike_tradehub, vike_datahub, vike_gatewayhub_scripted },
        (cli, mcp, tradehub, datahub, gatewayhub) => panic!(
            "this test drives the SHIPPED binaries and not every one is built in this tree.\n  \
             vike-cli:                  {}\n  vike-mcp:                  {}\n  vike-tradehub:             {}\n  \
             vike-datahub:              {}\n  vike-gatewayhub-scripted:  {}\n\
             Build them first, then re-run:\n  \
             cargo build -p vike-cli -p vike-mcp -p vike-tradehub --bin vike-cli --bin vike-mcp --bin vike-tradehub\n  \
             cargo build -p vike-datahub --features datafusion-serve --bin vike-datahub\n  \
             cargo build -p vike-gatewayhub --features test-support,data --bin vike-gatewayhub-scripted",
            cli.map_or_else(|e| e, |p| p.display().to_string()),
            mcp.map_or_else(|e| e, |p| p.display().to_string()),
            tradehub.map_or_else(|e| e, |p| p.display().to_string()),
            datahub.map_or_else(|e| e, |p| p.display().to_string()),
            gatewayhub.map_or_else(|e| e, |p| p.display().to_string()),
        ),
    }
}
