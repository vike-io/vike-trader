//! The deploy's shell reader of a strategy file's SDK pin answers what this crate's parser answers.
//!
//! `deploy/docker/vikectl`'s `strategy_sdk_pin` reads `[strategy] sdk = N` out of a file's header (decision 0296) for
//! the two things a deploy says about pins: the pre-flight's list of files pinned to an SDK the box will no longer
//! build (`sdk_pin_report`) and the retention that keeps a pinned snapshot (`retire_sdks`). It is a line reader in
//! awk, not a second TOML parser, and its own comment names the spellings it does not see. This test RUNS it, as the
//! deploy does, on every header spelling below and compares its answer with
//! `vike_strategy::file::StrategyFileHeader::parse`'s `strategy.sdk`: the comment markers, blank lines, indentation, a
//! byte-order mark, CRLF, a trailing comment, a spaced table header, a second fenced block and a pin below the code.
//!
//! It reads the script at run time (`deploy/docker` ships in the public mirror) and needs `bash`, `sed` and `awk`,
//! which every box that runs `vikectl` has.

use std::io::Write as _;
use std::process::{Command, Stdio};

use vike_strategy::file::StrategyFileHeader;

const VIKECTL: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../deploy/docker/vikectl");

/// The function lifted out of the script and run on `source`, given on stdin, as the deploy runs it on a file.
const RUN: &str = r#"eval "$(sed -n '/^strategy_sdk_pin() {$/,/^}$/p' "$0")"
declare -F strategy_sdk_pin > /dev/null || { echo "no strategy_sdk_pin in $0" >&2; exit 3; }
strategy_sdk_pin /dev/stdin"#;

fn shell_pin(source: &str) -> Option<u32> {
    let mut child = Command::new("bash")
        .args(["-c", RUN, VIKECTL])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("bash runs");
    child.stdin.take().expect("stdin").write_all(source.as_bytes()).expect("the source");
    let out = child.wait_with_output().expect("bash ends");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let said = String::from_utf8(out.stdout).expect("utf-8");
    let said = said.trim();
    (!said.is_empty()).then(|| said.parse().unwrap_or_else(|_| panic!("not a number: {said:?}")))
}

#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "runs the deploy's bash function, as the Linux boxes do"
)]
fn the_shell_pin_reader_agrees_with_the_header_parser() {
    let cases: &[(&str, &str)] = &[
        (
            "a pin",
            "//! A file.\n//!\n//! ```toml\n//! [strategy]\n//! sdk = 3\n//! ```\npub fn build() {}\n",
        ),
        (
            "no pin",
            "//! A file.\n//!\n//! ```toml\n//! [strategy]\n//! runs_on = \"paper\"\n//! ```\n",
        ),
        (
            "a trailing comment",
            "//! ```toml\n//! [strategy]\n//! sdk = 7   # pinned on purpose\n//! ```\n",
        ),
        ("`///` lines", "/// ```toml\n/// [strategy]\n/// sdk = 12\n/// ```\n"),
        ("`//` lines", "// ```toml\n// [strategy]\n// fill = \"close\"\n// sdk = 4\n// ```\n"),
        (
            "blank and indented lines",
            "\n  //! Grid.\n\n  //! ```toml\n  //! [strategy]\n\n  //!   sdk = 5\n  //! ```\n",
        ),
        ("CRLF", "//! ```toml\r\n//! [strategy]\r\n//! sdk = 6\r\n//! ```\r\n"),
        ("a byte-order mark", "\u{feff}//! ```toml\n//! [strategy]\n//! sdk = 8\n//! ```\n"),
        (
            "a spaced table header",
            "//! ```toml\n//! [ strategy ]  # the table\n//! sdk = 9\n//! ```\n",
        ),
        (
            "a pin in a SECOND block",
            "//! ```toml\n//! [strategy]\n//! ```\n//! ```toml\n//! sdk = 9\n//! ```\n",
        ),
        (
            "a pin below the code",
            "//! ```toml\n//! [strategy]\n//! ```\npub fn build() {}\n//! sdk = 9\n",
        ),
        (
            "another table, no pin",
            "//! ```toml\n//! [strategy]\n//! [run]\n//! bound_exits = true\n//! ```\n",
        ),
        ("no spaces", "//! ```toml\n//! [strategy]\n//! sdk=11\n//! ```\n"),
        ("zero", "//! ```toml\n//! [strategy]\n//! sdk = 0\n//! ```\n"),
    ];
    for (what, source) in cases {
        let parsed = StrategyFileHeader::parse(source)
            .unwrap_or_else(|e| panic!("{what}: the case must be a header the parser accepts: {e}"))
            .strategy
            .sdk;
        assert_eq!(
            shell_pin(source),
            parsed,
            "{what}: vikectl's strategy_sdk_pin against the parser"
        );
    }
    // Not vacuous: the cases above carry pins, and the reader reads them.
    assert_eq!(shell_pin(cases[0].1), Some(3));
}
