//! The process around the protocol: flags, startup refusals, stderr, exit codes. ⚠ On every path here
//! stdout is either empty or what was asked of it (`--help`, `--version`): it is the protocol's.

use std::time::Duration;

use super::harness::{Golden, World, run_process};

/// One value of the kind no writer of this tree produces, in a sealed store with the row count intact:
/// the state of `crates/vike-cli/tests/cli_verbs/seal_enforcement.rs`'s `plant_unreadable_row`.
fn plant_an_unsound_seal(world: &World) {
    let dir = world.settings();
    crate::common::seed_store(&dir, &[]);
    vike_config::write_setting_row(
        &dir,
        "policy.max_notional_per_order",
        "250",
        Duration::from_secs(5),
    )
    .expect("the first row seals the store");
    let source = vike_secrets::read_settings_in(&dir).expect("the fixture store opens");
    let mut rows = source.rows().expect("the fixture has rows").clone();
    rows.settings.first_mut().expect("the fixture has a row").value =
        r"'C:\vike\state'".to_string();
    vike_secrets::write_settings_in(&dir, &rows).expect("plant the row");
}

/// A database file that is not one: readable as a file, refused as a store, for any user.
fn plant_an_unreadable_store(world: &World) {
    let db = world.settings().join("db");
    std::fs::create_dir_all(&db).expect("the store's directory");
    std::fs::write(db.join("vike.db"), vec![b'x'; 4096]).expect("plant the store");
}

/// The flags: what each prints, where, and on which exit code.
pub(super) fn flags() -> Vec<Golden> {
    let plain: [(&str, &[&str]); 10] = [
        ("version", &["--version"]),
        ("version_short", &["-V"]),
        ("bad_flag", &["--definitely-not-a-flag"]),
        ("stray_positional", &["stray"]),
        ("profile_nope", &["--profile", "nope"]),
        ("profile_no_value", &["--profile"]),
        ("deny_tool_nope", &["--deny-tool", "nope"]),
        ("deny_tool_hyphen", &["--deny-tool", "submit-order"]),
        ("unattended_with_a_value", &["--unattended=false"]),
        ("no_flags", &[]),
    ];
    let mut out: Vec<Golden> = plain
        .into_iter()
        .map(|(tag, argv)| {
            run_process(&format!("process.{tag}"), World::empty(), argv)
                .what("one command line, stdin closed: stdout, stderr, exit")
                .golden()
        })
        .collect();
    for (tag, flag) in [("help", "--help"), ("help_short", "-h")] {
        out.push(
            run_process(&format!("process.{tag}"), World::empty(), &[flag])
                .what("the help page, on stdout")
                .golden(),
        );
    }
    let world = World::empty();
    let dir = world.arg("agent");
    out.push(
        run_process("process.trace_dir", world, &["--trace-dir", &dir])
            .what("`--trace-dir`: it says where the transcript goes, on stderr")
            .golden(),
    );
    out.push(
        run_process("process.trace_project", World::empty(), &["--trace"])
            .what("`--trace` inside a project")
            .golden(),
    );
    out.push(
        run_process("process.trace_no_project", World::no_project(), &["--trace"])
            .what("`--trace` with no project above the working directory is refused")
            .golden(),
    );
    out.push(
        run_process("process.no_project", World::no_project(), &[])
            .what("no project at all: the server still starts, and says so on stderr")
            .golden(),
    );
    out
}

/// What the boot refuses or warns about before a line of protocol, including that `--help` itself is
/// refused in those states, because the boot runs before the parse.
pub(super) fn boot() -> Vec<Golden> {
    let mut out = Vec::new();
    let removed: [(&str, &[&str]); 2] = [("removed_env", &[]), ("removed_env_help", &["--help"])];
    for (tag, argv) in removed {
        out.push(
            run_process(
                &format!("boot.{tag}"),
                World::empty().env("VIKE_MAX_ORDER_NOTIONAL", "1"),
                argv,
            )
            .what("a removed variable refuses the start, naming what replaced it")
            .golden(),
        );
    }
    out.push(
        run_process(
            "boot.exported_hub_key",
            World::empty().env("VIKE_TRADEHUB_CONTROL_KEY", "DUMMY-NEVER-ECHOED"),
            &[],
        )
        .what("an exported hub key refuses the start, and the value is not echoed")
        .golden(),
    );
    let sealed: [(&str, &[&str]); 2] = [("unsound_seal", &[]), ("unsound_seal_help", &["--help"])];
    for (tag, argv) in sealed {
        let world = World::empty();
        plant_an_unsound_seal(&world);
        out.push(
            run_process(&format!("boot.{tag}"), world, argv)
                .what("a seal that does not hold refuses this actor")
                .golden(),
        );
    }
    let world = World::empty();
    plant_an_unreadable_store(&world);
    out.push(
        run_process("boot.unreadable_store", world, &[])
            .what("a store that exists and cannot be read refuses this actor")
            .golden(),
    );
    let world = World::empty();
    let indicators = world.path("user_data/indicators");
    std::fs::create_dir_all(&indicators).expect("the user indicators directory");
    std::fs::write(indicators.join("broken.rhai"), "fn broken( {").expect("plant the indicator");
    out.push(
        run_process("boot.bad_user_indicator", world, &[])
            .what("a user indicator that does not compile is reported, and the server still starts")
            .golden(),
    );
    out.extend(store_permission_warning());
    out
}

/// A store a stranger could read is reported on stderr, and the server still starts.
#[cfg(unix)]
fn store_permission_warning() -> Vec<Golden> {
    use std::os::unix::fs::PermissionsExt;

    let world =
        World::with_store(&[("VIKE_TRADEHUB_OBSERVE_KEY", "DUMMY-observe-key-for-mcp-wire")]);
    let db = world.settings().join("db").join("vike.db");
    std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o644)).expect("chmod the store");
    vec![
        run_process("boot.store_permissions", world, &[])
            .what("a store a stranger could read is reported on stderr, and the server starts")
            .golden(),
    ]
}

/// There is no such state to pin where modes do not exist, and no test runs there anyway.
#[cfg(not(unix))]
fn store_permission_warning() -> Vec<Golden> {
    Vec::new()
}
