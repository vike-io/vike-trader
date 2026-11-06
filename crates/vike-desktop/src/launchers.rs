//! The launcher row: each tool's name, its bundled icon and the window kind a click opens.

use super::*;

/// vike's launcher row, left→right, with each tool's _DRAW icon name + (functional WinKind).
/// The bundled PNGs are rendered from Python's icons.py (exact line-art + TOOL_COLORS), except
/// chart, account, trade, polymarket and greeks: those five were redrawn in-house as 48x48 SVGs on
/// 2026-10-05 (two candles, a person, a bolt, a pie with a slice, a delta triangle) and rasterised
/// to transparent 8-bit RGBA PNGs of the same size.
pub(crate) const LAUNCHERS: &[(&str, &[u8], Option<workspace::WinKind>)] = &[
    ("chart", include_bytes!("../../../assets/icons/chart.png"), Some(workspace::WinKind::Chart)),
    // The old Trade slot opens the Account window (equity, accounts, working orders) and the old
    // DOM slot the new Trade window (spec §3.10, pre-flight C4). Each has its own icon; the name is
    // the hover text and the texture key.
    (
        "account",
        include_bytes!("../../../assets/icons/account.png"),
        Some(workspace::WinKind::Account),
    ),
    ("trade", include_bytes!("../../../assets/icons/trade.png"), Some(workspace::WinKind::Trade)),
    // Polymarket scalp cockpit — its own pie-with-a-slice icon.
    (
        "polymarket",
        include_bytes!("../../../assets/icons/polymarket.png"),
        Some(workspace::WinKind::Polymarket),
    ),
    (
        "studio",
        include_bytes!("../../../assets/icons/studio.png"),
        Some(workspace::WinKind::Studio),
    ),
    ("screener", include_bytes!("../../../assets/icons/screener.png"), None),
    // The journal-book glyph opens the Tearsheet: the execution history and the performance summary
    // over it, read from the datahub's store.
    (
        "journal",
        include_bytes!("../../../assets/icons/journal.png"),
        Some(workspace::WinKind::Tearsheet),
    ),
    ("alerts", include_bytes!("../../../assets/icons/alerts.png"), None),
    ("data", include_bytes!("../../../assets/icons/data.png"), Some(workspace::WinKind::Data)),
    // ⚠ REMOVED 2026-10-05 (final-review I3): the standalone Connections window
    // (`WinKind::Connections`) is deleted outright, and this row used to open `WinKind::Data` too
    // — a byte-identical duplicate of the "data" row above it, since the toolbar's generic
    // launcher dispatch (`app_ui.rs`'s `draw_caption`) resolves a click to a `WinKind` alone and
    // cannot tell two same-kind rows apart, so there was no way to land THIS one on
    // `DataDest::Gateway` without the other. No other row in this table duplicates another
    // row's kind; removing this one restores that convention rather than breaking it. The
    // `connections.png` icon asset is unreferenced now. The Trade ticket's Connect click, which
    // genuinely always means "go to Gateway" and has no such ambiguity (one call site, one
    // destination), keeps working — see `apply_trade_frame`'s `SpawnRequest::Kind`
    // `data_dest_seed`. `VIKE_SHOT_WIN=gateway-compact` / `VIKE_TOOL=connections` are UNRELATED QA/env
    // spellings resolved in `crate::ui::startup`/`crate::ui::initial_arrange`, not this table, and
    // this removal changes neither (see those modules for their own current behaviour).
    ("news", include_bytes!("../../../assets/icons/news.png"), Some(workspace::WinKind::News)),
    (
        "calendar",
        include_bytes!("../../../assets/icons/calendar.png"),
        Some(workspace::WinKind::Calendar),
    ),
    (
        "options",
        include_bytes!("../../../assets/icons/options.png"),
        Some(workspace::WinKind::Options),
    ),
    // Greeks: the delta triangle (the options row above keeps its own line-art).
    (
        "greeks",
        include_bytes!("../../../assets/icons/greeks.png"),
        Some(workspace::WinKind::Greeks),
    ),
];
