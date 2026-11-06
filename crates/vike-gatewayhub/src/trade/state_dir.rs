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
//! * **`gateway-id`** — a random token proving that two processes share one project folder. The
//!   tradehub compares it with the file it reads itself; it is created once, never rewritten, and
//!   holds nothing secret (it is a name for a folder, not a key).
//!
//! ⚠ The unit's `ReadWritePaths` decides whether this daemon can write here at all; a directory it
//! cannot write makes a LIVE claim fail (loudly, as a refusal) and the token come back empty (logged
//! once). Neither is ever papered over as "no lock".

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use vike_boot::live_lock::LiveLock;
use vike_hub_proto::auth::fresh_nonce;

/// The token file's name inside the state directory.
pub const PROJECT_TOKEN_FILE: &str = "gateway-id";

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
        if let Some(token) = read_token(&path) {
            return Ok(token);
        }
        std::fs::create_dir_all(&self.path)
            .map_err(|e| format!("could not create {}: {e}", self.path.display()))?;
        let token: String = fresh_nonce().iter().take(16).map(|b| format!("{b:02x}")).collect();
        // create_new: never rewrite a token another process (or an earlier start) made.
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                file.write_all(token.as_bytes())
                    .and_then(|()| file.sync_all())
                    .map_err(|e| format!("could not write {}: {e}", path.display()))?;
                Ok(token)
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => read_token(&path)
                .ok_or_else(|| format!("{} exists and is empty or unreadable", path.display())),
            Err(e) => Err(format!("could not create {}: {e}", path.display())),
        }
    }
}

/// The trimmed content of the token file, if it holds one.
fn read_token(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let token = text.trim();
    (!token.is_empty()).then(|| token.to_string())
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
