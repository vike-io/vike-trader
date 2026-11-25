//! The GUI's appearance words: what the `preferences.theme`, `preferences.market_colors`,
//! `preferences.density` and `preferences.text_size` settings rows store (design system spec §3.1–§3.4,
//! §5). ONE spelling for the two crates that read them: the settings loader (`vike-config`, layer 20),
//! which refuses a word outside these lists, and the design system (`vike-ui-theme`, layer 70), whose
//! `ThemePaletteId`, `ThemeMarketColorsId`, `ThemeDensity` and `ThemeTextSize` key each option by its
//! place here (`key` is the word at the option's place in its `ALL`, which is its declaration
//! order). Neither crate can name the other's lists the other way round, so they live below both.
//!
//! ⚠ **A word added to one of these lists later is a STORE-FORMAT change for every older binary.**
//! The loader refuses a word it does not know (`vike-config`'s `one_of`), exactly as it refuses a key
//! it does not know, so once a newer build writes its new word, an older build reading the same store
//! refuses the store rather than falling back to a default. That is deliberate — a word the GUI
//! cannot show would otherwise display as effective in `config show` — and it holds for the BACKEND's
//! store too: an appearance row written there (these keys are editable in Connections → Backend
//! settings) stops an older daemon's boot after a rollback, although no daemon reads it. Rebuild every
//! binary that reads a store before writing a new word into it. A word's SPELLING is a stored value,
//! and changing it strands every row written before.

/// `preferences.theme`'s words: the four themes, in `ThemePaletteId::ALL` order.
pub const THEMES: [&str; 4] = ["graphite", "midnight", "dusk", "carbon"];
/// `preferences.market_colors`'s words: the four market-colour sets, in `ThemeMarketColorsId::ALL`
/// order.
pub const MARKET_COLOR_SETS: [&str; 4] = ["classic", "tradingview", "exchange", "colorblind"];
/// `preferences.density`'s words, in `ThemeDensity::ALL` order.
pub const DENSITIES: [&str; 3] = ["compact", "normal", "comfortable"];
/// `preferences.text_size`'s words, in `ThemeTextSize::ALL` order. Three since 2026-10-05: `small` is
/// the scale `standard` used to be, `standard` (still the default) is the one `large` used to be, and
/// `large` is new. A store row written before then keeps its word and means the next scale up for
/// `standard` and `large` alike.
pub const TEXT_SIZES: [&str; 3] = ["small", "standard", "large"];
