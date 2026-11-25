//! The project's STATE DIRECTORY, as the trade lane uses it: the live-account lock and the project token.
//!
//! The directory is `<project>/settings/state`, exactly the one the boot's single project walk resolved
//! (`vike_boot::Booted::state_dir`); this module resolves nothing and walks nothing, it is handed the
//! path. Two things live in it for the trade lane:
//!
//! * **`LIVE-<route_key>.lock`** — the one-live-process-per-account claim (`vike_boot::live_lock`). Before
//!   the venue half moved here the tradehub claimed it as it mounted a live engine; the gatewayhub is now
//!   the process that holds the venue session, so it claims at `TradeOpen`, for an armed live route
//!   key, in addition to the lease. The refusal is the lock's own `AddrInUse` text, unchanged.
//! * **`gateway-id`** (`vike_paths::state_path::PROJECT_TOKEN_FILE`, read by
//!   `vike_paths::state_path::read_project_token`, the tradehub's read too) — a random token proving that two
//!   processes share one project folder. The tradehub compares it with the file it reads itself; it is
//!   created once, never rewritten (but a BLANK one, what a crash mid-create leaves, is replaced whole),
//!   and holds nothing secret (it is a name for a folder, not a key).
//!
//! ⚠ The unit's `ReadWritePaths` decides whether this daemon can write here at all; a directory it
//! cannot write makes a LIVE claim fail (loudly, as a refusal) and the token come back empty (logged
//! once). Neither is ever papered over as "no lock".

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use vike_boot::live_lock::LiveLock;
use vike_hub_protocol::auth::fresh_nonce;
use vike_paths::state_path::{PROJECT_TOKEN_FILE, read_project_token};

/// The project's state directory.
#[derive(Debug, Clone)]
pub struct StateDir {
    path: PathBuf,
}

impl StateDir {
    /// The directory at `path` (not created until something is written).
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Claim the live-account lock of `route_key` (`LIVE-<route_key>.lock`). The guard is the claim:
    /// hold it as long as the live adapter exists. `Err` is the lock's own: `AddrInUse` when another
    /// process holds the account, anything else when the sentinel cannot be made.
    pub fn claim_live(&self, route_key: &str) -> io::Result<LiveLock> {
        LiveLock::acquire(&self.path, route_key)
    }

    /// The project token: the content of `gateway-id`, created on first use. `Err` carries the reason it
    /// could not be read or made (the caller reports an empty token and logs it).
    pub fn project_token(&self) -> Result<String, String> {
        let path = self.path.join(PROJECT_TOKEN_FILE);
        if let Some(token) = read_token(&self.path) {
            return Ok(token);
        }
        std::fs::create_dir_all(&self.path)
            .map_err(|e| format!("could not create {}: {e}", self.path.display()))?;
        let token = vike_utility::hex::hex_lower(&fresh_nonce()[..16]);
        // create_new: never rewrite a token another process (or an earlier start) made.
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                file.write_all(token.as_bytes())
                    .and_then(|()| file.sync_all())
                    .map_err(|e| format!("could not write {}: {e}", path.display()))?;
                Ok(token)
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                match read_project_token(&self.path) {
                    // Another process (or an earlier start) made it.
                    Ok(held) if !held.is_empty() => Ok(held),
                    // Blank: a crash between the `create_new` and the `sync_all` above left it. Replaced whole,
                    // or the tradehub has nothing to compare and the cross-project check stays off for good.
                    Ok(_) => vike_utility::atomic_file::write_atomic(&path, token.as_bytes())
                        .map(|()| token)
                        .map_err(|e| {
                            format!("could not replace the blank {}: {e}", path.display())
                        }),
                    Err(e) => Err(format!("{} exists and cannot be read: {e}", path.display())),
                }
            }
            Err(e) => Err(format!("could not create {}: {e}", path.display())),
        }
    }
}

/// The token in `state_dir`, if the file holds one: an absent, unreadable or blank file is none (the caller
/// makes the token, or says why it could not).
fn read_token(state_dir: &Path) -> Option<String> {
    read_project_token(state_dir).ok().filter(|token| !token.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_project_token_is_made_once_and_read_back_never_rewritten() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let state = StateDir::new(dir.path().join("state"));
        let first = state.project_token().expect("created");
        assert_eq!(first.len(), 32);
        assert!(first.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(state.project_token().expect("read back"), first);
        assert_eq!(StateDir::new(dir.path().join("state")).project_token().expect("read"), first);
    }

    /// **A blank token file (a crash between its create and its sync) is replaced, not an error for good**: an
    /// empty token would switch the tradehub's cross-project check off at every later start.
    #[test]
    fn a_blank_token_file_is_replaced_whole_and_then_kept() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let state = StateDir::new(dir.path());
        std::fs::write(dir.path().join(PROJECT_TOKEN_FILE), b" \n").expect("a blank token file");
        let made = state.project_token().expect("replaced");
        assert_eq!(made.len(), 32);
        assert_eq!(read_project_token(dir.path()).expect("on disk"), made);
        assert_eq!(state.project_token().expect("read back"), made, "and never rewritten after");
    }

    #[test]
    fn a_state_dir_that_cannot_be_made_is_an_error_not_an_empty_token() {
        let dir = tempfile::tempdir().expect("a temp dir");
        // A FILE where the directory should be.
        let blocked = dir.path().join("state");
        std::fs::write(&blocked, b"x").expect("a file in the way");
        let err = StateDir::new(&blocked).project_token().expect_err("cannot make the directory");
        assert!(err.contains("could not"), "{err}");
    }

    #[test]
    fn a_second_claim_of_one_account_is_the_locks_own_refusal() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let state = StateDir::new(dir.path());
        let held = state.claim_live("binance").expect("first claim");
        let refused = state.claim_live("binance").expect_err("the account is held");
        assert_eq!(refused.kind(), io::ErrorKind::AddrInUse);
        assert!(refused.to_string().contains("another live process already trades"), "{refused}");
        // A different account is its own lock; releasing frees the account.
        assert!(state.claim_live("binance#ALT").is_ok());
        drop(held);
        assert!(state.claim_live("binance").is_ok());
    }
}
