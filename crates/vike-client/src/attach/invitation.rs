//! This box's own device key and a guest's ONE paste (decision
//! `docs/decisions/0288-every-device-signs-with-its-own-key-one-access-list-decides-who-and-a-guest-joins-by-one-invitation.md`).
//!
//! **The device key** ([`this_device`]): an Ed25519 pair born HERE, once, kept as its PKCS#8 document in this box's
//! `device_key` table (`vike_secrets::access`), and never printed, logged or sent. Only its PUBLIC half leaves: piped
//! to the server's `vike-cli access add` (`connect --server-project`, `--local-server`), or inside an invitation's
//! registration. The pair is made under the person `owner` (this device's owner, from its own side) and this
//! computer's host name; the server's list names the person it grants.
//!
//! **`connect --invitation`** ([`join`]): the invitation line `vike-invite-1 <user@host:/project> <id> <secret>
//! <host key> <one-time ssh key>` arrives on `vike-cli`'s STDIN or in the desktop's paste field (it carries a secret
//! and a private key, so never argv), and then:
//!
//! 1. the server's ed25519 host key, which the owner's vikectl wrote into the line, is pinned in `~/.ssh/known_hosts`
//!    ([`pin_host_key`]): the address must be a plain `user@host` (no ssh pattern), a line there naming ANOTHER key
//!    for that host (hashed and pattern lines included) is refused, and every ssh step runs with
//!    `StrictHostKeyChecking=yes`, so a server that shows a different key is refused before anything is sent;
//! 2. this box's device key and its own ssh key (`~/.ssh/id_ed25519`, made without a passphrase when absent: the
//!    desktop has no terminal to type one into);
//! 3. the one-time key in a private file beside it, deleted however the run ends ([`OneTimeKey`]);
//! 4. ssh with the one-time key forwards ONE free local port to the tradehub, and [`register_at`] sends `Register`
//!    there (the proof binds the secret to the connection's nonce, so the secret never crosses the wire);
//! 5. that ssh is stopped, and the one-time key asks the server to `swap` its line for the guest's own;
//! 6. `connect` then raises the ordinary tunnel with the guest's own key and verifies with the device key.

use std::fmt;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use base64::Engine as _;
use vike_config::config::DEFAULT_TRADEHUB_PORT;
use vike_hub_protocol::access::{DeviceKey, valid_name, valid_ssh_key};
use vike_hub_protocol::call::HubCallError;
use vike_hub_protocol::handshake::Registered;
use vike_secrets::access::{DeviceAdded, device_key_insert, device_key_load};
use vike_utility::errors::FailureOutcome;

use super::{
    AttachError, AttachResult, TUNNEL_WAIT, Words, hide_window, port_answers, wait_for_forwards,
};

/// An invitation's first word (decision 0288 item 7.2).
const INVITATION_TAG: &str = "vike-invite-1";

/// The person a device's own pair is made under: its owner, from its own side.
pub const DEVICE_OWNER: &str = "owner";

/// One invitation, as `vikectl access invite` prints it. `Debug` shows the id and the address, never the secret
/// or the key.
pub struct Invitation {
    /// The ssh destination, `user@host`.
    pub host: String,
    /// The server's project folder.
    pub project: String,
    /// The invitation's public id.
    pub id: String,
    /// The one-time registration secret (64 lower-case hex).
    secret: String,
    /// The server's ed25519 host key, its base64 blob (public: what `known_hosts` pins).
    host_key: String,
    /// The one-time OpenSSH private key file's bytes.
    one_time_key: Vec<u8>,
}

impl fmt::Debug for Invitation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Invitation({} for {}:{})", self.id, self.host, self.project)
    }
}

/// Read an invitation line. PURE. Every refusal names a FIELD, never what it held.
pub fn parse_invitation(line: &str) -> Result<Invitation, String> {
    let shape = format!(
        "an invitation is ONE line of six fields, as `vikectl access invite` prints it: \
         `{INVITATION_TAG} user@host:/folder ID SECRET HOSTKEY KEY`"
    );
    let fields: Vec<&str> = line.split_whitespace().collect();
    let [tag, address, id, secret, host_key, key] = fields[..] else { return Err(shape) };
    if tag != INVITATION_TAG {
        return Err(format!("{shape}; this one does not start with `{INVITATION_TAG}`"));
    }
    let plain = |c: char| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-');
    let (host, project) = address.rsplit_once(':').unwrap_or_default();
    if !plain_destination(host) || !project.starts_with('/') || !project.chars().all(plain) {
        return Err(format!(
            "{shape}; its address is not user@host:/folder (a plain user name, and a plain host name or IP address)"
        ));
    }
    let lower_hex = |s: &str, len: usize| {
        s.len() == len && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    if !lower_hex(id, 8) {
        return Err(format!("{shape}; its id is not 8 lower-case hex characters"));
    }
    if !lower_hex(secret, 64) {
        return Err(format!("{shape}; its secret is not 64 lower-case hex characters"));
    }
    // The blob of an ed25519 key, the exact shape an ssh line's key has.
    if !valid_ssh_key(&format!("ssh-ed25519 {host_key}")) {
        return Err(format!("{shape}; its host key is not an ed25519 key's base64 blob"));
    }
    let one_time_key = base64::engine::general_purpose::STANDARD
        .decode(key)
        .ok()
        .filter(|k| k.starts_with(b"-----BEGIN OPENSSH PRIVATE KEY-----"))
        .ok_or_else(|| format!("{shape}; its key is not a base64 OpenSSH private key"))?;
    Ok(Invitation {
        host: host.to_string(),
        project: project.trim_end_matches('/').to_string(),
        id: id.to_string(),
        secret: secret.to_string(),
        host_key: host_key.to_string(),
        one_time_key,
    })
}

/// A plain ssh destination `user@host`: a user name, and a host name or an IP address, with nothing
/// `known_hosts` or ssh reads as a pattern (`*`, `?`, `!`, `,`, `[`, `|`, whitespace, …), so the
/// host key pinned for it names that one host and no other. PURE.
fn plain_destination(destination: &str) -> bool {
    let Some((user, host)) = destination.split_once('@') else { return false };
    let user_plain = !user.is_empty()
        && !user.starts_with('-')
        && user.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    let label = |l: &str| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
    };
    let host_plain = host.parse::<std::net::IpAddr>().is_ok()
        || (host.len() <= 253 && host.split('.').all(label));
    user_plain && host_plain
}

/// The `known_hosts` lines at `path` that ssh would use for `name`, as `ssh-keygen -F` finds them:
/// hashed lines (`HashKnownHosts yes`) and patterns (`*`) included. None when there is no file.
fn known_host_lines(path: &Path, name: &str) -> AttachResult<Vec<String>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let mut find = Command::new("ssh-keygen");
    find.arg("-F").arg(name).arg("-f").arg(path).stdin(Stdio::null());
    hide_window(&mut find);
    let out = find.output().map_err(|e| {
        AttachError::failed(format!("could not run ssh-keygen to read {}: {e}", path.display()))
    })?;
    match out.status.code() {
        Some(0) => Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.starts_with('#'))
            .map(str::to_string)
            .collect()),
        // `-F` found no line for the host.
        Some(1) => Ok(Vec::new()),
        _ => Err(AttachError::failed(format!(
            "ssh-keygen could not read {}: {}. Nothing was sent.",
            path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ))),
    }
}

/// Pin the invitation's host key for its host in `known_hosts` (in `ssh_dir`): appended when no line
/// there holds it for the host, and REFUSED when any line ssh would use for the host (hashed and
/// pattern lines included, [`known_host_lines`]) names another ed25519 key. Nothing is written on a
/// refusal.
fn pin_host_key(ssh_dir: &Path, invitation: &Invitation, words: &Words<'_>) -> AttachResult<()> {
    let name = invitation.host.rsplit_once('@').map_or(invitation.host.as_str(), |(_, h)| h);
    let path = ssh_dir.join("known_hosts");
    let held = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(AttachError::failed(format!("could not read {}: {e}", path.display())));
        }
    };
    let mut pinned = false;
    // A marker line (`@cert-authority`, `@revoked`) pins no host key.
    for line in known_host_lines(&path, name)?.iter().filter(|l| !l.starts_with('@')) {
        let mut words = line.split_whitespace();
        let (Some(_), Some(kind), Some(key)) = (words.next(), words.next(), words.next()) else {
            continue;
        };
        if kind != "ssh-ed25519" {
            continue;
        }
        if key != invitation.host_key {
            return Err(AttachError::failed(format!(
                "{} already holds ANOTHER ed25519 key for {name} than the invitation names, so this \
                 may not be the owner's server. Nothing was sent; ask the owner which key is right \
                 (`ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub` on the server).",
                path.display()
            )));
        }
        pinned = true;
    }
    if pinned {
        return Ok(());
    }
    std::fs::create_dir_all(ssh_dir)
        .map_err(|e| AttachError::failed(format!("could not create {}: {e}", ssh_dir.display())))?;
    let gap = if held.is_empty() || held.ends_with('\n') { "" } else { "\n" };
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| writeln!(f, "{gap}{name} ssh-ed25519 {}", invitation.host_key))
        .map_err(|e| AttachError::failed(format!("could not write {}: {e}", path.display())))?;
    (words.say)(&format!("pinned {name}'s host key from the invitation in {}", path.display()));
    Ok(())
}

/// This box's device key and its device name, when it holds one.
pub fn held_device(dir: &Path) -> AttachResult<Option<(DeviceKey, String)>> {
    let row = device_key_load(dir).map_err(|e| {
        AttachError::failed(format!(
            "could not read this box's device key in {}: {e}",
            dir.display()
        ))
    })?;
    row.map(|row| {
        DeviceKey::from_pkcs8(&row.private_key).map(|key| (key, row.device)).map_err(|e| {
            AttachError::failed(format!("this box's device key {} cannot be used: {e}", row.key_id))
        })
    })
    .transpose()
}

/// This box's device key and its device name, made and stored ONCE when it holds none. Only the key id is
/// printed.
pub fn this_device(dir: &Path, words: &Words<'_>) -> AttachResult<(DeviceKey, String)> {
    if let Some(held) = held_device(dir)? {
        return Ok(held);
    }
    let device = name_of_host(&host_name());
    let (key, pkcs8) = DeviceKey::generate()
        .map_err(|e| AttachError::failed(format!("could not make this box's device key: {e}")))?;
    let added = device_key_insert(dir, &key.key_id(), DEVICE_OWNER, &device, &pkcs8)
        .map_err(|e| AttachError::failed(format!("could not store this box's device key: {e}")))?;
    if added != DeviceAdded::Added {
        // Another run stored one first: that one is this box's key.
        return held_device(dir)?
            .ok_or_else(|| AttachError::failed("this box's device key went missing".to_string()));
    }
    (words.say)(&format!(
        "made this box's device key {} ({DEVICE_OWNER}/{device}); its private half stays in {}",
        key.key_id(),
        dir.display()
    ));
    Ok((key, device))
}

/// This computer's name, or empty.
fn host_name() -> String {
    let mut command = Command::new("hostname");
    command.stdin(Stdio::null()).stderr(Stdio::null());
    hide_window(&mut command);
    command
        .output()
        .ok()
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .unwrap_or_default()
}

/// A device name from a host name: its first label, lower-cased, every other character a `-`, else `pc`. PURE.
fn name_of_host(raw: &str) -> String {
    let label = raw.trim().split('.').next().unwrap_or_default();
    let mapped: String = label
        .chars()
        .map(|c| c.to_ascii_lowercase())
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '-' })
        .collect();
    let name: String =
        mapped.trim_start_matches(|c: char| !c.is_ascii_alphanumeric()).chars().take(32).collect();
    if valid_name(&name) { name } else { "pc".to_string() }
}

/// `~/.ssh`, where ssh looks for its keys.
fn ssh_dir() -> AttachResult<PathBuf> {
    std::env::home_dir().map(|home| home.join(".ssh")).ok_or_else(|| {
        AttachError::failed("this computer names no home folder, so there is no ~/.ssh".to_string())
    })
}

/// This computer's own ssh PUBLIC key, `ssh-ed25519 <base64>` (no comment): `~/.ssh/id_ed25519.pub`, made by
/// `ssh-keygen` WITHOUT a passphrase when neither half is there.
fn own_ssh_key(dir: &Path, words: &Words<'_>) -> AttachResult<String> {
    let private = dir.join("id_ed25519");
    let public = dir.join("id_ed25519.pub");
    if !private.exists() {
        std::fs::create_dir_all(dir)
            .map_err(|e| AttachError::failed(format!("could not create {}: {e}", dir.display())))?;
        let mut keygen = Command::new("ssh-keygen");
        keygen
            .args(["-q", "-t", "ed25519", "-N", "", "-C", "vike", "-f"])
            .arg(&private)
            .stdin(Stdio::null());
        hide_window(&mut keygen);
        let out = keygen
            .output()
            .map_err(|e| AttachError::failed(format!("could not run ssh-keygen: {e}")))?;
        if !out.status.success() {
            return Err(AttachError::failed(format!(
                "ssh-keygen could not make {}: {}",
                private.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        (words.say)(&format!("made this computer's ssh key {}", public.display()));
    }
    let text = std::fs::read_to_string(&public).map_err(|e| {
        AttachError::failed(format!(
            "could not read {} ({e}); its private half is there",
            public.display()
        ))
    })?;
    let line = text.split_whitespace().take(2).collect::<Vec<_>>().join(" ");
    if valid_ssh_key(&line) {
        Ok(line)
    } else {
        Err(AttachError::failed(format!("{} is not an ssh-ed25519 public key", public.display())))
    }
}

/// The invitation's one-time private key, in a file only this user may read, removed when dropped.
struct OneTimeKey(PathBuf);

impl OneTimeKey {
    fn write(dir: &Path, invitation: &Invitation) -> AttachResult<OneTimeKey> {
        let path = dir.join(format!("vike-invite-{}", invitation.id));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let file = OneTimeKey(path);
        options.open(&file.0).and_then(|mut f| f.write_all(&invitation.one_time_key)).map_err(
            |e| {
                AttachError::failed(format!(
                    "could not write the one-time key {}: {e}",
                    file.0.display()
                ))
            },
        )?;
        Ok(file)
    }
}

impl Drop for OneTimeKey {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// The ssh options every one-time step shares: no prompt, only this key, and the server's host key
/// checked strictly against the one [`pin_host_key`] pinned from the invitation (a server showing
/// another key is refused, never trusted on first sight). PURE.
fn one_time_options(key: &Path) -> Vec<String> {
    let mut args: Vec<String> =
        ["BatchMode=yes", "IdentitiesOnly=yes", "StrictHostKeyChecking=yes"]
            .into_iter()
            .flat_map(|option| ["-o".to_string(), option.to_string()])
            .collect();
    args.extend(["-i".to_string(), key.display().to_string()]);
    args
}

/// The one-time forward: local `port` to the server's tradehub, nothing else. PURE.
fn registration_tunnel_args(key: &Path, port: u16, host: &str) -> Vec<String> {
    let mut args = vec!["-N".to_string(), "-o".to_string(), "ExitOnForwardFailure=yes".to_string()];
    args.extend(one_time_options(key));
    args.extend([
        "-L".to_string(),
        format!("{port}:localhost:{DEFAULT_TRADEHUB_PORT}"),
        host.to_string(),
    ]);
    args
}

/// The one-time key's last use: the server's forced command swaps its line for the guest's own. PURE.
fn swap_args(key: &Path, host: &str) -> Vec<String> {
    let mut args = vec!["-T".to_string()];
    args.extend(one_time_options(key));
    args.extend([host.to_string(), "swap".to_string()]);
    args
}

/// Register `device` and `ssh_key` with `invitation` at the tradehub on `dial`: the client's one registration
/// verb, `vike_tradehub_client::register_device`, fed from the pasted line.
fn register_at(
    dial: &str,
    invitation: &Invitation,
    device: &DeviceKey,
    device_name: &str,
    ssh_key: &str,
) -> Result<Registered, HubCallError> {
    vike_tradehub_client::register_device(
        dial,
        &invitation.id,
        &invitation.secret,
        device_name,
        device,
        ssh_key,
    )
}

/// [`join`]'s step 3: the one-time forward up, the registration, the forward down.
fn register_through_tunnel(
    key: &Path,
    invitation: &Invitation,
    device: &DeviceKey,
    device_name: &str,
    ssh_key: &str,
) -> AttachResult<Registered> {
    let host = invitation.host.as_str();
    let port = std::net::TcpListener::bind(("127.0.0.1", 0))
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .map_err(|e| {
            AttachError::failed(format!("no free local port for the registration: {e}"))
        })?;
    let mut ssh = Command::new("ssh");
    ssh.args(registration_tunnel_args(key, port, host))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    hide_window(&mut ssh);
    let mut child = ssh
        .spawn()
        .map_err(|e| AttachError::connect(format!("could not run `ssh` to reach {host}: {e}")))?;
    wait_for_forwards(&mut child, vec![port], port_answers, TUNNEL_WAIT).map_err(|why| {
        AttachError::connect(format!(
            "the invitation's one-time key did not open a forward to {host} ({why}). It may be past \
             its 15 minutes or used already: ask for a new invitation. Nothing was registered."
        ))
    })?;
    let registered =
        register_at(&format!("127.0.0.1:{port}"), invitation, device, device_name, ssh_key);
    let _ = child.kill();
    let _ = child.wait();
    registered.map_err(|e| {
        let why = format!(
            "the server did not take invitation {}: {e}. Ask for a new invitation (`vikectl access \
             invite`).",
            invitation.id
        );
        // The server ANSWERED (a refused invitation, a server too old for it, a reply that is not a
        // registration's) is final; only a link that carried no answer is worth another try.
        match e.failure_outcome() {
            FailureOutcome::Refused => AttachError::failed(why),
            _ => AttachError::connect(why),
        }
    })
}

/// A guest's ONE paste, up to the server's swap (the module doc's steps 1 to 5); `connect` raises the ordinary
/// tunnel and verifies after it.
///
/// # Errors
/// The step that refused, on its rung.
pub fn join(
    invitation: &Invitation,
    settings_dir: &Path,
    words: &Words<'_>,
) -> AttachResult<(DeviceKey, String)> {
    let (device, device_name) = this_device(settings_dir, words)?;
    let ssh_dir = ssh_dir()?;
    pin_host_key(&ssh_dir, invitation, words)?;
    let ssh_key = own_ssh_key(&ssh_dir, words)?;
    let one_time = OneTimeKey::write(&ssh_dir, invitation)?;
    let registered =
        register_through_tunnel(&one_time.0, invitation, &device, &device_name, &ssh_key)?;
    (words.say)(&format!(
        "registered at {}: {}/{} with the role {} (device key {})",
        invitation.host, registered.person, registered.device, registered.role, registered.key_id
    ));
    let mut swap = Command::new("ssh");
    swap.args(swap_args(&one_time.0, &invitation.host));
    let swapped = words.run(swap, None).map_err(|e| {
        AttachError::connect(format!(
            "could not run `ssh` for the swap at {}: {e}",
            invitation.host
        ))
    })?;
    if !swapped.success() {
        return Err(AttachError::connect(format!(
            "{} registered this device but did not swap the one-time ssh line for this computer's \
             own key (ssh exit {}). Ask the server's owner to run `vikectl access swap {}`.",
            invitation.host,
            swapped.code().map_or_else(|| "signal".to_string(), |c| c.to_string()),
            invitation.id
        )));
    }
    (words.say)(&format!(
        "{} now admits this computer's own ssh key; the one-time key is gone",
        invitation.host
    ));
    drop(one_time);
    Ok((device, device_name))
}

#[cfg(test)]
#[path = "invitation_tests.rs"]
mod invitation_tests;
