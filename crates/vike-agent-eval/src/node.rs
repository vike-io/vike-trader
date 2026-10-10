//! A real PAPER `vike-tradehub` node, in a throwaway project folder, for one case.
//!
//! The spawn shape is `crates/vike-tradehub/tests/daemon/audit_reaches_disk.rs`'s `Daemon::start`:
//! one self-contained project root, `VIKE_SETTINGS_DIR` naming it outright, the two node HMAC keys
//! in the credential store the daemon itself reads, and the two gates (`VIKE_TRADEHUB_ADDR`,
//! `VIKE_TRADEHUB_CONTROL`) spelled as environment.
//!
//! ⚠ **The keys come from the operator's own flow, not from this crate.**
//! `Project::provision_node_store` runs `vike-cli secrets init` (the empty settings
//! database) and then `vike-cli backend setup`, which MINTS both keys into its `node_key` table.
//! The daemon reads only that table, and every client this harness spawns (`vike-cli mcp`, the
//! unattended runner's server) is handed the same `VIKE_SETTINGS_DIR` and NO key in its
//! environment, so `vike-cli`'s node-key resolution (environment first, then the store) lands on
//! the same two rows. Nothing here knows a key's value, and nothing needs to. The daemon profile
//! is an ACTIVE ROW (0086) rather than a file — `Project::bootstrap_daemon_profile` shells out to
//! `vike-cli config bootstrap-daemon` for it, since this crate depends on no `vike-*` crate.
//! `scripts/cli_mcp_smoke.sh`'s `node_start` carries the same shape (its own
//! `provision_node_store`/`bootstrap_daemon_profile` shell functions, plain bash rather than
//! `Command`).
//!
//! ⚠ NOTHING here can arm a venue. The mount is PAPER — `crates/vike-mount/src/engine.rs`'s
//! `make_engine` consults the per-venue ceiling BEFORE it reads a credential, and this project has
//! neither a `policy.venues` row nor a venue credential, so every engine is the paper client. The
//! only secrets in its store are the two node keys `backend setup` minted for this one project.

use std::collections::HashSet;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use crate::mcp::{CaseEnv, apply_case_env};

/// The paper mount's symbol. Any string works — nothing resolves it against a venue — and this one
/// is the symbol the prompts in `crates/vike-agent-eval/src/cases.rs` name, so a case's expectation
/// can pin the exact `symbol` that reached the node.
pub const NODE_SYMBOL: &str = "CLI_MCP_SMOKE_TOKEN";

/// The venue the paper mount reports. A polymarket profile lowers onto
/// `MakerMountConfig::outcome_token`, the recommended default the minimal profile below inherits,
/// and its venue is what a `node_snapshot` shows the agent — so the agent LEARNS it rather than
/// being told it in the prompt.
pub const NODE_VENUE: &str = "polymarket";

/// The two node-key NAMES, spelled the way the server spells them. A client child must not
/// INHERIT either from the harness's own environment: `vike-cli` reads the environment before the
/// store, so an exported key would beat the pair `backend setup` minted and the handshake would
/// fail with an auth denial that looks like the node's fault. [`crate::mcp::apply_case_env`]
/// removes both. The literals are deliberate: this crate depends on no vike crate.
pub const NODE_KEY_NAMES: [&str; 2] = ["VIKE_TRADEHUB_OBSERVE_KEY", "VIKE_TRADEHUB_CONTROL_KEY"];

/// How long a LIVING daemon has to accept a connection on its port.
///
/// ⚠ It bounds only the case where the child is still running and has not bound yet: a child that
/// has EXITED is detected by `try_wait` inside the poll and reported at once, with its status and
/// its own stderr. Both halves are load-bearing, and the missing one cost a whole lane: a daemon
/// that died 2 ms in (a bad profile path) was waited on for the full window, three attempts over,
/// 270 s per case and 22 minutes for the run — long enough that under the runner's per-test kill
/// (`.config/nextest.toml`) the answer would have been a kill with no reason attached rather than
/// the daemon's own `No such file or directory`.
const LISTEN_TIMEOUT: Duration = Duration::from_secs(20);
/// How many times a node is started before the failure is reported. A bind can lose a race the
/// free-port probe cannot see, so one retry is worth having; the product `LISTEN_TIMEOUT * ATTEMPTS`
/// is the per-case ceiling the suite's nextest budget is written against.
const ATTEMPTS: u32 = 3;
/// How long a stopped daemon has to release its port.
const STOP_TIMEOUT: Duration = Duration::from_secs(30);

/// One case's throwaway project folder: the shape `docs/ops/tradehub-the CI box.md` calls "one
/// self-contained project folder".
pub struct Project {
    pub root: PathBuf,
    pub settings: PathBuf,
}

impl Project {
    /// Create the folder. It holds no store yet: [`Project::provision_node_store`] creates one.
    ///
    /// ⚠ **The root is made ABSOLUTE here, and that is the boundary the whole crate relies on.**
    /// Every path a child is handed — `--config <profile>`, `VIKE_SETTINGS_DIR`, `VIKE_STATE_ROOT`
    /// — is derived from this one, and `PaperNode::spawn` sets the daemon's `current_dir` to it. A
    /// RELATIVE root therefore reaches the child as a path it re-resolves against its own new
    /// working directory, where nothing exists: the daemon exits with `bad profile …: No such file
    /// or directory` and the credential store holding its own node keys is invisible to it. That is
    /// not hypothetical — `main.rs`'s default `--work-dir` is the relative `agent-eval-work`, so
    /// the shipped binary failed every node-bearing case while the test twin (whose work directory
    /// comes from `tempfile::tempdir()`, always absolute) passed. Absolutizing at this one boundary
    /// is what makes the two paths the same shape.
    pub fn create(root: &Path) -> Result<Self, String> {
        // `absolute`, not `canonicalize`: lexical, so it does not require the path to exist yet, and
        // it does not rewrite a Windows path into the `\\?\` verbatim form some child processes
        // cannot open.
        let root = &std::path::absolute(root)
            .map_err(|e| format!("resolve {} to an absolute path: {e}", root.display()))?;
        let settings = root.join("settings");
        for dir in [settings.join("state").join("logs"), root.join("data"), root.join("tmp")] {
            std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        }
        Ok(Self { root: root.to_path_buf(), settings })
    }

    /// Give the project the store a node needs, the way an operator does on a fresh box:
    /// `vike-cli secrets init` creates the EMPTY settings database (the only credential
    /// store; [`Project::bootstrap_daemon_profile`] refuses without one), then `vike-cli backend
    /// setup` mints both node keys into its `node_key` table.
    ///
    /// ⚠ Once per project. `backend setup` refuses to replace keys a client may already hold unless
    /// told `--rotate`, so a second call on the same project fails rather than re-keying it.
    ///
    /// `setup` also writes `config.tradehub_addr` (its default bind address). That row decides
    /// nothing here: [`PaperNode::spawn`] hands the daemon `VIKE_TRADEHUB_ADDR`, which overrides
    /// it, and every client is told the node's address with `--node`. No `--control`: the control
    /// gate stays the `VIKE_TRADEHUB_CONTROL` the spawn sets, as it always was.
    pub fn provision_node_store(&self, vike_cli: &Path) -> Result<(), String> {
        self.run_cli(vike_cli, &["secrets", "init"])?;
        self.run_cli(vike_cli, &["backend", "setup"])
    }

    /// Run one `vike-cli` verb against this project's settings directory, failing with its stderr.
    fn run_cli(&self, vike_cli: &Path, args: &[&str]) -> Result<(), String> {
        let mut cmd = Command::new(vike_cli);
        cmd.args(args);
        // Same absolutizing boundary `PaperNode::spawn` relies on: `self.root`/`self.settings` are
        // already absolute (`Project::create`'s doc), so this child sees them correctly regardless
        // of the harness process's own working directory.
        cmd.current_dir(&self.root);
        cmd.env("VIKE_SETTINGS_DIR", &self.settings);
        for name in NODE_KEY_NAMES {
            cmd.env_remove(name);
        }
        cmd.env_remove("RUST_LOG");
        cmd.env_remove("VIKE_LOG");
        cmd.env("VIKE_LOG_FILE_LEVEL", "warn");
        let verb = args.join(" ");
        let out = cmd.output().map_err(|e| format!("spawn {} {verb}: {e}", vike_cli.display()))?;
        if !out.status.success() {
            return Err(format!(
                "{} {verb} exited {}: {}",
                vike_cli.display(),
                out.status,
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        Ok(())
    }

    /// The minimal PAPER daemon profile, as an ACTIVE ROW rather than a file — `vike-tradehub`
    /// reads no profile TOML any more (0086), so the only way to give a fresh project's store a
    /// daemon profile is the bootstrap verb this shells out to, which builds a `StoredProfile`
    /// straight from argv and activates it in one act. `NODE_VENUE`/`NODE_SYMBOL` are the whole
    /// requirement: every other mount field is optional and inherits the bootstrap verb's own
    /// recommended default.
    ///
    /// This crate depends on no `vike-*` crate, so the row is built by running the SHIPPED
    /// `vike-cli` binary — never by importing `vike_secrets::profile_store` directly. Requires
    /// [`Project::provision_node_store`] to have run first.
    pub fn bootstrap_daemon_profile(&self, vike_cli: &Path) -> Result<(), String> {
        self.run_cli(
            vike_cli,
            &[
                "config",
                "bootstrap-daemon",
                "agent-eval",
                "--venue",
                NODE_VENUE,
                "--asset-class",
                "PredictionMarket",
                "--token-id",
                NODE_SYMBOL,
            ],
        )
    }
}

/// Is nothing listening on this loopback port?
fn port_is_free(port: u16) -> bool {
    TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}").parse().expect("a loopback address parses"),
        Duration::from_millis(200),
    )
    .is_err()
}

/// Every port [`pick_port`] has handed out in THIS process — never released, so a run of cases
/// can never be handed one port twice (a process runs a few dozen cases at most).
fn handed_out() -> &'static Mutex<HashSet<u16>> {
    static PORTS: OnceLock<Mutex<HashSet<u16>>> = OnceLock::new();
    PORTS.get_or_init(|| Mutex::new(HashSet::new()))
}

/// A free loopback port BELOW THE EPHEMERAL FLOOR, and one this process has not handed out before.
///
/// ⚠ The floor is the point. [`port_is_free`] finds a LISTENER; it cannot see an OUTBOUND
/// connection whose local end happens to be that port, and a bind on one of those fails with
/// EADDRINUSE while the daemon keeps trading headless. Measured on the CI box 2026-09-05 by
/// `scripts/cli_mcp_smoke.sh`'s `pick_port`, whose rule this is: a port inside the kernel's
/// ephemeral range was taken by runner traffic between the probe and the bind. Below the range's
/// low bound the kernel never hands a port out on its own, so the probe's answer is the whole truth.
///
/// ⚠ **…about OTHER processes. Inside this one, the probe is not enough, and the ledger is why.**
/// `crates/vike-agent-eval/tests/scripted_pipeline.rs` runs its cases CONCURRENTLY, and a port a
/// case has picked reads free to the probe until that case's daemon binds it — so a second case
/// probing in that window would be handed the same port. Two outcomes, both silent: its daemon
/// loses the bind while [`PaperNode::wait_listening`] sees the FIRST daemon listening and reports
/// success, or a case's dead "unreachable datahub" port turns out to be another case's live node.
/// Either way one case's MCP server dials another case's daemon. Each case's keys are minted for
/// its own project, so the handshake refuses that — but the case then fails on an auth denial
/// that names nothing about ports, which is a worse diagnosis than never sharing a port. So the
/// pick and its record are one step under one lock — the probe included, which serialises picks (a
/// loopback connect each, a few per case) and nothing else.
pub fn pick_port() -> Result<u16, String> {
    let mut low: u32 = 32768;
    if let Ok(text) = std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range")
        && let Some(first) = text.split_whitespace().next()
        && let Ok(v) = first.parse::<u32>()
    {
        low = v;
    }
    if low <= 20100 {
        low = 32768;
    }
    let span = low - 20000;
    // A cheap, dependency-free spread: the process id and the clock, folded. Uniqueness is not
    // required of it — the ledger and the probe below decide — only that two harnesses starting
    // together do not walk the same sequence.
    let mut seed = u64::from(std::process::id()).wrapping_mul(2_654_435_761).wrapping_add(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| u64::from(d.subsec_nanos()))
            .unwrap_or(0),
    );
    let mut taken = handed_out().lock().unwrap_or_else(PoisonError::into_inner);
    for _ in 0..64 {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        let port = 20000 + u16::try_from((seed >> 33) % u64::from(span)).unwrap_or(0);
        if !taken.contains(&port) && port_is_free(port) {
            taken.insert(port);
            return Ok(port);
        }
    }
    Err("no free loopback port below the ephemeral floor after 64 probes".into())
}

/// A running paper node.
pub struct PaperNode {
    child: Option<Child>,
    binary: PathBuf,
    project_root: PathBuf,
    settings: PathBuf,
    log: PathBuf,
    /// Variables removed from every child's environment on top of [`apply_case_env`]'s own list —
    /// the harness binary's secrets, which no child of it has any use for.
    scrub: Vec<String>,
    pub port: u16,
    pub addr: String,
}

impl PaperNode {
    /// Pick a port, spawn the daemon, and wait — bounded — until it accepts a connection.
    ///
    /// A bind can still lose a race the probe cannot see, so a daemon that never listened is
    /// stopped and started again on a fresh port, up to [`ATTEMPTS`] times. A daemon that EXITED
    /// is not retried at all: [`PaperNode::wait_listening`] returns its status immediately, and
    /// starting a dead configuration again on a different port would only spend the budget to
    /// reach the same answer.
    pub fn start(
        vike_cli: &Path,
        binary: &Path,
        project: &Project,
        scrub: &[&str],
    ) -> Result<Self, String> {
        project.provision_node_store(vike_cli)?;
        project.bootstrap_daemon_profile(vike_cli)?;
        let log = project.root.join("node.err");
        let mut last = String::new();
        for attempt in 1..=ATTEMPTS {
            let port = pick_port()?;
            let mut node = Self {
                child: None,
                binary: binary.to_path_buf(),
                project_root: project.root.clone(),
                settings: project.settings.clone(),
                log: log.clone(),
                scrub: scrub.iter().map(|s| (*s).to_string()).collect(),
                port,
                addr: format!("127.0.0.1:{port}"),
            };
            node.spawn()?;
            match node.wait_listening() {
                Ok(()) => return Ok(node),
                Err(why) => {
                    last = format!("attempt {attempt} of {ATTEMPTS}: {why}; {}", node.log_tail());
                    let exited = node.child.is_none();
                    let _ = node.stop();
                    if exited {
                        return Err(last);
                    }
                }
            }
        }
        Err(last)
    }

    /// Spawn the daemon on the port this node already holds. Used by [`PaperNode::start`] and by a
    /// case that stops the node and brings it back on the SAME port.
    pub fn spawn(&mut self) -> Result<(), String> {
        let out = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.project_root.join("node.out"))
            .map_err(|e| format!("open node.out: {e}"))?;
        let err = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)
            .map_err(|e| format!("open {}: {e}", self.log.display()))?;
        let mut cmd = Command::new(&self.binary);
        // No `--config`: the daemon reads its profile from the ACTIVE row `Project::
        // bootstrap_daemon_profile` already stored and activated (0086) — the argument is retired
        // and accepted-but-ignored, so passing it would only claim a file this harness never writes.
        // ⚠ Every path handed to this child — the two directories `apply_case_env` sets below — is
        // ABSOLUTE (`Project::create` is the boundary that makes it so), because this line moves the
        // child's working directory out from under them.
        cmd.current_dir(&self.project_root);
        let scrub: Vec<&str> = self.scrub.iter().map(String::as_str).collect();
        apply_case_env(&mut cmd, &CaseEnv { settings_dir: &self.settings, scrub: &scrub });
        cmd.env("VIKE_STATE_ROOT", self.settings.join("state"));
        cmd.env("VIKE_TRADEHUB_ADDR", &self.addr);
        cmd.env("VIKE_TRADEHUB_CONTROL", "1");
        // Stdout is this daemon's PROTOCOL (a periodic JSON summary line) — to a FILE rather than a
        // pipe, because an undrained pipe eventually fills and BLOCKS the daemon. Stdin is null so
        // it can never inherit and hold open the MCP server's stdin pipe, which is the shape that
        // hung a whole smoke run for an hour on the CI box (2026-09-05).
        cmd.stdin(Stdio::null()).stdout(Stdio::from(out)).stderr(Stdio::from(err));
        self.child =
            Some(cmd.spawn().map_err(|e| format!("spawn {}: {e}", self.binary.display()))?);
        Ok(())
    }

    /// Poll `connect` rather than watching for a log line: the socket is the thing the next step
    /// actually needs, and a wait on a message keeps passing after the daemon stops binding.
    ///
    /// ⚠ This probe cannot tell OUR daemon's listener from anyone else's, and the question it
    /// raises has to be answered rather than waved at: the CI box runs a live `vike-tradehub`, and this
    /// harness is in the derived CI lane. What makes the answer safe is not the port arithmetic —
    /// it is the KEYS. Every link the MCP server opens is HMAC-authenticated with the pair
    /// `backend setup` minted into THIS project's store a moment earlier (256 random bits each),
    /// which no real node shares, so a session that somehow reached a foreign daemon would be
    /// refused at the handshake and the case would fail loudly. There is no arrangement of ports in
    /// which a key minted for a throwaway project writes to a real node.
    ///
    /// ⚠ **The child is polled alongside the port.** A wait on the socket alone cannot distinguish
    /// "not bound yet" from "exited two milliseconds ago", so it spends the whole window on a
    /// daemon that is already gone and then reports the generic sentence — hiding the daemon's own
    /// one-line reason behind a timeout. `try_wait` answers that question directly: an exited child
    /// is reaped, the handle cleared (so the caller can tell a dead configuration from a lost bind
    /// race), and its exit status named.
    pub fn wait_listening(&mut self) -> Result<(), String> {
        let deadline = Instant::now() + LISTEN_TIMEOUT;
        while Instant::now() < deadline {
            if !port_is_free(self.port) {
                return Ok(());
            }
            match self.child.as_mut().map(Child::try_wait) {
                Some(Ok(Some(status))) => {
                    self.child = None;
                    return Err(format!(
                        "the daemon EXITED before it listened on {} ({status})",
                        self.addr
                    ));
                }
                Some(Err(e)) => return Err(format!("waiting on the daemon: {e}")),
                _ => {}
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Err(format!(
            "the daemon never accepted a connection on {} within {}s",
            self.addr,
            LISTEN_TIMEOUT.as_secs()
        ))
    }

    /// Kill the daemon and wait — bounded — for its port to come free.
    pub fn stop(&mut self) -> Result<(), String> {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let deadline = Instant::now() + STOP_TIMEOUT;
        while Instant::now() < deadline {
            if port_is_free(self.port) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Err(format!(
            "{} was still accepting connections {}s after the daemon was killed",
            self.addr,
            STOP_TIMEOUT.as_secs()
        ))
    }

    /// The tail of the daemon's stderr — the evidence a failure message would otherwise lack.
    pub fn log_tail(&self) -> String {
        let text = std::fs::read_to_string(&self.log).unwrap_or_default();
        let tail: Vec<&str> = text.lines().rev().take(20).collect();
        if tail.is_empty() {
            "its stderr is empty".into()
        } else {
            format!(
                "daemon stderr (tail): {}",
                tail.into_iter().rev().collect::<Vec<_>>().join(" | ")
            )
        }
    }
}

impl Drop for PaperNode {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
