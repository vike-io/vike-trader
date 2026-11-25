//! **The desktop connects with no `vike-cli` anywhere.** The Server group's three connects — a remote
//! server over ssh, the server on this computer by `docker exec -i`, and a guest's invitation — run IN
//! THE DESKTOP'S OWN PROCESS (`vike_app_core::backend::server_link::connect_now`, over
//! `vike_client::attach`), against a stand-in `ssh` and a stand-in `docker` on a PATH that holds
//! nothing else: no `vike-cli` on it, and none is looked for beside the binary.
//!
//! Each connect runs in a CHILD of this test binary (an `#[ignore]`d test it re-runs), started with that
//! PATH and a scratch HOME, because a test may not change its own process's environment (that needs
//! `unsafe`, which the workspace forbids). The child's working directory is the scenario's folder,
//! which names the link it runs (`link.txt`). The guest's one-time forward is served by a THIRD run of
//! this binary, `door_plays_the_tradehub`, which the stand-in `ssh` starts on the forwarded port and
//! which answers the invitation's registration as the tradehub's door does. A brand-new PC with no
//! settings database gets one on its first connect; one that exists and cannot be read is refused.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

use vike_app_core::backend::server_link::{BackendLink, connect_now, parse_server_line};
use vike_client::attach::Words;
use vike_hub_protocol::frame::{read_frame, write_frame};
use vike_hub_protocol::handshake::{HandshakeFrame, HandshakeReply, Registered};

/// The server's ed25519 host key an invitation carries (its base64 blob).
const HOST_KEY: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIHostKeyHostKeyHostKeyHostKeyHostKeyHostKeyH";

/// The argument a planted stand-in answers with exit 0 and nothing else, so it can be settled.
const SETTLE_ARG: &str = "--vike-settle";

/// Exec a freshly planted stand-in once, retrying ONLY `ETXTBSY`, so the connect's exec of it cannot lose that
/// race (this binary's cases run as parallel threads, each planting while another spawns). A local copy of
/// `crates/vike-strategy-builder/tests/child_env.rs`'s `settle_planted_execution`: this crate takes no `vike-ops`
/// dependency for a test helper.
fn settle_planted_execution(path: &Path) {
    const ETXTBSY: i32 = 26;
    const ATTEMPTS: u32 = 8;
    let mut backoff = std::time::Duration::from_millis(5);
    for attempt in 1..=ATTEMPTS {
        match Command::new(path).arg(SETTLE_ARG).output() {
            Ok(out) if out.status.success() => return,
            Ok(out) => panic!(
                "settle {}: the stand-in RAN and exited {:?} — it did not answer `{SETTLE_ARG}`. stderr: {}",
                path.display(),
                out.status.code(),
                String::from_utf8_lossy(&out.stderr).trim()
            ),
            Err(e) if e.raw_os_error() == Some(ETXTBSY) => {}
            Err(e) => panic!(
                "settle {}: could not be spawned and it is NOT the ETXTBSY race — errno {:?}: {e}",
                path.display(),
                e.raw_os_error()
            ),
        }
        if attempt < ATTEMPTS {
            std::thread::sleep(backoff);
            backoff *= 2;
        }
    }
    panic!("settle {}: still ETXTBSY after {ATTEMPTS} attempts", path.display());
}

/// The stand-in `ssh`: it logs its argv; `-f` is the tunnel (up at once); the one-time forward
/// (`-L` without `-f`) starts the door on the forwarded port; `swap` is the server's forced command;
/// anything else is `access add` on the server, whose stdin (the PUBLIC key) it keeps.
fn fake_ssh(dir: &Path, exe: &Path) -> String {
    format!(
        "#!/bin/sh\n\
         [ \"$1\" = {SETTLE_ARG} ] && exit 0\n\
         dir='{dir}'\n\
         printf '%s\\n' \"$*\" >> \"$dir/ssh.log\"\n\
         case \" $* \" in *\" -f \"*) exit 0 ;; esac\n\
         last=''; prev=''; spec=''\n\
         for a; do if [ \"$prev\" = -L ]; then spec=\"$a\"; fi; prev=\"$a\"; last=\"$a\"; done\n\
         if [ \"$last\" = swap ]; then exit 0; fi\n\
         if [ -n \"$spec\" ]; then\n\
         printf '%s' \"${{spec%%:*}}\" > \"$dir/door.port\"\n\
         cd \"$dir\" && exec '{exe}' --ignored --exact door_plays_the_tradehub --nocapture\n\
         fi\n\
         /bin/cat > \"$dir/ssh.stdin\"\n",
        dir = dir.display(),
        exe = exe.display()
    )
}

/// The stand-in `docker`: it logs its argv and keeps its stdin (the PUBLIC key `access add` reads).
fn fake_docker(dir: &Path) -> String {
    format!(
        "#!/bin/sh\n[ \"$1\" = {SETTLE_ARG} ] && exit 0\nprintf '%s\\n' \"$*\" >> '{dir}/docker.log'\n\
         /bin/cat > '{dir}/docker.stdin'\n",
        dir = dir.display()
    )
}

/// One scenario's folder: a settings database, a HOME with an ssh key, the stand-ins on a PATH of
/// their own, and `link.txt`.
struct Scenario {
    root: tempfile::TempDir,
}

impl Scenario {
    fn new(link: &str) -> Self {
        let s = Self::bare(link);
        vike_secrets::create_store(s.settings().to_str()).expect("the PC's store");
        s
    }

    /// A brand-new PC: everything but the settings database.
    fn bare(link: &str) -> Self {
        use std::os::unix::fs::PermissionsExt as _;
        let root = tempfile::Builder::new().prefix("vike-server-link-").tempdir().expect("scratch");
        let dir = root.path();
        let ssh = dir.join("home").join(".ssh");
        std::fs::create_dir_all(&ssh).expect("~/.ssh");
        std::fs::write(ssh.join("id_ed25519"), "private, never read here\n").expect("own key");
        let own = format!("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI{} vike\n", "B".repeat(43));
        std::fs::write(ssh.join("id_ed25519.pub"), own).expect("own public key");
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).expect("bin");
        let exe = std::env::current_exe().expect("this test binary");
        for (name, body) in [("ssh", fake_ssh(dir, &exe)), ("docker", fake_docker(dir))] {
            let path = bin.join(name);
            std::fs::write(&path, body).expect("a stand-in");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
            settle_planted_execution(&path);
        }
        std::fs::write(dir.join("link.txt"), link).expect("link.txt");
        Self { root }
    }

    fn dir(&self) -> &Path {
        self.root.path()
    }

    fn settings(&self) -> PathBuf {
        self.dir().join("settings")
    }

    /// Run the connect in a child of this binary, as the desktop would run it, and hand back what it
    /// said and how it ended.
    fn run(&self) -> String {
        let bin = self.dir().join("bin");
        assert!(!bin.join("vike-cli").exists(), "no vike-cli on the PATH");
        let out = Command::new(std::env::current_exe().expect("this test binary"))
            .args(["--ignored", "--exact", "child_connects_in_this_process", "--nocapture"])
            .current_dir(self.dir())
            .env_clear()
            .env("PATH", &bin)
            .env("HOME", self.dir().join("home"))
            .output()
            .expect("the child runs");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        std::fs::read_to_string(self.dir().join("outcome.txt"))
            .expect("the child wrote its outcome")
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.dir().join(name)).unwrap_or_default()
    }

    fn setting(&self, section: &str, key: &str) -> Option<String> {
        let rows = vike_secrets::read_settings_in(&self.settings()).ok()?;
        let found = rows.rows()?.settings.iter().find(|r| r.section == section && r.key == key)?;
        Some(found.value.clone())
    }

    /// This PC's device key's PUBLIC half, as the registration pipes it.
    fn public_hex(&self) -> String {
        let row = vike_secrets::access::device_key_load(&self.settings())
            .expect("readable")
            .expect("the connect made this PC's device key");
        let key =
            vike_hub_protocol::access::DeviceKey::from_pkcs8(&row.private_key).expect("a key");
        vike_utility::hex::hex_lower(&key.public_key())
    }
}

/// A remote server: the PUBLIC key goes to the server's own `vike-cli access add` over ssh, the
/// tunnel is raised and recorded, and the two dial rows are written.
#[test]
fn a_remote_server_connects_in_this_process_with_no_vike_cli() {
    let s = Scenario::new("server app@203.0.113.7:/srv/vike");
    assert_eq!(s.run(), "ok");
    let log = s.read("ssh.log");
    assert!(
        log.contains(
            "-T -o ConnectTimeout=15 app@203.0.113.7 VIKE_SETTINGS_DIR='/srv/vike/settings' \
             '/srv/vike/bin/vike-cli' access add owner pc --role admin"
        ),
        "{log}"
    );
    assert!(log.contains("-N -f -o ExitOnForwardFailure=yes"), "the tunnel: {log}");
    assert_eq!(s.read("ssh.stdin"), format!("{}\n", s.public_hex()));
    assert_eq!(s.setting("config", "node_addr").as_deref(), Some("\"127.0.0.1:7879\""));
    assert_eq!(s.setting("config", "datahub_addr").as_deref(), Some("\"127.0.0.1:7878\""));
    assert!(s.settings().join("state/node/tunnel.json").exists(), "the tunnel is recorded");
    // What the desktop then redials is the address the connect STORED, not an assumed one.
    assert_eq!(s.setting("config", "node_addr"), Some(format!("\"{}\"", s.read("dial.txt"))));
}

/// The server on this computer: `docker exec -i` into `vikectl`'s container, no tunnel.
#[test]
fn the_server_on_this_computer_connects_by_docker_with_no_vike_cli() {
    let s = Scenario::new("local");
    assert_eq!(s.run(), "ok");
    assert_eq!(
        s.read("docker.log"),
        "exec -i vike /opt/vike/bin/vike-cli access add owner pc --role admin\n"
    );
    assert_eq!(s.read("docker.stdin"), format!("{}\n", s.public_hex()));
    assert!(s.read("ssh.log").is_empty(), "no ssh for a server on this computer");
    assert_eq!(s.setting("config", "node_addr").as_deref(), Some("\"127.0.0.1:7879\""));
    assert_eq!(s.setting("config", "node_addr"), Some(format!("\"{}\"", s.read("dial.txt"))));
}

/// A guest's ONE paste: the host key pinned, the device key and this PC's own ssh key registered at
/// the tradehub's door through the one-time forward (strict host-key checking), the swap, the one-time
/// key gone, then the ordinary tunnel.
#[test]
fn an_invitation_joins_in_this_process_with_no_vike_cli() {
    // Standard base64 of a three-line OpenSSH private key file (`b3BlbnNzaA==` its body).
    let key = "LS0tLS1CRUdJTiBPUEVOU1NIIFBSSVZBVEUgS0VZLS0tLS0KYjNCbGJuTnphQT09Ci0tLS0tRU5EIE9QRU5TU0ggUFJJVkFURSBLRVktLS0tLQo=";
    let secret = "0123456789abcdef".repeat(4);
    let line =
        format!("vike-invite-1 app@203.0.113.7:/srv/vike 1a2b3c4d {secret} {HOST_KEY} {key}");
    let s = Scenario::new(&format!("invitation {line}"));
    assert_eq!(s.run(), "ok");
    let log = s.read("ssh.log");
    assert!(log.contains("StrictHostKeyChecking=yes"), "{log}");
    assert!(log.contains(":localhost:7879 app@203.0.113.7"), "the one-time forward: {log}");
    assert!(log.lines().any(|l| l.ends_with("app@203.0.113.7 swap")), "the swap: {log}");
    assert!(log.contains("-N -f -o ExitOnForwardFailure=yes"), "the ordinary tunnel: {log}");
    let registered = s.read("registered.txt");
    assert_eq!(
        registered,
        format!("pc {} ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI{}", s.public_hex(), "B".repeat(43))
    );
    let known = std::fs::read_to_string(s.dir().join("home/.ssh/known_hosts")).expect("pinned");
    assert_eq!(known, format!("203.0.113.7 ssh-ed25519 {HOST_KEY}\n"));
    assert!(!s.dir().join("home/.ssh/vike-invite-1a2b3c4d").exists(), "the one-time key is gone");
    assert!(!format!("{log}{}", s.read("said.txt")).contains(&secret), "the secret never crossed");
    assert_eq!(s.setting("config", "node_addr").as_deref(), Some("\"127.0.0.1:7879\""));
}

/// A brand-new PC with no settings database: the first connect makes it EMPTY (the creator `vike-cli
/// secrets init` runs) and then connects, with no `vike-cli` anywhere.
#[test]
fn a_fresh_pc_with_no_store_makes_it_and_connects_with_no_vike_cli() {
    let s = Scenario::bare("server app@203.0.113.7:/srv/vike");
    assert!(!s.settings().exists(), "a brand-new PC");
    assert_eq!(s.run(), "ok");
    assert!(s.settings().join("db/vike.db").is_file(), "the store is made");
    assert!(s.read("said.txt").contains("created EMPTY"), "{}", s.read("said.txt"));
    assert_eq!(s.read("ssh.stdin"), format!("{}\n", s.public_hex()));
    assert_eq!(s.setting("config", "node_addr").as_deref(), Some("\"127.0.0.1:7879\""));
}

/// A store that EXISTS and cannot be read is refused in words, never made again: the file is left
/// byte for byte, and nothing is sent anywhere.
#[test]
fn an_unreadable_store_is_refused_and_left_untouched() {
    let s = Scenario::bare("server app@203.0.113.7:/srv/vike");
    let db = s.settings().join("db/vike.db");
    std::fs::create_dir_all(s.settings().join("db")).expect("db/");
    let bytes = b"not a database, and somebody's only copy\n";
    std::fs::write(&db, bytes).expect("the unreadable store");
    let outcome = s.run();
    assert!(outcome.contains("settings database cannot be used"), "{outcome}");
    assert_eq!(std::fs::read(&db).expect("still there"), bytes, "the store is untouched");
    assert!(s.read("ssh.log").is_empty(), "nothing was sent");
}

/// The child: run the connect `link.txt` names, as the Server group does, and write its outcome.
#[test]
#[ignore = "run by the tests above, in a child with their PATH and HOME"]
fn child_connects_in_this_process() {
    let here = std::env::current_dir().expect("the scenario's folder");
    let Ok(text) = std::fs::read_to_string(here.join("link.txt")) else { return };
    let link = match text.split_once(' ').unwrap_or((text.as_str(), "")) {
        ("server", line) => BackendLink::Server(parse_server_line(line).expect("a line")),
        ("invitation", line) => BackendLink::Invitation(line.to_string()),
        _ => BackendLink::ThisComputer,
    };
    let said = std::cell::RefCell::new(String::new());
    let keep = |line: &str| said.borrow_mut().push_str(&format!("{line}\n"));
    let words = Words { say: &keep, warn: &keep, inherit: false };
    let process = vike_change_journal::ChangeProcess::new("vike-test", 1, "0");
    let outcome = connect_now(&link, &here.join("settings"), Some(&process), &words);
    std::fs::write(here.join("said.txt"), said.borrow().as_str()).expect("said.txt");
    let (dial, outcome) = match outcome {
        Ok(dial) => (dial, "ok".to_string()),
        Err(why) => (String::new(), why),
    };
    std::fs::write(here.join("dial.txt"), dial).expect("dial.txt");
    std::fs::write(here.join("outcome.txt"), outcome).expect("outcome.txt");
}

/// The tradehub's door on the one-time forward's port: Hello, a challenge offering
/// `access-register`, the registration kept in `registered.txt`, and the granted row back.
#[test]
#[ignore = "started by the stand-in ssh on the one-time forward's port"]
fn door_plays_the_tradehub() {
    let here = std::env::current_dir().expect("the scenario's folder");
    let Ok(port) = std::fs::read_to_string(here.join("door.port")) else { return };
    let listener =
        std::net::TcpListener::bind(("127.0.0.1", port.trim().parse::<u16>().expect("a port")))
            .expect("the forwarded port");
    // The first connection is the forward's own probe (`port_answers`); the registration comes next.
    for stream in listener.incoming() {
        let Ok(mut s) = stream else { continue };
        let Ok(HandshakeFrame::Hello { proto_version }) = read_frame::<_, HandshakeFrame>(&mut s)
        else {
            continue;
        };
        let features = vec![vike_hub_protocol::access::FEATURE_ACCESS_REGISTER.to_string()];
        let welcome = HandshakeReply::Welcome { proto_version, features, nonce: Some([9u8; 32]) };
        write_frame(&mut s, &welcome).expect("the challenge");
        let Ok(HandshakeFrame::Register(reg)) = read_frame::<_, HandshakeFrame>(&mut s) else {
            continue;
        };
        let line = format!(
            "{} {} {}",
            reg.device,
            vike_utility::hex::hex_lower(&reg.public_key),
            reg.ssh_key
        );
        std::fs::write(here.join("registered.txt"), line).expect("registered.txt");
        let granted = Registered {
            person: "alice".into(),
            device: reg.device,
            role: "observe".into(),
            key_id: "nk-0".into(),
        };
        write_frame(&mut s, &HandshakeReply::Registered(granted)).expect("the answer");
    }
}
