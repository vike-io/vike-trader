//! The Enumerable catalog's cache SHAPE ([`CatalogCache`], [`CatalogCacheVenueStamp`]), which the desktop holds in
//! memory.
//!
//! ⚠ **Nothing writes it to disk any more** (the owner's ruling of 2026-11-12): the venues' lists are
//! the DATAHUB's, kept one file per venue in `<project>/instruments` and refreshed weekly there
//! (`crates/vike-datahub/src/instrument_lists.rs`), and the desktop's old `catalog.json` is read and
//! written by nothing. [`save_cache`] and [`load_cache`] have no production caller; they stay because
//! two gates use this file as their control (`crates/vike-ops/tests/hygiene/journal_scratch_gate.rs`
//! and `crates/vike-ops/tests/hygiene/system_temp_gate/ratchet.rs`).
//! shortcut: dead helpers kept for the gates; delete them with those gates' re-pin.

use crate::CatalogError;
use serde::{Deserialize, Serialize};
use std::path::Path;
use vike_model::Instrument;

/// Per-venue refresh stamp shown in the Data Manager Catalog/Sources view.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogCacheVenueStamp {
    pub venue: String,
    pub last_refreshed_ms: i64,
    pub count: usize,
}

/// The serialized Enumerable catalog + per-venue stamps.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct CatalogCache {
    pub fetched: Vec<CatalogCacheVenueStamp>,
    pub instruments: Vec<Instrument>,
}

pub fn save_cache(path: &Path, cache: &CatalogCache) -> Result<(), CatalogError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| CatalogError(e.to_string()))?;
    }
    let json = serde_json::to_vec(cache).map_err(|e| CatalogError(e.to_string()))?;
    std::fs::write(path, json).map_err(|e| CatalogError(e.to_string()))
}

pub fn load_cache(path: &Path) -> Result<CatalogCache, CatalogError> {
    let bytes = std::fs::read(path).map_err(|e| CatalogError(e.to_string()))?;
    serde_json::from_slice(&bytes).map_err(|e| CatalogError(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_row;
    use vike_model::AssetClass;

    #[test]
    fn cache_roundtrips_through_disk() {
        let dir = std::env::temp_dir().join(format!("vikecat_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("catalog.json");
        let cache = CatalogCache {
            fetched: vec![CatalogCacheVenueStamp {
                venue: "binance".into(),
                last_refreshed_ms: 111,
                count: 1,
            }],
            instruments: vec![test_row(
                "binance",
                "BTCUSDT",
                "BTC",
                "USDT",
                AssetClass::CryptoSpot,
            )],
        };
        save_cache(&path, &cache).unwrap();
        let back = load_cache(&path).unwrap();
        assert_eq!(back.instruments.len(), 1);
        assert_eq!(back.fetched[0].venue, "binance");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn save_creates_missing_parent_dir() {
        let dir = std::env::temp_dir().join(format!("vikecat_{}_new", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let path = dir.join("sub").join("catalog.json");
        assert!(!path.parent().unwrap().exists());

        let cache = CatalogCache {
            fetched: vec![CatalogCacheVenueStamp {
                venue: "okx".into(),
                last_refreshed_ms: 222,
                count: 1,
            }],
            instruments: vec![test_row(
                "okx",
                "BTC-USDT-SWAP",
                "BTC",
                "USDT",
                AssetClass::CryptoPerp,
            )],
        };

        save_cache(&path, &cache).expect("save_cache should create missing parent dir");
        assert!(path.exists());

        let back = load_cache(&path).unwrap();
        assert_eq!(back.instruments.len(), 1);
        assert_eq!(back.fetched[0].venue, "okx");

        std::fs::remove_dir_all(&dir).ok();
    }
}
