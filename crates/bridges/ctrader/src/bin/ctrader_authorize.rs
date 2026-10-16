//! Local OAuth catcher for cTrader Open API — removes the human copy/paste round-trip. Starts a
//! plain HTTP server on `http://localhost:5033/` (the default redirect URI; `--redirect-uri`
//! overrides it), prints the authorize URL, blocks for the browser's `?code=...` redirect,
//! exchanges the code for a [`vike_ctrader::oauth::Token`] IN THE SAME PROCESS, prints the redacted
//! token, writes it to `<project>/settings/state/ctrader_token.json` (camelCase, matching the
//! venue's own wire shape, so it round-trips through the same JSON a future loader would parse),
//! and writes the grant into the credential store as the named ACCOUNT's `ACCESS_TOKEN` /
//! `REFRESH_TOKEN` fields.
//!
//! # The grant belongs to an EXISTING account (decision 0156 item 6)
//!
//! A credential belongs to its account, and the account — with its tier — is the operator's to
//! create; this tool never creates one. So it is told which account the grant is for:
//! `--account-id ID` (the row's id, as `vike-cli secrets accounts` prints it), or
//! `--tier TIER [--label LABEL]` (the ONE active ctrader row at that tier with that label; none is
//! the unlabelled account). No such row is REFUSED before the browser flow starts, naming
//! `vike-cli secrets account add --venue ctrader --tier …` as the way in ([`resolve_target`]). The
//! grant is written through `vike_secrets::save_owned_credentials` — one transaction for the pair,
//! one `credential_write` journal record under `Actor::cli` — the same writer the daemon's
//! rotation (`vike_ctrader::token_store`'s `persist`) uses for the same two fields.
//!
//! # ⚠ The token FILE holds a LIVE access + refresh token
//!
//! It used to be written as `token.json` **in the process working directory** — with no resolver at
//! all, so a live credential landed in whatever directory the binary happened to be started from,
//! which on a server is frequently world-readable. Two things changed:
//!
//! 1. **It is resolved, like every other credential-adjacent file** (settings-unification #1084):
//!    `--token-file` → `<project>/settings/state/ctrader_token.json` → `./token.json`, the project
//!    being the one `VIKE_SETTINGS_DIR` names when it is set (the same answer the credential read
//!    gets) and the working directory's otherwise. The last rung is unchanged pre-#1084 behaviour
//!    and is reached only by a run with no override and no project above its working directory;
//!    `settings/` is gitignored (#1085), so the default path cannot be committed by accident.
//! 2. **It is written owner-only on unix (0600)** — the posture
//!    `crates/vike-secrets/src/store/warnings.rs`'s `PermissionWarning` demands of the credential store, and
//!    it applies on EVERY rung including the CWD fallback, so even the unresolved case is no longer
//!    world-readable.
//!
//! The token's VALUE is never printed or logged (only [`redact`]ed head/tail), and this tool never
//! reads, moves or deletes a pre-existing `token.json` — a box that has one keeps it, untouched and
//! unread, and simply gets the next token at the resolved path.
//!
//! Direct Rust port of the proven-live `scratchpad/ctrader_catcher.py` catcher (see
//! `.superpowers/sdd/task-2-brief.md`, Step 5) — this bin only covers the catch+exchange+write
//! half; the "prove the connection" half (ApplicationAuth/AccountAuth/live quote) is later tasks'
//! `conn`/`exec` work, not this bridge crate's OAuth layer.
//!
//! Usage: `ctrader_authorize (--account-id ID | --tier TIER [--label LABEL]) [--redirect-uri URI]
//! [--scope SCOPE] [--token-file PATH]` — the defaults are `http://localhost:5033/`, `trading` and
//! the resolved path above. [`AuthorizeCli`] is the whole command line, read by `clap` through
//! `vike_util::cli::Program` with `hide_values` (decision 0155): a usage error names a flag and
//! never echoes a value, and a stray positional — what a pasted secret looks like — is printed as
//! `<hidden>`. The app pair is read from the CREDENTIAL STORE as the VENUE's application fields
//! `CLIENT_ID` / `CLIENT_SECRET` — the fields every cTrader start reads, held once for every
//! account — never argv or the environment (decision 0095). The five process variables this tool
//! used to read (`CTRADER_REDIRECT_URI`, `CTRADER_SCOPE`, `CTRADER_TOKEN_FILE` and the app pair)
//! are retired: this tool REFUSES to run while one is set ([`refuse_retired_variables`]), as every
//! booting root refuses to start. The settings it takes out of its process-environment sweep are
//! `VIKE_SETTINGS_DIR`, for the credential store and for the token path alike, so the two answer
//! "which project" the same way, and `VIKE_NO_LIVE_KEYS`, the credential read's scope (a value
//! other than `true` or `false` refuses the run); the only other names it looks up in the sweep are
//! those five, and only to refuse them.
//!
//! This is a developer tool, not a library entry point: all output is raw `println!`/`eprintln!`,
//! not `tracing` (per CLAUDE.md — protocol/result stdout stays raw).

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};

use vike_ctrader::config::{ACCESS_TOKEN, CLIENT_ID, CLIENT_SECRET, REFRESH_TOKEN};
use vike_ctrader::oauth::{self, Token};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::account_credentials::CredentialBook;
use vike_util::cli::{CliExit, Program};
/// The venue whose application pair this tool reads and whose account it writes the grant to.
const VENUE: &str = "ctrader";

const LISTEN_ADDR: &str = "127.0.0.1:5033";
const DEFAULT_REDIRECT_URI: &str = "http://localhost:5033/";
const DEFAULT_SCOPE: &str = "trading";

/// The token file's basename inside the project state directory.
///
/// Venue-qualified rather than the bare `token.json` it used to be: `settings/state/` is a SHARED
/// directory (`workspace.json`, `studio_workspace.json`, `pace.json`, `layouts/`), and a file called
/// `token.json` in it says nothing about whose token it is — the next venue to want one would
/// collide silently.
const TOKEN_FILE: &str = "ctrader_token.json";

/// The pre-#1084 location: `token.json`, relative to the process working directory.
const LEGACY_TOKEN_FILE: &str = "token.json";

/// Resolve where the token is written: an explicit `--token-file` → the project's
/// `settings/state/ctrader_token.json` → the legacy CWD-relative `token.json`.
///
/// PURE — the flag and the working directory's project arrive as parameters. A blank flag never
/// gets here: [`non_empty_path`] refuses it by name, so the CWD is reached only through the last rung.
fn resolve_token_path(explicit: Option<PathBuf>, project_state_dir: Option<PathBuf>) -> PathBuf {
    explicit
        .or_else(|| project_state_dir.map(|d| d.join(TOKEN_FILE)))
        .unwrap_or_else(|| PathBuf::from(LEGACY_TOKEN_FILE))
}

/// **Which account the grant is written to** — an EXISTING ctrader `account` row, named one of two
/// ways. A credential belongs to its account (decision 0156 item 6) and this tool never creates
/// one, so there is no default: an unnamed account is refused by [`parse_args_from`].
#[derive(Debug, Clone, PartialEq, Eq)]
enum AccountTarget {
    /// `--account-id ID` — the row's id, as `vike-cli secrets accounts` prints it.
    Id(i64),
    /// `--tier TIER [--label LABEL]` — the ONE active ctrader row at that tier with that label
    /// ([`AccountLabel::Default`], the unlabelled account, when no label is given).
    Address { tier: AccountTier, label: AccountLabel },
}

/// The command line as `clap` reads it (decision 0155: through `vike_util::cli::Program`, built
/// with `hide_values` — this tool takes a credential-adjacent command line, so a usage error names
/// a flag but never echoes a value, and a stray positional, the shape a pasted secret takes, is
/// printed as `<hidden>`). The account is three RAW options here and becomes an [`AccountTarget`]
/// in [`AuthorizeCli::into_args`], because "exactly one of two ways" is a rule over all three.
#[derive(clap::Parser, Debug, Clone, PartialEq, Eq)]
#[command(
    name = "ctrader_authorize",
    about = "Local OAuth catcher for the cTrader Open API: prints the authorize URL, catches the \
             redirect, exchanges the code, writes the token owner-only and files the grant \
             against an EXISTING ctrader account"
)]
struct AuthorizeCli {
    /// The redirect URI registered with the application (the catcher listens on its port).
    #[arg(
        long,
        value_name = "URI",
        value_parser = vike_util::cli::non_empty,
        default_value = DEFAULT_REDIRECT_URI,
    )]
    redirect_uri: String,
    /// The OAuth scope to request.
    #[arg(
        long,
        value_name = "SCOPE",
        value_parser = vike_util::cli::non_empty,
        default_value = DEFAULT_SCOPE,
    )]
    scope: String,
    /// Where the token is written (default: <project>/settings/state/ctrader_token.json).
    #[arg(long, value_name = "PATH", value_parser = non_empty_path)]
    token_file: Option<PathBuf>,
    /// The ctrader account the grant belongs to, by its row number (`vike-cli secrets accounts`
    /// prints it). Exclusive with --tier/--label.
    #[arg(long, value_name = "ID", value_parser = vike_util::cli::non_empty)]
    account_id: Option<String>,
    /// The tier of the ONE active ctrader account the grant belongs to (paper | demo | live).
    #[arg(long, value_name = "TIER", value_parser = vike_util::cli::non_empty)]
    tier: Option<String>,
    /// With --tier: the account's label (none = the unlabelled account).
    #[arg(long, value_name = "LABEL", value_parser = vike_util::cli::non_empty)]
    label: Option<String>,
}

/// The flow's parameters, from the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AuthorizeArgs {
    redirect_uri: String,
    scope: String,
    token_file: Option<PathBuf>,
    account: AccountTarget,
}

/// `--token-file`'s value parser: trimmed, blank refused. ⚠ The error text never carries the value.
fn non_empty_path(raw: &str) -> Result<PathBuf, String> {
    vike_util::cli::non_empty(raw).map(PathBuf::from)
}

const USAGE: &str = "usage: ctrader_authorize (--account-id ID | --tier TIER [--label LABEL]) \
                     [--redirect-uri URI] [--scope SCOPE] [--token-file PATH]";

impl AuthorizeCli {
    /// The rule `clap` cannot say as one line: the account is named exactly one way, and an id, a
    /// tier or a label is a legal one. The refusals are decision 0156 item 6's own.
    fn into_args(self) -> Result<AuthorizeArgs, String> {
        let Self { redirect_uri, scope, token_file, account_id, tier, label } = self;
        let account = match (account_id, tier, label) {
            (Some(id), None, None) => AccountTarget::Id(id.parse::<i64>().map_err(|_| {
                "--account-id takes the account row's number (`vike-cli secrets accounts` prints \
                 it)"
                .to_string()
            })?),
            (None, Some(tier), label) => AccountTarget::Address {
                tier: tier.parse::<AccountTier>().map_err(|e| format!("--tier: {e}"))?,
                label: match label {
                    Some(l) => AccountLabel::parse(&l).map_err(|e| format!("--label: {e}"))?,
                    None => AccountLabel::Default,
                },
            },
            (Some(_), _, _) => {
                return Err("--account-id and --tier/--label name the account two ways — pass one"
                    .to_string());
            }
            (None, None, Some(_)) => {
                return Err("--label names an account only beside --tier".to_string());
            }
            (None, None, None) => {
                return Err(
                    "name the account the grant belongs to — the grant is a credential of \
                            ONE account, and this tool never creates one"
                        .to_string(),
                );
            }
        };
        Ok(AuthorizeArgs { redirect_uri, scope, token_file, account })
    }
}

/// The program's identity for the parse. `hide_values`: no value of this command line ever appears
/// in a usage error.
fn program() -> Program<'static> {
    Program::new("ctrader_authorize", concat!("ctrader_authorize ", env!("CARGO_PKG_VERSION")))
        .hide_values()
}

/// Read `argv` (WITHOUT the program name) as the flow's parameters; nothing is printed and no
/// process exits — the outcome of `--help`, `--version` and every refusal is the [`CliExit`].
///
/// # Errors
/// The [`CliExit`] a process would have printed.
fn parse_args_from(argv: &[String]) -> Result<AuthorizeArgs, CliExit> {
    let cli: AuthorizeCli = program().try_parse_from(argv)?;
    // A refusal the rule made after the parse: stderr, the usage rung, the usage line beneath it —
    // and, like every refusal of this tool, no value of the command line is quoted back.
    cli.into_args().map_err(|msg| CliExit {
        code: vike_util::cli::USAGE_EXIT,
        to_stdout: false,
        text: format!("ctrader_authorize: {msg}\n{USAGE}\n"),
    })
}

/// The project state directory the token lands in: the project `VIKE_SETTINGS_DIR` names when the
/// sweep carries it — the SAME project the credential read resolves, so the grant lands beside the
/// store that supplied its app pair — else the working directory's.
fn token_state_dir(
    process_env: &std::collections::HashMap<String, String>,
    cwd: &Path,
) -> Option<PathBuf> {
    vike_paths::state_path::project_state_dir_from_env(process_env, cwd)
}

/// The process variables this tool read until decision 0095, each with what replaced it.
///
/// ⚠ Refused HERE, not merely ignored: this tool boots nothing, so `vike_config::REMOVED_ENV`'s
/// startup refusal never reaches it, and ignoring a leftover one would do the wrong thing in silence
/// — `CTRADER_SCOPE=accounts` minting a `trading` grant, `CTRADER_TOKEN_FILE` writing the token
/// somewhere else than the variable says. The `vike-config` table carries the same five for every
/// booting root.
const RETIRED_VARIABLES: [(&str, &str); 5] = [
    ("CTRADER_REDIRECT_URI", "pass --redirect-uri <URI> instead"),
    ("CTRADER_SCOPE", "pass --scope <SCOPE> instead"),
    ("CTRADER_TOKEN_FILE", "pass --token-file <PATH> instead"),
    (
        "CTRADER_CLIENT_ID",
        "the app pair is read from the credential store as cTrader's VENUE application field \
         `CLIENT_ID` — write it there (the value never on argv) once the variable is unset",
    ),
    (
        "CTRADER_CLIENT_SECRET",
        "the app pair is read from the credential store as cTrader's VENUE application field \
         `CLIENT_SECRET` — write it there (the value never on argv) once the variable is unset",
    ),
];

/// Refuse to run while a [`RETIRED_VARIABLES`] name is set — by NAME, never by value (one of them
/// is a secret). A blank value is unset, the rule every booting root applies to the same five.
fn refuse_retired_variables(
    process_env: &std::collections::HashMap<String, String>,
) -> Result<(), String> {
    let set: Vec<String> = RETIRED_VARIABLES
        .iter()
        .filter(|(var, _)| process_env.get(*var).is_some_and(|v| !v.trim().is_empty()))
        .map(|(var, instead)| {
            format!("  {var} is set, but this tool no longer reads it: {instead}")
        })
        .collect();
    if set.is_empty() {
        return Ok(());
    }
    Err(format!(
        "refusing to start — decision 0095 retired these process variables, and a set one would be \
         ignored in silence:\n{}\nUnset them and run again. Nothing was read and nothing was written.",
        set.join("\n")
    ))
}

/// The credential store, out of the ONE process-environment sweep: opened by
/// `vike_secrets::open_credential_store`, the door for a root that runs no boot —
/// `VIKE_SETTINGS_DIR` (else the workspace walk) and `VIKE_NO_LIVE_KEYS` parsed strictly. A store
/// that exists and would not open is refused at its first read (`main` names the reason).
///
/// # Errors
/// `vike_secrets::ScopeError`'s message, as is, when `VIKE_NO_LIVE_KEYS` is neither `true` nor
/// `false`: the tool refuses to start on it.
fn open_store(
    process_env: &std::collections::HashMap<String, String>,
) -> Result<vike_secrets::CredentialStore, String> {
    vike_secrets::open_credential_store(process_env).map_err(|e| e.to_string())
}

/// **The ONE ctrader `account` row the grant is written to**, or why there is none — asked BEFORE
/// the browser flow, so a refused run has spent no authorization code.
///
/// A credential belongs to its account and the account is the operator's to create (decision 0156
/// item 6), so every miss is a refusal naming the way in, never a create: no `account` table, no
/// such row, a row of another venue (a cTrader grant filed under a binance account would be read by
/// nothing and would sit beside that account's keys), or — for a `(tier, label)` address — more
/// than one active row answering it.
fn resolve_target(
    accounts: &vike_secrets::Accounts,
    target: &AccountTarget,
) -> Result<i64, String> {
    const CREATE: &str = "create it first with `vike-cli secrets account add --venue ctrader --tier \
                          <demo|live> [--label <LABEL>]`, then run this again (`vike-cli secrets \
                          accounts` prints every row and its id)";
    let Some(rows) = accounts.known() else {
        return Err(format!(
            "the credential store has no `account` table to write the grant to — a credential \
             belongs to its account and this tool never creates one: run `vike-cli secrets init`, \
             then {CREATE}"
        ));
    };
    match target {
        AccountTarget::Id(id) => match rows.iter().find(|r| r.id == *id) {
            Some(row) if row.venue == VENUE => Ok(row.id),
            Some(row) => Err(format!(
                "account row {id} is a `{}` account, not a ctrader one — refusing to write a \
                 cTrader grant to it",
                row.venue
            )),
            None => Err(format!(
                "no account row {id} in the credential store — this tool never creates one: \
                 {CREATE}"
            )),
        },
        AccountTarget::Address { tier, label } => {
            let matched: Vec<i64> = rows
                .iter()
                .filter(|r| {
                    r.active
                        && r.venue == VENUE
                        && r.tier == *tier
                        && r.label.as_deref() == label.text()
                })
                .map(|r| r.id)
                .collect();
            match matched.as_slice() {
                [id] => Ok(*id),
                [] => Err(format!(
                    "no active ctrader account at tier `{tier}` labelled `{label}` — this tool \
                     never creates one: {CREATE}"
                )),
                many => Err(format!(
                    "{} active ctrader accounts answer tier `{tier}` labelled `{label}` (ids {:?}) \
                     — name one with --account-id",
                    many.len(),
                    many
                )),
            }
        }
    }
}

/// The Spotware application pair: the VENUE's `CLIENT_ID` / `CLIENT_SECRET` fields, out of the
/// credential store's book — held once for every ctrader account, never an account's own field.
fn app_pair(book: &CredentialBook) -> Result<(String, String), String> {
    let get = |field: &str| {
        book.venue(VENUE).and_then(|v| v.get(field)).map(str::to_string).ok_or_else(|| {
            format!(
                "cTrader's application field `{field}` is not in the credential store — the OAuth \
                 client pair is the VENUE's, held once for every ctrader account; write it there \
                 (the value never on argv) and run this again"
            )
        })
    };
    Ok((get(CLIENT_ID)?, get(CLIENT_SECRET)?))
}

/// Write `bytes` to `path` with OWNER-ONLY permissions (0600) — the posture
/// `crates/vike-secrets/src/store/warnings.rs`'s `PermissionWarning` demands of the credential store, for the
/// same reason: this file carries a live access + refresh token.
///
/// `.mode()` applies only when the file is CREATED, so an existing file would keep whatever mode a
/// previous run left it; the explicit `set_permissions` narrows that case too, and it runs BEFORE
/// `write_all`, so no token byte is ever on disk under a wider mode. (The `truncate` that precedes
/// it can leave a momentarily-empty file at the old mode — empty, and never containing a secret.)
///
/// ⚠ **`O_NOFOLLOW`, and it is the important flag here.** Every step above follows a symlink: the
/// open TRUNCATES the link's target, `write_all` puts a live access + refresh token into it, and
/// `set_permissions` then chmods THAT file 0600. A link planted at the token path therefore turns
/// this binary into a primitive for destroying an arbitrary file and replacing its contents with a
/// credential. `O_NOFOLLOW` makes the open itself fail (`ELOOP`) instead, which is race-free in a
/// way no `stat`-then-open check can be — the check and the open are one syscall.
#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    f.write_all(bytes)?;
    f.sync_all()
}

/// Windows twin: no unix mode bits (the equivalent question is an ACL one, which the credential
/// store does not answer either), and no `O_NOFOLLOW` — the closest Win32 flag,
/// `FILE_FLAG_OPEN_REPARSE_POINT`, does the opposite of what is wanted here (it opens the reparse
/// point rather than refusing it).
///
/// So the symlink question is asked with an explicit `symlink_metadata` probe. That is a
/// TOCTOU-racy check where the unix arm has a race-free flag — a link planted between the probe and
/// the write still wins — but it refuses the realistic case (a link already sitting at the token
/// path), and the alternative is a Windows build that follows one silently. Windows symlinks also
/// need a privilege or Developer Mode to create in the first place, so the exposure is narrower to
/// begin with.
#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "{} is a symlink: refusing to write a live OAuth token through it",
                path.display()
            ),
        ));
    }
    std::fs::write(path, bytes)
}

/// Redact a secret for println!-safe display: first 6 + last 4 **chars** (not bytes), or
/// `<none>` if empty. Iterates `char_indices`/`chars().rev()` rather than byte-slicing, so a
/// secret containing multi-byte UTF-8 can never land a slice on a non-char-boundary and panic.
fn redact(s: &str) -> String {
    let char_count = s.chars().count();
    if s.is_empty() {
        "<none>".to_string()
    } else if char_count <= 10 {
        "<redacted>".to_string()
    } else {
        let head: String = s.chars().take(6).collect();
        let tail: String = s.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
        format!("{head}...{tail}")
    }
}

/// Minimal percent-decoder for a query-string value: `%XX` -> the byte `0xXX`, `+` -> space (the
/// `application/x-www-form-urlencoded` convention `parse_qs` also applies), everything else
/// passed through. No external URL crate: the one value this catcher reads (the OAuth `code`) is
/// from the same small ASCII-safe set the encoder side
/// (`vike_util::percent::percent_encode`) writes. Invalid/truncated `%` escapes are passed
/// through literally rather than erroring, since a malformed code is a webserver's problem, not
/// this catcher's.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    None => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Extract the `code` query parameter from an HTTP request line's path, e.g.
/// `GET /?code=abc123&scope=trading HTTP/1.1` -> `Some("abc123")`. Minimal parse: no external URL
/// crate, only what this one redirect shape needs. The returned value is percent-decoded (the
/// Python reference used `parse_qs`, which auto-decodes) so a code containing `%2F`/`%2B`/`%3D`
/// round-trips correctly instead of being passed to `exchange_code` still escaped.
fn extract_code_from_request_line(line: &str) -> Option<String> {
    let path = line.split_whitespace().nth(1)?; // "/?code=..."
    let query = path.split_once('?')?.1;
    for pair in query.split('&') {
        if let Some(value) = pair.strip_prefix("code=") {
            return Some(percent_decode(value));
        }
    }
    None
}

/// Handle one accepted connection: read the request line, extract `?code=`, write the response
/// body, and return the captured code (if any). Mirrors the Python `Handler.do_GET`: a bare hit
/// (no code) gets a "waiting" page; a code hit gets the "captured" page.
fn handle_connection(stream: &mut TcpStream) -> Option<String> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).ok()?;

    // Drain the rest of the request headers (up to the blank line) so the client doesn't hang.
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) if line == "\r\n" || line == "\n" => break,
            Ok(_) => continue,
        }
    }

    let code = extract_code_from_request_line(&request_line);
    let body: &[u8] = if code.is_some() {
        b"<h2>Code captured. You can close this tab and return to the terminal.</h2>"
    } else {
        b"<h2>cTrader catcher ready. Waiting for ?code=...</h2>"
    };
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
    code
}

/// Print a fatal error to stderr and exit(1). Returns `!` so it can be used in any expression
/// position (`unwrap_or_else` closures, match arms).
fn fatal_exit(msg: &str) -> ! {
    eprintln!("[FAIL] {msg}");
    std::process::exit(1);
}

fn main() {
    // The command line FIRST: `--help`, `--version` and every usage error answer before anything is
    // read from the environment or the credential store.
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = parse_args_from(&argv).unwrap_or_else(|exit| exit.emit());
    // The credential chain from the ONE process-environment sweep this root owns — the shape of
    // `hyperliquid_builder_fee_approve`: the settings-directory override comes out of the sweep and
    // nothing below reads the environment again.
    let process_env: std::collections::HashMap<String, String> = std::env::vars().collect();
    refuse_retired_variables(&process_env).unwrap_or_else(|e| fatal_exit(&e));
    let store = open_store(&process_env).unwrap_or_else(|e| fatal_exit(&e));
    // The ACCOUNT first, before anything else is read or the browser is opened: a grant with no
    // account to hold it would be an authorization code spent for nothing.
    let accounts = store
        .accounts()
        .unwrap_or_else(|e| fatal_exit(&format!("the credential store would not open: {e}")));
    let account = resolve_target(&accounts, &args.account).unwrap_or_else(|e| fatal_exit(&e));
    let settings_dir = store
        .location()
        .map(|l| l.settings_dir().to_path_buf())
        .unwrap_or_else(|| fatal_exit("no credential store was located to write the grant to"));
    let (client_id, client_secret) = store
        .credential_book()
        .map_err(|e| format!("the credential store would not open: {e}"))
        .and_then(|book| app_pair(&book))
        .unwrap_or_else(|e| fatal_exit(&e));
    let (redirect_uri, scope) = (args.redirect_uri, args.scope);

    let authorize_url = oauth::build_authorize_url(&client_id, &redirect_uri, &scope);

    let listener = TcpListener::bind(LISTEN_ADDR)
        .unwrap_or_else(|e| fatal_exit(&format!("bind {LISTEN_ADDR}: {e}")));

    println!("{}", "=".repeat(70));
    println!("OPEN THIS URL IN YOUR BROWSER, then click Allow:\n");
    println!("   {authorize_url}\n");
    println!("(catcher listening on {redirect_uri} — will auto-exchange on redirect)");
    println!("{}", "=".repeat(70));

    let mut captured_code: Option<String> = None;
    for stream in listener.incoming() {
        let mut stream = match stream {
            Ok(s) => s,
            Err(_) => continue,
        };
        if let Some(code) = handle_connection(&mut stream) {
            captured_code = Some(code);
            break;
        }
    }

    let code = match captured_code {
        Some(c) => c,
        None => fatal_exit("no code captured"),
    };
    println!(
        "[+] Captured code {}... — exchanging immediately.",
        code.chars().take(8).collect::<String>()
    );

    let token: Token = match oauth::exchange_code(&client_id, &client_secret, &code, &redirect_uri)
    {
        Ok(t) => t,
        Err(e) => fatal_exit(&format!("token exchange: {e}")),
    };
    println!(
        "[+] access_token={} refresh_token={} expiresIn={}s",
        redact(&token.access_token),
        redact(&token.refresh_token),
        token.expires_in
    );

    let json = serde_json::json!({
        "accessToken": token.access_token,
        "refreshToken": token.refresh_token,
        "expiresIn": token.expires_in,
    })
    .to_string();
    let path = resolve_token_path(
        args.token_file,
        std::env::current_dir().ok().and_then(|cwd| token_state_dir(&process_env, &cwd)),
    );
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty())
        && let Err(e) = std::fs::create_dir_all(dir)
    {
        fatal_exit(&format!("creating {}: {e}", dir.display()));
    }
    if let Err(e) = write_private(&path, json.as_bytes()) {
        fatal_exit(&format!("writing {}: {e}", path.display()));
    }
    // The PATH, never the contents.
    println!("[+] {} written, owner-only (reusable ~30 days).", path.display());

    // THE GRANT, onto the account's two fields: one transaction for the pair and one journal
    // record (`Actor::cli`), through the writer the daemon's rotation uses for the same fields.
    // `Add`: the account may hold no grant yet. The file above is written FIRST, so a refused
    // store write still leaves the operator holding the grant the venue just issued.
    let fields = [
        (ACCESS_TOKEN.to_string(), token.access_token.clone()),
        (REFRESH_TOKEN.to_string(), token.refresh_token.clone()),
    ];
    let journal = vike_secrets::AccountJournal {
        actor: vike_change_journal::Actor::cli("ctrader_authorize"),
        proc: vike_change_journal::Proc::current(env!("CARGO_PKG_VERSION")),
        now_ms: vike_model::now_ms(),
    };
    match vike_secrets::save_owned_credentials(
        &settings_dir,
        &vike_secrets::CredentialOwner::Account(account),
        &fields,
        vike_secrets::UpsertIntent::Add,
        journal,
    ) {
        // The account row and the FIELD names, never a value.
        Ok((_, journal_error)) => {
            println!(
                "[+] grant written to account row {account} ({ACCESS_TOKEN} / {REFRESH_TOKEN}) in {}",
                vike_secrets::db_path_in(&settings_dir).display()
            );
            if let Some(e) = journal_error {
                eprintln!("[WARN] {e}");
            }
        }
        Err(e) => fatal_exit(&format!(
            "writing the grant to account row {account}: {e} — the grant is in {} (owner-only)",
            path.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_code_from_redirect_request_line() {
        let line = "GET /?code=abc123&scope=trading HTTP/1.1\r\n";
        assert_eq!(extract_code_from_request_line(line), Some("abc123".to_string()));
    }

    #[test]
    fn no_code_when_bare_hit() {
        let line = "GET / HTTP/1.1\r\n";
        assert_eq!(extract_code_from_request_line(line), None);
    }

    #[test]
    fn no_code_when_only_other_params() {
        let line = "GET /?scope=trading HTTP/1.1\r\n";
        assert_eq!(extract_code_from_request_line(line), None);
    }

    #[test]
    fn percent_decode_common_escapes() {
        assert_eq!(percent_decode("abc%2Fdef%2Bghi%3D"), "abc/def+ghi=");
        assert_eq!(percent_decode("plain"), "plain");
        assert_eq!(percent_decode("a+b"), "a b");
    }

    #[test]
    fn percent_decode_truncated_escape_passes_through() {
        // A trailing '%' or '%X' with no valid hex pair is passed through literally rather than
        // panicking or erroring — a malformed capture is not this catcher's problem to solve.
        assert_eq!(percent_decode("abc%2"), "abc%2");
        assert_eq!(percent_decode("abc%"), "abc%");
    }

    #[test]
    fn extracts_and_decodes_percent_encoded_code() {
        let line = "GET /?code=abc%2Fdef%3D%3D&scope=trading HTTP/1.1\r\n";
        assert_eq!(extract_code_from_request_line(line), Some("abc/def==".to_string()));
    }

    #[test]
    fn redact_short_secret_never_leaks() {
        let r = redact("SECRET");
        assert!(!r.contains("SECRET"));
    }

    #[test]
    fn redact_empty_is_none_marker() {
        assert_eq!(redact(""), "<none>");
    }

    #[test]
    fn redact_long_secret_keeps_head_and_tail_only() {
        let r = redact("AT_1234567890ABCDEF");
        assert!(!r.contains("1234567890ABC")); // middle is hidden
        assert!(r.starts_with("AT_123"));
    }

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_string()).collect()
    }

    /// Decision 0095: the flow's parameters are FLAGS, each with the default the variable had.
    #[test]
    fn the_flow_parameters_are_flags_with_the_old_defaults() {
        let d = parse_args_from(&args(&["--account-id", "7"])).unwrap();
        assert_eq!(
            (d.redirect_uri.as_str(), d.scope.as_str()),
            (DEFAULT_REDIRECT_URI, DEFAULT_SCOPE)
        );
        assert_eq!(d.token_file, None);
        let a = parse_args_from(&args(&[
            "--redirect-uri",
            "http://localhost:9999/",
            "--scope",
            "accounts",
            "--token-file",
            "/secure/tok.json",
            "--account-id",
            "7",
        ]))
        .unwrap();
        assert_eq!(a.redirect_uri, "http://localhost:9999/");
        assert_eq!(a.scope, "accounts");
        assert_eq!(a.token_file, Some(PathBuf::from("/secure/tok.json")));
    }

    /// POSITIVE argv pins (decision 0155, written against the hand-rolled parser and kept through
    /// the `clap` migration): each flag alone, all three in any order, and a value is TRIMMED. Each
    /// line now names an account too: decision 0156 item 6 made that required.
    #[test]
    fn every_flag_alone_and_in_any_order_is_accepted() {
        let only_scope =
            parse_args_from(&args(&["--scope", "accounts", "--account-id", "7"])).unwrap();
        assert_eq!(only_scope.scope, "accounts");
        assert_eq!(only_scope.redirect_uri, DEFAULT_REDIRECT_URI);
        let only_uri = parse_args_from(&args(&[
            "--redirect-uri",
            "http://localhost:9999/cb",
            "--tier",
            "demo",
        ]))
        .unwrap();
        assert_eq!(only_uri.redirect_uri, "http://localhost:9999/cb");
        assert_eq!(only_uri.scope, DEFAULT_SCOPE);
        let only_file =
            parse_args_from(&args(&["--token-file", "tok.json", "--account-id", "7"])).unwrap();
        assert_eq!(only_file.token_file, Some(PathBuf::from("tok.json")));
        let reordered = parse_args_from(&args(&[
            "--token-file",
            "/secure/tok.json",
            "--scope",
            "accounts",
            "--redirect-uri",
            "http://localhost:9999/",
            "--account-id",
            "7",
        ]))
        .unwrap();
        assert_eq!(reordered.token_file, Some(PathBuf::from("/secure/tok.json")));
        assert_eq!(reordered.scope, "accounts");
        assert_eq!(reordered.redirect_uri, "http://localhost:9999/");
        // The value is trimmed (a stray space from a pasted line is not part of the path).
        let padded =
            parse_args_from(&args(&["--scope", "  accounts  ", "--account-id", "7"])).unwrap();
        assert_eq!(padded.scope, "accounts");
    }

    /// Decision 0156 item 6: the grant belongs to ONE account, named by its id or by its
    /// `(tier, label)` address — never both, never neither, and never a tier or label the account
    /// table could not hold.
    #[test]
    fn the_account_is_named_exactly_one_way() {
        assert_eq!(
            parse_args_from(&args(&["--account-id", "7"])).unwrap().account,
            AccountTarget::Id(7)
        );
        assert_eq!(
            parse_args_from(&args(&["--tier", "demo"])).unwrap().account,
            AccountTarget::Address { tier: AccountTier::Demo, label: AccountLabel::Default }
        );
        assert_eq!(
            parse_args_from(&args(&["--tier", "live", "--label", "ALT"])).unwrap().account,
            AccountTarget::Address {
                tier: AccountTier::Live,
                label: AccountLabel::parse("ALT").expect("a legal label"),
            }
        );
        for (argv, needle) in [
            (&[][..], "name the account"),
            (&["--account-id", "7", "--tier", "demo"][..], "two ways"),
            (&["--label", "ALT"][..], "beside --tier"),
            (&["--account-id", "seven"][..], "--account-id"),
            (&["--tier", "sim"][..], "--tier"),
            (&["--tier", "demo", "--label", "alt"][..], "--label"),
        ] {
            let err = parse_args_from(&args(argv)).expect_err("refused").text;
            assert!(err.contains(needle), "{argv:?}: {err}");
            assert!(err.contains(USAGE), "{argv:?} must print the usage: {err}");
        }
    }

    fn row(id: i64, venue: &str, tier: AccountTier, label: Option<&str>) -> vike_secrets::Account {
        vike_secrets::Account {
            id,
            venue: venue.to_string(),
            tier,
            label: label.map(str::to_string),
            venue_account_id: None,
            parent_id: None,
            active: true,
            last_verified_at: None,
            max_exposure: None,
        }
    }

    /// The grant is written to an EXISTING ctrader row only: an id or an address resolves to its
    /// row; no row, a row of another venue, an ambiguous address and a store with no `account`
    /// table are each REFUSED — and a refusal for a missing account names how to create it.
    #[test]
    fn the_grant_targets_an_existing_ctrader_account_or_is_refused() {
        let accounts = vike_secrets::Accounts::Known(vec![
            row(3, "binance", AccountTier::Demo, None),
            row(7, VENUE, AccountTier::Demo, None),
            row(8, VENUE, AccountTier::Live, Some("ALT")),
            row(9, VENUE, AccountTier::Demo, Some("TWIN")),
            row(10, VENUE, AccountTier::Demo, Some("TWIN")),
        ]);
        let demo = AccountTarget::Address { tier: AccountTier::Demo, label: AccountLabel::Default };
        let alt = AccountTarget::Address {
            tier: AccountTier::Live,
            label: AccountLabel::parse("ALT").expect("a legal label"),
        };
        assert_eq!(resolve_target(&accounts, &AccountTarget::Id(7)), Ok(7));
        assert_eq!(resolve_target(&accounts, &demo), Ok(7));
        assert_eq!(resolve_target(&accounts, &alt), Ok(8));

        let missing = resolve_target(&accounts, &AccountTarget::Id(99)).expect_err("no row 99");
        assert!(missing.contains("vike-cli secrets account add --venue ctrader"), "{missing}");
        let other = resolve_target(&accounts, &AccountTarget::Id(3)).expect_err("a binance row");
        assert!(other.contains("binance"), "{other}");
        let unaddressed =
            AccountTarget::Address { tier: AccountTier::Live, label: AccountLabel::Default };
        let none = resolve_target(&accounts, &unaddressed).expect_err("no live unlabelled row");
        assert!(none.contains("vike-cli secrets account add --venue ctrader"), "{none}");
        let twin = AccountTarget::Address {
            tier: AccountTier::Demo,
            label: AccountLabel::parse("TWIN").expect("a legal label"),
        };
        let two = resolve_target(&accounts, &twin).expect_err("two rows answer TWIN");
        assert!(two.contains("--account-id"), "{two}");

        let no_table =
            vike_secrets::Accounts::Unanswerable(vike_secrets::NoAccountTable::NoStore {
                db: PathBuf::from("/p/settings/db/vike.db"),
            });
        let err = resolve_target(&no_table, &AccountTarget::Id(7)).expect_err("no table");
        assert!(err.contains("vike-cli secrets init"), "{err}");
    }

    #[test]
    fn a_flag_without_a_value_or_an_unknown_flag_is_refused() {
        assert!(parse_args_from(&args(&["--scope"])).unwrap_err().text.contains("--scope"));
        assert!(parse_args_from(&args(&["--nope"])).unwrap_err().text.contains("--nope"));
        let blank = parse_args_from(&args(&["--token-file", "  "])).unwrap_err();
        assert!(blank.text.contains("--token-file"), "{}", blank.text);
        // Every one is a usage error: stderr, exit 2.
        for line in [&["--scope"][..], &["--nope"], &["--token-file", "  "]] {
            let exit = parse_args_from(&args(line)).unwrap_err();
            assert_eq!((exit.code, exit.to_stdout), (2, false), "{line:?}");
        }
    }

    /// An unexpected argument is refused WITHOUT being printed: a secret pasted onto the command
    /// line by mistake must not come back on the terminal. A flag-shaped one is named by its flag
    /// alone, never by an `=value` it carries. (`hide_values`, decision 0155: no value of this
    /// command line reaches a usage error, wherever it sits.)
    #[test]
    fn an_unexpected_argument_is_refused_without_echoing_it() {
        let err = parse_args_from(&args(&["hunter2-pasted-secret"])).unwrap_err().text;
        assert!(!err.contains("hunter2"), "{err}");
        assert!(err.contains("<hidden>"), "the positional is named as hidden: {err}");
        let err = parse_args_from(&args(&["--client-secret=hunter2"])).unwrap_err().text;
        assert!(err.contains("--client-secret"), "the flag is named: {err}");
        assert!(!err.contains("hunter2"), "its value is not: {err}");
        // A stray positional AFTER valid flags, a secret glued to a short flag, a secret in the
        // value of a flag that is itself refused, and a repeated flag carrying one.
        for line in [
            &["--scope", "accounts", "hunter2-pasted-secret"][..],
            &["-xhunter2"],
            &["--token-file", "hunter2", "--nope"],
            &["--scope", "hunter2", "--scope", "hunter2"],
            &["--scope", "hunter2", "hunter2"],
            &["--redirect-uri=hunter2", "hunter2-b"],
        ] {
            let err = parse_args_from(&args(line)).unwrap_err().text;
            assert!(!err.contains("hunter2"), "{line:?} leaked: {err}");
        }
    }

    /// `-h`/`--help` and `-V`/`--version` exist (this tool had no help of its own flag before) and
    /// print on stdout with exit 0, before any credential is read.
    #[test]
    fn help_and_version_are_a_success_on_stdout() {
        for flag in ["-h", "--help"] {
            let exit = parse_args_from(&args(&[flag])).unwrap_err();
            assert_eq!((exit.code, exit.to_stdout), (0, true), "{flag}");
            assert!(exit.text.contains("--token-file"), "{}", exit.text);
        }
        let exit = parse_args_from(&args(&["--version"])).unwrap_err();
        assert_eq!(exit.text, concat!("ctrader_authorize ", env!("CARGO_PKG_VERSION"), "\n"));
    }

    /// Decision 0095 retired the five process variables this tool read. A leftover one is REFUSED
    /// by name — it is neither read nor ignored in silence (a leftover `CTRADER_SCOPE=accounts`
    /// would otherwise mint a `trading` grant, a leftover `CTRADER_TOKEN_FILE` write the token
    /// somewhere else) — and its value is never printed: one of them is a secret.
    #[test]
    fn a_retired_variable_refuses_by_name_and_never_echoes_its_value() {
        for (var, replacement) in [
            (concat!("CTRADER", "_REDIRECT_URI"), "--redirect-uri"),
            (concat!("CTRADER", "_SCOPE"), "--scope"),
            (concat!("CTRADER", "_TOKEN_FILE"), "--token-file"),
            (concat!("CTRADER", "_CLIENT_ID"), "VENUE application field `CLIENT_ID`"),
            (concat!("CTRADER", "_CLIENT_SECRET"), "VENUE application field `CLIENT_SECRET`"),
        ] {
            let env: std::collections::HashMap<String, String> =
                [(var.to_string(), "hunter2-value".to_string())].into();
            let err = refuse_retired_variables(&env).expect_err(var);
            assert!(err.contains(var), "{err}");
            assert!(err.contains(replacement), "{var}: {err}");
            assert!(!err.contains("hunter2-value"), "{var}: {err}");
        }
        // Blank is unset — the booting roots' rule for the same five names — and a variable that
        // is not one of them is not this tool's business.
        let blank: std::collections::HashMap<String, String> =
            [(concat!("CTRADER", "_SCOPE").to_string(), "  ".to_string())].into();
        assert_eq!(refuse_retired_variables(&blank), Ok(()));
        let other: std::collections::HashMap<String, String> =
            [("UNRELATED".to_string(), "1".to_string())].into();
        assert_eq!(refuse_retired_variables(&other), Ok(()));
    }

    /// The token lands in the project the credential read resolved: `VIKE_SETTINGS_DIR` names it
    /// outright, from ANY working directory. A run from outside a checkout with the override set
    /// wrote the live grant to `./token.json` while the app pair came from the named project.
    #[test]
    fn the_token_follows_the_settings_dir_override() {
        let root = tempfile::tempdir().unwrap();
        let elsewhere = root.path().join("deploy").join("settings");
        let outside = root.path().join("not-a-project");
        std::fs::create_dir_all(&outside).unwrap();
        let env: std::collections::HashMap<String, String> = [(
            vike_paths::state_path::SETTINGS_DIR_ENV.to_string(),
            elsewhere.display().to_string(),
        )]
        .into();
        assert_eq!(token_state_dir(&env, &outside), Some(elsewhere.join("state")));
        // …and with no override the working directory's own project answers, as it always did.
        assert_eq!(token_state_dir(&std::collections::HashMap::new(), &outside), None);
    }

    /// The token path, pure: an explicit flag → the project state dir → the legacy CWD file.
    #[test]
    fn token_path_precedence_flag_then_project_then_legacy() {
        let proj = PathBuf::from("/tmp/proj/settings/state");
        assert_eq!(
            resolve_token_path(Some(PathBuf::from("/secure/tok.json")), Some(proj.clone())),
            PathBuf::from("/secure/tok.json")
        );
        assert_eq!(resolve_token_path(None, Some(proj.clone())), proj.join("ctrader_token.json"));
        assert_eq!(resolve_token_path(None, None), PathBuf::from("token.json"));
    }

    /// The Spotware app pair comes from the credential store as the VENUE's application fields —
    /// never argv, never the env, and never an account's own field of the same name.
    #[test]
    fn the_app_pair_is_the_venues_fields() {
        let mut book = CredentialBook::new();
        book.insert_venue(VENUE, CLIENT_ID, "id");
        book.insert_venue(VENUE, CLIENT_SECRET, "secret");
        assert_eq!(app_pair(&book).unwrap(), ("id".to_string(), "secret".to_string()));

        let mut account_only = CredentialBook::new();
        account_only.insert_account(7, CLIENT_ID, "id");
        account_only.insert_account(7, CLIENT_SECRET, "secret");
        let err = app_pair(&account_only).expect_err("an account's field is not the venue's");
        assert!(err.contains("application field `CLIENT_ID`"), "{err}");
        assert!(app_pair(&CredentialBook::new()).is_err());
    }

    /// `project_state_dir` is what puts the token under `settings/state`, and it is found by
    /// walking UP for the `Cargo.toml` marker — so a token written from a nested working directory
    /// still lands in the one project-owned folder.
    #[test]
    fn the_project_rung_resolves_to_the_settings_state_dir() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("proj");
        std::fs::create_dir_all(project.join("crates/bridges/ctrader")).unwrap();
        std::fs::write(project.join("Cargo.toml"), "[workspace]\n").unwrap();

        let from_nested =
            vike_paths::state_path::project_state_dir(&project.join("crates/bridges/ctrader"));
        assert_eq!(from_nested.as_deref(), Some(project.join("settings/state").as_path()));
        assert_eq!(
            resolve_token_path(None, from_nested),
            project.join("settings/state/ctrader_token.json")
        );
        // Outside any project there is no marker to find, which is what selects the legacy rung.
        assert_eq!(vike_paths::state_path::project_state_dir(root.path()), None);
    }

    /// The token file must never be group/world readable — the same assertion
    /// `vike_secrets::store` makes about the credential store.
    #[test]
    #[cfg(unix)]
    fn the_token_file_is_written_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(TOKEN_FILE);

        write_private(&p, b"{\"accessToken\":\"x\"}").unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "a live OAuth token must not be group/world readable");

        // A pre-existing file left wide open by an older run is NARROWED, not trusted: `.mode()`
        // alone only applies at creation, so this is the case the explicit set_permissions covers.
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_private(&p, b"{\"accessToken\":\"y\"}").unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "an existing wide-open token file must be narrowed on rewrite");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{\"accessToken\":\"y\"}");
    }

    /// **A symlink at the token path must not be written THROUGH.**
    ///
    /// This is the worst shape in the settings-path symlink family, because [`write_private`] does
    /// three things in a row that every one of them follows the link: it TRUNCATES the target,
    /// writes a live OAuth access + refresh token into it, and then narrows THAT file to 0600 — so
    /// a link planted at the token path turns this binary into a primitive for destroying an
    /// arbitrary file and replacing its contents with a live credential.
    ///
    /// The load-bearing assertion is the second one: the decoy's CONTENT is intact. A refusal that
    /// truncated first would still satisfy an `is_err()` check.
    #[test]
    #[cfg(unix)]
    fn a_symlinked_token_path_is_refused_and_its_target_is_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let decoy = dir.path().join("important.conf");
        std::fs::write(&decoy, "DO NOT TRUNCATE ME").unwrap();
        let link = dir.path().join(TOKEN_FILE);
        std::os::unix::fs::symlink(&decoy, &link).unwrap();

        let err = write_private(&link, b"{\"accessToken\":\"live-token-do-not-write\"}")
            .expect_err("a symlinked token path must be refused, not written through");
        assert_eq!(
            std::fs::read_to_string(&decoy).unwrap(),
            "DO NOT TRUNCATE ME",
            "the link's target must be untouched (refusal was: {err})"
        );
    }

    #[test]
    fn redact_multibyte_utf8_never_panics() {
        // Each '€' is 3 bytes in UTF-8; byte-offset slicing here would land mid-character and
        // panic. Chars-based slicing must handle it without panicking.
        let secret = "€€€€€€€€€€€€€€€€€€€€";
        let r = redact(secret);
        assert!(!r.is_empty());
    }
}
