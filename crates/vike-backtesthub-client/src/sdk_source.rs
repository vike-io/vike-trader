//! **The versioned strategy SDK's source, as the backtest server serves it** (decision 0296, verdict 3:
//! "Studio gets two read verbs that serve an SDK's source"): the SDKs the server holds
//! ([`WireSdkListing`], the answer to `Request::ListSdks`) and one file or folder of one of them
//! ([`WireSdkFile`], the answer to `Request::ReadSdkFile`). The Studio shows the SDK a strategy file
//! builds against read-only, as NinjaTrader ships its indicators' source.
//!
//! The server writes these and the Studio reads them, so a key renamed on one side is a compile error on
//! the other.
//!
//! ⚠ **The shape is a contract with more than one reader.** `Request::ListSdks` carries no field and
//! answers [`WireSdkListing`] (the folder read, then every SDK newest first, each [`WireSdk`]'s `name`,
//! `number`, `commit`, `abi_version`, `usable`, `why_unusable`); `Request::ReadSdkFile { sdk, path }`
//! answers [`WireSdkFile`]. The SDK source view reads the list, and so does any other Studio surface that
//! picks an SDK (the SDK diff's picker): call `crate::BacktesthubClient::list_sdks` and resolve a pin with
//! [`WireSdkListing::resolve`] rather than re-deciding which SDK a file builds against. The bytes are
//! frozen by `tests/backtest_plane_wire_pin.rs` (`fixtures/datahub_wire/v7_backtest_plane_*.jsonl`): a new
//! field is additive (`serde(default)`) and adds a line there; it never renames or removes one.
//!
//! ⚠ **Sized for the tunnel** (13-40 KB/s, `crate::study`'s module doc): one SDK holds about 1,300
//! files, so nothing answers a whole SDK's file list. A FOLDER reads as its entries, one level, so the
//! Studio's tree opens a folder at a time; a file reads whole up to [`SDK_FILE_MAX_BYTES`] and is
//! refused above it.
//!
//! ⚠ **Only [`SDK_CRATES_FOLDER`] is served.** A path names a file or folder under `crates/` of the SDK
//! (`crates/vike-tactics/src/lib.rs`); anything else — the snapshot's `Cargo.lock` and marker, and on a
//! dev checkout ("SDK dev") everything a checkout holds besides its crates, its settings database
//! included — is refused by the server, which checks every path it is sent
//! (`crates/vike-backtesthub/src/sdk_source.rs`'s module doc).

use serde::{Deserialize, Serialize};

/// The one folder of an SDK the read verb serves, relative to the SDK's root (`sdk-N/crates`): its
/// crates, as `scripts/package_strategy_source.sh` packs them. The Studio's tree starts by reading it.
pub const SDK_CRATES_FOLDER: &str = "crates";

/// The most bytes of one SDK file `Request::ReadSdkFile` answers: a larger file is refused in words,
/// never cut. The SDK's largest source file is about 170 KB (a test of `vike-indicators`, measured
/// 2026-11-19); what lies above is test data (a 940 KB fixture csv).
pub const SDK_FILE_MAX_BYTES: u64 = 256 * 1024;

/// The SDKs the server holds, as `Request::ListSdks` answers them.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WireSdkListing {
    /// Where the SDKs are on the SERVER's box (`<project>/vendor/vike-strategy-source`, or the checkout
    /// on a dev checkout), named so a user knows what the server read.
    pub root: String,
    /// Every SDK, newest first: on a dev checkout the one `sdk-dev`, else each installed `sdk-N`.
    pub sdks: Vec<WireSdk>,
}

impl WireSdkListing {
    /// The SDK a strategy file whose header pins `pin` builds against on this server (decision 0296,
    /// verdict 2, and the design's dev rule): on a dev checkout `sdk-dev`, whatever the pin; a pin names
    /// its `sdk-N`, listed or not (`None` when it is not on the server); no pin names the newest USABLE
    /// SDK. Only a name of [`Self::sdks`] is answered, and the usability is the server's
    /// ([`WireSdk::usable`]).
    pub fn resolve(&self, pin: Option<u32>) -> Option<&WireSdk> {
        if let Some(dev) = self.sdks.iter().find(|s| s.number.is_none()) {
            return Some(dev);
        }
        match pin {
            Some(pin) => self.sdks.iter().find(|s| s.number == Some(pin)),
            None => self.sdks.iter().find(|s| s.usable),
        }
    }
}

/// One SDK of the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireSdk {
    /// Its folder's name, which `Request::ReadSdkFile` names it by: `sdk-N`, or `sdk-dev`.
    pub name: String,
    /// N; `None` for `sdk-dev`.
    pub number: Option<u32>,
    /// The commit it was cut at (for `sdk-dev`, the commit the server was built from); empty when its
    /// marker could not be read.
    #[serde(default)]
    pub commit: String,
    /// The interface version it was cut with (for `sdk-dev`, the server's); `None` when its marker could
    /// not be read.
    #[serde(default)]
    pub abi_version: Option<u32>,
    /// Whether a strategy file builds against it on this server, by the SDK vocabulary's one rule
    /// (`crates/vike-rust-strategy/src/sdk.rs`'s `usable`: the boundary matches and it is inside the
    /// support window).
    pub usable: bool,
    /// Why it is not usable, in the server's words (and the fix); empty when it is.
    #[serde(default)]
    pub why_unusable: String,
}

/// One entry of an SDK folder, as [`WireSdkFile::entries`] lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireSdkEntry {
    /// Its name in the folder.
    pub name: String,
    /// A folder (else a regular file).
    pub folder: bool,
}

/// One file or folder of one SDK, as `Request::ReadSdkFile` answers it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireSdkFile {
    /// The SDK, as asked.
    pub sdk: String,
    /// The path under the SDK's root, as asked (`crates/vike-tactics/src/lib.rs`).
    pub path: String,
    /// It is a folder: its [`Self::entries`] are listed and [`Self::text`] is empty.
    pub folder: bool,
    /// A file's text (UTF-8, whole).
    #[serde(default)]
    pub text: String,
    /// A folder's entries, folders first, each group in name order; a link or anything else that is
    /// not a regular file or a folder is never listed.
    #[serde(default)]
    pub entries: Vec<WireSdkEntry>,
}

#[path = "sdk_source_tests.rs"]
#[cfg(test)]
mod sdk_source_tests;
