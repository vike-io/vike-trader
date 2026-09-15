//! The on-disk cache of the Enumerable catalog. Loaded instantly at startup; rewritten only on an
//! explicit user refresh (no auto/background re-fetch). QueryBacked venues are never cached.

use crate::{CatalogError, Instrument};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Per-venue refresh stamp shown in the Data Manager Catalog/Sources view.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VenueStamp {
    pub venue: String,
    pub last_refreshed_ms: i64,
    pub count: usize,
}

/// The serialized Enumerable catalog + per-venue stamps.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct CatalogCache {
    pub fetched: Vec<VenueStamp>,
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
    use crate::AssetClass;

    #[test]
    fn cache_roundtrips_through_disk() {
        let dir = std::env::temp_dir().join(format!("vikecat_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("catalog.json");
        let cache = CatalogCache {
            fetched: vec![VenueStamp { venue: "binance".into(), last_refreshed_ms: 111, count: 1 }],
            instruments: vec![crate::Instrument {
                venue: "binance".into(),
                raw_symbol: "BTCUSDT".into(),
                asset_class: AssetClass::CryptoSpot,
                base: "BTC".into(),
                quote: "USDT".into(),
                description: String::new(),
                properties: Default::default(),
            }],
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
            fetched: vec![VenueStamp { venue: "okx".into(), last_refreshed_ms: 222, count: 1 }],
            instruments: vec![crate::Instrument {
                venue: "okx".into(),
                raw_symbol: "BTC-USDT-SWAP".into(),
                asset_class: AssetClass::CryptoPerp,
                base: "BTC".into(),
                quote: "USDT".into(),
                description: String::new(),
                properties: Default::default(),
            }],
        };

        save_cache(&path, &cache).expect("save_cache should create missing parent dir");
        assert!(path.exists());

        let back = load_cache(&path).unwrap();
        assert_eq!(back.instruments.len(), 1);
        assert_eq!(back.fetched[0].venue, "okx");

        std::fs::remove_dir_all(&dir).ok();
    }
}
