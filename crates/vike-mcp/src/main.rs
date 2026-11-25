//! `vike-mcp`: the program. Its process reads (the one environment sweep, the working directory the
//! project walk starts from, the argv), then the library's start and server
//! (`crates/vike-mcp/src/lib.rs`'s `boot` and `run`), which `vike-desktop --mcp` runs too.
//!
//! ⚠ **The order is the program's behaviour, and a refusal in `boot` comes before the argv is read**,
//! so `--help` under an unsound seal or a removed variable is refused like every other start.

use std::process::ExitCode;

fn main() -> ExitCode {
    let cwd = std::env::current_dir().ok();
    match vike_mcp::boot(std::env::vars().collect(), cwd.as_deref()) {
        // argv[0] is the binary.
        Ok(resolved) => vike_mcp::run(std::env::args().skip(1), &resolved),
        Err(code) => code,
    }
}
