//! **Every venue link in the settings store is read BY NUMBER** — ruling 3 of
//! `docs/superpowers/specs/2026-09-22-the-settings-store-plane-design.md`, ordered finished by the
//! owner on 2026-09-30.
//!
//! Every table of the shipped shape carries `venue_id` alone — the last text `venue` column left
//! the DDL with `venue_arming` (decision 0119) — so no statement can read a venue but by its
//! number. What is left to prove is the shape itself (no table carries a text venue), that a
//! venue-scoped credential names its venue by number, the credential scope check, and that the
//! credential filer's fallback account lookup (`AccountResolver::load`) finds its account.

use vike_secrets::{SettingsSource, StoredSettings};

use crate::support::Fixture;
use crate::support::sql::has_text_venue;

mod by_venue_id;
mod converted_readers;
mod fixture;

use fixture::*;
