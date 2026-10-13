// @generated from crates/vike-ui-theme/ui-theme.toml by crates/vike-ui-theme/tests/tokens_gen — do not edit; change the TOML and regenerate (see tests/brand_assets.rs).

/// Button: the `[[map]]` rows of `ui-theme.toml` whose `map` is `button`.
pub mod button {
    /// The one action a view exists for (Save, Apply, Run; the strip's Place): the fill in the theme's accent, no outline, the label in the on-fill black; under the pointer the fill is lifted in code a step toward the text colour.
    pub const PRIMARY: super::MapRow = super::MapRow { key: "PRIMARY", colour: super::ColourRole::None, fill: super::ColourRole::Accent, stroke: super::ColourRole::None, text: super::ColourRole::OnFill, word: None, icon: None, count: None, flag: None, doc: "The one action a view exists for (Save, Apply, Run; the strip's Place): the fill in the theme's accent, no outline, the label in the on-fill black; under the pointer the fill is lifted in code a step toward the text colour." };
    /// Everything else (Refresh, Cancel), and the Trade window's plain button: the surface fill, an outline in the border grey, the label in the UI text; under the pointer the fill is the theme's hover fill.
    pub const SECONDARY: super::MapRow = super::MapRow { key: "SECONDARY", colour: super::ColourRole::Hover, fill: super::ColourRole::Surface, stroke: super::ColourRole::Border, text: super::ColourRole::TextUi, word: None, icon: None, count: None, flag: None, doc: "Everything else (Refresh, Cancel), and the Trade window's plain button: the surface fill, an outline in the border grey, the label in the UI text; under the pointer the fill is the theme's hover fill." };
    /// Buy, also the Trade window's tall Buy: the fill in the market set's up colour, no outline, the label in the on-fill black; under the pointer the fill is lifted in code a step toward the text colour.
    pub const BUY: super::MapRow = super::MapRow { key: "BUY", colour: super::ColourRole::None, fill: super::ColourRole::Up, stroke: super::ColourRole::None, text: super::ColourRole::OnFill, word: None, icon: None, count: None, flag: None, doc: "Buy, also the Trade window's tall Buy: the fill in the market set's up colour, no outline, the label in the on-fill black; under the pointer the fill is lifted in code a step toward the text colour." };
    /// Sell, also the Trade window's tall Sell: the fill in the market set's down colour, no outline, the label in the on-fill black; under the pointer the fill is lifted in code a step toward the text colour.
    pub const SELL: super::MapRow = super::MapRow { key: "SELL", colour: super::ColourRole::None, fill: super::ColourRole::Down, stroke: super::ColourRole::None, text: super::ColourRole::OnFill, word: None, icon: None, count: None, flag: None, doc: "Sell, also the Trade window's tall Sell: the fill in the market set's down colour, no outline, the label in the on-fill black; under the pointer the fill is lifted in code a step toward the text colour." };
    /// An action that destroys something (Delete): the fill in the status red, no outline, the label in the on-fill black; under the pointer the fill is lifted in code a step toward the text colour.
    pub const DANGER: super::MapRow = super::MapRow { key: "DANGER", colour: super::ColourRole::None, fill: super::ColourRole::Error, stroke: super::ColourRole::None, text: super::ColourRole::OnFill, word: None, icon: None, count: None, flag: None, doc: "An action that destroys something (Delete): the fill in the status red, no outline, the label in the on-fill black; under the pointer the fill is lifted in code a step toward the text colour." };
    /// The Trade window's chosen button (a chosen order type, Reduce only while on, the one-click padlock while one-click is on): the card fill ringed in the theme's accent, the label in the full text colour; it does not change under the pointer.
    pub const CHOSEN: super::MapRow = super::MapRow { key: "CHOSEN", colour: super::ColourRole::None, fill: super::ColourRole::Card, stroke: super::ColourRole::Accent, text: super::ColourRole::Text, word: None, icon: None, count: None, flag: None, doc: "The Trade window's chosen button (a chosen order type, Reduce only while on, the one-click padlock while one-click is on): the card fill ringed in the theme's accent, the label in the full text colour; it does not change under the pointer." };

    /// Every row of this map, in the order the TOML lists them.
    pub const ALL: &[&super::MapRow] = &[&PRIMARY, &SECONDARY, &BUY, &SELL, &DANGER, &CHOSEN];
}

/// Callput: the `[[map]]` rows of `ui-theme.toml` whose `map` is `callput`.
pub mod callput {
    /// A call on the options board: the CALLS header, the call volume bars and the working-order marker, in the series blue, which is #57A5FF, exactly the info status's value (a series colour, not a status).
    pub const CALL: super::MapRow = super::MapRow { key: "CALL", colour: super::ColourRole::Info, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::Info, word: None, icon: None, count: None, flag: None, doc: "A call on the options board: the CALLS header, the call volume bars and the working-order marker, in the series blue, which is #57A5FF, exactly the info status's value (a series colour, not a status)." };
    /// A put on the options board: the PUTS header in the market's down text and the put volume bars in its down.
    pub const PUT: super::MapRow = super::MapRow { key: "PUT", colour: super::ColourRole::Down, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::DownText, word: None, icon: None, count: None, flag: None, doc: "A put on the options board: the PUTS header in the market's down text and the put volume bars in its down." };

    /// Every row of this map, in the order the TOML lists them.
    pub const ALL: &[&super::MapRow] = &[&CALL, &PUT];
}

/// Chart Role: the `[[map]]` rows of `ui-theme.toml` whose `map` is `chart_role`.
pub mod chart_role {
    /// Candle bodies, and every mark a bar's direction colours (bars, columns, Kagi, PnF): the market set's up, unless the user picked one for this chart.
    pub const UP: super::MapRow = super::MapRow { key: "UP", colour: super::ColourRole::Up, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "Candle bodies, and every mark a bar's direction colours (bars, columns, Kagi, PnF): the market set's up, unless the user picked one for this chart." };
    /// The same marks going down: the market set's down, unless the user picked one for this chart.
    pub const DOWN: super::MapRow = super::MapRow { key: "DOWN", colour: super::ColourRole::Down, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The same marks going down: the market set's down, unless the user picked one for this chart." };
    /// A rising candle's body border: the market set's up, unless the user picked a border colour or edited the body (the border then follows the body).
    pub const BORDER_UP: super::MapRow = super::MapRow { key: "BORDER_UP", colour: super::ColourRole::Up, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "A rising candle's body border: the market set's up, unless the user picked a border colour or edited the body (the border then follows the body)." };
    /// A falling candle's body border: the market set's down, unless the user picked a border colour or edited the body (the border then follows the body).
    pub const BORDER_DOWN: super::MapRow = super::MapRow { key: "BORDER_DOWN", colour: super::ColourRole::Down, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "A falling candle's body border: the market set's down, unless the user picked a border colour or edited the body (the border then follows the body)." };
    /// A rising candle's wick: the market set's up, unless the user picked a wick colour or edited the body (the wick then follows the body).
    pub const WICK_UP: super::MapRow = super::MapRow { key: "WICK_UP", colour: super::ColourRole::Up, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "A rising candle's wick: the market set's up, unless the user picked a wick colour or edited the body (the wick then follows the body)." };
    /// A falling candle's wick: the market set's down, unless the user picked a wick colour or edited the body (the wick then follows the body).
    pub const WICK_DOWN: super::MapRow = super::MapRow { key: "WICK_DOWN", colour: super::ColourRole::Down, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "A falling candle's wick: the market set's down, unless the user picked a wick colour or edited the body (the wick then follows the body)." };
    /// Up as a graphic away from the candles (histograms, markers, the baseline style, the last-price line, footprint tints): the market set's up, unless the user picked one.
    pub const UP_S: super::MapRow = super::MapRow { key: "UP_S", colour: super::ColourRole::Up, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "Up as a graphic away from the candles (histograms, markers, the baseline style, the last-price line, footprint tints): the market set's up, unless the user picked one." };
    /// Down as a graphic away from the candles: the market set's down, unless the user picked one.
    pub const DOWN_S: super::MapRow = super::MapRow { key: "DOWN_S", colour: super::ColourRole::Down, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "Down as a graphic away from the candles: the market set's down, unless the user picked one." };
    /// Up as text (the OHLC legend's numbers and change): the market set's up-as-text, unless the user picked a semantic up.
    pub const UP_TEXT: super::MapRow = super::MapRow { key: "UP_TEXT", colour: super::ColourRole::UpText, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "Up as text (the OHLC legend's numbers and change): the market set's up-as-text, unless the user picked a semantic up." };
    /// Down as text (the OHLC legend's numbers and change): the market set's down-as-text, unless the user picked a semantic down.
    pub const DOWN_TEXT: super::MapRow = super::MapRow { key: "DOWN_TEXT", colour: super::ColourRole::DownText, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "Down as text (the OHLC legend's numbers and change): the market set's down-as-text, unless the user picked a semantic down." };
    /// Up volume bars: the market set's up faded to the volume pane's strength (the fade stays in code), or the user's semantic up faded the same way.
    pub const UP_VOLUME: super::MapRow = super::MapRow { key: "UP_VOLUME", colour: super::ColourRole::Up, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "Up volume bars: the market set's up faded to the volume pane's strength (the fade stays in code), or the user's semantic up faded the same way." };
    /// Down volume bars: the market set's down faded to the volume pane's strength (the fade stays in code), or the user's semantic down faded the same way.
    pub const DOWN_VOLUME: super::MapRow = super::MapRow { key: "DOWN_VOLUME", colour: super::ColourRole::Down, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "Down volume bars: the market set's down faded to the volume pane's strength (the fade stays in code), or the user's semantic down faded the same way." };
    /// Line, area and step series: the market set's up, unless the user picked one.
    pub const LINE: super::MapRow = super::MapRow { key: "LINE", colour: super::ColourRole::Up, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "Line, area and step series: the market set's up, unless the user picked one." };
    /// The chart's grid lines: the theme's hover grey, unless the user picked one.
    pub const GRID: super::MapRow = super::MapRow { key: "GRID", colour: super::ColourRole::Hover, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The chart's grid lines: the theme's hover grey, unless the user picked one." };
    /// The crosshair's lines: the theme's secondary text, unless the user picked one.
    pub const CROSS: super::MapRow = super::MapRow { key: "CROSS", colour: super::ColourRole::Text2, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The crosshair's lines: the theme's secondary text, unless the user picked one." };
    /// The canvas: the theme's background, unless the user picked one.
    pub const BG: super::MapRow = super::MapRow { key: "BG", colour: super::ColourRole::Bg, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The canvas: the theme's background, unless the user picked one." };
    /// The top of the canvas's gradient, when the gradient is on: the theme's gradient top, unless the user picked one.
    pub const BG_TOP: super::MapRow = super::MapRow { key: "BG_TOP", colour: super::ColourRole::GradTop, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The top of the canvas's gradient, when the gradient is on: the theme's gradient top, unless the user picked one." };
    /// Words on the canvas (the OHLC legend's letters, the crosshair tags' text): the theme's primary text.
    pub const TEXT: super::MapRow = super::MapRow { key: "TEXT", colour: super::ColourRole::Text, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "Words on the canvas (the OHLC legend's letters, the crosshair tags' text): the theme's primary text." };
    /// Pane names: the theme's secondary text.
    pub const TEXT2: super::MapRow = super::MapRow { key: "TEXT2", colour: super::ColourRole::Text2, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "Pane names: the theme's secondary text." };
    /// Axis labels: the theme's caption grey.
    pub const AXIS: super::MapRow = super::MapRow { key: "AXIS", colour: super::ColourRole::Text3, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "Axis labels: the theme's caption grey." };
    /// The crosshair tags' fill: the theme's border grey.
    pub const TAG_BG: super::MapRow = super::MapRow { key: "TAG_BG", colour: super::ColourRole::Border, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The crosshair tags' fill: the theme's border grey." };
    /// The line between two panes: the theme's border grey.
    pub const DIVIDER: super::MapRow = super::MapRow { key: "DIVIDER", colour: super::ColourRole::Border, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The line between two panes: the theme's border grey." };
    /// That line under the pointer: the theme's analysis line.
    pub const DIVIDER_HOVER: super::MapRow = super::MapRow { key: "DIVIDER_HOVER", colour: super::ColourRole::AnalysisLine, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "That line under the pointer: the theme's analysis line." };
    /// The line between plus and minus (the CVD zero line, the baseline style's anchor): the theme's analysis line.
    pub const ZERO_LINE: super::MapRow = super::MapRow { key: "ZERO_LINE", colour: super::ColourRole::AnalysisLine, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The line between plus and minus (the CVD zero line, the baseline style's anchor): the theme's analysis line." };
    /// An oscillator's reference level (RSI 30/50/70, Stoch 20/80) the user has not coloured: the theme's analysis line.
    pub const LEVEL_LINE: super::MapRow = super::MapRow { key: "LEVEL_LINE", colour: super::ColourRole::AnalysisLine, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "An oscillator's reference level (RSI 30/50/70, Stoch 20/80) the user has not coloured: the theme's analysis line." };
    /// Text on a chip filled with a market or series colour: the kit's black, which every design fill takes (a fill a user picked darker takes the theme's text instead, in code).
    pub const ON_FILL: super::MapRow = super::MapRow { key: "ON_FILL", colour: super::ColourRole::OnFill, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "Text on a chip filled with a market or series colour: the kit's black, which every design fill takes (a fill a user picked darker takes the theme's text instead, in code)." };

    /// Every row of this map, in the order the TOML lists them.
    pub const ALL: &[&super::MapRow] = &[&UP, &DOWN, &BORDER_UP, &BORDER_DOWN, &WICK_UP, &WICK_DOWN, &UP_S, &DOWN_S, &UP_TEXT, &DOWN_TEXT, &UP_VOLUME, &DOWN_VOLUME, &LINE, &GRID, &CROSS, &BG, &BG_TOP, &TEXT, &TEXT2, &AXIS, &TAG_BG, &DIVIDER, &DIVIDER_HOVER, &ZERO_LINE, &LEVEL_LINE, &ON_FILL];
}

/// Connection: the `[[map]]` rows of `ui-theme.toml` whose `map` is `connection`.
pub mod connection {
    /// A live feed: the status strip's dot and the Connections tool's Status cell in the status green, the cell reading Connected.
    pub const CONNECTED: super::MapRow = super::MapRow { key: "CONNECTED", colour: super::ColourRole::Ok, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("Connected"), icon: None, count: None, flag: None, doc: "A live feed: the status strip's dot and the Connections tool's Status cell in the status green, the cell reading Connected." };
    /// A feed that is dialling: the dot and the cell in the status amber, the cell reading Connecting.
    pub const CONNECTING: super::MapRow = super::MapRow { key: "CONNECTING", colour: super::ColourRole::Warning, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("Connecting"), icon: None, count: None, flag: None, doc: "A feed that is dialling: the dot and the cell in the status amber, the cell reading Connecting." };
    /// A feed that is down or idle: the dot and the cell in the status grey, the cell reading Disconnected (the Trade window's FEED badge paints this same state red, in the feed_badge map).
    pub const DISCONNECTED: super::MapRow = super::MapRow { key: "DISCONNECTED", colour: super::ColourRole::Muted, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("Disconnected"), icon: None, count: None, flag: None, doc: "A feed that is down or idle: the dot and the cell in the status grey, the cell reading Disconnected (the Trade window's FEED badge paints this same state red, in the feed_badge map)." };
    /// A feed that reported a fault: the dot and the cell in the status red, the cell reading Error.
    pub const ERROR: super::MapRow = super::MapRow { key: "ERROR", colour: super::ColourRole::Error, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("Error"), icon: None, count: None, flag: None, doc: "A feed that reported a fault: the dot and the cell in the status red, the cell reading Error." };
    /// A feed whose producer has not said anything yet: the dot and the cell in the status grey, the cell saying the producer has not reported.
    pub const UNKNOWN: super::MapRow = super::MapRow { key: "UNKNOWN", colour: super::ColourRole::Muted, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("Unknown (producer has not reported)"), icon: None, count: None, flag: None, doc: "A feed whose producer has not said anything yet: the dot and the cell in the status grey, the cell saying the producer has not reported." };
    /// A venue that nothing in this build produces a feed status for (the Connections tool's cell only, never a state the strip's dot has): the cell in the status grey, saying so.
    pub const NO_PRODUCER: super::MapRow = super::MapRow { key: "NO_PRODUCER", colour: super::ColourRole::Muted, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("no feed producer in this build"), icon: None, count: None, flag: None, doc: "A venue that nothing in this build produces a feed status for (the Connections tool's cell only, never a state the strip's dot has): the cell in the status grey, saying so." };

    /// Every row of this map, in the order the TOML lists them.
    pub const ALL: &[&super::MapRow] = &[&CONNECTED, &CONNECTING, &DISCONNECTED, &ERROR, &UNKNOWN, &NO_PRODUCER];
}

/// Control Link: the `[[map]]` rows of `ui-theme.toml` whose `map` is `control_link`.
pub mod control_link {
    /// The write channel is up: the segment's dot and words in the loud status amber and the words led by the warning icon, because this observer can place real orders on the daemon.
    pub const CONNECTED: super::MapRow = super::MapRow { key: "CONNECTED", colour: super::ColourRole::Warning, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: Some("WARNING"), count: None, flag: None, doc: "The write channel is up: the segment's dot and words in the loud status amber and the words led by the warning icon, because this observer can place real orders on the daemon." };
    /// The write channel is down: the segment's dot and words in the status red, with no icon.
    pub const DISCONNECTED: super::MapRow = super::MapRow { key: "DISCONNECTED", colour: super::ColourRole::Error, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The write channel is down: the segment's dot and words in the status red, with no icon." };

    /// Every row of this map, in the order the TOML lists them.
    pub const ALL: &[&super::MapRow] = &[&CONNECTED, &DISCONNECTED];
}

/// Coverage: the `[[map]]` rows of `ui-theme.toml` whose `map` is `coverage`.
pub mod coverage {
    /// The empty track behind a series' span: the theme's background.
    pub const TRACK: super::MapRow = super::MapRow { key: "TRACK", colour: super::ColourRole::Bg, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The empty track behind a series' span: the theme's background." };
    /// The span a series covers: the status blue, its legend word covered.
    pub const COVERED: super::MapRow = super::MapRow { key: "COVERED", colour: super::ColourRole::Info, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("covered"), icon: None, count: None, flag: None, doc: "The span a series covers: the status blue, its legend word covered." };
    /// A span whose last row lags past the stale threshold: the status amber, its legend word stale; the Updated column's date takes the same amber.
    pub const STALE: super::MapRow = super::MapRow { key: "STALE", colour: super::ColourRole::Warning, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("stale"), icon: None, count: None, flag: None, doc: "A span whose last row lags past the stale threshold: the status amber, its legend word stale; the Updated column's date takes the same amber." };
    /// A missing-day range cut out of a span: the track's own colour, the theme's background, so it reads as a hole.
    pub const GAP: super::MapRow = super::MapRow { key: "GAP", colour: super::ColourRole::Bg, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "A missing-day range cut out of a span: the track's own colour, the theme's background, so it reads as a hole." };
    /// An instrument with a partial day in some other kind of data: the warning icon in the status amber, in the grid's Partial column and in the legend, which says partial day.
    pub const PARTIAL_DAY: super::MapRow = super::MapRow { key: "PARTIAL_DAY", colour: super::ColourRole::Warning, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("partial day"), icon: Some("WARNING"), count: None, flag: None, doc: "An instrument with a partial day in some other kind of data: the warning icon in the status amber, in the grid's Partial column and in the legend, which says partial day." };

    /// Every row of this map, in the order the TOML lists them.
    pub const ALL: &[&super::MapRow] = &[&TRACK, &COVERED, &STALE, &GAP, &PARTIAL_DAY];
}

/// Data Dest: the `[[map]]` rows of `ui-theme.toml` whose `map` is `data_dest`.
pub mod data_dest {
    /// What needs attention, the window's landing screen: the label Overview and the four-squares glyph beside it in the rail.
    pub const OVERVIEW: super::MapRow = super::MapRow { key: "OVERVIEW", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("Overview"), icon: Some("OVERVIEW"), count: None, flag: None, doc: "What needs attention, the window's landing screen: the label Overview and the four-squares glyph beside it in the rail." };
    /// The stored inventory grid: the label All series and the table glyph beside it in the rail.
    pub const ALL_SERIES: super::MapRow = super::MapRow { key: "ALL_SERIES", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("All series"), icon: Some("ALL_SERIES"), count: None, flag: None, doc: "The stored inventory grid: the label All series and the table glyph beside it in the rail." };
    /// The stored grid filtered to series with a hole in their timeline: the label Has gaps and the crossed-calendar glyph beside it in the rail (the same glyph leads the foot line's gap count).
    pub const HAS_GAPS: super::MapRow = super::MapRow { key: "HAS_GAPS", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("Has gaps"), icon: Some("HAS_GAPS"), count: None, flag: None, doc: "The stored grid filtered to series with a hole in their timeline: the label Has gaps and the crossed-calendar glyph beside it in the rail (the same glyph leads the foot line's gap count)." };
    /// The stored grid filtered to series that stopped updating: the label Stale and the hourglass glyph beside it in the rail.
    pub const STALE: super::MapRow = super::MapRow { key: "STALE", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("Stale"), icon: Some("STALE"), count: None, flag: None, doc: "The stored grid filtered to series that stopped updating: the label Stale and the hourglass glyph beside it in the rail." };
    /// One row per venue, rollups only: the label By venue and the buildings glyph beside it in the rail.
    pub const BY_VENUE: super::MapRow = super::MapRow { key: "BY_VENUE", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("By venue"), icon: Some("BY_VENUE"), count: None, flag: None, doc: "One row per venue, rollups only: the label By venue and the buildings glyph beside it in the rail." };
    /// The live bar feeds held in memory: the label Cached feeds and the broadcast glyph beside it in the rail.
    pub const CACHED_FEEDS: super::MapRow = super::MapRow { key: "CACHED_FEEDS", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("Cached feeds"), icon: Some("CACHED_FEEDS"), count: None, flag: None, doc: "The live bar feeds held in memory: the label Cached feeds and the broadcast glyph beside it in the rail." };
    /// Where backfilled data can come from: the label Providers and the cloud-download glyph beside it in the rail.
    pub const PROVIDERS: super::MapRow = super::MapRow { key: "PROVIDERS", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("Providers"), icon: Some("PROVIDERS"), count: None, flag: None, doc: "Where backfilled data can come from: the label Providers and the cloud-download glyph beside it in the rail." };
    /// What the Data Manager did this session, newest first: the label Activity log and the scroll glyph beside it in the rail.
    pub const ACTIVITY_LOG: super::MapRow = super::MapRow { key: "ACTIVITY_LOG", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("Activity log"), icon: Some("ACTIVITY_LOG"), count: None, flag: None, doc: "What the Data Manager did this session, newest first: the label Activity log and the scroll glyph beside it in the rail." };
    /// The editor of saved symbol universes: the label DataSets and the stack glyph beside it in the rail.
    pub const DATA_SETS: super::MapRow = super::MapRow { key: "DATA_SETS", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("DataSets"), icon: Some("DATASETS"), count: None, flag: None, doc: "The editor of saved symbol universes: the label DataSets and the stack glyph beside it in the rail." };
    /// Per-venue credential presence, the ceiling and what the mount did — folded in from the standalone Connections window's Credentials tab and the old Venues destination it replaces: the label Credentials and the key glyph beside it in the rail.
    pub const CREDENTIALS: super::MapRow = super::MapRow { key: "CREDENTIALS", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("Credentials"), icon: Some("CREDENTIALS"), count: None, flag: None, doc: "Per-venue credential presence, the ceiling and what the mount did — folded in from the standalone Connections window's Credentials tab and the old Venues destination it replaces: the label Credentials and the key glyph beside it in the rail." };
    /// Which backend box this app observes, and its connection state — folded in from the standalone Connections window's Backend tab: the label Backend and the plug glyph beside it in the rail.
    pub const BACKEND: super::MapRow = super::MapRow { key: "BACKEND", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("Backend"), icon: Some("BACKEND"), count: None, flag: None, doc: "Which backend box this app observes, and its connection state — folded in from the standalone Connections window's Backend tab: the label Backend and the plug glyph beside it in the rail." };
    /// The cross-venue instrument catalogue the symbol picker searches: the label Instruments and the list-search glyph beside it in the rail.
    pub const INSTRUMENTS: super::MapRow = super::MapRow { key: "INSTRUMENTS", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("Instruments"), icon: Some("INSTRUMENTS"), count: None, flag: None, doc: "The cross-venue instrument catalogue the symbol picker searches: the label Instruments and the list-search glyph beside it in the rail." };
    /// Which stores are mounted, what each can do and the layout on disk: the label Store and the hard-drives glyph beside it in the rail.
    pub const STORE: super::MapRow = super::MapRow { key: "STORE", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("Store"), icon: Some("STORE"), count: None, flag: None, doc: "Which stores are mounted, what each can do and the layout on disk: the label Store and the hard-drives glyph beside it in the rail." };

    /// Every row of this map, in the order the TOML lists them.
    pub const ALL: &[&super::MapRow] = &[&OVERVIEW, &ALL_SERIES, &HAS_GAPS, &STALE, &BY_VENUE, &CACHED_FEEDS, &PROVIDERS, &ACTIVITY_LOG, &DATA_SETS, &CREDENTIALS, &BACKEND, &INSTRUMENTS, &STORE];
}

/// Empty Pane: the `[[map]]` rows of `ui-theme.toml` whose `map` is `empty_pane`.
pub mod empty_pane {
    /// Still asking: a spinner in the caption grey and the pane's words in the caption grey (a spinner is no icon of the registry).
    pub const LOADING: super::MapRow = super::MapRow { key: "LOADING", colour: super::ColourRole::Text3, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::Text3, word: None, icon: None, count: None, flag: None, doc: "Still asking: a spinner in the caption grey and the pane's words in the caption grey (a spinner is no icon of the registry)." };
    /// Asked and got nothing: the empty tray in the caption grey and the pane's words in the secondary text; a bookless ladder whose link is up or not known shows the same tray, named empty.
    pub const EMPTY: super::MapRow = super::MapRow { key: "EMPTY", colour: super::ColourRole::Text3, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::Text2, word: Some("empty"), icon: Some("EMPTY"), count: None, flag: None, doc: "Asked and got nothing: the empty tray in the caption grey and the pane's words in the secondary text; a bookless ladder whose link is up or not known shows the same tray, named empty." };
    /// Could not ask: the unreachable cloud in the status amber and the pane's words in the primary text; a bookless ladder whose link is down shows the same cloud, named unreachable.
    pub const UNREACHABLE: super::MapRow = super::MapRow { key: "UNREACHABLE", colour: super::ColourRole::Warning, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::Text, word: Some("unreachable"), icon: Some("UNREACHABLE"), count: None, flag: None, doc: "Could not ask: the unreachable cloud in the status amber and the pane's words in the primary text; a bookless ladder whose link is down shows the same cloud, named unreachable." };

    /// Every row of this map, in the order the TOML lists them.
    pub const ALL: &[&super::MapRow] = &[&LOADING, &EMPTY, &UNREACHABLE];
}

/// Feed Badge: the `[[map]]` rows of `ui-theme.toml` whose `map` is `feed_badge`.
pub mod feed_badge {
    /// The depth link is live: the badge outlined in the status green with its words in the same green.
    pub const CONNECTED: super::MapRow = super::MapRow { key: "CONNECTED", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::Ok, text: super::ColourRole::Ok, word: Some("FEED UP"), icon: None, count: None, flag: None, doc: "The depth link is live: the badge outlined in the status green with its words in the same green." };
    /// The depth link is dialling: the badge outlined in the status amber with its words in the same amber.
    pub const CONNECTING: super::MapRow = super::MapRow { key: "CONNECTING", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::Warning, text: super::ColourRole::Warning, word: Some("FEED DIALLING"), icon: None, count: None, flag: None, doc: "The depth link is dialling: the badge outlined in the status amber with its words in the same amber." };
    /// The depth link is down: the badge outlined in the status red with its words in the same red (owner decision 4, 2026-09-29: a dead link is the alarm, where the status strip's wordless dot is grey).
    pub const DISCONNECTED: super::MapRow = super::MapRow { key: "DISCONNECTED", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::Error, text: super::ColourRole::Error, word: Some("FEED DOWN"), icon: None, count: None, flag: None, doc: "The depth link is down: the badge outlined in the status red with its words in the same red (owner decision 4, 2026-09-29: a dead link is the alarm, where the status strip's wordless dot is grey)." };
    /// The depth link reported a fault: the badge outlined in the status red with its words in the same red.
    pub const ERROR: super::MapRow = super::MapRow { key: "ERROR", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::Error, text: super::ColourRole::Error, word: Some("FEED FAULT"), icon: None, count: None, flag: None, doc: "The depth link reported a fault: the badge outlined in the status red with its words in the same red." };
    /// Nobody reported on the depth link: the badge outlined in the status grey with its words in the caption grey, because the status grey is under the 4.5:1 text floor as ink.
    pub const UNKNOWN: super::MapRow = super::MapRow { key: "UNKNOWN", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::Muted, text: super::ColourRole::Text3, word: Some("FEED ?"), icon: None, count: None, flag: None, doc: "Nobody reported on the depth link: the badge outlined in the status grey with its words in the caption grey, because the status grey is under the 4.5:1 text floor as ink." };

    /// Every row of this map, in the order the TOML lists them.
    pub const ALL: &[&super::MapRow] = &[&CONNECTED, &CONNECTING, &DISCONNECTED, &ERROR, &UNKNOWN];
}

/// Importance: the `[[map]]` rows of `ui-theme.toml` whose `map` is `importance`.
pub mod importance {
    /// A high-importance calendar event: all three bars lit, in the status red, the glyph's unlit part in the theme's border.
    pub const HIGH: super::MapRow = super::MapRow { key: "HIGH", colour: super::ColourRole::Error, fill: super::ColourRole::Border, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: Some(3), flag: None, doc: "A high-importance calendar event: all three bars lit, in the status red, the glyph's unlit part in the theme's border." };
    /// A medium-importance calendar event: two bars lit, in the status amber, the third in the theme's border.
    pub const MEDIUM: super::MapRow = super::MapRow { key: "MEDIUM", colour: super::ColourRole::Warning, fill: super::ColourRole::Border, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: Some(2), flag: None, doc: "A medium-importance calendar event: two bars lit, in the status amber, the third in the theme's border." };
    /// Any other calendar event (low, holiday, no rating): one bar lit, in the theme's caption grey, the other two in its border.
    pub const OTHER: super::MapRow = super::MapRow { key: "OTHER", colour: super::ColourRole::Text3, fill: super::ColourRole::Border, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: Some(1), flag: None, doc: "Any other calendar event (low, holiday, no rating): one bar lit, in the theme's caption grey, the other two in its border." };

    /// Every row of this map, in the order the TOML lists them.
    pub const ALL: &[&super::MapRow] = &[&HIGH, &MEDIUM, &OTHER];
}

/// Key Presence: the `[[map]]` rows of `ui-theme.toml` whose `map` is `key_presence`.
pub mod key_presence {
    /// No name typed yet: nothing is drawn beside the field.
    pub const BLANK: super::MapRow = super::MapRow { key: "BLANK", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "No name typed yet: nothing is drawn beside the field." };
    /// The typed key name is in the credential store: the check icon and in store, in the weak text colour.
    pub const PRESENT: super::MapRow = super::MapRow { key: "PRESENT", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("in store"), icon: Some("CHECK"), count: None, flag: None, doc: "The typed key name is in the credential store: the check icon and in store, in the weak text colour." };
    /// The typed key name is not in the credential store, which is likely a typo: the warning icon and not in store, in the weak text colour.
    pub const ABSENT: super::MapRow = super::MapRow { key: "ABSENT", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("not in store"), icon: Some("WARNING"), count: None, flag: None, doc: "The typed key name is not in the credential store, which is likely a typo: the warning icon and not in store, in the weak text colour." };

    /// Every row of this map, in the order the TOML lists them.
    pub const ALL: &[&super::MapRow] = &[&BLANK, &PRESENT, &ABSENT];
}

/// Order Pill: the `[[map]]` rows of `ui-theme.toml` whose `map` is `order_pill`.
pub mod order_pill {
    /// A resting Buy limit order: the fill in the market set's up colour at PILL_FILL_ALPHA, a solid one-point edge in the same at PILL_EDGE_ALPHA, the words in the up text colour.
    pub const BUY_LIMIT: super::MapRow = super::MapRow { key: "BUY_LIMIT", colour: super::ColourRole::None, fill: super::ColourRole::Up, stroke: super::ColourRole::Up, text: super::ColourRole::UpText, word: None, icon: None, count: None, flag: Some(false), doc: "A resting Buy limit order: the fill in the market set's up colour at PILL_FILL_ALPHA, a solid one-point edge in the same at PILL_EDGE_ALPHA, the words in the up text colour." };
    /// A resting Buy stop order: hollow, a DASHED one-point edge in the market set's up colour at full strength (the v3 design's stop pill), the words in the up text colour.
    pub const BUY_STOP: super::MapRow = super::MapRow { key: "BUY_STOP", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::Up, text: super::ColourRole::UpText, word: None, icon: None, count: None, flag: Some(true), doc: "A resting Buy stop order: hollow, a DASHED one-point edge in the market set's up colour at full strength (the v3 design's stop pill), the words in the up text colour." };
    /// A resting Sell limit order: the fill in the market set's down colour at PILL_FILL_ALPHA, a solid one-point edge in the same at PILL_EDGE_ALPHA, the words in the down text colour.
    pub const SELL_LIMIT: super::MapRow = super::MapRow { key: "SELL_LIMIT", colour: super::ColourRole::None, fill: super::ColourRole::Down, stroke: super::ColourRole::Down, text: super::ColourRole::DownText, word: None, icon: None, count: None, flag: Some(false), doc: "A resting Sell limit order: the fill in the market set's down colour at PILL_FILL_ALPHA, a solid one-point edge in the same at PILL_EDGE_ALPHA, the words in the down text colour." };
    /// A resting Sell stop order: hollow, a DASHED one-point edge in the market set's down colour at full strength (the v3 design's stop pill), the words in the down text colour.
    pub const SELL_STOP: super::MapRow = super::MapRow { key: "SELL_STOP", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::Down, text: super::ColourRole::DownText, word: None, icon: None, count: None, flag: Some(true), doc: "A resting Sell stop order: hollow, a DASHED one-point edge in the market set's down colour at full strength (the v3 design's stop pill), the words in the down text colour." };
    /// The edge of a pill, either side and either type, on a venue that cannot reprice an order by dragging: the caption grey in place of the side's colour (its fill, its words and whether it is dashed stay the pill's own).
    pub const NOT_DRAGGABLE: super::MapRow = super::MapRow { key: "NOT_DRAGGABLE", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::Text3, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The edge of a pill, either side and either type, on a venue that cannot reprice an order by dragging: the caption grey in place of the side's colour (its fill, its words and whether it is dashed stay the pill's own)." };

    /// Every row of this map, in the order the TOML lists them.
    pub const ALL: &[&super::MapRow] = &[&BUY_LIMIT, &BUY_STOP, &SELL_LIMIT, &SELL_STOP, &NOT_DRAGGABLE];
}

/// Presence: the `[[map]]` rows of `ui-theme.toml` whose `map` is `presence`.
pub mod presence {
    /// Every key of the tier is in the store: the mark in the status green, its legend word configured.
    pub const CONFIGURED: super::MapRow = super::MapRow { key: "CONFIGURED", colour: super::ColourRole::Ok, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("configured"), icon: None, count: None, flag: None, doc: "Every key of the tier is in the store: the mark in the status green, its legend word configured." };
    /// The tier exists and nothing is stored for it: the mark in the status grey, its legend word not set.
    pub const NOT_SET: super::MapRow = super::MapRow { key: "NOT_SET", colour: super::ColourRole::Muted, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("not set"), icon: None, count: None, flag: None, doc: "The tier exists and nothing is stored for it: the mark in the status grey, its legend word not set." };
    /// The venue has no such tier: the mark in the theme's caption grey (the Connections tool dims the status grey instead, in the tier_state map), its legend word no such tier.
    pub const NO_SUCH_TIER: super::MapRow = super::MapRow { key: "NO_SUCH_TIER", colour: super::ColourRole::Text3, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("no such tier"), icon: None, count: None, flag: None, doc: "The venue has no such tier: the mark in the theme's caption grey (the Connections tool dims the status grey instead, in the tier_state map), its legend word no such tier." };
    /// The store could not be opened, so nothing was measured: the mark in the status amber, its legend word not measured.
    pub const UNKNOWN: super::MapRow = super::MapRow { key: "UNKNOWN", colour: super::ColourRole::Warning, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("not measured"), icon: None, count: None, flag: None, doc: "The store could not be opened, so nothing was measured: the mark in the status amber, its legend word not measured." };

    /// Every row of this map, in the order the TOML lists them.
    pub const ALL: &[&super::MapRow] = &[&CONFIGURED, &NOT_SET, &NO_SUCH_TIER, &UNKNOWN];
}

/// Rail Group: the `[[map]]` rows of `ui-theme.toml` whose `map` is `rail_group`.
pub mod rail_group {
    /// The heading over the rail's destinations that look at what is stored: the word BROWSE.
    pub const BROWSE: super::MapRow = super::MapRow { key: "BROWSE", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("BROWSE"), icon: None, count: None, flag: None, doc: "The heading over the rail's destinations that look at what is stored: the word BROWSE." };
    /// The heading over the rail's destinations that look at what is running and where it comes from: the word LIVE.
    pub const LIVE: super::MapRow = super::MapRow { key: "LIVE", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("LIVE"), icon: None, count: None, flag: None, doc: "The heading over the rail's destinations that look at what is running and where it comes from: the word LIVE." };
    /// The heading over the rail's destinations that change something: the word CONFIGURE.
    pub const CONFIGURE: super::MapRow = super::MapRow { key: "CONFIGURE", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("CONFIGURE"), icon: None, count: None, flag: None, doc: "The heading over the rail's destinations that change something: the word CONFIGURE." };

    /// Every row of this map, in the order the TOML lists them.
    pub const ALL: &[&super::MapRow] = &[&BROWSE, &LIVE, &CONFIGURE];
}

/// Side: the `[[map]]` rows of `ui-theme.toml` whose `map` is `side`.
pub mod side {
    /// A buy: an order or a print on the buying side (a resting buy's marker and pill, a print's bubble, the side word of a working order): the graphic in the market's up, the word in its up text.
    pub const BUY: super::MapRow = super::MapRow { key: "BUY", colour: super::ColourRole::Up, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::UpText, word: None, icon: None, count: None, flag: None, doc: "A buy: an order or a print on the buying side (a resting buy's marker and pill, a print's bubble, the side word of a working order): the graphic in the market's up, the word in its up text." };
    /// A sell: an order or a print on the selling side (a resting sell's marker and pill, a print's bubble, the side word of a working order): the graphic in the market's down, the word in its down text.
    pub const SELL: super::MapRow = super::MapRow { key: "SELL", colour: super::ColourRole::Down, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::DownText, word: None, icon: None, count: None, flag: None, doc: "A sell: an order or a print on the selling side (a resting sell's marker and pill, a print's bubble, the side word of a working order): the graphic in the market's down, the word in its down text." };
    /// A long position: the Trade ticket's LONG word, a backtest trade's long side, the options chain's badge for a net long position: the word and the badge in the market's up.
    pub const LONG: super::MapRow = super::MapRow { key: "LONG", colour: super::ColourRole::Up, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::UpText, word: None, icon: None, count: None, flag: None, doc: "A long position: the Trade ticket's LONG word, a backtest trade's long side, the options chain's badge for a net long position: the word and the badge in the market's up." };
    /// A short position: the Trade ticket's SHORT word, a backtest trade's short side, the options chain's badge for a net short position: the word and the badge in the market's down.
    pub const SHORT: super::MapRow = super::MapRow { key: "SHORT", colour: super::ColourRole::Down, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::DownText, word: None, icon: None, count: None, flag: None, doc: "A short position: the Trade ticket's SHORT word, a backtest trade's short side, the options chain's badge for a net short position: the word and the badge in the market's down." };
    /// The bid side of a book: a bid's depth size, the bid bar and the inside-bid wash on a ladder, the Bid button, the bid step line, an options chain's bid cells and their hover wash: the graphic in the market's up, the numbers in its up text.
    pub const BID: super::MapRow = super::MapRow { key: "BID", colour: super::ColourRole::Up, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::UpText, word: None, icon: None, count: None, flag: None, doc: "The bid side of a book: a bid's depth size, the bid bar and the inside-bid wash on a ladder, the Bid button, the bid step line, an options chain's bid cells and their hover wash: the graphic in the market's up, the numbers in its up text." };
    /// The ask side of a book: an ask's depth size, the ask bar and the inside-ask wash on a ladder, the Ask button, the ask step line, an options chain's ask cells and their hover wash: the graphic in the market's down, the numbers in its down text.
    pub const ASK: super::MapRow = super::MapRow { key: "ASK", colour: super::ColourRole::Down, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::DownText, word: None, icon: None, count: None, flag: None, doc: "The ask side of a book: an ask's depth size, the ask bar and the inside-ask wash on a ladder, the Ask button, the ask step line, an options chain's ask cells and their hover wash: the graphic in the market's down, the numbers in its down text." };
    /// Money made: a P&L, a realized, unrealized or funding figure, a trade's profit, a curve that ends above where it began: the number in the market's up text, the curve in its up.
    pub const GAIN: super::MapRow = super::MapRow { key: "GAIN", colour: super::ColourRole::Up, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::UpText, word: None, icon: None, count: None, flag: None, doc: "Money made: a P&L, a realized, unrealized or funding figure, a trade's profit, a curve that ends above where it began: the number in the market's up text, the curve in its up." };
    /// Money lost: a P&L, a realized, unrealized or funding figure, a trade's loss, a drawdown, a curve that ends below where it began: the number in the market's down text, the curve in its down.
    pub const LOSS: super::MapRow = super::MapRow { key: "LOSS", colour: super::ColourRole::Down, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::DownText, word: None, icon: None, count: None, flag: None, doc: "Money lost: a P&L, a realized, unrealized or funding figure, a trade's loss, a drawdown, a curve that ends below where it began: the number in the market's down text, the curve in its down." };
    /// A number above zero that is not money (a Greek, a tearsheet metric, a Sharpe, the move off the price to beat, an earnings surprise): the number in the market's up text.
    pub const POSITIVE: super::MapRow = super::MapRow { key: "POSITIVE", colour: super::ColourRole::Up, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::UpText, word: None, icon: None, count: None, flag: None, doc: "A number above zero that is not money (a Greek, a tearsheet metric, a Sharpe, the move off the price to beat, an earnings surprise): the number in the market's up text." };
    /// A number below zero that is not money (a Greek, a tearsheet metric, a Sharpe, the move off the price to beat, an earnings surprise): the number in the market's down text.
    pub const NEGATIVE: super::MapRow = super::MapRow { key: "NEGATIVE", colour: super::ColourRole::Down, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::DownText, word: None, icon: None, count: None, flag: None, doc: "A number below zero that is not money (a Greek, a tearsheet metric, a Sharpe, the move off the price to beat, an earnings surprise): the number in the market's down text." };
    /// A take-profit exit: the ladder's T marker and the ticket's TP words: the marker in the market's up, the words in its up text.
    pub const TAKE_PROFIT: super::MapRow = super::MapRow { key: "TAKE_PROFIT", colour: super::ColourRole::Up, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::UpText, word: None, icon: None, count: None, flag: None, doc: "A take-profit exit: the ladder's T marker and the ticket's TP words: the marker in the market's up, the words in its up text." };
    /// A stop-loss exit: the ladder's S marker and the ticket's SL words: the marker in the market's down, the words in its down text.
    pub const STOP_LOSS: super::MapRow = super::MapRow { key: "STOP_LOSS", colour: super::ColourRole::Down, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::DownText, word: None, icon: None, count: None, flag: None, doc: "A stop-loss exit: the ladder's S marker and the ticket's SL words: the marker in the market's down, the words in its down text." };
    /// A rising candle as the chart-style icons draw their examples: the candle in the market's up.
    pub const RISE: super::MapRow = super::MapRow { key: "RISE", colour: super::ColourRole::Up, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::UpText, word: None, icon: None, count: None, flag: None, doc: "A rising candle as the chart-style icons draw their examples: the candle in the market's up." };
    /// A falling candle as the chart-style icons draw their examples: the candle in the market's down.
    pub const FALL: super::MapRow = super::MapRow { key: "FALL", colour: super::ColourRole::Down, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::DownText, word: None, icon: None, count: None, flag: None, doc: "A falling candle as the chart-style icons draw their examples: the candle in the market's down." };
    /// A released figure above its forecast, in the Calendar's Actual column: the figure in the market's up text.
    pub const ABOVE_FORECAST: super::MapRow = super::MapRow { key: "ABOVE_FORECAST", colour: super::ColourRole::Up, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::UpText, word: None, icon: None, count: None, flag: None, doc: "A released figure above its forecast, in the Calendar's Actual column: the figure in the market's up text." };
    /// A released figure below its forecast, in the Calendar's Actual column: the figure in the market's down text.
    pub const BELOW_FORECAST: super::MapRow = super::MapRow { key: "BELOW_FORECAST", colour: super::ColourRole::Down, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::DownText, word: None, icon: None, count: None, flag: None, doc: "A released figure below its forecast, in the Calendar's Actual column: the figure in the market's down text." };
    /// The Up outcome of a Polymarket up/down market (its odds on the chain cards and in the header, its payout preview on the ticket): the words in the market's up text.
    pub const OUTCOME_UP: super::MapRow = super::MapRow { key: "OUTCOME_UP", colour: super::ColourRole::Up, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::UpText, word: None, icon: None, count: None, flag: None, doc: "The Up outcome of a Polymarket up/down market (its odds on the chain cards and in the header, its payout preview on the ticket): the words in the market's up text." };
    /// The Down outcome of a Polymarket up/down market (its odds on the chain cards and in the header, its payout preview on the ticket): the words in the market's down text.
    pub const OUTCOME_DOWN: super::MapRow = super::MapRow { key: "OUTCOME_DOWN", colour: super::ColourRole::Down, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::DownText, word: None, icon: None, count: None, flag: None, doc: "The Down outcome of a Polymarket up/down market (its odds on the chain cards and in the header, its payout preview on the ticket): the words in the market's down text." };

    /// Every row of this map, in the order the TOML lists them.
    pub const ALL: &[&super::MapRow] = &[&BUY, &SELL, &LONG, &SHORT, &BID, &ASK, &GAIN, &LOSS, &POSITIVE, &NEGATIVE, &TAKE_PROFIT, &STOP_LOSS, &RISE, &FALL, &ABOVE_FORECAST, &BELOW_FORECAST, &OUTCOME_UP, &OUTCOME_DOWN];
}

/// Strip: the `[[map]]` rows of `ui-theme.toml` whose `map` is `strip`.
pub mod strip {
    /// A note from the app that asks nothing of the trader ("Not sent.", "Waiting for the node.", an order status that is neither a fill nor a failure): the dot in the caption grey, the words in the secondary text.
    pub const INFO: super::MapRow = super::MapRow { key: "INFO", colour: super::ColourRole::Text3, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::Text2, word: None, icon: None, count: None, flag: None, doc: "A note from the app that asks nothing of the trader (\"Not sent.\", \"Waiting for the node.\", an order status that is neither a fill nor a failure): the dot in the caption grey, the words in the secondary text." };
    /// An order the venue filled: the dot and the words in the status green.
    pub const OK: super::MapRow = super::MapRow { key: "OK", colour: super::ColourRole::Ok, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::Ok, word: None, icon: None, count: None, flag: None, doc: "An order the venue filled: the dot and the words in the status green." };
    /// A failure or a refusal (an order the venue rejected, denied or let expire; a stopped or halted node; a click the window refused): the dot and the words in the status red.
    pub const ERROR: super::MapRow = super::MapRow { key: "ERROR", colour: super::ColourRole::Error, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::Error, word: None, icon: None, count: None, flag: None, doc: "A failure or a refusal (an order the venue rejected, denied or let expire; a stopped or halted node; a click the window refused): the dot and the words in the status red." };
    /// The ticket's own standing refusal, said where "Ready" would be because Buy and Sell send nothing (the window takes no order, a TP/SL ticked where it cannot be used, an instrument with no lot size, a TP/SL leg that is not a percentage): the dot in the status amber, the words in the secondary text.
    pub const REFUSAL: super::MapRow = super::MapRow { key: "REFUSAL", colour: super::ColourRole::Warning, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::Text2, word: None, icon: None, count: None, flag: None, doc: "The ticket's own standing refusal, said where \"Ready\" would be because Buy and Sell send nothing (the window takes no order, a TP/SL ticked where it cannot be used, an instrument with no lot size, a TP/SL leg that is not a percentage): the dot in the status amber, the words in the secondary text." };
    /// Nothing waits and the ticket can send ("Ready · every order asks first", "Ready · one-click trading is on"): the dot in the caption grey, the words in the secondary text.
    pub const READY: super::MapRow = super::MapRow { key: "READY", colour: super::ColourRole::Text3, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::Text2, word: None, icon: None, count: None, flag: None, doc: "Nothing waits and the ticket can send (\"Ready · every order asks first\", \"Ready · one-click trading is on\"): the dot in the caption grey, the words in the secondary text." };

    /// Every row of this map, in the order the TOML lists them.
    pub const ALL: &[&super::MapRow] = &[&INFO, &OK, &ERROR, &REFUSAL, &READY];
}

/// Studio Tab: the `[[map]]` rows of `ui-theme.toml` whose `map` is `studio_tab`.
pub mod studio_tab {
    /// The Sweep and Validate tool, the Studio's first tab: the label Sweep and the grid glyph on the rail.
    pub const SWEEP: super::MapRow = super::MapRow { key: "SWEEP", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("Sweep"), icon: Some("SWEEP"), count: None, flag: None, doc: "The Sweep and Validate tool, the Studio's first tab: the label Sweep and the grid glyph on the rail." };
    /// The strategy source (the Rhai editor, or a native strategy and its params): the label Strategy and the puzzle-piece glyph on the rail.
    pub const STRATEGY: super::MapRow = super::MapRow { key: "STRATEGY", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("Strategy"), icon: Some("STRATEGY"), count: None, flag: None, doc: "The strategy source (the Rhai editor, or a native strategy and its params): the label Strategy and the puzzle-piece glyph on the rail." };
    /// The stored data the Studio runs over: the label Data and the database glyph on the rail.
    pub const DATA: super::MapRow = super::MapRow { key: "DATA", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("Data"), icon: Some("DATA"), count: None, flag: None, doc: "The stored data the Studio runs over: the label Data and the database glyph on the rail." };
    /// The indicators tool: the label Indicators and the function glyph on the rail.
    pub const INDICATORS: super::MapRow = super::MapRow { key: "INDICATORS", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("Indicators"), icon: Some("INDICATORS"), count: None, flag: None, doc: "The indicators tool: the label Indicators and the function glyph on the rail." };
    /// The saved strategies: the label Saved and the bookmarks glyph on the rail.
    pub const SAVED: super::MapRow = super::MapRow { key: "SAVED", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("Saved"), icon: Some("SAVED"), count: None, flag: None, doc: "The saved strategies: the label Saved and the bookmarks glyph on the rail." };
    /// The user's own studies and the runs they left behind: the label Research and the binoculars glyph on the rail.
    pub const RESEARCH: super::MapRow = super::MapRow { key: "RESEARCH", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("Research"), icon: Some("RESEARCH"), count: None, flag: None, doc: "The user's own studies and the runs they left behind: the label Research and the binoculars glyph on the rail." };
    /// The AI copilot: the label AI Chat and the chat glyph on the rail.
    pub const CHAT: super::MapRow = super::MapRow { key: "CHAT", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("AI Chat"), icon: Some("CHAT"), count: None, flag: None, doc: "The AI copilot: the label AI Chat and the chat glyph on the rail." };

    /// Every row of this map, in the order the TOML lists them.
    pub const ALL: &[&super::MapRow] = &[&SWEEP, &STRATEGY, &DATA, &INDICATORS, &SAVED, &RESEARCH, &CHAT];
}

/// Tier State: the `[[map]]` rows of `ui-theme.toml` whose `map` is `tier_state`.
pub mod tier_state {
    /// Every key of the tier is in the store: the mark in the status green, the detail pane reading configured.
    pub const CONFIGURED: super::MapRow = super::MapRow { key: "CONFIGURED", colour: super::ColourRole::Ok, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("configured"), icon: None, count: None, flag: None, doc: "Every key of the tier is in the store: the mark in the status green, the detail pane reading configured." };
    /// The tier exists and nothing is stored for it: the mark in the status grey, the detail pane reading not set.
    pub const NOT_SET: super::MapRow = super::MapRow { key: "NOT_SET", colour: super::ColourRole::Muted, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("not set"), icon: None, count: None, flag: None, doc: "The tier exists and nothing is stored for it: the mark in the status grey, the detail pane reading not set." };
    /// The venue has no such tier: the mark in the status grey DIMMED (flag), the detail pane reading not configurable. The kit's presence map paints the same state in the caption grey instead.
    pub const NOT_CONFIGURABLE: super::MapRow = super::MapRow { key: "NOT_CONFIGURABLE", colour: super::ColourRole::Muted, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("not configurable"), icon: None, count: None, flag: Some(true), doc: "The venue has no such tier: the mark in the status grey DIMMED (flag), the detail pane reading not configurable. The kit's presence map paints the same state in the caption grey instead." };
    /// The credential store exists and could not be opened, so nothing was measured: the mark in the status amber, the detail pane reading unknown — store unreadable.
    pub const UNKNOWN: super::MapRow = super::MapRow { key: "UNKNOWN", colour: super::ColourRole::Warning, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("unknown — store unreadable"), icon: None, count: None, flag: None, doc: "The credential store exists and could not be opened, so nothing was measured: the mark in the status amber, the detail pane reading unknown — store unreadable." };

    /// Every row of this map, in the order the TOML lists them.
    pub const ALL: &[&super::MapRow] = &[&CONFIGURED, &NOT_SET, &NOT_CONFIGURABLE, &UNKNOWN];
}

/// Visuals: the `[[map]]` rows of `ui-theme.toml` whose `map` is `visuals`.
pub mod visuals {
    /// The fill of a central panel or side panel: the theme's background.
    pub const PANEL_FILL: super::MapRow = super::MapRow { key: "PANEL_FILL", colour: super::ColourRole::Bg, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The fill of a central panel or side panel: the theme's background." };
    /// The fill of a window, a title bar, the rail and a dialog: the theme's background, the same dark as the plot (only the 1 px border sets them apart).
    pub const WINDOW_FILL: super::MapRow = super::MapRow { key: "WINDOW_FILL", colour: super::ColourRole::Bg, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The fill of a window, a title bar, the rail and a dialog: the theme's background, the same dark as the plot (only the 1 px border sets them apart)." };
    /// The fill of a text field, a slider's track and the plot canvas: the theme's background.
    pub const EXTREME_BG_COLOR: super::MapRow = super::MapRow { key: "EXTREME_BG_COLOR", colour: super::ColourRole::Bg, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The fill of a text field, a slider's track and the plot canvas: the theme's background." };
    /// The stripe of an alternating row and other faint backgrounds: the theme's surface.
    pub const FAINT_BG_COLOR: super::MapRow = super::MapRow { key: "FAINT_BG_COLOR", colour: super::ColourRole::Surface, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The stripe of an alternating row and other faint backgrounds: the theme's surface." };
    /// The background of a code span: the theme's surface.
    pub const CODE_BG_COLOR: super::MapRow = super::MapRow { key: "CODE_BG_COLOR", colour: super::ColourRole::Surface, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The background of a code span: the theme's surface." };
    /// The 1 px border round a window: the theme's border grey.
    pub const WINDOW_STROKE: super::MapRow = super::MapRow { key: "WINDOW_STROKE", colour: super::ColourRole::Border, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The 1 px border round a window: the theme's border grey." };
    /// The fill behind selected text and a selected row: the theme's hover fill.
    pub const SELECTION_BG_FILL: super::MapRow = super::MapRow { key: "SELECTION_BG_FILL", colour: super::ColourRole::Hover, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The fill behind selected text and a selected row: the theme's hover fill." };
    /// The outline of a selection: the theme's UI text.
    pub const SELECTION_STROKE: super::MapRow = super::MapRow { key: "SELECTION_STROKE", colour: super::ColourRole::TextUi, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The outline of a selection: the theme's UI text." };
    /// A hyperlink: the status blue.
    pub const HYPERLINK_COLOR: super::MapRow = super::MapRow { key: "HYPERLINK_COLOR", colour: super::ColourRole::Info, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "A hyperlink: the status blue." };
    /// Text egui draws as a warning: the status amber.
    pub const WARN_FG_COLOR: super::MapRow = super::MapRow { key: "WARN_FG_COLOR", colour: super::ColourRole::Warning, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "Text egui draws as a warning: the status amber." };
    /// Text egui draws as an error: the status red.
    pub const ERROR_FG_COLOR: super::MapRow = super::MapRow { key: "ERROR_FG_COLOR", colour: super::ColourRole::Error, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "Text egui draws as an error: the status red." };
    /// The caret of a text field: the theme's UI text.
    pub const TEXT_CURSOR_STROKE: super::MapRow = super::MapRow { key: "TEXT_CURSOR_STROKE", colour: super::ColourRole::TextUi, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The caret of a text field: the theme's UI text." };
    /// The underline of the text being composed by an input method, in its active clause: the theme's UI text.
    pub const IME_COMPOSITION_ACTIVE_UNDERLINE_STROKE: super::MapRow = super::MapRow { key: "IME_COMPOSITION_ACTIVE_UNDERLINE_STROKE", colour: super::ColourRole::TextUi, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The underline of the text being composed by an input method, in its active clause: the theme's UI text." };
    /// The underline of the text being composed by an input method, in its inactive clauses: the theme's caption grey.
    pub const IME_COMPOSITION_INACTIVE_UNDERLINE_STROKE: super::MapRow = super::MapRow { key: "IME_COMPOSITION_INACTIVE_UNDERLINE_STROKE", colour: super::ColourRole::Text3, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The underline of the text being composed by an input method, in its inactive clauses: the theme's caption grey." };
    /// The fill of a widget that cannot be interacted with (a label, a separator, a frame): the theme's surface.
    pub const NONINTERACTIVE_BG_FILL: super::MapRow = super::MapRow { key: "NONINTERACTIVE_BG_FILL", colour: super::ColourRole::Surface, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The fill of a widget that cannot be interacted with (a label, a separator, a frame): the theme's surface." };
    /// The fill of a widget that cannot be interacted with (a label, a separator, a frame) when it is drawn weakly (a button's frame): the theme's surface.
    pub const NONINTERACTIVE_WEAK_BG_FILL: super::MapRow = super::MapRow { key: "NONINTERACTIVE_WEAK_BG_FILL", colour: super::ColourRole::Surface, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The fill of a widget that cannot be interacted with (a label, a separator, a frame) when it is drawn weakly (a button's frame): the theme's surface." };
    /// The outline of a widget that cannot be interacted with (a label, a separator, a frame): the theme's border grey.
    pub const NONINTERACTIVE_BG_STROKE: super::MapRow = super::MapRow { key: "NONINTERACTIVE_BG_STROKE", colour: super::ColourRole::Border, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The outline of a widget that cannot be interacted with (a label, a separator, a frame): the theme's border grey." };
    /// The glyphs and text of a widget that cannot be interacted with (a label, a separator, a frame): the theme's secondary text.
    pub const NONINTERACTIVE_FG_STROKE: super::MapRow = super::MapRow { key: "NONINTERACTIVE_FG_STROKE", colour: super::ColourRole::Text2, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The glyphs and text of a widget that cannot be interacted with (a label, a separator, a frame): the theme's secondary text." };
    /// The fill of a widget at rest (a button, a checkbox, a text field): the theme's surface.
    pub const INACTIVE_BG_FILL: super::MapRow = super::MapRow { key: "INACTIVE_BG_FILL", colour: super::ColourRole::Surface, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The fill of a widget at rest (a button, a checkbox, a text field): the theme's surface." };
    /// The fill of a widget at rest (a button, a checkbox, a text field) when it is drawn weakly (a button's frame): the theme's surface.
    pub const INACTIVE_WEAK_BG_FILL: super::MapRow = super::MapRow { key: "INACTIVE_WEAK_BG_FILL", colour: super::ColourRole::Surface, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The fill of a widget at rest (a button, a checkbox, a text field) when it is drawn weakly (a button's frame): the theme's surface." };
    /// The outline of a widget at rest (a button, a checkbox, a text field): the theme's border grey.
    pub const INACTIVE_BG_STROKE: super::MapRow = super::MapRow { key: "INACTIVE_BG_STROKE", colour: super::ColourRole::Border, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The outline of a widget at rest (a button, a checkbox, a text field): the theme's border grey." };
    /// The glyphs and text of a widget at rest (a button, a checkbox, a text field): the theme's secondary text.
    pub const INACTIVE_FG_STROKE: super::MapRow = super::MapRow { key: "INACTIVE_FG_STROKE", colour: super::ColourRole::Text2, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The glyphs and text of a widget at rest (a button, a checkbox, a text field): the theme's secondary text." };
    /// The fill of a widget under the pointer: the theme's hover fill.
    pub const HOVERED_BG_FILL: super::MapRow = super::MapRow { key: "HOVERED_BG_FILL", colour: super::ColourRole::Hover, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The fill of a widget under the pointer: the theme's hover fill." };
    /// The fill of a widget under the pointer when it is drawn weakly (a button's frame): the theme's hover fill.
    pub const HOVERED_WEAK_BG_FILL: super::MapRow = super::MapRow { key: "HOVERED_WEAK_BG_FILL", colour: super::ColourRole::Hover, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The fill of a widget under the pointer when it is drawn weakly (a button's frame): the theme's hover fill." };
    /// The outline of a widget under the pointer: the theme's border grey.
    pub const HOVERED_BG_STROKE: super::MapRow = super::MapRow { key: "HOVERED_BG_STROKE", colour: super::ColourRole::Border, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The outline of a widget under the pointer: the theme's border grey." };
    /// The glyphs and text of a widget under the pointer: the theme's UI text.
    pub const HOVERED_FG_STROKE: super::MapRow = super::MapRow { key: "HOVERED_FG_STROKE", colour: super::ColourRole::TextUi, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The glyphs and text of a widget under the pointer: the theme's UI text." };
    /// The fill of a widget being pressed or dragged: the theme's hover fill.
    pub const ACTIVE_BG_FILL: super::MapRow = super::MapRow { key: "ACTIVE_BG_FILL", colour: super::ColourRole::Hover, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The fill of a widget being pressed or dragged: the theme's hover fill." };
    /// The fill of a widget being pressed or dragged when it is drawn weakly (a button's frame): the theme's hover fill.
    pub const ACTIVE_WEAK_BG_FILL: super::MapRow = super::MapRow { key: "ACTIVE_WEAK_BG_FILL", colour: super::ColourRole::Hover, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The fill of a widget being pressed or dragged when it is drawn weakly (a button's frame): the theme's hover fill." };
    /// The outline of a widget being pressed or dragged: the theme's border grey.
    pub const ACTIVE_BG_STROKE: super::MapRow = super::MapRow { key: "ACTIVE_BG_STROKE", colour: super::ColourRole::Border, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The outline of a widget being pressed or dragged: the theme's border grey." };
    /// The glyphs and text of a widget being pressed or dragged: the theme's UI text.
    pub const ACTIVE_FG_STROKE: super::MapRow = super::MapRow { key: "ACTIVE_FG_STROKE", colour: super::ColourRole::TextUi, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The glyphs and text of a widget being pressed or dragged: the theme's UI text." };
    /// The fill of a widget whose menu or popup is open: the theme's hover fill.
    pub const OPEN_BG_FILL: super::MapRow = super::MapRow { key: "OPEN_BG_FILL", colour: super::ColourRole::Hover, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The fill of a widget whose menu or popup is open: the theme's hover fill." };
    /// The fill of a widget whose menu or popup is open when it is drawn weakly (a button's frame): the theme's hover fill.
    pub const OPEN_WEAK_BG_FILL: super::MapRow = super::MapRow { key: "OPEN_WEAK_BG_FILL", colour: super::ColourRole::Hover, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The fill of a widget whose menu or popup is open when it is drawn weakly (a button's frame): the theme's hover fill." };
    /// The outline of a widget whose menu or popup is open: the theme's border grey.
    pub const OPEN_BG_STROKE: super::MapRow = super::MapRow { key: "OPEN_BG_STROKE", colour: super::ColourRole::Border, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The outline of a widget whose menu or popup is open: the theme's border grey." };
    /// The glyphs and text of a widget whose menu or popup is open: the theme's UI text.
    pub const OPEN_FG_STROKE: super::MapRow = super::MapRow { key: "OPEN_FG_STROKE", colour: super::ColourRole::TextUi, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: None, icon: None, count: None, flag: None, doc: "The glyphs and text of a widget whose menu or popup is open: the theme's UI text." };

    /// Every row of this map, in the order the TOML lists them.
    pub const ALL: &[&super::MapRow] = &[&PANEL_FILL, &WINDOW_FILL, &EXTREME_BG_COLOR, &FAINT_BG_COLOR, &CODE_BG_COLOR, &WINDOW_STROKE, &SELECTION_BG_FILL, &SELECTION_STROKE, &HYPERLINK_COLOR, &WARN_FG_COLOR, &ERROR_FG_COLOR, &TEXT_CURSOR_STROKE, &IME_COMPOSITION_ACTIVE_UNDERLINE_STROKE, &IME_COMPOSITION_INACTIVE_UNDERLINE_STROKE, &NONINTERACTIVE_BG_FILL, &NONINTERACTIVE_WEAK_BG_FILL, &NONINTERACTIVE_BG_STROKE, &NONINTERACTIVE_FG_STROKE, &INACTIVE_BG_FILL, &INACTIVE_WEAK_BG_FILL, &INACTIVE_BG_STROKE, &INACTIVE_FG_STROKE, &HOVERED_BG_FILL, &HOVERED_WEAK_BG_FILL, &HOVERED_BG_STROKE, &HOVERED_FG_STROKE, &ACTIVE_BG_FILL, &ACTIVE_WEAK_BG_FILL, &ACTIVE_BG_STROKE, &ACTIVE_FG_STROKE, &OPEN_BG_FILL, &OPEN_WEAK_BG_FILL, &OPEN_BG_STROKE, &OPEN_FG_STROKE];
}

/// Window Kind: the `[[map]]` rows of `ui-theme.toml` whose `map` is `window_kind`.
pub mod window_kind {
    /// The chart window: its name, Chart, and the line-chart glyph the title bar shows in its place where the name does not fit.
    pub const CHART: super::MapRow = super::MapRow { key: "CHART", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("Chart"), icon: Some("CHART"), count: None, flag: None, doc: "The chart window: its name, Chart, and the line-chart glyph the title bar shows in its place where the name does not fit." };
    /// The Trade window: its name, Trade, and the price-ladder glyph (the ladder is what the window is) the title bar shows in its place where the name does not fit.
    pub const TRADE: super::MapRow = super::MapRow { key: "TRADE", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("Trade"), icon: Some("DOM"), count: None, flag: None, doc: "The Trade window: its name, Trade, and the price-ladder glyph (the ladder is what the window is) the title bar shows in its place where the name does not fit." };
    /// The Account window (equity, accounts, working orders): its name, Account, and the wallet glyph the title bar shows in its place where the name does not fit.
    pub const ACCOUNT: super::MapRow = super::MapRow { key: "ACCOUNT", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("Account"), icon: Some("ACCOUNT"), count: None, flag: None, doc: "The Account window (equity, accounts, working orders): its name, Account, and the wallet glyph the title bar shows in its place where the name does not fit." };
    /// The options-chain window: its name, Options, and the target glyph the title bar shows in its place where the name does not fit.
    pub const OPTIONS: super::MapRow = super::MapRow { key: "OPTIONS", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("Options"), icon: Some("OPTIONS"), count: None, flag: None, doc: "The options-chain window: its name, Options, and the target glyph the title bar shows in its place where the name does not fit." };
    /// The Greeks window: its name, Greeks, and the math-operations glyph the title bar shows in its place where the name does not fit.
    pub const GREEKS: super::MapRow = super::MapRow { key: "GREEKS", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("Greeks"), icon: Some("GREEKS"), count: None, flag: None, doc: "The Greeks window: its name, Greeks, and the math-operations glyph the title bar shows in its place where the name does not fit." };
    /// The News window: its name, News, and the newspaper glyph the title bar shows in its place where the name does not fit.
    pub const NEWS: super::MapRow = super::MapRow { key: "NEWS", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("News"), icon: Some("NEWS"), count: None, flag: None, doc: "The News window: its name, News, and the newspaper glyph the title bar shows in its place where the name does not fit." };
    /// The economic-calendar window: its name, Calendar, and the calendar glyph the title bar shows in its place where the name does not fit.
    pub const CALENDAR: super::MapRow = super::MapRow { key: "CALENDAR", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("Calendar"), icon: Some("CALENDAR"), count: None, flag: None, doc: "The economic-calendar window: its name, Calendar, and the calendar glyph the title bar shows in its place where the name does not fit." };
    /// The Data Manager window: its name, Data Manager, and the database glyph the title bar shows in its place where the name does not fit.
    pub const DATA: super::MapRow = super::MapRow { key: "DATA", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("Data Manager"), icon: Some("DATA"), count: None, flag: None, doc: "The Data Manager window: its name, Data Manager, and the database glyph the title bar shows in its place where the name does not fit." };
    /// The Studio window: its name, Studio, and the flask glyph the title bar shows in its place where the name does not fit.
    pub const STUDIO: super::MapRow = super::MapRow { key: "STUDIO", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("Studio"), icon: Some("STUDIO"), count: None, flag: None, doc: "The Studio window: its name, Studio, and the flask glyph the title bar shows in its place where the name does not fit." };
    /// The Tearsheet window: its name, Tearsheet, and the bar-chart glyph the title bar shows in its place where the name does not fit.
    pub const TEARSHEET: super::MapRow = super::MapRow { key: "TEARSHEET", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("Tearsheet"), icon: Some("TEARSHEET"), count: None, flag: None, doc: "The Tearsheet window: its name, Tearsheet, and the bar-chart glyph the title bar shows in its place where the name does not fit." };
    /// The Polymarket scalp-cockpit window: its name, Polymarket, and the dice glyph the title bar shows in its place where the name does not fit.
    pub const POLYMARKET: super::MapRow = super::MapRow { key: "POLYMARKET", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("Polymarket"), icon: Some("POLYMARKET"), count: None, flag: None, doc: "The Polymarket scalp-cockpit window: its name, Polymarket, and the dice glyph the title bar shows in its place where the name does not fit." };
    /// The Settings window: its name, Settings, and the gear glyph the title bar shows in its place where the name does not fit.
    pub const SETTINGS: super::MapRow = super::MapRow { key: "SETTINGS", colour: super::ColourRole::None, fill: super::ColourRole::None, stroke: super::ColourRole::None, text: super::ColourRole::None, word: Some("Settings"), icon: Some("SETTINGS"), count: None, flag: None, doc: "The Settings window: its name, Settings, and the gear glyph the title bar shows in its place where the name does not fit." };

    /// Every row of this map, in the order the TOML lists them.
    pub const ALL: &[&super::MapRow] = &[&CHART, &TRADE, &ACCOUNT, &OPTIONS, &GREEKS, &NEWS, &CALENDAR, &DATA, &STUDIO, &TEARSHEET, &POLYMARKET, &SETTINGS];
}

/// Every map, alphabetical, with its rows: what the brand book and the tests iterate.
pub const MAPS: &[Map] = &[
    Map { name: "button", rows: button::ALL },
    Map { name: "callput", rows: callput::ALL },
    Map { name: "chart_role", rows: chart_role::ALL },
    Map { name: "connection", rows: connection::ALL },
    Map { name: "control_link", rows: control_link::ALL },
    Map { name: "coverage", rows: coverage::ALL },
    Map { name: "data_dest", rows: data_dest::ALL },
    Map { name: "empty_pane", rows: empty_pane::ALL },
    Map { name: "feed_badge", rows: feed_badge::ALL },
    Map { name: "importance", rows: importance::ALL },
    Map { name: "key_presence", rows: key_presence::ALL },
    Map { name: "order_pill", rows: order_pill::ALL },
    Map { name: "presence", rows: presence::ALL },
    Map { name: "rail_group", rows: rail_group::ALL },
    Map { name: "side", rows: side::ALL },
    Map { name: "strip", rows: strip::ALL },
    Map { name: "studio_tab", rows: studio_tab::ALL },
    Map { name: "tier_state", rows: tier_state::ALL },
    Map { name: "visuals", rows: visuals::ALL },
    Map { name: "window_kind", rows: window_kind::ALL },
];
