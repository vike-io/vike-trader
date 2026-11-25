use super::*;

/// The line `vikectl` prints reads back into a host and a folder; anything ssh could read as an
/// option, or a folder `vike-cli` would refuse, is refused here first.
#[test]
fn an_address_line_reads_into_a_host_and_a_folder() {
    let line = parse_server_line("  app@203.0.113.7:/home/app/vike/ \n").unwrap();
    assert_eq!(
        line,
        BackendLinkAddress { host: "app@203.0.113.7".into(), project: "/home/app/vike".into() }
    );
    for bad in [
        "",
        "app@host",
        ":/srv/vike",
        "-oProxyCommand=x:/srv/vike",
        "app@host:relative/vike",
        "app@host:/srv/a b",
        "app@host:/srv/$(id)",
        "a b@host:/srv/vike",
    ] {
        assert!(parse_server_line(bad).is_err(), "{bad:?}");
    }
}

/// Only a line that starts as an invitation enables the paste, and an invite run's output yields its line to
/// copy.
#[test]
fn only_an_invitation_enables_the_paste_and_an_invite_run_yields_its_line() {
    assert!(is_invitation("  vike-invite-1 app@box:/srv/vike 1a2b3c4d s k"));
    for not in ["", "vike-invite-1", "vike-invite-2 a b c d", "app@box:/srv/vike"] {
        assert!(!is_invitation(not), "{not:?}");
    }
    let lines = vec!["minting".to_string(), "vike-invite-1 a b c d".to_string()];
    assert_eq!(invitation_in(&lines), Some("vike-invite-1 a b c d"));
    assert_eq!(invitation_in(&lines[..1]), None);
}

/// The Access group's verbs run on the server over the owner's ssh with no prompt: `vikectl` for what
/// touches the ssh lines, the project's `vike-cli` for a role, and a word that could reach the remote
/// shell as anything but a name or a role is refused before anything runs.
#[test]
fn the_access_verbs_run_on_the_server_and_refuse_a_hostile_word() {
    let line = parse_server_line("app@box:/srv/vike").unwrap();
    let remote = |verb: BackendAccessVerb| {
        let args = access_args(&line, &verb).unwrap();
        assert_eq!(args[..6], ["-T", "-o", "BatchMode=yes", "-o", "ConnectTimeout=15", "app@box"]);
        args[6].clone()
    };
    let vikectl = "'/srv/vike/bin/vikectl' --project '/srv/vike' access";
    assert_eq!(remote(BackendAccessVerb::List), format!("{vikectl} list"));
    assert_eq!(
        remote(BackendAccessVerb::Invite { person: "alice".into(), role: "observe".into() }),
        format!("{vikectl} invite alice --role observe")
    );
    assert_eq!(
        remote(BackendAccessVerb::Revoke { who: "alice/macbook".into() }),
        format!("{vikectl} revoke alice/macbook")
    );
    assert_eq!(
        remote(BackendAccessVerb::Role { who: "alice".into(), role: "control".into() }),
        "VIKE_SETTINGS_DIR='/srv/vike/settings' '/srv/vike/bin/vike-cli' access role alice control"
    );
    for bad in [
        BackendAccessVerb::Invite { person: "Alice".into(), role: "observe".into() },
        BackendAccessVerb::Invite { person: "alice".into(), role: "trade".into() },
        BackendAccessVerb::Role { who: "alice; rm -rf /".into(), role: "admin".into() },
        BackendAccessVerb::Role { who: "alice/".into(), role: "admin".into() },
        BackendAccessVerb::Revoke { who: "$(id)".into() },
        BackendAccessVerb::Revoke { who: String::new() },
    ] {
        assert!(access_args(&line, &bad).is_err(), "{bad:?}");
    }
    // The refusal is built from `ROLES` and keeps its bytes.
    let refused = BackendAccessVerb::Invite { person: "alice".into(), role: "trade".into() };
    assert_eq!(access_args(&line, &refused).unwrap_err(), "a role is observe, control or admin");
}

/// The start-up tunnel is raised only when one was recorded, the desktop dials its mouth and nothing
/// answers there.
#[test]
fn a_recorded_tunnel_is_raised_only_when_its_silent_mouth_is_dialled() {
    let rec = || Some(("app@box".to_string(), 17879));
    assert_eq!(tunnel_to_raise(rec(), "127.0.0.1:17879", |_| false), Some("app@box".into()));
    assert_eq!(tunnel_to_raise(rec(), "127.0.0.1:17879", |_| true), None, "already up");
    assert_eq!(tunnel_to_raise(rec(), "<host>:7879", |_| false), None, "another backend");
    assert_eq!(tunnel_to_raise(None, "127.0.0.1:17879", |_| false), None, "no record");
    assert_eq!(tunnel_to_raise(Some((String::new(), 17879)), "127.0.0.1:17879", |_| false), None);
}

/// A run that connected asks for ONE reconnect when it ends, to the address IT stored (here a tunnel
/// mouth on another port than 7879), never an assumed one; a failed run and a tunnel run ask none, and
/// a line of an older run lands nowhere.
#[test]
fn a_connected_run_reconnects_once_to_the_address_it_stored() {
    let dial = || Some("127.0.0.1:17879".to_string());
    with_run(|r| *r = Some(BackendLinkRun { generation: 7, ..BackendLinkRun::default() }));
    assert_eq!(take_reconnect(), None, "still running");
    finish(6, Some("old".into()), true, dial());
    assert_eq!(current().unwrap().ok, None, "another run's end changes nothing");
    finish(7, None, true, dial());
    assert_eq!(current().unwrap().ok, Some(true));
    assert_eq!(take_reconnect(), dial());
    assert_eq!(take_reconnect(), None, "once");
    with_run(|r| *r = Some(BackendLinkRun { generation: 8, ..BackendLinkRun::default() }));
    finish(8, Some("exit 3".into()), false, None);
    let run = current().unwrap();
    assert_eq!((run.ok, run.lines), (Some(false), vec!["exit 3".to_string()]));
    assert_eq!(take_reconnect(), None, "a failed run stored no address");
    with_run(|r| *r = Some(BackendLinkRun { generation: 9, ..BackendLinkRun::default() }));
    finish(9, None, true, None);
    assert_eq!(take_reconnect(), None, "a tunnel run takes no keys");
}
