//! The SANDBOX: what a jailed thread or process may read, write and reach, closed with kernel
//! mechanisms a same-user process can use without privilege (decision 0273). Two users:
//!
//! - **the strategy builder** jails itself and every build it starts to [`Grants`]
//!   ([`restrict_current_thread`], [`run_jailed`]); the cargo-specific grants stay in
//!   `crates/vike-strategy-builder/src/jail.rs`;
//! - **a strategy process** — the child a hub spawns to run its compiled strategy files (one, or each
//!   `.rs` mount of a portfolio backtest) — jails ITSELF with [`strategy_process_policy`]'s
//!   [`StrategyProcessPolicy::apply`] before it loads them (a user file can run code at load time
//!   through `.init_array`, so the jail comes first). A parent starts it with [`spawn_strategy_process`].
//!
//! The mechanisms, each applied to the CALLING THREAD (so whatever that thread starts inherits it,
//! and no other thread is touched):
//!
//! - **Landlock** (ABI 4, Linux 6.7+): the filesystem rights, and TCP `bind`/`connect` refused.
//! - **seccomp**: what Landlock 4 does not govern — every socket (UDP, UNIX, abstract), `io_uring`,
//!   and for a strategy process the calls that reach ANOTHER process.
//!
//! # The strategy process policy (a same-user process's channels, each closed by name)
//!
//! | channel | closed by |
//! |---|---|
//! | read `vike.db`, `.ssh`, `/etc/vike`, `/proc/<ppid>/mem`, any file | Landlock: READ only (no execute, no write anywhere) on each listed artifact as the INODE [`StrategyProcessPolicy::apply`] opened (never its folder, never a link's target: "The artifact grant" below), the dynamic loader's cache and the system libraries |
//! | TCP bind/connect | Landlock network rules, none granted |
//! | UNIX sockets, UDP, raw sockets, `io_uring` | seccomp: `socket`, `socketpair`, `io_uring_setup` fail `EACCES` |
//! | signal the hub (`kill`, `tkill`, `tgkill`, `rt_sigqueueinfo`, `rt_tgsigqueueinfo`, `pidfd_send_signal`) | seccomp `EACCES` unless the target is its own pid (so `abort` still works); `pidfd_send_signal` always |
//! | read or write another process (`ptrace`, `process_vm_readv/writev`, `pidfd_getfd`, `kcmp`) | seccomp `EACCES` |
//! | slow the hub down (`prlimit64`, `setpriority`, `ioprio_set`, `sched_setaffinity`, `sched_setscheduler`, `sched_setparam`, `sched_setattr` on another pid) | seccomp `EACCES` unless the target is itself (`0` or its pid) |
//! | the user's kernel keyrings (`keyctl`, `add_key`, `request_key`) | seccomp `EACCES` |
//! | start a program (`execve`, `execveat`) | seccomp `EACCES` (and Landlock grants no execute) |
//! | fork (`fork`, `vfork`, `clone` without `CLONE_THREAD`) | seccomp `EPERM`; `clone3` answers `ENOSYS`, so glibc falls back to `clone`, whose flags seccomp can read |
//! | x32 system calls (x86_64) | seccomp `EACCES`: they share the 64-bit audit arch, so the lists above would not see them |
//! | outlive the hub, hog memory or descriptors | `PR_SET_PDEATHSIG(SIGKILL)`, `RLIMIT_AS` ([`LIVE_ADDRESS_SPACE_BYTES`], or the unit's cgroup), `RLIMIT_NOFILE` [`OPEN_FILES`], `no_new_privs` |
//! | an inherited descriptor (an already-open file bypasses Landlock) | REFUSED: [`StrategyProcessPolicy::apply`] refuses a process with any descriptor above stderr. Closing one would take `unsafe`, which this workspace forbids |
//!
//! # The artifact grant (what a symbolic link, or a path swapped later, cannot widen)
//!
//! [`StrategyProcessPolicy::apply`] opens [`StrategyProcessPolicy::artifact_root`] (resolved as given: an
//! operator's link ABOVE it is followed), then walks each artifact's path beneath it ONE COMPONENT AT A TIME
//! from the folder's descriptor (`openat(O_PATH | O_NOFOLLOW)`, no `..`, no root), requiring `fstat` to say a
//! DIRECTORY for each folder on the way and a REGULAR file at the end. So an artifact
//! path holding a symbolic link in ANY of its components (a link to a file, to a directory, a linked folder on
//! the way), a `..` or an absolute path that leaves the root, a directory or a device is REFUSED, naming the
//! artifact, and the whole jail with it: nothing is granted and nothing loads. ⚠ Not `openat2(RESOLVE_BENEATH |
//! RESOLVE_NO_SYMLINKS)`: systemd's `RestrictSUIDSGID=yes` (the host-mode unit's) answers EVERY `openat2`
//! `ENOSYS`, because its flags sit behind a pointer seccomp cannot read, and the strategy process inherits that
//! filter (the CI box, 2026-11-24: every live mount refused). The Landlock rule is then built
//! from THAT descriptor (`landlock::PathBeneath` over it, `ReadFile` only), never from the path, so the grant is
//! the inode opened: a path swapped afterwards (renamed over, or replaced by a link to `vike.db`) names another
//! inode, which no rule grants, and a read through it is refused. The descriptors close as their rules are
//! added (Landlock holds the inode, not the descriptor), so none outlives `apply`.
//!
//! ⚠ **Where the jail cannot be applied, the caller REFUSES; it never runs unjailed.** [`probe`] says
//! why (a platform other than Linux, a kernel without Landlock ABI 4, an architecture seccomp is not
//! written for). There is no override (decision 0273). ⚠ A systemd unit whose `SystemCallFilter=`
//! lacks `@sandbox` KILLS the process at the first Landlock call (systemd 255's `@system-service`
//! does not include it): [`probe_in_child`] asks a CHILD, so only that child dies and the caller
//! refuses by name.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;

/// The hidden argument that makes a program answer [`probe`] and exit (0: the jail can be applied;
/// 3: it cannot, the reason on stderr) — what [`probe_in_child`] re-enters it with.
pub const PROBE_ARG: &str = "--jail-probe";

/// The reason [`probe_in_child`] found, kept for the process: once set, [`probe`] answers it and this
/// process makes no Landlock call at all.
static UNAVAILABLE: OnceLock<String> = OnceLock::new();

/// What a jailed thread, and everything it starts, may touch. Paths that do not exist are skipped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Grants {
    /// Read and execute.
    pub read: Vec<PathBuf>,
    /// Read, execute, write, create and remove.
    pub write: Vec<PathBuf>,
    /// TCP and new sockets allowed. Only the builder's one-time crate fetch sets it
    /// (`render::prefetch`): cargo's own code over the fixed template manifest, no build script.
    pub network: bool,
}

/// Run `f` on a fresh thread jailed to `grants`, and return what it returned. Only that thread and
/// what it starts are jailed; the caller's thread is untouched.
///
/// # Errors
/// Why the jail could not be applied (`f` then never ran), or that the thread panicked.
pub fn run_jailed<T: Send>(grants: &Grants, f: impl FnOnce() -> T + Send) -> Result<T, String> {
    std::thread::scope(|s| {
        s.spawn(|| {
            restrict_current_thread(grants)?;
            Ok(f())
        })
        .join()
        .unwrap_or_else(|_| Err("the jailed build thread panicked".to_string()))
    })
}

/// Whether this box can apply the jail: `Ok` on Linux with Landlock ABI 4 and an architecture the
/// seccomp filters are written for; otherwise the reason, in words an operator acts on.
///
/// # Errors
/// The reason no jail can be applied here.
pub fn probe() -> Result<(), String> {
    if let Some(why) = UNAVAILABLE.get() {
        return Err(why.clone());
    }
    imp::probe()
}

/// [`probe`], asked of a CHILD — this program re-entered with `reentry` then [`PROBE_ARG`] — so a
/// syscall filter that KILLS on a Landlock call kills the child, not the caller. A refusal is
/// remembered for the process ([`probe`] then answers it without a syscall). The program must
/// answer [`PROBE_ARG`] by calling [`probe`] and exiting 0 or 3 (the reason on stderr).
///
/// # Errors
/// Why no jail can be applied: the child's own reason, that a signal ended it (the unit's filter),
/// or that this program's own path is unknown.
pub fn probe_in_child(reentry: &[&str]) -> Result<(), String> {
    let verdict = std::env::current_exe()
        .map_err(|e| {
            format!("this program's own path is unknown ({e}), so the jail cannot be probed")
        })
        .and_then(|program| imp::probe_in_child(&program, reentry));
    if let Err(why) = &verdict {
        let _ = UNAVAILABLE.set(why.clone());
    }
    verdict
}

/// Jail the CALLING thread to `grants`, irreversibly. Everything it starts afterwards inherits it.
///
/// # Errors
/// Why the jail could not be applied; the thread is then in an unknown state and must not go on to
/// run a build.
pub fn restrict_current_thread(grants: &Grants) -> Result<(), String> {
    imp::restrict_current_thread(grants)
}

/// `RLIMIT_AS` of a LIVE strategy process: 2 GiB of address space. A backtest leaves it to the unit's
/// cgroup (`address_space_bytes: None`), because a whole run with its data lives inside.
pub const LIVE_ADDRESS_SPACE_BYTES: u64 = 2 << 30;

/// `RLIMIT_NOFILE` of a strategy process: its three pipes plus what the dynamic loader opens. The loader's
/// descriptors are transient (each artifact is opened, mapped and closed in turn), so the count does not grow with
/// a portfolio's artifacts. MEASURED 2026-11-23 on the CI box (`crates/vike-strategy-sandbox/tests/backtest_child.rs`):
/// one artifact and eight load under a limit of 5 alike, and the backtest verb holds 4 open once they are loaded.
pub const OPEN_FILES: u64 = 16;

/// What a strategy process reads besides its artifacts, by path: the dynamic loader's cache and the system
/// libraries the artifact links (`libc`, `libgcc_s`, `libm`, the loader itself). NOT `/proc`, `/etc`,
/// `/dev`, `/home`, the toolchain or the project. A missing path is skipped.
const SYSTEM_LIBRARIES: &[&str] = &[
    "/lib",
    "/lib64",
    "/lib32",
    "/libx32",
    "/usr/lib",
    "/usr/lib64",
    "/usr/lib32",
    "/usr/libx32",
    "/usr/local/lib",
    "/etc/ld.so.cache",
];

/// The jail of a strategy process (this module's table). Plain data, so a caller adjusts what its
/// mode needs (a backtest sets `address_space_bytes` to `None`) before the child applies it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrategyProcessPolicy {
    /// The directory the artifacts are opened beneath: the builds directory, for a backtest, a study and
    /// a live mount alike.
    pub artifact_root: PathBuf,
    /// The compiled files the process loads, each RELATIVE to [`Self::artifact_root`], read only:
    /// granted by the inode [`Self::apply`] opens, never by the path (the module doc's "The artifact grant").
    pub artifacts: Vec<PathBuf>,
    /// Read only, by path — no execute, no write, no directory outside these: the dynamic loader's
    /// cache and the system libraries.
    pub read: Vec<PathBuf>,
    /// `RLIMIT_AS`, soft and hard; `None` leaves the address space to the unit's cgroup.
    pub address_space_bytes: Option<u64>,
    /// `RLIMIT_NOFILE`, soft and hard.
    pub open_files: u64,
}

/// The live policy for a strategy process that runs the compiled `artifacts` (each relative to
/// `artifact_root`, the files the child then `dlopen`s: a live mount's one, a portfolio backtest's one per
/// `.rs` mount). Each is granted as the regular FILE [`StrategyProcessPolicy::apply`] opens beneath the root,
/// so a sibling in its folder stays unreadable; nothing else is added for a second artifact.
pub fn strategy_process_policy(
    artifact_root: &Path,
    artifacts: &[PathBuf],
) -> StrategyProcessPolicy {
    StrategyProcessPolicy {
        artifact_root: artifact_root.to_path_buf(),
        artifacts: artifacts.to_vec(),
        read: SYSTEM_LIBRARIES.iter().map(PathBuf::from).collect(),
        address_space_bytes: Some(LIVE_ADDRESS_SPACE_BYTES),
        open_files: OPEN_FILES,
    }
}

impl StrategyProcessPolicy {
    /// Jail THIS WHOLE PROCESS to the policy, irreversibly: the artifacts opened (the module doc's "The
    /// artifact grant"), `PR_SET_PDEATHSIG(SIGKILL)`, Landlock (with `no_new_privs`), the rlimits, then the
    /// seccomp filters.
    ///
    /// ⚠ **Call it FIRST in the child's `main`, before the artifact loads**: before any thread
    /// starts (Landlock and seccomp bind the calling thread, so a thread already running would stay
    /// free — it is refused here) and before anything opens a descriptor (an inherited or early one
    /// is an open file Landlock no longer checks — refused here too, naming it).
    ///
    /// # Errors
    /// Why the jail could not be applied, by name: not Linux, a kernel without Landlock ABI 4, an
    /// unknown architecture, a second thread, a descriptor above stderr, an artifact that is not a regular
    /// file reached beneath the root without a symbolic link, a refused filter. An artifact's refusal comes
    /// before anything changes; any other leaves the process in an unknown state. Either way it must exit
    /// without loading the artifact.
    pub fn apply(&self) -> Result<(), String> {
        imp::apply(self)
    }
}

/// Start `program args` as a strategy process: an EMPTY environment, working directory `/`, stdin and
/// stdout piped (the protocol), stderr piped (the reason; the reader bounds what it keeps). Every
/// other descriptor Rust opens is close-on-exec, so the child starts with exactly these three; it
/// then jails itself ([`StrategyProcessPolicy::apply`]).
///
/// ⚠ **Spawn from the thread that owns the child for its whole life** (its supervisor): the kernel
/// sends `PR_SET_PDEATHSIG` when the THREAD that forked the child exits, not the process, so a child
/// started from a short-lived thread is killed when that thread ends. A parent that dies before the
/// child set it is still seen: the protocol pipe then reads end-of-file.
///
/// # Errors
/// The spawn's own (`program` missing or not executable).
pub fn spawn_strategy_process(
    program: &Path,
    args: impl IntoIterator<Item = impl AsRef<OsStr>>,
) -> std::io::Result<Child> {
    Command::new(program)
        .args(args)
        .env_clear()
        .current_dir("/")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

/// The capacity [`widen_pipe`] asks for: 1 MiB, the default `/proc/sys/fs/pipe-max-size` (the most an
/// unprivileged process may set).
pub const WIDE_PIPE_BYTES: usize = 1 << 20;

/// Raise `pipe`'s kernel buffer from the default 64 KiB to [`WIDE_PIPE_BYTES`] (or the box's
/// `/proc/sys/fs/pipe-max-size`, if lower), so a writer streaming bulk frames to a child is not held to
/// 64 KiB a wake. The capacity is the PIPE's, so the parent's end sets it for both. Best effort: a refusal
/// (a user over `pipe-user-pages-soft`, an older kernel) keeps the default, silently. Linux only.
#[cfg(target_os = "linux")]
pub fn widen_pipe(pipe: impl std::os::fd::AsFd) {
    let max = std::fs::read_to_string("/proc/sys/fs/pipe-max-size")
        .ok()
        .and_then(|max| max.trim().parse().ok())
        .unwrap_or(WIDE_PIPE_BYTES);
    let _ = rustix::pipe::fcntl_setpipe_size(pipe, WIDE_PIPE_BYTES.min(max));
}

/// Elsewhere the pipe keeps the platform's buffer.
#[cfg(not(target_os = "linux"))]
pub fn widen_pipe<P>(_pipe: P) {}

#[cfg(target_os = "linux")]
mod imp {
    use std::collections::BTreeMap;
    use std::os::fd::{AsFd, OwnedFd};
    use std::path::{Component, Path, PathBuf};

    use landlock::{
        ABI, Access, AccessFs, AccessNet, BitFlags, CompatLevel, Compatible, PathBeneath, Ruleset,
        RulesetAttr, RulesetCreatedAttr, RulesetError, RulesetStatus, path_beneath_rules,
    };
    use rustix::fs::{FileType, Mode, OFlags};
    use rustix::process::{Resource, Rlimit, Signal};
    use seccompiler::{
        BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
        SeccompRule, TargetArch,
    };

    use super::{Grants, StrategyProcessPolicy};

    /// Landlock's filesystem rights plus TCP `bind`/`connect`: kernel 6.7.
    const ABI_REQUIRED: ABI = ABI::V4;

    fn kernel_lacks(e: impl std::fmt::Display) -> String {
        format!(
            "this kernel does not offer Landlock ABI 4 (Linux 6.7 or later, with `landlock` in \
             /sys/kernel/security/lsm): {e}"
        )
    }

    pub(super) fn probe() -> Result<(), String> {
        arch()?;
        Ruleset::default()
            .set_compatibility(CompatLevel::HardRequirement)
            .handle_access(AccessFs::from_all(ABI_REQUIRED))
            .and_then(|r| r.handle_access(AccessNet::from_all(ABI_REQUIRED)))
            .and_then(|r| r.create())
            .map(drop)
            .map_err(kernel_lacks)
    }

    pub(super) fn probe_in_child(
        program: &std::path::Path,
        reentry: &[&str],
    ) -> Result<(), String> {
        use std::os::unix::process::ExitStatusExt;
        use std::process::{Command, Stdio};
        let out = Command::new(program)
            .args(reentry)
            .arg(super::PROBE_ARG)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| format!("the jail probe could not start ({}): {e}", program.display()))?;
        if out.status.success() {
            return Ok(());
        }
        if let Some(signal) = out.status.signal() {
            return Err(format!(
                "this process's syscall filter refuses the Landlock calls (the probe was ended by \
                 signal {signal}); a systemd unit running the builder needs \
                 `SystemCallFilter=@sandbox`, which `vikectl` writes into the host-mode unit"
            ));
        }
        let said = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if said.is_empty() {
            format!("the jail probe exited with status {:?}", out.status.code())
        } else {
            said
        })
    }

    fn arch() -> Result<TargetArch, String> {
        TargetArch::try_from(std::env::consts::ARCH).map_err(|e| {
            format!("the socket filter is not written for {}: {e}", std::env::consts::ARCH)
        })
    }

    pub(super) fn restrict_current_thread(g: &Grants) -> Result<(), String> {
        landlock(&g.read, AccessFs::from_read(ABI_REQUIRED), &g.write, g.network, Vec::new())?;
        if g.network {
            Ok(())
        } else {
            install(
                [libc::SYS_socket, libc::SYS_io_uring_setup].into_iter().map(|nr| (nr, Vec::new())),
                libc::EACCES,
            )
        }
    }

    /// Landlock over the calling thread: `read` paths at `read_access`, `write` paths at every right,
    /// each of `files` (regular files, [`open_artifacts`]'s) at `ReadFile` by the inode its descriptor holds,
    /// TCP refused unless `network`. Refuses a partial enforcement or a missing `no_new_privs`.
    fn landlock(
        read: &[PathBuf],
        read_access: BitFlags<AccessFs>,
        write: &[PathBuf],
        network: bool,
        files: Vec<OwnedFd>,
    ) -> Result<(), String> {
        let mut ruleset = Ruleset::default()
            .set_compatibility(CompatLevel::HardRequirement)
            .handle_access(AccessFs::from_all(ABI_REQUIRED))
            .map_err(kernel_lacks)?;
        if !network {
            // Handled with no port rule: every TCP bind and connect is refused.
            ruleset =
                ruleset.handle_access(AccessNet::from_all(ABI_REQUIRED)).map_err(kernel_lacks)?;
        }
        // Each rule closes its descriptor once added: the kernel keeps the inode, not the descriptor.
        let files = files
            .into_iter()
            .map(|file| Ok::<_, RulesetError>(PathBeneath::new(file, AccessFs::ReadFile)));
        let status = ruleset
            .create()
            .and_then(|r| r.add_rules(path_beneath_rules(read, read_access)))
            .and_then(|r| r.add_rules(path_beneath_rules(write, AccessFs::from_all(ABI_REQUIRED))))
            .and_then(|r| r.add_rules(files))
            .and_then(|r| r.restrict_self())
            .map_err(|e| format!("Landlock refused the jail: {e}"))?;
        if status.ruleset != RulesetStatus::FullyEnforced || !status.no_new_privs {
            return Err(format!("Landlock enforced the jail only partly: {status:?}"));
        }
        Ok(())
    }

    /// One seccomp filter over the calling thread: each listed call whose rules match (no rule: always)
    /// answers `errno`; every other call is untouched.
    fn install(
        rules: impl IntoIterator<Item = (i64, Vec<SeccompRule>)>,
        errno: i32,
    ) -> Result<(), String> {
        let filter = SeccompFilter::new(
            rules.into_iter().collect::<BTreeMap<_, _>>(),
            SeccompAction::Allow,
            SeccompAction::Errno(errno as u32),
            arch()?,
        )
        .map_err(|e| format!("a seccomp filter could not be built: {e}"))?;
        let program: BpfProgram =
            filter.try_into().map_err(|e| format!("a seccomp filter did not compile: {e}"))?;
        seccompiler::apply_filter(&program)
            .map_err(|e| format!("a seccomp filter could not be applied: {e}"))
    }

    /// A rule matching when every `(argument, value)` pair DIFFERS (32-bit compare: pids, flags).
    fn differs(args: &[(u8, u64)]) -> Result<SeccompRule, String> {
        let conditions = args
            .iter()
            .map(|&(arg, value)| {
                SeccompCondition::new(arg, SeccompCmpArgLen::Dword, SeccompCmpOp::Ne, value)
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("a seccomp condition could not be built: {e}"))?;
        SeccompRule::new(conditions).map_err(|e| format!("a seccomp rule could not be built: {e}"))
    }

    pub(super) fn apply(p: &StrategyProcessPolicy) -> Result<(), String> {
        arch()?;
        only_thread()?;
        no_inherited_descriptor()?;
        // After the descriptor check (these are this process's own) and before anything changes.
        let artifacts = open_artifacts(&p.artifact_root, &p.artifacts)?;
        rustix::process::set_parent_process_death_signal(Some(Signal::KILL))
            .map_err(|e| format!("the parent-death signal could not be set: {e}"))?;
        // Before the rlimits: the artifacts' descriptors stay open until their rules are added.
        landlock(&p.read, AccessFs::ReadFile | AccessFs::ReadDir, &[], false, artifacts)?;
        if let Some(bytes) = p.address_space_bytes {
            limit(Resource::As, bytes, "RLIMIT_AS")?;
        }
        limit(Resource::Nofile, p.open_files, "RLIMIT_NOFILE")?;

        let me = u64::from(rustix::process::getpid().as_raw_nonzero().get().unsigned_abs());
        let mut rules: Vec<(i64, Vec<SeccompRule>)> = [
            libc::SYS_socket,
            libc::SYS_socketpair,
            libc::SYS_io_uring_setup,
            libc::SYS_execve,
            libc::SYS_execveat,
            libc::SYS_ptrace,
            libc::SYS_process_vm_readv,
            libc::SYS_process_vm_writev,
            libc::SYS_pidfd_getfd,
            libc::SYS_pidfd_send_signal,
            libc::SYS_kcmp,
            libc::SYS_keyctl,
            libc::SYS_add_key,
            libc::SYS_request_key,
        ]
        .into_iter()
        .map(|nr| (nr, Vec::new()))
        .collect();
        // A signal only to itself: argument 0 is the target pid (`kill(0)` and `kill(-1)` included).
        let signals = [
            libc::SYS_kill,
            libc::SYS_tkill,
            libc::SYS_tgkill,
            libc::SYS_rt_sigqueueinfo,
            libc::SYS_rt_tgsigqueueinfo,
        ];
        for nr in signals {
            rules.push((nr, vec![differs(&[(0, me)])?]));
        }
        // Scheduling and limits only on itself: argument 0 is the pid, `0` meaning the caller.
        for nr in [
            libc::SYS_prlimit64,
            libc::SYS_sched_setaffinity,
            libc::SYS_sched_setscheduler,
            libc::SYS_sched_setparam,
            libc::SYS_sched_setattr,
        ] {
            rules.push((nr, vec![differs(&[(0, 0), (0, me)])?]));
        }
        // `setpriority(which, who, …)`/`ioprio_set(which, who, …)`: only "this process", by 0 or pid.
        const PRIO_PROCESS: u64 = 0;
        const IOPRIO_WHO_PROCESS: u64 = 1;
        for (nr, process) in
            [(libc::SYS_setpriority, PRIO_PROCESS), (libc::SYS_ioprio_set, IOPRIO_WHO_PROCESS)]
        {
            rules.push((nr, vec![differs(&[(0, process)])?, differs(&[(1, 0), (1, me)])?]));
        }
        install(rules, libc::EACCES)?;

        // Fork: `clone` without `CLONE_THREAD` (a thread stays allowed), and the direct calls.
        let no_thread = SeccompCondition::new(
            0,
            SeccompCmpArgLen::Dword,
            SeccompCmpOp::MaskedEq(libc::CLONE_THREAD as u64),
            0,
        )
        .and_then(|c| SeccompRule::new(vec![c]))
        .map_err(|e| format!("the fork rule could not be built: {e}"))?;
        let forks = [(libc::SYS_clone, vec![no_thread])].into_iter();
        #[cfg(target_arch = "x86_64")]
        let forks = forks.chain([(libc::SYS_fork, Vec::new()), (libc::SYS_vfork, Vec::new())]);
        install(forks, libc::EPERM)?;
        // `clone3` hides its flags behind a pointer seccomp cannot read: `ENOSYS` makes glibc retry
        // with `clone`, which the filter above judges.
        install([(libc::SYS_clone3, Vec::new())], libc::ENOSYS)?;
        refuse_x32()
    }

    /// Each of `artifacts` opened beneath `root` with no symbolic link followed and no way out of it (a walk,
    /// one `openat(O_PATH | O_NOFOLLOW)` per component from the folder's descriptor: every component a plain
    /// name, each folder on the way a directory), and required to be a regular file: the descriptors
    /// [`landlock`] grants by inode (the module doc's "The artifact grant", which says why not `openat2`). The
    /// first refusal is the whole jail's, naming the artifact.
    fn open_artifacts(root: &Path, artifacts: &[PathBuf]) -> Result<Vec<OwnedFd>, String> {
        let folder = rustix::fs::open(
            root,
            OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| {
            format!("the artifacts' folder {} could not be opened: {e}", root.display())
        })?;
        artifacts
            .iter()
            .map(|artifact| {
                let refused = |why: String| {
                    format!(
                        "the artifact {} under {} is refused: {why}",
                        artifact.display(),
                        root.display()
                    )
                };
                let names = artifact
                    .components()
                    .filter(|c| *c != Component::CurDir)
                    .map(|c| match c {
                        Component::Normal(name) => Ok(name),
                        _ => Err(refused(
                            "its path leaves that folder (a `..`, a root or a prefix)".to_string(),
                        )),
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let mut at: Option<OwnedFd> = None;
                for (step, name) in names.iter().enumerate() {
                    let last = step + 1 == names.len();
                    let beneath = at.as_ref().map_or(folder.as_fd(), AsFd::as_fd);
                    // `O_NOFOLLOW` with `O_PATH` opens a link ITSELF, so `fstat` below names it.
                    let next = rustix::fs::openat(
                        beneath,
                        *name,
                        OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                        Mode::empty(),
                    )
                    .map_err(|e| {
                        refused(format!(
                            "`{}` could not be opened inside that folder ({e})",
                            name.display()
                        ))
                    })?;
                    let (wanted, noun) = if last {
                        (FileType::RegularFile, "regular file")
                    } else {
                        (FileType::Directory, "folder")
                    };
                    let kind = rustix::fs::fstat(&next).map(|s| FileType::from_raw_mode(s.st_mode));
                    match kind {
                        Ok(kind) if kind == wanted => at = Some(next),
                        Ok(kind) => {
                            return Err(refused(format!(
                                "`{}` is a {kind:?}, not a {noun}",
                                name.display()
                            )));
                        }
                        Err(e) => {
                            return Err(refused(format!(
                                "the kind of `{}` could not be read ({e})",
                                name.display()
                            )));
                        }
                    }
                }
                at.ok_or_else(|| refused("it names no file".to_string()))
            })
            .collect()
    }

    fn limit(resource: Resource, value: u64, name: &str) -> Result<(), String> {
        rustix::process::setrlimit(resource, Rlimit { current: Some(value), maximum: Some(value) })
            .map_err(|e| format!("{name} could not be set to {value}: {e}"))
    }

    /// Refused unless this process runs ONE thread: the jail binds the calling thread only.
    fn only_thread() -> Result<(), String> {
        let threads = std::fs::read_dir("/proc/self/task")
            .map_err(|e| format!("this process's threads could not be counted (/proc): {e}"))?
            .count();
        if threads == 1 {
            Ok(())
        } else {
            Err(format!(
                "the strategy process policy must be applied before any thread starts, and this \
                 process runs {threads}: the others would stay outside the jail"
            ))
        }
    }

    /// Refused when a descriptor above stderr is open: an open file is one Landlock no longer checks.
    fn no_inherited_descriptor() -> Result<(), String> {
        let listed: Vec<String> = std::fs::read_dir("/proc/self/fd")
            .map_err(|e| format!("this process's descriptors could not be listed (/proc): {e}"))?
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
            .collect();
        // The listing's own descriptor is closed by now, so whatever still resolves is open.
        let open: Vec<String> = listed
            .into_iter()
            .filter(|n| n.parse::<u32>().is_ok_and(|fd| fd > 2))
            .filter_map(|n| {
                let target = std::fs::read_link(format!("/proc/self/fd/{n}")).ok()?;
                Some(format!("{n} ({})", target.display()))
            })
            .collect();
        if open.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "a strategy process starts with stdin, stdout and stderr only, and this one holds \
                 descriptor(s) {}: the spawning process leaked them (one not close-on-exec)",
                open.join(", ")
            ))
        }
    }

    /// x86_64 only: an x32 call (`nr | 0x4000_0000`) carries the 64-bit audit arch, so the numbered
    /// filters above never match it; this one refuses every x32 call. Elsewhere: nothing to do.
    fn refuse_x32() -> Result<(), String> {
        #[cfg(target_arch = "x86_64")]
        {
            use seccompiler::sock_filter;
            const LD_W_ABS: u16 = 0x20; // BPF_LD | BPF_W | BPF_ABS
            const JEQ_K: u16 = 0x15; // BPF_JMP | BPF_JEQ | BPF_K
            const JGE_K: u16 = 0x35; // BPF_JMP | BPF_JGE | BPF_K
            const RET_K: u16 = 0x06; // BPF_RET | BPF_K
            const AUDIT_ARCH_X86_64: u32 = 0xC000_003E;
            const X32_SYSCALL_BIT: u32 = 0x4000_0000;
            let at = |code, jt, jf, k| sock_filter { code, jt, jf, k };
            let program: BpfProgram = vec![
                at(LD_W_ABS, 0, 0, 4), // seccomp_data.arch
                at(JEQ_K, 0, 3, AUDIT_ARCH_X86_64),
                at(LD_W_ABS, 0, 0, 0), // seccomp_data.nr
                at(JGE_K, 0, 1, X32_SYSCALL_BIT),
                at(RET_K, 0, 0, libc::SECCOMP_RET_ERRNO | libc::EACCES as u32),
                at(RET_K, 0, 0, libc::SECCOMP_RET_ALLOW),
            ];
            seccompiler::apply_filter(&program)
                .map_err(|e| format!("the x32 filter could not be applied: {e}"))?;
        }
        Ok(())
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::{Grants, StrategyProcessPolicy};

    const NOT_LINUX: &str =
        "the jail is Linux's Landlock and seccomp, and this platform is not Linux";

    pub(super) fn probe() -> Result<(), String> {
        Err(NOT_LINUX.to_string())
    }

    pub(super) fn probe_in_child(_: &std::path::Path, _: &[&str]) -> Result<(), String> {
        Err(NOT_LINUX.to_string())
    }

    pub(super) fn restrict_current_thread(_: &Grants) -> Result<(), String> {
        Err(NOT_LINUX.to_string())
    }

    pub(super) fn apply(_: &StrategyProcessPolicy) -> Result<(), String> {
        Err(NOT_LINUX.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_strategy_policy_reads_the_artifact_and_the_system_libraries_only() {
        let root = Path::new("/srv/p/user_data/strategy-builds");
        let artifact = PathBuf::from("sdk-1/0123abcd/cd.so");
        let p = strategy_process_policy(root, std::slice::from_ref(&artifact));
        assert_eq!((p.artifact_root.as_path(), p.artifacts.as_slice()), (root, &[artifact][..]));
        for secret in
            ["/srv/p/settings", "/srv/p/user_data", "/home", "/etc/vike", "/proc", "/dev", "/tmp"]
        {
            assert!(
                !p.read.iter().any(|g| Path::new(secret).starts_with(g)),
                "the strategy policy shows {secret}"
            );
        }
        assert_eq!(p.address_space_bytes, Some(LIVE_ADDRESS_SPACE_BYTES));
        assert_eq!(p.open_files, OPEN_FILES);
    }

    /// A portfolio's artifacts are each granted as itself, and a second artifact adds nothing else: the artifacts
    /// are exactly those named, in order, and the read list is the system libraries a one-artifact policy reads.
    #[test]
    fn several_artifacts_add_themselves_and_nothing_else() {
        let artifacts: Vec<PathBuf> = ["a", "b", "c"]
            .iter()
            .map(|s| PathBuf::from(format!("sdk-1/0123abcd/{s}.so")))
            .collect();
        let p = strategy_process_policy(Path::new("/b"), &artifacts);
        let one = strategy_process_policy(Path::new("/b"), &artifacts[..1]);
        assert_eq!(p.artifacts, artifacts);
        assert_eq!(p.read, one.read, "the same system libraries, nothing new");
        assert_eq!(
            (p.address_space_bytes, p.open_files),
            (one.address_space_bytes, one.open_files)
        );
    }
}
