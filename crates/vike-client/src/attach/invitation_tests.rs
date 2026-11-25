use super::*;

use vike_hub_protocol::access::{
    FEATURE_ACCESS_REGISTER, invite_auth_secret, invite_proof_verifies,
};
use vike_hub_protocol::frame::{read_frame, write_frame};
use vike_hub_protocol::handshake::{HandshakeFrame, HandshakeReply, Registration};
use vike_tradehub_client::proto::NODE_PROTO_VERSION;

/// A run that says nothing: these tests read what it wrote, not what it said.
const QUIET: Words<'static> = Words { say: &nothing, warn: &nothing, inherit: false };

fn nothing(_: &str) {}

const SECRET: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// The server's ed25519 host key as the line carries it (its base64 blob).
const HOST_KEY: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIHostKeyHostKeyHostKeyHostKeyHostKeyHostKeyH";

/// An invitation line as `vikectl access invite` prints it.
fn line() -> String {
    let key = base64::engine::general_purpose::STANDARD.encode(
        b"-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaA==\n-----END OPENSSH PRIVATE KEY-----\n",
    );
    format!("{INVITATION_TAG} app@203.0.113.7:/home/app/vike/ 1a2b3c4d {SECRET} {HOST_KEY} {key}")
}

fn store() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    vike_secrets::plant_settings_rows(d.path(), &vike_secrets::StoredSettings::default()).unwrap();
    d
}

/// The six fields read back into an address, an id, the secret, the host key and the one-time key's
/// bytes; `Debug` and every refusal name no secret byte.
#[test]
fn an_invitation_line_reads_into_its_six_fields_and_shows_no_secret() {
    let inv = parse_invitation(&format!("  {}\r\n", line())).unwrap();
    assert_eq!(
        (inv.host.as_str(), inv.project.as_str(), inv.id.as_str()),
        ("app@203.0.113.7", "/home/app/vike", "1a2b3c4d")
    );
    assert_eq!((inv.secret.as_str(), inv.host_key.as_str()), (SECRET, HOST_KEY));
    assert!(inv.one_time_key.starts_with(b"-----BEGIN OPENSSH PRIVATE KEY-----"));
    let shown = format!("{inv:?}");
    assert!(!shown.contains(SECRET) && !shown.contains("OPENSSH"), "{shown}");

    let good = line();
    let fields: Vec<&str> = good.split(' ').collect();
    let with = |i: usize, v: &str| {
        let mut f = fields.clone();
        f[i] = v;
        f.join(" ")
    };
    let rsa = format!("AAAAB3NzaC1yc2E{}", "A".repeat(53));
    let five = fields.iter().enumerate().filter(|(i, _)| *i != 4).map(|(_, f)| *f);
    for bad in [
        String::new(),
        with(0, "vike-invite-2"),
        with(1, "-oProxyCommand=x:/srv/vike"),
        with(1, "app@host:relative"),
        with(1, "app@host:/srv/$(id)"),
        with(2, "1A2B3C4D"),
        with(2, "1a2b3c"),
        with(3, &SECRET[1..]),
        with(4, &rsa),
        with(4, &HOST_KEY[1..]),
        with(5, "bm90IGEga2V5"),
        five.collect::<Vec<_>>().join(" "),
        format!("{good} extra"),
    ] {
        let err = parse_invitation(&bad).unwrap_err();
        assert!(!err.contains(SECRET) && !err.contains(fields[5]), "{err}");
    }
}

/// The host key is pinned in `known_hosts` once and every one-time ssh step checks it strictly; a
/// line already naming ANOTHER key for that host is refused with the file untouched, and other hosts'
/// lines stay as they were.
#[test]
fn the_host_key_is_pinned_and_a_mismatched_one_is_refused() {
    let inv = parse_invitation(&line()).unwrap();
    let pinned = format!("203.0.113.7 ssh-ed25519 {HOST_KEY}");
    let dir = tempfile::tempdir().unwrap();
    let known = dir.path().join("known_hosts");
    std::fs::write(&known, "other.example ssh-ed25519 AAAAother").unwrap();
    pin_host_key(dir.path(), &inv, &QUIET).unwrap();
    pin_host_key(dir.path(), &inv, &QUIET).unwrap();
    let text = std::fs::read_to_string(&known).unwrap();
    assert_eq!(text, format!("other.example ssh-ed25519 AAAAother\n{pinned}\n"), "once, own line");

    let stale = tempfile::tempdir().unwrap();
    let before = format!("203.0.113.7,box ssh-ed25519 {}\n", HOST_KEY.replace('H', "X"));
    std::fs::write(stale.path().join("known_hosts"), &before).unwrap();
    let err = pin_host_key(stale.path(), &inv, &QUIET).unwrap_err().message;
    assert!(err.contains("ANOTHER") && err.contains("Nothing was sent"), "{err}");
    assert_eq!(std::fs::read_to_string(stale.path().join("known_hosts")).unwrap(), before);

    let fresh = tempfile::tempdir().unwrap();
    pin_host_key(&fresh.path().join(".ssh"), &inv, &QUIET).unwrap();
    let made = std::fs::read_to_string(fresh.path().join(".ssh").join("known_hosts")).unwrap();
    assert_eq!(made, format!("{pinned}\n"));
    let options = one_time_options(Path::new("k")).join(" ");
    assert!(options.contains("StrictHostKeyChecking=yes") && !options.contains("accept-new"));
}

/// The address takes a plain user name and a plain host name or IP address only: a host or user ssh reads as a
/// pattern would pin the server's key for MORE than one host, so it is refused before anything is written.
#[test]
fn an_address_ssh_would_read_as_a_pattern_is_refused() {
    for good in ["app@203.0.113.7", "app@box.example.com", "deploy_user@my-host", "app@::1"] {
        assert!(plain_destination(good), "{good}");
    }
    for bad in [
        "app@*",
        "app@*.example.com",
        "app@box?",
        "app@!box",
        "app@a,b",
        "app@[box]",
        "app@box|x",
        "app@box%eth0",
        "app@-box",
        "app@",
        "*@box",
        "a*p@box",
        "-app@box",
        "box",
        "app@box@other",
    ] {
        assert!(!plain_destination(bad), "{bad}");
        let address = format!("{bad}:/srv/vike");
        assert!(
            parse_invitation(&line().replace("app@203.0.113.7:/home/app/vike/", &address)).is_err()
        );
    }
}

/// `known_hosts` lines ssh would use for the host are seen before anything is written, HASHED ones and patterns
/// included: another key is refused and the file untouched, the invitation's own key is the pin already.
#[test]
fn a_hashed_or_pattern_known_hosts_line_is_seen_before_anything_is_written() {
    let inv = parse_invitation(&line()).unwrap();
    let other = HOST_KEY.replace('H', "X");
    for (held, hash, refused) in [
        (format!("203.0.113.7 ssh-ed25519 {other}\n"), true, true),
        (format!("203.0.113.7 ssh-ed25519 {HOST_KEY}\n"), true, false),
        (format!("* ssh-ed25519 {other}\n"), false, true),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let known = dir.path().join("known_hosts");
        std::fs::write(&known, &held).unwrap();
        if hash {
            let out = Command::new("ssh-keygen").arg("-H").arg("-f").arg(&known).output().unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        }
        let before = std::fs::read_to_string(&known).unwrap();
        assert!(!hash || before.starts_with("|1|"), "hashed: {before}");
        assert_eq!(pin_host_key(dir.path(), &inv, &QUIET).is_err(), refused, "{held}");
        assert_eq!(std::fs::read_to_string(&known).unwrap(), before, "nothing appended: {held}");
    }
}

/// A host name becomes a device name the access list takes, and a name it cannot take becomes `pc`.
#[test]
fn a_host_name_becomes_a_valid_device_name() {
    assert_eq!(name_of_host("DESKTOP-K3J2\r\n"), "desktop-k3j2");
    assert_eq!(name_of_host("laptop.example.org"), "laptop");
    assert_eq!(name_of_host("Mein Rechner"), "mein-rechner");
    assert_eq!(name_of_host("-x"), "x");
    assert_eq!(name_of_host(""), "pc");
    assert_eq!(name_of_host("ÄÖÜ"), "pc");
    assert_eq!(name_of_host(&"a".repeat(40)).len(), 32);
}

/// A box makes its device key ONCE: the second ask returns the same key, and the stored row carries the private
/// half as PKCS#8 only.
#[test]
fn the_device_key_is_made_once_and_then_held() {
    let d = store();
    assert!(held_device(d.path()).unwrap().is_none());
    let (first, device) = this_device(d.path(), &QUIET).unwrap();
    assert!(valid_name(&device), "{device}");
    let (again, same) = this_device(d.path(), &QUIET).unwrap();
    assert_eq!((first.key_id(), device), (again.key_id(), same));
    let row = device_key_load(d.path()).unwrap().unwrap();
    assert_eq!((row.key_id.as_str(), row.person.as_str()), (first.key_id().as_str(), DEVICE_OWNER));
    assert_eq!(DeviceKey::from_pkcs8(&row.private_key).unwrap().public_key(), first.public_key());
    assert!(!format!("{row:?}").contains(&format!("{:?}", row.private_key)));
}

/// The one-time key's file is this user's alone and gone when the run ends.
#[test]
fn the_one_time_key_file_is_private_and_removed() {
    let d = tempfile::tempdir().unwrap();
    let inv = parse_invitation(&line()).unwrap();
    let path = {
        let key = OneTimeKey::write(d.path(), &inv).unwrap();
        assert_eq!(std::fs::read(&key.0).unwrap(), inv.one_time_key);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(std::fs::metadata(&key.0).unwrap().permissions().mode() & 0o777, 0o600);
        }
        key.0.clone()
    };
    assert!(!path.exists());
}

/// The one-time steps use that key alone, never prompt, forward ONE port to the tradehub, and ask for the swap.
#[test]
fn the_one_time_ssh_runs_forward_one_port_and_ask_for_the_swap() {
    let key = Path::new("/home/me/.ssh/vike-invite-1a2b3c4d");
    let tunnel = registration_tunnel_args(key, 40111, "app@box").join(" ");
    assert_eq!(
        tunnel,
        "-N -o ExitOnForwardFailure=yes -o BatchMode=yes -o IdentitiesOnly=yes -o \
         StrictHostKeyChecking=yes -i /home/me/.ssh/vike-invite-1a2b3c4d -L \
         40111:localhost:7879 app@box"
    );
    let swap = swap_args(key, "app@box");
    assert_eq!(swap.first().map(String::as_str), Some("-T"));
    assert_eq!(&swap[swap.len() - 2..], ["app@box", "swap"]);
}

/// A fake tradehub door: Hello, a challenge advertising `features`, then whatever `answer` makes of the
/// registration and its nonce.
fn door(
    features: Vec<String>,
    answer: impl FnOnce(Registration, [u8; 32]) -> HandshakeReply + Send + 'static,
) -> (String, std::thread::JoinHandle<Option<Registration>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let handle = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let HandshakeFrame::Hello { proto_version } = read_frame(&mut s).unwrap() else { panic!() };
        assert_eq!(proto_version, NODE_PROTO_VERSION);
        let nonce = [7u8; 32];
        let welcome = HandshakeReply::Welcome { proto_version, features, nonce: Some(nonce) };
        write_frame(&mut s, &welcome).unwrap();
        let Ok(HandshakeFrame::Register(reg)) = read_frame::<_, HandshakeFrame>(&mut s) else {
            return None;
        };
        write_frame(&mut s, &answer(reg.clone(), nonce)).unwrap();
        Some(reg)
    });
    (addr, handle)
}

/// The paste's registration against a fake door: the device's PUBLIC key, its name and the ssh key arrive, the
/// proof verifies under the secret's hash and this connection's nonce, the secret itself never crosses, and the
/// granted row comes back.
#[test]
fn a_registration_sends_the_public_keys_and_a_proof_never_the_secret() {
    let (device, _) = DeviceKey::generate().unwrap();
    let inv = parse_invitation(&line()).unwrap();
    let ssh_key = format!("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI{}", "A".repeat(43));
    let (addr, door) = door(vec![FEATURE_ACCESS_REGISTER.to_string()], |reg, nonce| {
        assert!(invite_proof_verifies(
            &invite_auth_secret(SECRET),
            &nonce,
            NODE_PROTO_VERSION,
            &reg.proof
        ));
        HandshakeReply::Registered(Registered {
            person: "alice".into(),
            device: reg.device,
            role: "observe".into(),
            key_id: "nk-1".into(),
        })
    });
    let got = register_at(&addr, &inv, &device, "macbook", &ssh_key).unwrap();
    assert_eq!(
        (got.person.as_str(), got.device.as_str(), got.role.as_str()),
        ("alice", "macbook", "observe")
    );
    let reg = door.join().unwrap().unwrap();
    assert_eq!((reg.invite.as_str(), reg.ssh_key.as_str()), ("1a2b3c4d", ssh_key.as_str()));
    assert_eq!(reg.public_key, device.public_key());
    assert!(!serde_json::to_string(&reg).unwrap().contains(SECRET));
}

/// A refusal comes back in the node's words; a node that does not advertise the door is sent nothing.
#[test]
fn a_refused_or_older_door_registers_nothing() {
    let (device, _) = DeviceKey::generate().unwrap();
    let inv = parse_invitation(&line()).unwrap();
    let (addr, door1) = door(vec![FEATURE_ACCESS_REGISTER.to_string()], |_, _| {
        HandshakeReply::AuthDenied { reason: "invitation refused".into() }
    });
    let err = register_at(&addr, &inv, &device, "pc", "ssh-ed25519 x").unwrap_err();
    assert!(matches!(err, HubCallError::AccessDenied(_)), "{err:?}");
    assert_eq!(
        err.to_string(),
        "error [hub.access.denied]: tradehub registration refused: invitation refused"
    );
    door1.join().unwrap();
    let (addr, door2) = door(Vec::new(), |_, _| unreachable!("nothing may be sent"));
    let err = register_at(&addr, &inv, &device, "pc", "ssh-ed25519 x").unwrap_err();
    assert!(matches!(err, HubCallError::NotOffered { .. }), "{err:?}");
    assert!(door2.join().unwrap().is_none());
}
