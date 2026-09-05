//! `vike-secrets` — the credential store: ONE file, in the project.
//!
//! ```text
//! <project>/settings/secrets.env      every API key, every venue, every checkout of this project
//! ```
//!
//! That is the whole story. [`resolve`] opens a path the caller names, [`resolve_project`] asks for
//! the project's own, and [`workspace_dotenv_path`] is how a caller with no opinion learns where
//! that is. There is no precedence and no second location, so there is nothing to guess at and
//! nothing to debug: point at the file, edit the file.
//!
//! `<project>` is resolved at RUNTIME by walking UP for a project marker — see
//! [`project_settings_dir`] for the two markers and their dispositions — and
//! `VIKE_SETTINGS_DIR` ([`SETTINGS_DIR_ENV`]) names the directory outright when a deployment wants
//! to. That value arrives as a PARAMETER: this crate performs no environment read of its own, so
//! nothing here joins the settings registry's `Layer::Library` work-list.
//!
//! # Absent credentials ARE the live gate
//!
//! No store ⇒ an EMPTY map ⇒ every venue loader returns `None` ⇒ every venue stays paper. That is
//! the designed behaviour, not a degradation, and it is why [`resolve`] treats an absent file as an
//! answer rather than an error. A store that EXISTS and cannot be read is the opposite case and does
//! error: "not configured" and "cannot open" must never look the same to an operator.
//!
//! # Writing the store
//!
//! Nothing here ever REGENERATES, reorders or deletes the store — it is the user's only copy of
//! live venue credentials. [`upsert_env`]/[`save_credentials`] are the one sanctioned write, and
//! they are a byte-preserving UPSERT: named keys are replaced in place, new ones appended, and
//! every comment, blank line and unrelated key survives verbatim, written back atomically. That is
//! what makes the store a safe home for a credential that ROTATES (a venue's OAuth grant) as well
//! as for one a human typed. See `env_write`'s module doc; a caller reaching for `fs::write` on
//! this file is the bug it exists to prevent.
//!
//! ⚠ **The gate and a botched UPGRADE produce the same empty map**, which is why [`resolve`]'s
//! absent arm also reports [`legacy_store_warning`]: a `<project>/.env` — the store's predecessor —
//! left in place while the new one was never created. That is a FINDING and never a refusal, because
//! a `.env` also has a legitimate second life as a systemd `EnvironmentFile`. See
//! [`LegacyStoreWarning`], and `docs/ops/upgrading.md` for the whole upgrade path.
//!
//! # Layering
//!
//! **ZERO dependencies** — no `vike-*` crate and no external crate either (`tempfile` is a dev-dep
//! for the project-walk tests). Two consumers need this tree and must not drag each other in:
//! `vike-bridge-core`, which owns the venue transport stack, and `vike-cli`, which is
//! DataFusion-free, transport-free and rides the FAST CI lane and would otherwise have had to link
//! `ureq`/`tungstenite`/`rustls` to read a `KEY=VALUE` file.
//!
//! The consequence for paths is that this crate never resolves a directory it was not given:
//! [`project_settings_dir`] walks up from a `&Path` the caller supplies, and the only `std::env`
//! contact anywhere here is [`workspace_dotenv_path`]'s `current_dir`.
//!
//! `vike_bridge_core::credentials` re-exports [`parse_dotenv`], [`workspace_dotenv_path`] and
//! [`load_workspace_dotenv`] under their historical paths, so all ~179 existing call sites are
//! unchanged.
//!
//! # Redaction
//!
//! Matching `vike_bridge_core::credentials::Credentials` exactly — a manual `Debug` that redacts,
//! with tests asserting it. [`SecretMap`] has no `Display` at all and a `Debug` that prints key
//! NAMES and never a value; reaching the plaintext requires [`SecretMap::into_map`], whose name
//! says so. [`SecretsError`] carries a path and an OS reason, never file contents.

mod dotenv;
mod env_write;
mod store;

pub use dotenv::{
    SECRETS_FILE, SETTINGS_DIR, SETTINGS_DIR_ENV, STATE_DIR, load_workspace_dotenv,
    load_workspace_dotenv_from, parse_dotenv, project_secrets_path, project_secrets_path_from,
    project_settings_dir, project_settings_dir_for, project_settings_dir_from,
    workspace_dotenv_path, workspace_dotenv_path_from,
};
pub use env_write::{save_credentials, upsert_env};
pub use store::{
    Finding, LEGACY_STORE_FILE, LegacyStoreWarning, PermissionWarning, Resolved, SecretMap,
    SecretsError, Source, legacy_store_warning, permission_warning, resolve, resolve_project,
};
