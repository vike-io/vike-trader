//! Local OAuth catcher for cTrader Open API — removes the human copy/paste round-trip. Starts a
//! plain HTTP server on `http://localhost:5033/` (the default redirect URI; `--redirect-uri`
//! overrides it), prints the authorize URL, blocks for the browser's `?code=...` redirect,
//! exchanges the code for a [`vike_ctrader::oauth::Token`] IN THE SAME PROCESS, prints the redacted
//! token, and writes it to `<project>/settings/state/ctrader_token.json` (camelCase, matching the
//! venue's own wire shape, so it round-trips through the same JSON a future loader would parse).
//!
//! # ⚠ This file holds a LIVE access + refresh token
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
//!    `crates/vike-secrets/src/store.rs`'s `PermissionWarning` demands of the credential store, and
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
//! Usage: `ctrader_authorize [--redirect-uri URI] [--scope SCOPE] [--token-file PATH]` — the
//! defaults are `http://localhost:5033/`, `trading` and the resolved path above. The app pair
//! (`CTRADER_CLIENT_ID`/`_SECRET`) is read from the CREDENTIAL STORE, the rows every cTrader mount
//! reads — never argv or the environment (decision 0095). The five process variables this tool
//! used to read (`CTRADER_REDIRECT_URI`, `CTRADER_SCOPE`, `CTRADER_TOKEN_FILE` and the app pair)
//! are retired: this tool REFUSES to run while one is set ([`refuse_retired_variables`]), as every
//! booting root refuses to start. The one setting it takes out of its process-environment sweep is
//! `VIKE_SETTINGS_DIR`, for the credential read and for the token path alike, so the two answer
//! "which project" the same way; the only other names it looks up in the sweep are those five, and
//! only to refuse them.
//!
//! This is a developer tool, not a library entry point: all output is raw `println!`/`eprintln!`,
//! not `tracing` (per CLAUDE.md — protocol/result stdout stays raw).

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};

use vike_ctrader::oauth::{self, Token};

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
/// gets here: [`parse_args`] refuses it by name, so the CWD is reached only through the last rung.
fn resolve_token_path(explicit: Option<PathBuf>, project_state_dir: Option<PathBuf>) -> PathBuf {
    explicit
        .or_else(|| project_state_dir.map(|d| d.join(TOKEN_FILE)))
        .unwrap_or_else(|| PathBuf::from(LEGACY_TOKEN_FILE))
}

/// The flow's parameters, from the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AuthorizeArgs {
    redirect_uri: String,
    scope: String,
    token_file: Option<PathBuf>,
}

const USAGE: &str =
    "usage: ctrader_authorize [--redirect-uri URI] [--scope SCOPE] [--token-file PATH]";

/// PURE over argv (without the program name). Each flag takes the next argument; a missing, blank
/// or unknown one is refused by name.
fn parse_args(argv: &[String]) -> Result<AuthorizeArgs, String> {
    let mut out = AuthorizeArgs {
        redirect_uri: DEFAULT_REDIRECT_URI.to_string(),
        scope: DEFAULT_SCOPE.to_string(),
        token_file: None,
    };
    let mut it = argv.iter();
    while let Some(flag) = it.next() {
        let mut value = || {
            it.next()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .ok_or_else(|| format!("{flag} needs a value\n{USAGE}"))
        };
        match flag.as_str() {
            "--redirect-uri" => out.redirect_uri = value()?,
            "--scope" => out.scope = value()?,
            "--token-file" => out.token_file = Some(PathBuf::from(value()?)),
            other => return Err(format!("unknown argument {}\n{USAGE}", unechoed(other))),
        }
    }
    Ok(out)
}

/// How an unexpected argument is NAMED in a refusal without being printed: a flag by its flag (an
/// `=value` it carries is elided), anything else by its length alone — a secret pasted onto the
/// command line by mistake must not come back on the terminal.
fn unechoed(arg: &str) -> String {
    if arg.starts_with('-') {
        match arg.split_once('=') {
            Some((flag, _)) => format!("`{flag}=…`"),
            None => format!("`{arg}`"),
        }
    } else {
        format!("(a positional argument of {} characters, not printed)", arg.chars().count())
    }
}

/// The project state directory the token lands in: the project `VIKE_SETTINGS_DIR` names when the
/// sweep carries it — the SAME project the credential read resolves, so the grant lands beside the
/// store that supplied its app pair — else the working directory's.
fn token_state_dir(
    process_env: &std::collections::HashMap<String, String>,
    cwd: &Path,
) -> Option<PathBuf> {
    vike_model::state_path::project_state_dir_from_env(process_env, cwd)
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
        "the app pair is read from the credential store — `vike-cli secrets set CTRADER_CLIENT_ID` \
         writes it there (the value on stdin) once the variable is unset",
    ),
    (
        "CTRADER_CLIENT_SECRET",
        "the app pair is read from the credential store — `vike-cli secrets set \
         CTRADER_CLIENT_SECRET` writes it there (the value on stdin) once the variable is unset",
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

/// The Spotware application pair, out of the credential store's map.
fn app_pair(vars: &std::collections::HashMap<String, String>) -> Result<(String, String), String> {
    let get = |k: &str| {
        vars.get(k).map(|v| v.trim().to_string()).filter(|v| !v.is_empty()).ok_or_else(|| {
            format!(
                "{k} is not in the credential store — write it with `vike-cli secrets set {k}` \
                 (the value on stdin)"
            )
        })
    };
    Ok((get("CTRADER_CLIENT_ID")?, get("CTRADER_CLIENT_SECRET")?))
}

/// Write `bytes` to `path` with OWNER-ONLY permissions (0600) — the posture
/// `crates/vike-secrets/src/store.rs`'s `PermissionWarning` demands of the credential store, for the
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
/// passed through. No external URL crate — mirrors `oauth::percent_encode`'s "small ASCII-safe
/// value set" scope. Invalid/truncated `%` escapes are passed through literally rather than
/// erroring, since a malformed code is a webserver's problem, not this catcher's.
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
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return;
    }
    let args = parse_args(&argv).unwrap_or_else(|e| fatal_exit(&e));
    // The credential chain from the ONE process-environment sweep this root owns — the shape of
    // `hyperliquid_builder_fee_approve`: the settings-directory override comes out of the sweep and
    // nothing below reads the environment again.
    let process_env: std::collections::HashMap<String, String> = std::env::vars().collect();
    refuse_retired_variables(&process_env).unwrap_or_else(|e| fatal_exit(&e));
    let vars = vike_bridge_core::credentials::load_workspace_secrets_from_env(&process_env);
    let (client_id, client_secret) = app_pair(&vars).unwrap_or_else(|e| fatal_exit(&e));
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
        let d = parse_args(&args(&[])).unwrap();
        assert_eq!(
            (d.redirect_uri.as_str(), d.scope.as_str()),
            (DEFAULT_REDIRECT_URI, DEFAULT_SCOPE)
        );
        assert_eq!(d.token_file, None);
        let a = parse_args(&args(&[
            "--redirect-uri",
            "http://localhost:9999/",
            "--scope",
            "accounts",
            "--token-file",
            "/secure/tok.json",
        ]))
        .unwrap();
        assert_eq!(a.redirect_uri, "http://localhost:9999/");
        assert_eq!(a.scope, "accounts");
        assert_eq!(a.token_file, Some(PathBuf::from("/secure/tok.json")));
    }

    #[test]
    fn a_flag_without_a_value_or_an_unknown_flag_is_refused() {
        assert!(parse_args(&args(&["--scope"])).unwrap_err().contains("--scope"));
        assert!(parse_args(&args(&["--nope"])).unwrap_err().contains("--nope"));
        assert!(parse_args(&args(&["--token-file", "  "])).unwrap_err().contains("--token-file"));
    }

    /// An unexpected argument is refused WITHOUT being printed: a secret pasted onto the command
    /// line by mistake must not come back on the terminal. A flag-shaped one is named by its flag
    /// alone, never by an `=value` it carries.
    #[test]
    fn an_unexpected_argument_is_refused_without_echoing_it() {
        let err = parse_args(&args(&["hunter2-pasted-secret"])).unwrap_err();
        assert!(!err.contains("hunter2"), "{err}");
        let err = parse_args(&args(&["--client-secret=hunter2"])).unwrap_err();
        assert!(err.contains("--client-secret"), "the flag is named: {err}");
        assert!(!err.contains("hunter2"), "its value is not: {err}");
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
            (
                concat!("CTRADER", "_CLIENT_ID"),
                concat!("vike-cli secrets set CTRADER", "_CLIENT_ID"),
            ),
            (
                concat!("CTRADER", "_CLIENT_SECRET"),
                concat!("vike-cli secrets set CTRADER", "_CLIENT_SECRET"),
            ),
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
            vike_model::state_path::SETTINGS_DIR_ENV.to_string(),
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

    /// The Spotware app pair comes from the credential store's map — never argv, never the env.
    #[test]
    fn the_app_pair_is_read_from_the_store_map() {
        let vars: std::collections::HashMap<String, String> = [
            (concat!("CTRADER", "_CLIENT_ID").to_string(), "id".to_string()),
            (concat!("CTRADER", "_CLIENT_SECRET").to_string(), "secret".to_string()),
        ]
        .into_iter()
        .collect();
        assert_eq!(app_pair(&vars).unwrap(), ("id".to_string(), "secret".to_string()));
        assert!(
            app_pair(&std::collections::HashMap::new())
                .unwrap_err()
                .contains("vike-cli secrets set")
        );
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
            vike_model::state_path::project_state_dir(&project.join("crates/bridges/ctrader"));
        assert_eq!(from_nested.as_deref(), Some(project.join("settings/state").as_path()));
        assert_eq!(
            resolve_token_path(None, from_nested),
            project.join("settings/state/ctrader_token.json")
        );
        // Outside any project there is no marker to find, which is what selects the legacy rung.
        assert_eq!(vike_model::state_path::project_state_dir(root.path()), None);
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
