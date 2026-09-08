//! Local OAuth catcher for cTrader Open API — removes the human copy/paste round-trip. Starts a
//! plain HTTP server on `http://localhost:5033/` (default; override via `CTRADER_REDIRECT_URI`),
//! prints the authorize URL, blocks for the browser's `?code=...` redirect, exchanges the code for
//! a [`vike_ctrader::oauth::Token`] IN THE SAME PROCESS, prints the redacted token, and writes it
//! to `<project>/settings/state/ctrader_token.json` (camelCase, matching the venue's own wire
//! shape, so it round-trips through the same JSON a future loader would parse).
//!
//! # ⚠ This file holds a LIVE access + refresh token
//!
//! It used to be written as `token.json` **in the process working directory** — with no resolver at
//! all, so a live credential landed in whatever directory the binary happened to be started from,
//! which on a server is frequently world-readable. Two things changed:
//!
//! 1. **It is resolved, like every other credential-adjacent file** (settings-unification #1084):
//!    `$CTRADER_TOKEN_FILE` → `<project>/settings/state/ctrader_token.json` → `./token.json`. The
//!    last rung is unchanged pre-#1084 behaviour and is reached only by a binary run with no
//!    project above its working directory; `settings/` is gitignored (#1085), so the default path
//!    cannot be committed by accident.
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
//! Env (never argv): `CTRADER_CLIENT_ID`, `CTRADER_CLIENT_SECRET` (required); `CTRADER_REDIRECT_URI`
//! (default `http://localhost:5033/`), `CTRADER_SCOPE` (default `trading`).
//!
//! This is a developer tool, not a library entry point: all output is raw `println!`/`eprintln!`,
//! not `tracing` (per CLAUDE.md — protocol/result stdout stays raw).

use std::env;
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

/// Resolve where the token is written: an explicit `$CTRADER_TOKEN_FILE` → the project's
/// `settings/state/ctrader_token.json` → the legacy CWD-relative `token.json`.
///
/// PURE — the environment and the working directory arrive as parameters and are read by
/// [`token_path`], per the rule `crates/vike-ops/tests/settings_registry.rs` enforces. An
/// empty/whitespace override falls THROUGH rather than resolving to `""`: an exported-but-blank
/// variable is an unset variable in every shell that produced it, and honouring it would put the
/// token back in the CWD — exactly what this resolver exists to stop.
fn resolve_token_path(env_override: Option<String>, project_state_dir: Option<PathBuf>) -> PathBuf {
    env_override
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| project_state_dir.map(|d| d.join(TOKEN_FILE)))
        .unwrap_or_else(|| PathBuf::from(LEGACY_TOKEN_FILE))
}

/// The env-reading half of [`resolve_token_path`]. `Layer::Binary` — this is a `src/bin/` file.
fn token_path() -> PathBuf {
    resolve_token_path(
        env::var("CTRADER_TOKEN_FILE").ok(),
        env::current_dir().ok().and_then(|cwd| vike_model::state_path::project_state_dir(&cwd)),
    )
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
    let client_id = env::var("CTRADER_CLIENT_ID")
        .unwrap_or_else(|_| fatal_exit("CTRADER_CLIENT_ID env var is required"));
    let client_secret = env::var("CTRADER_CLIENT_SECRET")
        .unwrap_or_else(|_| fatal_exit("CTRADER_CLIENT_SECRET env var is required"));
    let redirect_uri =
        env::var("CTRADER_REDIRECT_URI").unwrap_or_else(|_| DEFAULT_REDIRECT_URI.into());
    let scope = env::var("CTRADER_SCOPE").unwrap_or_else(|_| DEFAULT_SCOPE.into());

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
    let path = token_path();
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

    /// The resolver's precedence, pure: override → project state dir → the legacy CWD file.
    #[test]
    fn token_path_precedence_override_then_project_then_legacy() {
        let proj = PathBuf::from("/tmp/proj/settings/state");

        // 1. An explicit override wins over everything.
        assert_eq!(
            resolve_token_path(Some("/secure/tok.json".into()), Some(proj.clone())),
            PathBuf::from("/secure/tok.json")
        );
        // 2. No override ⇒ the project's state directory, venue-qualified basename.
        assert_eq!(resolve_token_path(None, Some(proj.clone())), proj.join("ctrader_token.json"));
        // 3. No project above the working directory ⇒ unchanged pre-#1084 behaviour.
        assert_eq!(resolve_token_path(None, None), PathBuf::from("token.json"));
        // …and that last rung is the ONLY way to reach the CWD: an exported-but-blank override
        // must not resolve to "" (which would also land in the CWD, but silently).
        assert_eq!(
            resolve_token_path(Some("   ".into()), Some(proj.clone())),
            proj.join("ctrader_token.json")
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
