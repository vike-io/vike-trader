//! The strategy process policy's NEGATIVE CONTROLS, run as a real process: this binary spawns itself
//! through [`vike_sandbox::spawn_strategy_process`], and the child applies
//! [`vike_sandbox::strategy_process_policy`] to itself, then tries every channel the policy closes.
//!
//! Three phases, all printed by the child as `<phase> <probe> allowed|refused: <why>`:
//!
//! - **control**, BEFORE the jail: each refused channel is OPEN (reading a file outside the grants,
//!   the parent's `/proc` entry, TCP, an abstract UNIX socket, signalling the parent, forking) — so a
//!   refusal later is the jail's doing, not the box's.
//! - **jailed**: the seven refusals of the brief — a file outside the allow-list, `/proc/<ppid>/mem`,
//!   TCP connect, a UNIX socket, `kill(getppid())`, `execve`, `fork` (`EPERM`, which tells the
//!   fork refusal apart from the exec one, `EACCES`).
//! - **jailed** positives, proving the jail is live and not merely broken: the artifact reads, a
//!   thread starts, the process may signal itself, and the parent-death signal and both rlimits hold.
//!
//! - **inherited**, FIRST: the parent holds a descriptor without close-on-exec (what CI's runner hands
//!   every test process), and the child REFUSES it by number; then the parent runs the boot's step 0
//!   (`vike_boot::descriptors`, as every hub does at start) and the child starts. The refusal is the
//!   second line and stays; the mark is the first.
//!
//! Its own `main` (`harness = false`): the policy binds a SINGLE-threaded process, and libtest runs
//! every test on a thread of its own. It answers nextest's `--list` itself, as one test.

const NAME: &str = "strategy_process_policy";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--list") {
        if !args.iter().any(|a| a == "--ignored") {
            println!("{NAME}: test");
        }
        return;
    }
    #[cfg(target_os = "linux")]
    linux::main(&args);
    #[cfg(not(target_os = "linux"))]
    println!("SKIP: the strategy process policy is Linux's; here it refuses by name");
}

#[cfg(target_os = "linux")]
mod linux {
    use std::ffi::OsString;
    use std::net::{TcpListener, TcpStream};
    use std::os::linux::net::SocketAddrExt as _;
    use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};
    use std::os::unix::process::CommandExt as _;
    use std::path::Path;
    use std::process::Command;

    use rustix::process::{Pid, Resource, Signal};

    const CHILD: &str = "--strategy-process-child";
    /// What the fork and exec probes run: exits 0 at once, printing nothing.
    const NOOP: &str = "--noop";
    const ARTIFACT_BYTES: &[u8] = b"the one artifact this process may read";

    pub fn main(args: &[String]) {
        match args.first().map(String::as_str) {
            Some(NOOP) => {}
            Some(CHILD) => child(&args[1..]),
            _ => parent(),
        }
    }

    fn parent() {
        if let Err(why) = vike_sandbox::probe() {
            println!("SKIP: this box cannot apply the jail ({why}); nothing to prove here");
            return;
        }
        let dir = tempfile::tempdir().expect("scratch dir");
        let artifact = dir.path().join("artifact.so");
        std::fs::write(&artifact, ARTIFACT_BYTES).expect("write the artifact");
        let secret = dir.path().join("vike.db");
        std::fs::write(&secret, b"pretend-credential").expect("write the secret");
        // Victims bound HERE, before the child exists, so a failed connect is the jail's doing.
        let tcp = TcpListener::bind("127.0.0.1:0").expect("bind a TCP victim");
        let tcp_address = tcp.local_addr().expect("the TCP victim's address");
        let unix_name = format!("vike-sandbox-negative-control-{}", std::process::id());
        let unix = UnixListener::bind_addr(
            &SocketAddr::from_abstract_name(unix_name.as_bytes()).expect("an abstract name"),
        )
        .expect("bind an abstract UNIX victim");

        let me = std::env::current_exe().expect("this test binary's path");
        let args: [OsString; 5] = [
            CHILD.into(),
            artifact.into_os_string(),
            secret.into_os_string(),
            tcp_address.to_string().into(),
            unix_name.into(),
        ];
        // The repro: a descriptor this parent holds WITHOUT close-on-exec reaches the child, which refuses
        // to apply its jail, naming it.
        let leaked = tempfile::tempfile().expect("a scratch file");
        rustix::io::fcntl_setfd(&leaked, rustix::io::FdFlags::empty())
            .expect("clear close-on-exec");
        let refused = vike_sandbox::spawn_strategy_process(&me, args.clone())
            .expect("spawn the child")
            .wait_with_output()
            .expect("the child ends");
        let why = String::from_utf8_lossy(&refused.stderr);
        let number = format!("{} (", std::os::fd::AsRawFd::as_raw_fd(&leaked));
        assert!(!refused.status.success(), "an inherited descriptor must be refused: {why}");
        assert!(
            why.contains("holds descriptor") && why.contains(&number),
            "named by number: {why}"
        );
        // The boot's step 0, as a hub runs it at start: marked, never closed, and the child starts.
        assert!(
            vike_boot::descriptors::mark_inherited_close_on_exec() >= 1,
            "the leaked one is marked"
        );
        assert!(leaked.metadata().is_ok(), "marked, not closed");

        let child = vike_sandbox::spawn_strategy_process(&me, args).expect("spawn the child");
        let out = child.wait_with_output().expect("the child ends");
        drop((tcp, unix));
        let report = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        println!("{report}");
        assert!(out.status.success(), "the child failed ({:?}): {stderr}", out.status);

        let verdict = |phase: &str, probe: &str| -> String {
            let prefix = format!("{phase} {probe} ");
            report
                .lines()
                .find_map(|l| l.strip_prefix(&prefix))
                .unwrap_or_else(|| panic!("no `{phase} {probe}` line — did the jail escape?"))
                .to_string()
        };
        for probe in
            ["read_secret", "parent_proc", "tcp_connect", "unix_socket", "signal_parent", "fork"]
        {
            assert_eq!(
                verdict("control", probe),
                "allowed",
                "control {probe}: the box itself refuses it"
            );
        }
        for probe in [
            "read_secret",
            "parent_memory",
            "tcp_connect",
            "unix_socket",
            "signal_parent",
            "fork",
            "exec",
        ] {
            let v = verdict("jailed", probe);
            assert!(v.starts_with("refused"), "jailed {probe} was NOT refused: {v}");
        }
        assert!(
            verdict("jailed", "fork").contains("os error 1)"),
            "the fork must be refused AT the fork (EPERM), not later at the exec"
        );
        for probe in [
            "read_artifact",
            "thread",
            "signal_self",
            "parent_death_signal",
            "open_files_limit",
            "address_space_limit",
        ] {
            assert_eq!(
                verdict("jailed", probe),
                "allowed",
                "jailed {probe}: the jail is too tight"
            );
        }
    }

    fn report(phase: &str, probe: &str, result: Result<(), String>) {
        match result {
            Ok(()) => println!("{phase} {probe} allowed"),
            Err(why) => println!("{phase} {probe} refused: {why}"),
        }
    }

    fn io<T, E: std::fmt::Display>(r: Result<T, E>) -> Result<(), String> {
        r.map(drop).map_err(|e| e.to_string())
    }

    fn check(ok: bool, what: &str) -> Result<(), String> {
        if ok { Ok(()) } else { Err(what.to_string()) }
    }

    fn child(args: &[String]) {
        let [artifact, secret, tcp, unix_name] = args else {
            panic!("the child takes four arguments, got {args:?}");
        };
        let me = std::env::current_exe().expect("this test binary's path");
        let parent = std::os::unix::process::parent_id();
        let parent_pid = Pid::from_raw(parent as i32).expect("a parent pid");
        let unix = || {
            SocketAddr::from_abstract_name(unix_name.as_bytes())
                .and_then(|a| UnixStream::connect_addr(&a))
        };
        let fork = || match Command::new(&me).arg(NOOP).status() {
            Ok(s) if s.success() => Ok(()),
            Ok(s) => Err(format!("the forked noop ended {s:?}")),
            Err(e) => Err(e.to_string()),
        };

        report("control", "read_secret", io(std::fs::read(secret)));
        report("control", "parent_proc", io(std::fs::read(format!("/proc/{parent}/stat"))));
        report("control", "tcp_connect", io(TcpStream::connect(tcp.as_str())));
        report("control", "unix_socket", io(unix()));
        report("control", "signal_parent", io(rustix::process::test_kill_process(parent_pid)));
        report("control", "fork", fork());

        if let Err(why) = vike_sandbox::strategy_process_policy(Path::new(artifact)).apply() {
            eprintln!("the policy could not be applied: {why}");
            std::process::exit(3);
        }

        report("jailed", "read_secret", io(std::fs::read(secret)));
        report("jailed", "parent_memory", io(std::fs::File::open(format!("/proc/{parent}/mem"))));
        report("jailed", "tcp_connect", io(TcpStream::connect(tcp.as_str())));
        report("jailed", "unix_socket", io(unix()));
        report("jailed", "signal_parent", io(rustix::process::test_kill_process(parent_pid)));
        report("jailed", "fork", fork());

        report(
            "jailed",
            "read_artifact",
            std::fs::read(artifact)
                .map_err(|e| e.to_string())
                .and_then(|b| check(b == ARTIFACT_BYTES, "the artifact read back different bytes")),
        );
        report(
            "jailed",
            "thread",
            std::thread::spawn(|| 7)
                .join()
                .map_err(|_| "the thread panicked".to_string())
                .and_then(|v| check(v == 7, "the thread returned a different value")),
        );
        report(
            "jailed",
            "signal_self",
            io(rustix::process::test_kill_process(rustix::process::getpid())),
        );
        report(
            "jailed",
            "parent_death_signal",
            rustix::process::parent_process_death_signal()
                .map_err(|e| e.to_string())
                .and_then(|s| check(s == Some(Signal::KILL), "PR_SET_PDEATHSIG is not SIGKILL")),
        );
        let limit = |r| rustix::process::getrlimit(r).current;
        report(
            "jailed",
            "open_files_limit",
            check(limit(Resource::Nofile) == Some(vike_sandbox::OPEN_FILES), "RLIMIT_NOFILE"),
        );
        report(
            "jailed",
            "address_space_limit",
            check(limit(Resource::As) == Some(vike_sandbox::LIVE_ADDRESS_SPACE_BYTES), "RLIMIT_AS"),
        );
        // LAST: were it allowed, this process would become the noop and print nothing more.
        let refused = Command::new(&me).arg(NOOP).exec();
        report("jailed", "exec", Err(refused.to_string()));
    }
}
