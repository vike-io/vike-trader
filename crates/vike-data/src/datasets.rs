//! DataSets — named symbol collections (WealthLab's first-class concept), ported from vike's
//! `data/datasets.py`. A DataSet bundles symbols + a default provider/interval/benchmark so the
//! Data Manager can edit/test a whole universe. The Python app persists these in its SQLite store;
//! this clone has no DB, so we use a JSON file (`storage/datasets.json`) — the pragmatic spike
//! equivalent. Seeded with the same defaults (Crypto Majors / FX Majors) on first run.

use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct DataSet {
    pub name: String,
    pub symbols: Vec<String>,
    pub provider: String, // "Auto" | "binance" | "dukascopy" | …
    pub interval: String,
    pub benchmark: String,
    #[serde(default)]
    pub user: bool, // true = user-created (shows under "My DataSets")
}

#[derive(Default, Clone, Serialize, Deserialize)]
pub struct Store {
    pub sets: Vec<DataSet>,
}

const PATH: &str = "storage/datasets.json";

/// Split a free-text symbol blob (commas / whitespace / newlines) → upper, deduped, ordered.
pub fn parse_symbols(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tok in text.split(|c: char| c.is_whitespace() || c == ',' || c == ';') {
        let t = tok.trim().to_uppercase();
        if !t.is_empty() && !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

impl Store {
    pub fn load() -> Self {
        if let Ok(txt) = std::fs::read_to_string(PATH)
            && let Ok(s) = serde_json::from_str::<Store>(&txt)
            && !s.sets.is_empty()
        {
            return s;
        }
        Self::seed()
    }

    pub fn save(&self) {
        if let Some(parent) = std::path::Path::new(PATH).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(txt) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(PATH, txt);
        }
    }

    fn seed() -> Self {
        let mk = |sy: &[&str]| sy.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        Store {
            sets: vec![
                DataSet {
                    name: "Crypto Majors".into(),
                    symbols: mk(&[
                        "BTCUSDT", "ETHUSDT", "SOLUSDT", "BNBUSDT", "XRPUSDT", "ADAUSDT",
                        "DOGEUSDT", "AVAXUSDT",
                    ]),
                    provider: "binance".into(),
                    interval: "1m".into(),
                    benchmark: "BTCUSDT".into(),
                    user: false,
                },
                DataSet {
                    name: "FX Majors".into(),
                    symbols: mk(&[
                        "EURUSD", "GBPUSD", "USDJPY", "USDCHF", "AUDUSD", "USDCAD", "NZDUSD",
                    ]),
                    provider: "dukascopy".into(),
                    interval: "1h".into(),
                    benchmark: "".into(),
                    user: false,
                },
            ],
        }
    }

    pub fn get(&self, name: &str) -> Option<&DataSet> {
        self.sets.iter().find(|s| s.name == name)
    }

    pub fn upsert(&mut self, ds: DataSet) {
        if let Some(e) = self.sets.iter_mut().find(|s| s.name == ds.name) {
            *e = ds;
        } else {
            self.sets.push(ds);
        }
        self.save();
    }

    pub fn delete(&mut self, name: &str) {
        self.sets.retain(|s| s.name != name);
        self.save();
    }

    /// A fresh untitled user DataSet name that doesn't collide.
    pub fn fresh_name(&self) -> String {
        let mut n = 1;
        loop {
            let name = if n == 1 { "New DataSet".to_string() } else { format!("New DataSet {n}") };
            if self.get(&name).is_none() {
                return name;
            }
            n += 1;
        }
    }
}

/// Deterministic local "Ask the AI" suggester (no LLM key in this clone — vike routes through the
/// vike.io gateway). Parses a count + asset class from the prompt and returns symbols from a
/// built-in universe. Honest stand-in for the real LLM call.
pub fn suggest_symbols(prompt: &str) -> Vec<String> {
    let p = prompt.to_lowercase();
    let n = p
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|t| t.parse::<usize>().ok())
        .next()
        .unwrap_or(10)
        .clamp(1, 30);
    const CRYPTO: &[&str] = &[
        "BTCUSDT",
        "ETHUSDT",
        "SOLUSDT",
        "BNBUSDT",
        "XRPUSDT",
        "ADAUSDT",
        "DOGEUSDT",
        "AVAXUSDT",
        "LINKUSDT",
        "DOTUSDT",
        "MATICUSDT",
        "LTCUSDT",
        "TRXUSDT",
        "ATOMUSDT",
        "UNIUSDT",
        "APTUSDT",
        "ARBUSDT",
        "OPUSDT",
        "NEARUSDT",
        "FILUSDT",
    ];
    const FOREX: &[&str] = &[
        "EURUSD", "GBPUSD", "USDJPY", "USDCHF", "AUDUSD", "USDCAD", "NZDUSD", "EURGBP", "EURJPY",
        "GBPJPY", "AUDJPY", "EURCHF",
    ];
    const STOCKS: &[&str] = &[
        "AAPL", "MSFT", "NVDA", "AMZN", "GOOGL", "META", "TSLA", "AMD", "NFLX", "JPM", "V", "WMT",
        "XOM", "UNH", "MA",
    ];
    let universe: &[&str] = if p.contains("forex") || p.contains("fx") || p.contains("currenc") {
        FOREX
    } else if p.contains("stock")
        || p.contains("equit")
        || p.contains("nasdaq")
        || p.contains("s&p")
    {
        STOCKS
    } else {
        CRYPTO
    };
    universe.iter().take(n).map(|s| s.to_string()).collect()
}
