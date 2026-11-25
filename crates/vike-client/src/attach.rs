//! **Attach this box to a tradehub** (decision
//! `docs/decisions/0288-every-device-signs-with-its-own-key-one-access-list-decides-who-and-a-guest-joins-by-one-invitation.md`):
//! this box's device key and its registration with the server, the ssh tunnel, and its record. ONE
//! implementation for both callers: `vike-cli backend connect` (which then writes the dial rows and
//! verifies) and the desktop's Server group, which runs it IN ITS OWN PROCESS, so a user who has only
//! `vike-desktop` needs no `vike-cli` beside it. The REMOTE side is unchanged: over ssh the server runs
//! its OWN `vike-cli access add`; locally this box runs the system `ssh` client and, for a server on
//! this computer, `docker exec -i`.
//!
//! # The tunnel, and why the exact flags are the flags
//!
//! ```text
//! ssh -N -f -o ExitOnForwardFailure=yes -o ServerAliveInterval=30 -o ServerAliveCountMax=3 \
//!     -o Compression=yes -L <port>:localhost:<port> -L 7878:localhost:7878 -L 7880:localhost:7880 <host>
//! ```
//!
//! - The second `-L` is the datahub's: `data` reads and `trade health`'s gatewayhub row (read through
//!   the datahub) go there, and a loopback-bound datahub has no other route; the third is the
//!   backtesthub's, so a remote Studio reaches it. One ssh, so one `vike-cli backend disconnect`
//!   closes them all; a port already taken here refuses the whole tunnel.
//! - `-N` runs no remote command: this is a forward, not a login.
//! - `-f` backgrounds ssh **after** the forward is established, which is what makes the pair below
//!   work at all: with `ExitOnForwardFailure=yes`, a non-zero exit from this command means the
//!   FORWARD failed, so the tunnel's success is synchronously observable instead of being something
//!   to poll for. ⚠ It is also why `disconnect` cannot simply hold a child handle.
//!   ⚠ **Windows' OpenSSH does not detach on `-f`** (System32's 9.5p2, measured 2026-11-07: a bare
//!   `ssh -N -f` still ran at 25 s, its forward answering after 1.0 s), so waiting for it hung
//!   `connect` silently. There ssh runs WITHOUT `-f` and the forward IS something to poll for:
//!   [`raise_unforked_tunnel`]. ⚠ **`creation_flags` and null stdio are not enough, measured the same
//!   day**: `std::process::Command` always creates a child with `bInheritHandles`, so ssh still held
//!   every inheritable handle of this process, and a caller capturing this command's stdout read no
//!   EOF until the tunnel died (and so did `cmd /c start`). Clearing the flag needs a Win32 call and
//!   this workspace forbids `unsafe`, so PowerShell's `Start-Process` starts ssh instead: the pipe ended
//!   at 1.5 s, ssh outlived its launcher, a bad name passed ssh's 255 through, and a silent address was
//!   stopped at the limit. No test runs on Windows, so the argument list, the script and the poll are
//!   tested on Linux, and only a run of this build on Windows proves the code does the same.
//! - `ExitOnForwardFailure=yes` turns "the port was already taken" from a silent success (an ssh
//!   session sitting there forwarding nothing) into a refusal.
//! - `ServerAliveInterval=30` / `ServerAliveCountMax=3` bound the SILENT death — a sleeping laptop,
//!   a dropped VPN — that the socket never reports.
//! - `Compression=yes` because everything the three hubs answer is JSON, and the link to a server is
//!   the slow part. Measured 2026-11-16 (the owner's PC to the CI box, 23% packet loss on that route): a
//!   Studio run of 66,786 bars took 2.6 s to compute and about 75 s to cross (an answer of about
//!   2.8 MB). 1.97 MB of that run's saved JSON (its equity series and trades) is about 5x smaller
//!   gzipped, and crossed the same ssh in 58 and 63 s compressed against 131 and 118 s plain.
//!
//! # The device key, and the three registrations
//!
//! This box signs with its OWN Ed25519 key ([`invitation::this_device`], made once and kept in this
//! box's `device_key` table), and no server secret is copied to it. [`Source::Server`] pipes the
//! key's PUBLIC half over ONE ssh session into `DIR/bin/vike-cli access add owner DEVICE --role admin`
//! ([`fetch_args`], [`remote_command`], [`send_public_key`]): whoever holds a shell there is the
//! server's owner. [`Source::ThisComputer`] is the same pipe into a server on THIS computer (a Docker
//! Desktop container by `docker exec -i`, or a host install's own `bin/`, [`local_command`]), dialled
//! at 127.0.0.1 with no tunnel. [`Source::Invitation`] is a guest's ONE paste ([`invitation`]).
//! [`Source::Held`] registers nothing and raises the tunnel for the key this box already holds.

pub mod invitation;

use std::fmt;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use vike_config::config::{DEFAULT_BACKTEST_PORT, DEFAULT_DATAHUB_PORT, TUNNEL_RECORD};
use vike_hub_protocol::access::{DeviceKey, OWNER_DEVICE_ROLE};

use invitation::{DEVICE_OWNER, Invitation, held_device, join, this_device};

/// Which rung a refusal belongs on: `vike-cli`'s exit ladder maps each one (`Usage` 2, `Failed` 1,
/// `Connect` 3, `Refused` 4); the desktop shows the message. [`attach`] uses the first three, the live
/// account report (`crate::account_report`'s `ReportError`) all four.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rung {
    /// The command line (or the pasted text) was wrong.
    Usage,
    /// This box refused, or a server answered no.
    Failed,
    /// A service could not be reached.
    Connect,
    /// The answer would be wrong, so none is given (`vike-cli`'s `Exit::Refused`).
    Refused,
}

/// A refusal of [`attach`] or one of its steps, in the words the verb always printed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachError {
    /// The rung it exits on.
    pub rung: Rung,
    /// What went wrong.
    pub message: String,
}

impl AttachError {
    /// The command line was wrong.
    pub fn usage(message: impl Into<String>) -> Self {
        Self { rung: Rung::Usage, message: message.into() }
    }

    /// This box refused, or a server answered no.
    pub fn failed(message: impl Into<String>) -> Self {
        Self { rung: Rung::Failed, message: message.into() }
    }

    /// A service could not be reached.
    pub fn connect(message: impl Into<String>) -> Self {
        Self { rung: Rung::Connect, message: message.into() }
    }
}

impl fmt::Display for AttachError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// A result of this module.
pub type AttachResult<T> = Result<T, AttachError>;

/// Where a run's words go. The CLI prints them; the desktop keeps them for its Server group, which
/// has no terminal.
pub struct Words<'a> {
    /// Each line the run says.
    pub say: &'a dyn Fn(&str),
    /// A warning that does not stop the run (the CLI's stderr).
    pub warn: &'a dyn Fn(&str),
    /// A child's (ssh's, docker's) own output: `true` lets it write to this process's streams as a
    /// terminal shows it (the CLI); `false` reads it and hands each line to [`Self::say`] once the
    /// child has ended (the desktop).
    pub inherit: bool,
}

impl Words<'_> {
    /// Run `command` to its end with its output where [`Self::inherit`] says, and its stdin piped
    /// only when `input` is given (written, then closed).
    fn run(&self, mut command: Command, input: Option<&str>) -> io::Result<ExitStatus> {
        let (out, err) = if self.inherit {
            (Stdio::inherit(), Stdio::inherit())
        } else {
            (Stdio::piped(), Stdio::piped())
        };
        command
            .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(out)
            .stderr(err);
        hide_window(&mut command);
        let mut child = command.spawn()?;
        if let (Some(text), Some(mut stdin)) = (input, child.stdin.take()) {
            // Dropping the pipe closes it, which ends the input.
            let _ = writeln!(stdin, "{text}");
        }
        if self.inherit {
            return child.wait();
        }
        let output = child.wait_with_output()?;
        for bytes in [&output.stdout, &output.stderr] {
            for line in String::from_utf8_lossy(bytes).lines().filter(|l| !l.trim().is_empty()) {
                (self.say)(line);
            }
        }
        Ok(output.status)
    }
}

/// Where this box registers its device key, if anywhere, before the tunnel.
pub enum Source<'a> {
    /// A remote server's project folder, reached over ssh (`connect HOST --server-project DIR`).
    Server(&'a str),
    /// The server on THIS computer: `docker:NAME` or its project folder (`connect --local-server`),
    /// dialled at 127.0.0.1 with no tunnel.
    ThisComputer(&'a str),
    /// A guest's invitation (`connect --invitation`).
    Invitation(&'a Invitation),
    /// None: the device key this box already holds.
    Held,
}

/// The `config` section's CLIENT-side dial key of the tradehub, the address [`Attached::dial`] is
/// filed under and the one `vike-cli backend status` asks. `crates/vike-config/src/config.rs`'s
/// `Config::node_addr` sorts every address by whose box holds the value and whether that box listens
/// or dials.
pub const DIAL_ADDRESS_KEY: &str = "config.node_addr";

/// The same box's dial key of its datahub (`Config::datahub_addr`), for [`Attached::datahub_dial`].
pub const DATAHUB_DIAL_KEY: &str = "config.datahub_addr";

/// What [`attach`] leaves: this box's device key and the two addresses it is now dialled at.
pub struct Attached {
    /// The key every dial of this box signs with.
    pub device: DeviceKey,
    /// The tradehub's dial address (`config.node_addr`).
    pub dial: String,
    /// The datahub's dial address (`config.datahub_addr`).
    pub datahub_dial: String,
}

impl Attached {
    /// The settings rows a connect files once attached, in the order it writes them: `vike-cli backend
    /// connect` and the desktop's Server group each write them through
    /// [`crate::settings_row::write_setting_journalled`].
    pub fn dial_rows(&self) -> [(&'static str, &str); 2] {
        [(DIAL_ADDRESS_KEY, &self.dial), (DATAHUB_DIAL_KEY, &self.datahub_dial)]
    }
}

/// `connect`'s steps 2 and 3: this box's device key and its registration (BEFORE the tunnel: a
/// server that will not take it raises nothing), then the tunnel and its record. `--no-tunnel`
/// (`no_tunnel`) dials the host itself, and so does [`Source::ThisComputer`] (a server on this
/// computer has no forward to raise); otherwise the dial address is the LOCAL mouth of the forward,
/// which is what makes a loopback-bound daemon reachable at all.
///
/// `state_dir` is where the tunnel's record goes ([`record_tunnel`]); `now_ms` stamps it.
///
/// # Errors
/// The step that refused, on its rung.
pub fn attach(
    host: &str,
    port: u16,
    no_tunnel: bool,
    source: Source<'_>,
    settings_dir: &Path,
    state_dir: Option<&Path>,
    now_ms: i64,
    words: &Words<'_>,
) -> AttachResult<Attached> {
    let local = matches!(source, Source::ThisComputer(_));
    let device = match source {
        Source::Invitation(invitation) => join(invitation, settings_dir, words)?.0,
        Source::Server(project) => {
            let (key, device) = this_device(settings_dir, words)?;
            let mut ssh = Command::new("ssh");
            ssh.args(fetch_args(host, &remote_command(project, &device)?));
            send_public_key(ssh, host, &key, &device, words)?;
            key
        }
        Source::ThisComputer(server) => {
            let (key, device) = this_device(settings_dir, words)?;
            let from = format!("the server on this computer ({server})");
            send_public_key(local_command(server, &device)?, &from, &key, &device, words)?;
            key
        }
        Source::Held => held_device(settings_dir)?.map(|(key, _)| key).ok_or_else(|| {
            AttachError::failed(format!(
                "this box holds no device key, so it has nothing to sign with. Register one with \
                 the server: `vike-cli backend connect {host} --server-project <its project \
                 folder>` (over your ssh login), or paste its owner's invitation into `vike-cli \
                 backend connect --invitation`. Nothing was done."
            ))
        })?,
    };
    let (dial, datahub_dial) = if no_tunnel || local {
        let why = if no_tunnel { "--no-tunnel" } else { "--local-server" };
        (words.say)(&format!("{why}: dialling {host}:{port} directly, raising no ssh forward"));
        (format!("{host}:{port}"), format!("{host}:{DEFAULT_DATAHUB_PORT}"))
    } else {
        raise_tunnel(host, port, words)?;
        (words.say)(&format!("tunnel up: {} {host}", forward_args(port).join(" ")));
        record_tunnel(state_dir, host, port, now_ms, words);
        (format!("127.0.0.1:{port}"), format!("127.0.0.1:{DEFAULT_DATAHUB_PORT}"))
    };
    Ok(Attached { device, dial, datahub_dial })
}

/// The `vike-cli` words that register this box's device key on a server: the OWNER's device at the
/// role `admin` (whoever runs a command there owns that server), the public key on stdin. `device` is
/// a `valid_name`, so it needs no quoting.
fn register_words(device: &str) -> [&str; 6] {
    ["access", "add", DEVICE_OWNER, device, "--role", OWNER_DEVICE_ROLE.name()]
}

/// The command `--server-project DIR` runs on the server, over ssh: that project's own `vike-cli`,
/// aimed at that project's settings, registering this box's device key from the pipe ssh gives it.
/// PURE.
///
/// ⚠ The path is put inside single quotes on the REMOTE shell's command line, so it is held to an
/// absolute path of plain characters (letters, digits, `/`, `.`, `_`, `-`): anything else — a quote,
/// a space, a `$` — is refused rather than escaped, and nothing is sent.
///
/// # Errors
/// A folder that is not such a path.
pub fn remote_command(project: &str, device: &str) -> AttachResult<String> {
    let root = project.trim_end_matches('/');
    let plain = |c: char| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-');
    if !root.starts_with('/') || !root.chars().all(plain) {
        return Err(AttachError::usage(format!(
            "--server-project takes the server's project folder as an absolute path of letters, \
             digits, `/`, `.`, `_` and `-` (the folder that holds bin/vike-cli and settings/), not \
             '{project}'. Nothing was sent."
        )));
    }
    let words = register_words(device).join(" ");
    Ok(format!("VIKE_SETTINGS_DIR='{root}/settings' '{root}/bin/vike-cli' {words}"))
}

/// The `ssh` argument list of the registration: no terminal on the far side (`-T`, so `access add`
/// reads the pipe), a bounded connect, the host and the one command. PURE.
pub fn fetch_args(host: &str, remote: &str) -> Vec<String> {
    ["-T", "-o", "ConnectTimeout=15", host, remote].map(str::to_string).to_vec()
}

/// The command `--local-server WHERE` runs on THIS computer, the pipe twin of [`remote_command`]:
/// `docker:NAME` runs the image's own `vike-cli` inside the container `NAME` (`docker exec -i`,
/// which inherits the container's `VIKE_SETTINGS_DIR` and passes the pipe), and an absolute folder
/// runs `WHERE/bin/vike-cli` aimed at `WHERE/settings` (the host install). No shell reads either
/// line, so nothing is quoted. Whoever can run either already owns that server's `vike.db`.
///
/// # Errors
/// A container name that reads as a flag, or a folder that is not absolute.
pub fn local_command(server: &str, device: &str) -> AttachResult<Command> {
    let words = register_words(device);
    if let Some(name) = server.strip_prefix("docker:") {
        if name.is_empty() || name.starts_with('-') {
            return Err(AttachError::usage(format!(
                "--local-server docker:NAME names the server's container (vikectl names it `vike`), \
                 not '{server}'. Nothing was sent."
            )));
        }
        let mut docker = Command::new("docker");
        docker.args(["exec", "-i", name, "/opt/vike/bin/vike-cli"]).args(words);
        return Ok(docker);
    }
    let root = Path::new(server);
    if !root.is_absolute() {
        return Err(AttachError::usage(format!(
            "--local-server takes `docker:NAME` or the server's project folder as an absolute path \
             (the folder that holds bin/vike-cli and settings/), not '{server}'. Nothing was sent."
        )));
    }
    let mut cli = Command::new(root.join("bin").join("vike-cli"));
    cli.args(words).env(vike_paths::state_path::SETTINGS_DIR_ENV, root.join("settings"));
    Ok(cli)
}

/// Register this box's device key at `from` by running `command` ([`fetch_args`]' ssh, or
/// [`local_command`]): the key's PUBLIC half, 64 lower-case hex characters, goes to its stdin, and
/// its own words go where `words` says. The private half never leaves this box.
///
/// # Errors
/// A command that would not run, or that exited non-zero.
pub fn send_public_key(
    command: Command,
    from: &str,
    key: &DeviceKey,
    device: &str,
    words: &Words<'_>,
) -> AttachResult<()> {
    let program = command.get_program().to_string_lossy().into_owned();
    let status = words
        .run(command, Some(&vike_utility::hex::hex_lower(&key.public_key())))
        .map_err(|e| {
            AttachError::connect(format!(
                "could not run `{program}` to register this box at {from}: {e}. Nothing was \
                 registered."
            ))
        })?;
    if !status.success() {
        let code = status.code().map_or_else(|| "signal".to_string(), |c| c.to_string());
        return Err(AttachError::connect(format!(
            "{from} did not register this box's device key (`{program}` exit {code}; its own words \
             are above). The usual causes: the host is unreachable or your ssh key is not accepted \
             there, --server-project or --local-server does not name the folder holding \
             bin/vike-cli (or the container is not running), or that vike-cli is older than \
             `access add` (decision 0288)."
        )));
    }
    let role = OWNER_DEVICE_ROLE.name();
    (words.say)(&format!(
        "registered this box's device key {} at {from} as {DEVICE_OWNER}/{device}, role {role} (its \
         public half only)",
        key.key_id()
    ));
    Ok(())
}

/// The `-L` arguments of the tunnel, one per [`forward_ports`] port, the node's first: `vike-cli
/// backend disconnect`'s pattern matches its spec and then the host, with anything between. PURE, so
/// the order is unit-tested.
pub fn forward_args(port: u16) -> Vec<String> {
    forward_ports(port)
        .into_iter()
        .flat_map(|p| ["-L".to_string(), format!("{p}:localhost:{p}")])
        .collect()
}

/// The whole `ssh` argument list. `fork` is every box's but Windows' (the module doc): `-f`, or else
/// `BatchMode=yes`, because an ssh with no console a person can see must fail at once where it would
/// ask. PURE, so both shapes are unit-tested here, where no Windows test runs.
pub fn ssh_args(host: &str, port: u16, fork: bool) -> Vec<String> {
    let mut args = vec!["-N".to_string()];
    if fork {
        args.push("-f".to_string());
    } else {
        args.extend(["-o".to_string(), "BatchMode=yes".to_string()]);
    }
    for option in [
        "ExitOnForwardFailure=yes",
        "ServerAliveInterval=30",
        "ServerAliveCountMax=3",
        "Compression=yes",
    ] {
        args.extend(["-o".to_string(), option.to_string()]);
    }
    args.extend(forward_args(port));
    args.push(host.to_string());
    args
}

/// The refusal of a tunnel that did not come up, `why` in ssh's own terms.
fn refused_to_forward(host: &str, port: u16, why: &str) -> AttachError {
    let ports = forward_ports(port);
    AttachError::connect(format!(
        "ssh refused to forward {ports:?} to {host} ({why}). The usual causes, in order: a \
         local port is already taken, by an earlier tunnel (`vike-cli backend disconnect` \
         closes the one this command raised) or by a datahub or backtesthub on this box, \
         the host is unreachable, or your ssh key is not accepted there. Nothing was written."
    ))
}

/// Raise the forward. Blocks until ssh has either established it or refused — see the module doc for
/// why `-f` plus `ExitOnForwardFailure=yes` is what buys that, and for Windows, where it does not.
/// ⚠ ssh's output is this process's own or nothing, never a pipe: a forked ssh would hold a pipe
/// open for the tunnel's whole life.
///
/// # Errors
/// An ssh that would not start or did not forward.
pub fn raise_tunnel(host: &str, port: u16, words: &Words<'_>) -> AttachResult<()> {
    if cfg!(windows) {
        return raise_unforked_tunnel(host, port);
    }
    let mut ssh = Command::new("ssh");
    ssh.args(ssh_args(host, port, true));
    if !words.inherit {
        ssh.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    }
    let status = ssh.status().map_err(no_ssh(host))?;
    if !status.success() {
        let code = status.code().map_or_else(|| "signal".to_string(), |c| c.to_string());
        return Err(refused_to_forward(host, port, &format!("exit {code}")));
    }
    Ok(())
}

/// The failure of an ssh that would not start.
fn no_ssh(host: &str) -> impl Fn(io::Error) -> AttachError + '_ {
    move |e| {
        AttachError::connect(format!(
            "could not run `ssh` to raise the tunnel to {host}: {e}. Pass --no-tunnel if this \
             client is already inside the trust boundary."
        ))
    }
}

/// How long the launcher of [`raise_unforked_tunnel`] gives ssh to log in and bind its forwards, for
/// a host that takes a connection and then says nothing.
pub const TUNNEL_WAIT: Duration = Duration::from_secs(30);

/// What that launcher exits with when [`TUNNEL_WAIT`] ran out and it stopped ssh.
const TUNNEL_GAVE_UP: i32 = 124;

/// This side's limit on the launcher itself, past [`TUNNEL_WAIT`] so that it is the launcher that
/// stops ssh. Meeting it means the launcher hung, which can leave an ssh behind, and the refusal says so.
const LAUNCHER_WAIT: Duration = Duration::from_secs(40);

/// [`raise_tunnel`] where `-f` does not return (the module doc): a launcher starts ssh with nothing
/// inherited and stays beside it until the forwards answer, and is then ended, which leaves ssh up.
///
/// ⚠ A port that answers BEFORE ssh runs is somebody else's, and asking afterwards could not tell: it
/// would answer at once while ssh, still logging in, has not yet met `ExitOnForwardFailure`. So it is
/// refused first, as ssh itself refuses it on every other box.
fn raise_unforked_tunnel(host: &str, port: u16) -> AttachResult<()> {
    let ports = forward_ports(port);
    if let Some(taken) = ports.iter().find(|p| port_answers(**p)) {
        return Err(refused_to_forward(host, port, &format!("local port {taken} already answers")));
    }
    let script = unforked_ssh_script(&ssh_args(host, port, false));
    let mut launcher = Command::new("powershell.exe");
    launcher
        .args(["-NoProfile", "-NonInteractive", "-Command", script.as_str()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    hide_window(&mut launcher);
    let mut child = launcher.spawn().map_err(|e| {
        AttachError::connect(format!(
            "could not run `powershell.exe`, which starts ssh on Windows, to raise the tunnel to \
             {host}: {e}. Pass --no-tunnel if this client is already inside the trust boundary."
        ))
    })?;
    wait_for_forwards(&mut child, ports, port_answers, LAUNCHER_WAIT)
        .map_err(|why| refused_to_forward(host, port, &why))?;
    // It holds the caller's pipes for as long as it lives. ssh is no child it can take with it.
    let _ = child.kill();
    let _ = child.wait();
    Ok(())
}

/// The launcher's script. `Start-Process` starts ssh through the shell, which inherits no handle, and
/// keeps its own console hidden; the script then waits on ssh, so the launcher's exit IS ssh's, and
/// stops it at [`TUNNEL_WAIT`]. PURE, so it is tested where no Windows test runs: a single quote in an
/// argument is doubled, the one escape of the literal each is written in.
fn unforked_ssh_script(ssh_args: &[String]) -> String {
    let quoted: Vec<String> =
        ssh_args.iter().map(|arg| format!("'{}'", arg.replace('\'', "''"))).collect();
    format!(
        "$ErrorActionPreference = 'Stop'; \
         $p = Start-Process ssh -WindowStyle Hidden -PassThru -ArgumentList {}; \
         $null = $p.Handle; \
         if ($p.WaitForExit({})) {{ exit $p.ExitCode }}; \
         Stop-Process -Id $p.Id -Force; exit {TUNNEL_GAVE_UP}",
        quoted.join(","),
        TUNNEL_WAIT.as_millis()
    )
}

/// The local ports a tunnel binds, the node's first, then the datahub's and the backtesthub's, none
/// twice (ssh would refuse a second bind of one port).
pub fn forward_ports(port: u16) -> Vec<u16> {
    let mut ports = vec![port];
    for other in [DEFAULT_DATAHUB_PORT, DEFAULT_BACKTEST_PORT] {
        if !ports.contains(&other) {
            ports.push(other);
        }
    }
    ports
}

/// Whether something takes a connection on this box's `port`. Once ssh has bound it that is ssh's
/// listener, so this says the forward is up and nothing of the daemon behind it.
pub fn port_answers(port: u16) -> bool {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok()
}

/// No console window of its own for a child, on a caller that has none to share (the desktop has
/// none, and starts every child through here).
#[cfg(windows)]
pub fn hide_window(command: &mut Command) {
    use std::os::windows::process::CommandExt as _;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

/// Unreached: only Windows opens a console for a console child.
#[cfg(not(windows))]
pub fn hide_window(_command: &mut Command) {}

/// One look at a raising tunnel.
#[derive(Debug, PartialEq, Eq)]
enum TunnelLook {
    Up,
    Wait,
    Refused(String),
}

/// What one look says. PURE. An exit outranks everything: ssh gone means the ports that answer are not
/// its own.
fn tunnel_look(exit: Option<i32>, all_answer: bool, out_of_time: bool) -> TunnelLook {
    match exit {
        Some(TUNNEL_GAVE_UP) => TunnelLook::Refused(format!(
            "its forwards did not answer within {}s, so it was stopped",
            TUNNEL_WAIT.as_secs()
        )),
        // Its stderr went nowhere: nothing it inherits may hold the caller's pipe.
        Some(code) => {
            TunnelLook::Refused(format!("exit {code}; `ssh -N <host>` by hand shows its own words"))
        }
        None if all_answer => TunnelLook::Up,
        None if out_of_time => TunnelLook::Refused(
            "its launcher did not answer in time, and an ssh may be left running: `Get-Process \
             ssh` shows it"
                .to_string(),
        ),
        None => TunnelLook::Wait,
    }
}

/// Wait for every one of `ports` to answer while `child` lives, for `wait` at most, each asked until it
/// has answered once. On a refusal the child is stopped and gone; on an answer it is left running.
///
/// # Errors
/// Why the forwards did not come up.
pub fn wait_for_forwards(
    child: &mut Child,
    mut ports: Vec<u16>,
    answers: impl Fn(u16) -> bool,
    wait: Duration,
) -> Result<(), String> {
    let started = Instant::now();
    loop {
        // An exit with no code (a Unix signal, never on Windows) or that cannot be read is an exit.
        let exit = match child.try_wait() {
            Ok(status) => status.map(|s| s.code().unwrap_or(-1)),
            Err(_) => Some(-1),
        };
        ports.retain(|port| !answers(*port));
        match tunnel_look(exit, ports.is_empty(), started.elapsed() >= wait) {
            TunnelLook::Up => return Ok(()),
            TunnelLook::Wait => std::thread::sleep(Duration::from_millis(200)),
            TunnelLook::Refused(why) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(why);
            }
        }
    }
}

/// `<state_dir>/node/tunnel.json`: one file, overwritten by each `connect`, removed by `vike-cli
/// backend disconnect`, and read by the desktop, which raises the forward again at its start. ⚠ It
/// records the FORWARD, not a pid, and that is forced by `-f`: ssh forks and the process this module
/// spawned exits immediately, so there is no child handle to keep and no pid to store that would
/// still be the tunnel's. What CAN be recorded is the thing that identifies it on any box — the
/// forward spec and the host. On Windows ssh is no child of this process either
/// ([`raise_unforked_tunnel`]), and the record says the same true thing.
pub fn tunnel_record_path(state_dir: Option<&Path>) -> Option<PathBuf> {
    state_dir.map(|d| d.join(TUNNEL_RECORD))
}

/// Record the forward just raised. Best-effort by design: a tunnel that is UP with no record is a
/// working client that `disconnect` cannot close automatically, while failing `connect` over an
/// unwritable state directory would refuse a chain that otherwise works.
pub fn record_tunnel(
    state_dir: Option<&Path>,
    host: &str,
    port: u16,
    now_ms: i64,
    words: &Words<'_>,
) {
    let Some(path) = tunnel_record_path(state_dir) else { return };
    let body = serde_json::json!({ "host": host, "port": port, "raised_ms": now_ms });
    let wrote = path.parent().map(std::fs::create_dir_all).transpose().and_then(|_| {
        vike_utility::atomic_file::write_atomic(&path, format!("{body}\n").as_bytes())
    });
    if let Err(e) = wrote {
        (words.warn)(&format!(
            "vike-cli backend: ⚠ the tunnel is up, but it could not be recorded in {} ({e}) — \
             `backend disconnect` will not find it; close it by hand.",
            path.display()
        ));
    }
}

/// The `(host, port)` a previous `connect` recorded, or `None` when there is no readable record.
///
/// A malformed record is `None` rather than an error: it is a convenience file this module wrote,
/// and the honest answer to an unreadable one is "I have no record", not a refusal to proceed.
/// ⚠ `Some` is what was raised, not what is up.
pub fn read_tunnel(path: &Path) -> Option<(String, u16)> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let host = v.get("host")?.as_str()?.to_string();
    let port = u16::try_from(v.get("port")?.as_u64()?).ok()?;
    Some((host, port))
}

#[cfg(test)]
#[path = "attach_tests.rs"]
mod attach_tests;
