use super::*;

/// A run that says nothing: these tests read what it did, not what it said.
const QUIET: Words<'static> = Words { say: &nothing, warn: &nothing, inherit: false };

fn nothing(_: &str) {}

/// Every forward rides one ssh with the node's first; the backtesthub's rides too (decision 0288).
#[test]
fn the_tunnel_forwards_the_node_then_the_datahub_and_the_backtesthub_and_never_one_port_twice() {
    let line = forward_args(7879).join(" ");
    assert_eq!(line, "-L 7879:localhost:7879 -L 7878:localhost:7878 -L 7880:localhost:7880");
    assert_eq!(
        forward_args(DEFAULT_DATAHUB_PORT).join(" "),
        "-L 7878:localhost:7878 -L 7880:localhost:7880"
    );
    assert_eq!(forward_ports(7879), [7879, DEFAULT_DATAHUB_PORT, DEFAULT_BACKTEST_PORT]);
    assert_eq!(forward_ports(DEFAULT_BACKTEST_PORT), [DEFAULT_BACKTEST_PORT, DEFAULT_DATAHUB_PORT]);
}

/// The forking ssh line is the one every box but Windows always ran, plus `Compression=yes` (the
/// module doc: a Studio answer is JSON over a slow link). The other trades `-f` for `BatchMode`, and
/// keeps the `-L` spec `disconnect`'s PowerShell line finds it by.
#[test]
fn the_forking_ssh_line_compresses_and_the_other_trades_the_flag_for_batch_mode() {
    let forked = ssh_args("user@the CI box", 7879, true).join(" ");
    assert_eq!(
        forked,
        "-N -f -o ExitOnForwardFailure=yes -o ServerAliveInterval=30 -o ServerAliveCountMax=3 \
         -o Compression=yes \
         -L 7879:localhost:7879 -L 7878:localhost:7878 -L 7880:localhost:7880 user@the CI box"
    );
    assert_eq!(
        ssh_args("user@the CI box", 7879, false).join(" "),
        forked.replace("-f ", "-o BatchMode=yes ")
    );
}

/// The launcher's script names every argument as one literal, its own single quotes doubled, and
/// says what its exit means: ssh's own, or that it stopped ssh at the limit.
#[test]
fn the_launcher_script_quotes_each_argument_and_stops_ssh_at_the_limit() {
    let script = unforked_ssh_script(&["-N".to_string(), "o'brien@host".to_string()]);
    assert!(script.contains("-ArgumentList '-N','o''brien@host';"), "{script}");
    assert!(script.contains("Start-Process ssh -WindowStyle Hidden -PassThru"), "{script}");
    assert!(script.contains("WaitForExit(30000)) { exit $p.ExitCode }"), "{script}");
    assert!(script.ends_with(&format!("-Force; exit {TUNNEL_GAVE_UP}")), "{script}");
    assert!(!script.contains('"'), "it travels as one argument: {script}");
    assert!(LAUNCHER_WAIT > TUNNEL_WAIT, "the launcher must be the one to stop ssh");
}

/// An exit outranks ports that answer (they are then not ssh's), and time runs out only on a live
/// ssh that has not answered.
#[test]
fn a_tunnel_is_up_only_while_ssh_lives_and_every_port_answers() {
    let refused = |look: TunnelLook, needle: &str| match look {
        TunnelLook::Refused(why) => assert!(why.contains(needle), "{why}"),
        other => panic!("{other:?} is no refusal"),
    };
    assert_eq!(tunnel_look(None, true, false), TunnelLook::Up);
    assert_eq!(tunnel_look(None, true, true), TunnelLook::Up, "answered as time ran out");
    assert_eq!(tunnel_look(None, false, false), TunnelLook::Wait);
    refused(tunnel_look(None, false, true), "an ssh may be left running");
    refused(tunnel_look(Some(255), true, false), "exit 255");
    refused(tunnel_look(Some(255), false, true), "exit 255");
    refused(tunnel_look(Some(TUNNEL_GAVE_UP), false, false), "did not answer within 30s");
}

/// The wait over a real child, the launcher's shape, which only Windows runs in production: it ends
/// on the child's exit, on every port answering once, and at the deadline, where the child is stopped.
#[cfg(unix)]
#[test]
fn the_wait_ends_on_an_exit_on_every_port_and_at_the_deadline_with_the_child_stopped() {
    let shell = |script: &str| {
        Command::new("sh").args(["-c", script]).spawn().expect("a shell on the lane")
    };
    let never = |_: u16| false;
    let long = Duration::from_secs(20);

    let why = wait_for_forwards(&mut shell("exit 7"), vec![1], never, long).unwrap_err();
    assert!(why.starts_with("exit 7"), "{why}");

    let asked = std::cell::RefCell::new(Vec::new());
    let answers = |port: u16| {
        asked.borrow_mut().push(port);
        port == 1 || asked.borrow().len() > 3
    };
    let mut up = shell("sleep 20");
    assert_eq!(wait_for_forwards(&mut up, vec![1, 2], answers, long), Ok(()));
    assert_eq!(up.try_wait().expect("readable"), None, "an up tunnel is left running");
    assert_eq!(asked.borrow().iter().filter(|p| **p == 1).count(), 1, "asked until it answers");
    up.kill().expect("end the double");
    up.wait().expect("reap the double");

    let mut silent = shell("sleep 20");
    let why = wait_for_forwards(&mut silent, vec![1], never, Duration::from_millis(1)).unwrap_err();
    assert!(why.contains("did not answer in time"), "{why}");
    assert!(silent.try_wait().expect("readable").is_some(), "the child is gone");
}

/// The tunnel record round-trips through the shape `record_tunnel` writes, and a malformed file
/// reads as "no record" rather than as an error — it is a convenience file, and refusing to
/// disconnect because of a broken one would strand a tunnel.
///
/// ⚠ The scratch directory is a `tempfile::TempDir` bound FOR THE WHOLE SCOPE, not a
/// `remove_dir_all` at the end: cleanup that is not in a `Drop` does not run when the work panics.
#[test]
fn a_tunnel_record_round_trips_and_a_broken_one_is_simply_absent() {
    let dir = tempfile::TempDir::new().expect("a scratch directory");
    record_tunnel(Some(dir.path()), "the CI box", 7879, 1, &QUIET);
    let path = tunnel_record_path(Some(dir.path())).expect("a state dir names a record");
    assert_eq!(read_tunnel(&path), Some(("the CI box".to_string(), 7879)));

    std::fs::write(&path, "{ not json").unwrap();
    assert_eq!(read_tunnel(&path), None);
    std::fs::write(&path, r#"{"host":"p"}"#).unwrap();
    assert_eq!(read_tunnel(&path), None, "a record with no port names no tunnel");
    assert_eq!(tunnel_record_path(None), None, "no project, no record");
}

/// **A new `connect` REPLACES the tunnel record, never truncates it in place**: a crash mid-write leaves the
/// last forward on record (a torn file reads as no record, and `disconnect` then cannot close it). A hard link
/// to the old record still holds every old byte after the next one is written.
#[test]
fn a_tunnel_record_never_writes_into_the_old_one() {
    let dir = tempfile::TempDir::new().expect("a scratch directory");
    record_tunnel(Some(dir.path()), "the CI box", 7879, 1, &QUIET);
    let path = tunnel_record_path(Some(dir.path())).expect("a state dir names a record");
    let old = std::fs::read(&path).unwrap();
    let witness = dir.path().join("old-tunnel.json");
    std::fs::hard_link(&path, &witness).unwrap();
    record_tunnel(Some(dir.path()), "the latency box", 7880, 2, &QUIET);
    assert_eq!(std::fs::read(&witness).unwrap(), old, "the old file was never written into");
    assert_eq!(read_tunnel(&path), Some(("the latency box".to_string(), 7880)));
}

/// The remote command registers this box's PUBLIC key with the server project's own `vike-cli`
/// against its own settings, as the owner's device at the role admin, and a path that could break
/// out of the single quotes is refused before anything is sent.
#[test]
fn the_remote_command_registers_with_the_projects_own_cli_and_a_hostile_path_is_refused() {
    assert_eq!(
        remote_command("/srv/vike-<unit>/", "laptop").unwrap(),
        "VIKE_SETTINGS_DIR='/srv/vike-<unit>/settings' '/srv/vike-<unit>/bin/vike-cli' access \
         add owner laptop --role admin"
    );
    for bad in ["relative/path", "/srv/it's", "/srv/a b", "/srv/$(id)", "/srv/`id`", "", "~/v"] {
        let err = remote_command(bad, "pc").unwrap_err();
        assert_eq!(err.rung, Rung::Usage, "{bad}");
        assert!(
            err.message.contains("--server-project") && err.message.contains("Nothing was sent"),
            "{bad}"
        );
    }
    let args = fetch_args("my-node", "CMD");
    assert_eq!(args, ["-T", "-o", "ConnectTimeout=15", "my-node", "CMD"]);
}

/// `--local-server` registers in the container (`docker exec -i`, the container's own settings
/// pointer, the pipe passed) or in the project folder (its `bin/vike-cli`, aimed at its
/// `settings/`), and refuses a relative folder or a container name that reads as a flag before
/// anything runs.
#[test]
fn the_local_server_registers_in_its_container_or_its_folder() {
    let argv = |c: &Command| -> Vec<String> {
        std::iter::once(c.get_program())
            .chain(c.get_args())
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    };
    let docker = local_command("docker:vike", "pc").unwrap();
    assert_eq!(
        argv(&docker).join(" "),
        "docker exec -i vike /opt/vike/bin/vike-cli access add owner pc --role admin"
    );
    assert_eq!(docker.get_envs().count(), 0);
    let folder = local_command("/srv/vike", "pc").unwrap();
    assert_eq!(argv(&folder).join(" "), "/srv/vike/bin/vike-cli access add owner pc --role admin");
    let envs: Vec<_> = folder.get_envs().collect();
    assert_eq!(
        envs,
        [(
            std::ffi::OsStr::new(vike_paths::state_path::SETTINGS_DIR_ENV),
            Some(Path::new("/srv/vike/settings").as_os_str())
        )]
    );
    for bad in ["docker:", "docker:-it", "relative/vike", ""] {
        let err = local_command(bad, "pc").unwrap_err().message;
        assert!(err.contains("--local-server") && err.contains("Nothing was sent"), "{bad}");
    }
}

/// The registration sends the device key's PUBLIC half, one line of 64 lower-case hex, to the
/// command's stdin, and a command that fails is a refusal naming where. With no terminal
/// (`inherit: false`, the desktop) the child's own words come back through `say`, both streams.
#[cfg(unix)]
#[test]
fn the_public_key_goes_to_the_commands_stdin_and_a_failure_is_named() {
    let (key, _) = DeviceKey::generate().unwrap();
    let out = tempfile::NamedTempFile::new().unwrap();
    let mut cat = Command::new("/bin/sh");
    cat.args(["-c", &format!("cat > '{}'", out.path().display())]);
    send_public_key(cat, "here", &key, "pc", &QUIET).unwrap();
    let sent = std::fs::read_to_string(out.path()).unwrap();
    assert_eq!(sent, format!("{}\n", vike_utility::hex::hex_lower(&key.public_key())));

    let said = std::cell::RefCell::new(Vec::<String>::new());
    let keep = |line: &str| said.borrow_mut().push(line.to_string());
    let words = Words { say: &keep, warn: &keep, inherit: false };
    let mut fail = Command::new("/bin/sh");
    fail.args(["-c", "cat > /dev/null; echo granted-not; echo refused-here >&2; exit 3"]);
    let err = send_public_key(fail, "box-x", &key, "pc", &words).unwrap_err();
    assert_eq!(err.rung, Rung::Connect);
    assert!(err.message.contains("box-x") && err.message.contains("exit 3"), "{}", err.message);
    assert_eq!(*said.borrow(), ["granted-not", "refused-here"], "the child's words, both streams");
}
