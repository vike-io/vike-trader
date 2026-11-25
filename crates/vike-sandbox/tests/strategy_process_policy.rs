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
//!   fork refusal apart from the exec one, `EACCES`) — and the folder the artifacts sit in, unlisted.
//! - **jailed** positives, proving the jail is live and not merely broken: BOTH listed artifacts read (a
//!   portfolio backtest's child is handed several), a thread starts, the process may signal itself, and
//!   the parent-death signal and both rlimits hold. The file outside the allow-list is the artifacts'
//!   SIBLING, so a granted artifact grants nothing beside it.
//! - **artifact**, before the jail (`vike_sandbox`'s module doc, "The artifact grant"): a policy whose list
//!   holds a symbolic link to the secret, a link to a folder of secrets, a linked folder on the way, a `..` out
//!   of the root, an absolute path, or a plain directory is REFUSED, naming it, and changes nothing. Each target
//!   reads in the control phase and stays unreadable once the listed artifacts are jailed.
//! - **the unit's filter**, FIRST in that child: what systemd's `RestrictSUIDSGID=yes` (the CI box's host-mode unit)
//!   hands every process it starts, `openat2` answering `ENOSYS`, so every phase above runs as a live strategy
//!   process there does (the regression of 2026-11-24: every artifact refused). The swap child runs without it.
//! - **swap**, a second child: jailed to one artifact, it reads it; the parent then renames a link to the
//!   secret over that path (and reads the secret through it, unjailed: the swap took); the child's read of
//!   the same path is refused. The grant is the inode opened, so the swapped path names nothing granted.
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
    use std::io::{BufRead as _, BufReader, Read as _, Write as _};
    use std::net::{TcpListener, TcpStream};
    use std::os::linux::net::SocketAddrExt as _;
    use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};
    use std::os::unix::process::CommandExt as _;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use rustix::process::{Pid, Resource, Signal};

    const CHILD: &str = "--strategy-process-child";
    /// The swap's child: `--swap-child <artifact> <secret>`.
    const SWAP_CHILD: &str = "--swap-child";
    /// What the fork and exec probes run: exits 0 at once, printing nothing.
    const NOOP: &str = "--noop";
    const ARTIFACT_BYTES: &[u8] = b"the first artifact this process may read";
    const SECOND_BYTES: &[u8] = b"the second artifact this process may read";
    const SECRET_BYTES: &[u8] = b"pretend-credential";
    /// The artifact paths a policy must refuse: the name the child reports each under, the path listed
    /// (relative to the artifacts' folder) and the secret that listing would have granted, read to prove it
    /// stays unreadable. The parent lays them out (`lay_out_refused`); `<folder>` is the folder's own name
    /// (a `..` that leaves it and comes back), `<secret>` the secret's absolute path.
    const REFUSED: [(&str, &str, &str); 6] = [
        ("link_to_file", "link.so", "link.so"),
        ("link_to_folder", "folder-link.so", "folder-link.so/keys.db"),
        ("linked_folder_on_the_way", "linked/keys.db", "linked/keys.db"),
        ("parent_folder", "../<folder>/vike.db", "../<folder>/vike.db"),
        ("absolute", "<secret>", "<secret>"),
        ("plain_folder", "secret-folder", "secret-folder/keys.db"),
    ];

    pub fn main(args: &[String]) {
        match args.first().map(String::as_str) {
            Some(NOOP) => {}
            Some(CHILD) => child(&args[1..]),
            Some(SWAP_CHILD) => swap_child(&args[1..]),
            _ => parent(),
        }
    }

    /// The links and the folder the [`REFUSED`] paths name, beside the artifacts in `folder`: `secret`'s
    /// link, a folder of secrets and two links to it.
    fn lay_out_refused(folder: &Path, secret: &Path) {
        let secrets = folder.join("secret-folder");
        std::fs::create_dir(&secrets).expect("a folder of secrets");
        std::fs::write(secrets.join("keys.db"), SECRET_BYTES).expect("a secret in it");
        std::os::unix::fs::symlink(secret, folder.join("link.so")).expect("a link to the secret");
        std::os::unix::fs::symlink(&secrets, folder.join("folder-link.so")).expect("a folder link");
        std::os::unix::fs::symlink(&secrets, folder.join("linked")).expect("a linked folder");
    }

    /// The `phase probe` verdict of `report`.
    fn verdict(report: &str, phase: &str, probe: &str) -> String {
        let prefix = format!("{phase} {probe} ");
        report
            .lines()
            .find_map(|l| l.strip_prefix(&prefix))
            .unwrap_or_else(|| panic!("no `{phase} {probe}` line — did the jail escape?"))
            .to_string()
    }

    fn parent() {
        if let Err(why) = vike_sandbox::probe() {
            println!("SKIP: this box cannot apply the jail ({why}); nothing to prove here");
            return;
        }
        let dir = tempfile::tempdir().expect("scratch dir");
        let artifact = dir.path().join("artifact.so");
        std::fs::write(&artifact, ARTIFACT_BYTES).expect("write the artifact");
        let second = dir.path().join("second.so");
        std::fs::write(&second, SECOND_BYTES).expect("write the second artifact");
        let secret = dir.path().join("vike.db");
        std::fs::write(&secret, SECRET_BYTES).expect("write the secret");
        lay_out_refused(dir.path(), &secret);
        // Victims bound HERE, before the child exists, so a failed connect is the jail's doing.
        let tcp = TcpListener::bind("127.0.0.1:0").expect("bind a TCP victim");
        let tcp_address = tcp.local_addr().expect("the TCP victim's address");
        let unix_name = format!("vike-sandbox-negative-control-{}", std::process::id());
        let unix = UnixListener::bind_addr(
            &SocketAddr::from_abstract_name(unix_name.as_bytes()).expect("an abstract name"),
        )
        .expect("bind an abstract UNIX victim");

        let me = std::env::current_exe().expect("this test binary's path");
        let args: [OsString; 6] = [
            CHILD.into(),
            artifact.into_os_string(),
            second.into_os_string(),
            secret.clone().into_os_string(),
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

        let verdict = |phase: &str, probe: &str| verdict(&report, phase, probe);
        for (case, _, _) in REFUSED {
            assert_eq!(
                verdict("control", case),
                "allowed",
                "control {case}: the box itself refuses it"
            );
            let listed = verdict("artifact", case);
            assert!(
                listed.starts_with("refused: the artifact ") && listed.contains(" is refused: "),
                "a policy listing {case} was NOT refused by name: {listed}"
            );
            let target = verdict("jailed", case);
            assert!(target.starts_with("refused"), "jailed, {case}'s target read: {target}");
        }
        for probe in [
            "read_secret",
            "list_folder",
            "parent_proc",
            "tcp_connect",
            "unix_socket",
            "signal_parent",
            "fork",
        ] {
            assert_eq!(
                verdict("control", probe),
                "allowed",
                "control {probe}: the box itself refuses it"
            );
        }
        for probe in [
            "read_secret",
            "list_folder",
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
            "read_second",
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
        a_path_swapped_after_the_grant_reads_nothing(&me, dir.path(), &secret);
    }

    /// The swap: a child jailed to `victim.so` reads it; the parent renames a link to `secret` over that path
    /// once the child says it is jailed (the pipe orders the two, no sleep); the child's next read of the same
    /// path is refused, and so is the secret. Landlock granted the inode the child opened, which the path no
    /// longer names.
    fn a_path_swapped_after_the_grant_reads_nothing(me: &Path, folder: &Path, secret: &Path) {
        let victim = folder.join("victim.so");
        std::fs::write(&victim, ARTIFACT_BYTES).expect("write the victim");
        let args = [OsString::from(SWAP_CHILD), victim.clone().into(), secret.into()];
        let mut child =
            vike_sandbox::spawn_strategy_process(me, args).expect("spawn the swap child");
        let mut said = BufReader::new(child.stdout.take().expect("the child's stdout"));
        let mut report = String::new();
        while !report.lines().any(|line| line == "granted") {
            let read = said.read_line(&mut report).expect("the child's report");
            assert!(read > 0, "the swap child ended before it was jailed: {report}");
        }
        let swap = folder.join("victim.so.swap");
        std::os::unix::fs::symlink(secret, &swap).expect("a link to the secret");
        std::fs::rename(&swap, &victim).expect("the link renamed over the granted path");
        assert_eq!(
            std::fs::read(&victim).expect("the swapped path, unjailed"),
            SECRET_BYTES,
            "the positive control: unjailed, the swapped path reads the secret"
        );
        let mut go = child.stdin.take().expect("the child's stdin");
        go.write_all(b"swapped\n").expect("tell the child");
        drop(go);
        said.read_to_string(&mut report).expect("the rest of the child's report");
        let out = child.wait_with_output().expect("the swap child ends");
        println!("{report}");
        assert!(out.status.success(), "{:?}: {}", out.status, String::from_utf8_lossy(&out.stderr));
        assert_eq!(verdict(&report, "granted", "read_victim"), "allowed", "the grant itself works");
        for probe in ["read_victim", "read_secret"] {
            let v = verdict(&report, "swapped", probe);
            assert!(v.starts_with("refused"), "after the swap, {probe} was NOT refused: {v}");
        }
    }

    /// The swap's child: jailed to `<artifact>` alone (opened beneath its folder), it reads it, says
    /// `granted`, waits for the parent's line, then reads the same path and the secret again.
    fn swap_child(args: &[String]) {
        let [victim, secret] = args else {
            panic!("the swap child takes two arguments, got {args:?}");
        };
        let victim = Path::new(victim);
        let folder = victim.parent().expect("the victim's folder");
        let name = PathBuf::from(victim.file_name().expect("the victim's name"));
        if let Err(why) = vike_sandbox::strategy_process_policy(folder, &[name]).apply() {
            eprintln!("the policy could not be applied: {why}");
            std::process::exit(3);
        }
        report(
            "granted",
            "read_victim",
            std::fs::read(victim)
                .map_err(|e| e.to_string())
                .and_then(|b| check(b == ARTIFACT_BYTES, "the victim read back different bytes")),
        );
        println!("granted");
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        report("swapped", "read_victim", io(std::fs::read(victim)));
        report("swapped", "read_secret", io(std::fs::read(secret)));
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

    /// systemd's `RestrictSUIDSGID=yes` filter as the strategy process inherits it: `openat2` answers `ENOSYS`
    /// (its flags sit behind a pointer seccomp cannot read). Asserted in force, or the run proves nothing.
    fn as_under_the_unit() {
        let filter = seccompiler::SeccompFilter::new(
            [(libc::SYS_openat2, Vec::new())].into_iter().collect(),
            seccompiler::SeccompAction::Allow,
            seccompiler::SeccompAction::Errno(libc::ENOSYS as u32),
            std::env::consts::ARCH.try_into().expect("an architecture seccomp is written for"),
        )
        .expect("the unit's openat2 filter");
        let program: seccompiler::BpfProgram = filter.try_into().expect("the filter compiles");
        seccompiler::apply_filter(&program).expect("the filter applies");
        let probe = rustix::fs::openat2(
            rustix::fs::CWD,
            "/",
            rustix::fs::OFlags::PATH | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
            rustix::fs::ResolveFlags::empty(),
        );
        assert_eq!(probe.map(drop), Err(rustix::io::Errno::NOSYS), "the unit's filter is in force");
    }

    fn child(args: &[String]) {
        let [artifact, second, secret, tcp, unix_name] = args else {
            panic!("the child takes five arguments, got {args:?}");
        };
        as_under_the_unit();
        let folder = Path::new(secret).parent().expect("the artifacts' folder");
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
        report("control", "list_folder", io(std::fs::read_dir(folder)));
        report("control", "parent_proc", io(std::fs::read(format!("/proc/{parent}/stat"))));
        report("control", "tcp_connect", io(TcpStream::connect(tcp.as_str())));
        report("control", "unix_socket", io(unix()));
        report("control", "signal_parent", io(rustix::process::test_kill_process(parent_pid)));
        report("control", "fork", fork());
        let folder_name = folder.file_name().expect("the folder's name").to_string_lossy();
        let named = |text: &str| {
            PathBuf::from(text.replace("<folder>", &folder_name).replace("<secret>", secret))
        };
        for (case, _, target) in REFUSED {
            report("control", case, io(std::fs::read(folder.join(named(target)))));
        }
        // Each refusal comes before the policy changes anything, so the next policy starts clean.
        for (case, listed, _) in REFUSED {
            let artifacts = [PathBuf::from("artifact.so"), named(listed)];
            let policy = vike_sandbox::strategy_process_policy(folder, &artifacts);
            report("artifact", case, policy.apply());
        }

        let listed = |path: &str| PathBuf::from(Path::new(path).file_name().expect("a file name"));
        let artifacts = [listed(artifact), listed(second)];
        if let Err(why) = vike_sandbox::strategy_process_policy(folder, &artifacts).apply() {
            eprintln!("the policy could not be applied: {why}");
            std::process::exit(3);
        }

        for (case, _, target) in REFUSED {
            report("jailed", case, io(std::fs::read(folder.join(named(target)))));
        }
        report("jailed", "read_secret", io(std::fs::read(secret)));
        report("jailed", "list_folder", io(std::fs::read_dir(folder)));
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
            "read_second",
            std::fs::read(second)
                .map_err(|e| e.to_string())
                .and_then(|b| check(b == SECOND_BYTES, "the second read back different bytes")),
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
