//! `vike-gatewayhub-scripted` — TEST SUPPORT, never shipped: a thin shim over
//! [`vike_gatewayhub::testkit::scripted_daemon::run`], the real gatewayhub daemon over a scripted polymarket lane
//! (that module's doc). Built only with the `test-support` and `data` features (its `[[bin]]` row's
//! `required-features`), which every test build of this crate turns on and no release build does.
//!
//! Like `src/bin/vike-gatewayhub.rs`, this file is the composition root: the ONE `std::env::vars()` sweep and the ONE
//! `current_dir()`.

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let vars: std::collections::HashMap<String, String> = std::env::vars().collect();
    let cwd = std::env::current_dir().ok();
    vike_gatewayhub::testkit::scripted_daemon::run(&vars, cwd.as_deref(), &args)
}
