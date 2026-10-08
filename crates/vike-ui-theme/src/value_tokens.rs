// @generated from crates/vike-ui-theme/ui-theme.toml by crates/vike-ui-theme/tests/tokens_gen — do not edit; change the TOML and regenerate (see tests/brand_assets.rs).

/// Account: the `[[value]]` rows of `ui-theme.toml` whose `group` is `account`.
pub mod account {
    /// The height of one cell in the Account window's tables (venue rows and working-order rows).
    pub const CELL_H: f32 = 18.0;
    /// The height of a numeric column header cell in the Account window's per-venue table.
    pub const HEAD_CELL_H: f32 = 16.0;
    /// The widest the Account window's content grows; wider windows leave the rest empty.
    pub const MAX_W: f32 = 960.0;
    /// How tall the Account window's working-orders list grows before it scrolls.
    pub const ORDERS_MAX_H: f32 = 160.0;
    /// The working-orders list: the quantity column's width.
    pub const ORDER_W_QTY: f32 = 78.0;
    /// The working-orders list: the BUY or SELL column's width.
    pub const ORDER_W_SIDE: f32 = 36.0;
    /// The working-orders list: the status column's width.
    pub const ORDER_W_STATUS: f32 = 74.0;
    /// The working-orders list: the order-type column's width.
    pub const ORDER_W_TYPE: f32 = 46.0;
    /// The Account window's per-venue table: the mode tag (paper, demo, live) column's width.
    pub const WM: f32 = 24.0;
    /// The Account window's per-venue table: each numeric column's width (equity, unrealised PnL, fees, funding).
    pub const WN: f32 = 66.0;
    /// The Account window's per-venue table: the venue-name column's width.
    pub const WV: f32 = 66.0;

    /// Every row of this group as data, in the order the TOML lists them.
    pub const ENTRIES: &[super::Entry] = &[
        super::Entry { group: "account", name: "CELL_H", unit: Some(super::Unit::Px), value: super::Data::F32(CELL_H), doc: "The height of one cell in the Account window's tables (venue rows and working-order rows)." },
        super::Entry { group: "account", name: "HEAD_CELL_H", unit: Some(super::Unit::Px), value: super::Data::F32(HEAD_CELL_H), doc: "The height of a numeric column header cell in the Account window's per-venue table." },
        super::Entry { group: "account", name: "MAX_W", unit: Some(super::Unit::Px), value: super::Data::F32(MAX_W), doc: "The widest the Account window's content grows; wider windows leave the rest empty." },
        super::Entry { group: "account", name: "ORDERS_MAX_H", unit: Some(super::Unit::Px), value: super::Data::F32(ORDERS_MAX_H), doc: "How tall the Account window's working-orders list grows before it scrolls." },
        super::Entry { group: "account", name: "ORDER_W_QTY", unit: Some(super::Unit::Px), value: super::Data::F32(ORDER_W_QTY), doc: "The working-orders list: the quantity column's width." },
        super::Entry { group: "account", name: "ORDER_W_SIDE", unit: Some(super::Unit::Px), value: super::Data::F32(ORDER_W_SIDE), doc: "The working-orders list: the BUY or SELL column's width." },
        super::Entry { group: "account", name: "ORDER_W_STATUS", unit: Some(super::Unit::Px), value: super::Data::F32(ORDER_W_STATUS), doc: "The working-orders list: the status column's width." },
        super::Entry { group: "account", name: "ORDER_W_TYPE", unit: Some(super::Unit::Px), value: super::Data::F32(ORDER_W_TYPE), doc: "The working-orders list: the order-type column's width." },
        super::Entry { group: "account", name: "WM", unit: Some(super::Unit::Px), value: super::Data::F32(WM), doc: "The Account window's per-venue table: the mode tag (paper, demo, live) column's width." },
        super::Entry { group: "account", name: "WN", unit: Some(super::Unit::Px), value: super::Data::F32(WN), doc: "The Account window's per-venue table: each numeric column's width (equity, unrealised PnL, fees, funding)." },
        super::Entry { group: "account", name: "WV", unit: Some(super::Unit::Px), value: super::Data::F32(WV), doc: "The Account window's per-venue table: the venue-name column's width." },
    ];
}

/// Backend Settings: the `[[value]]` rows of `ui-theme.toml` whose `group` is `backend_settings`.
pub mod backend_settings {
    /// How far the Backend settings table's columns shrink together before they stop shrinking and the row scrolls instead.
    pub const MIN_COL_SCALE: f32 = 0.55;
    /// The Backend settings editor's New value text field: its width.
    pub const EDIT_FIELD_W: f32 = 220.0;
    /// The Backend settings table's sticky header row: its height.
    pub const HEADER_H: f32 = 18.0;
    /// How tall the Backend settings table's rows grow before they scroll.
    pub const ROWS_MAX_H: f32 = 300.0;
    /// The Backend settings table's data rows: the height of one row.
    pub const ROW_H: f32 = 17.0;
    /// The Backend settings table's edit-button column: its width at full size.
    pub const W_EDIT: f32 = 44.0;
    /// The Backend settings table's Key column: its width at full size (the columns shrink together on a narrow window).
    pub const W_KEY: f32 = 230.0;
    /// The Backend settings table's Origin column (where the value comes from): its width at full size.
    pub const W_ORIGIN: f32 = 120.0;
    /// The Backend settings table's Read column (whether the daemon reads the key): its width at full size.
    pub const W_READ: f32 = 64.0;
    /// The Backend settings table's Value column: its width at full size.
    pub const W_VALUE: f32 = 150.0;

    /// Every row of this group as data, in the order the TOML lists them.
    pub const ENTRIES: &[super::Entry] = &[
        super::Entry { group: "backend_settings", name: "MIN_COL_SCALE", unit: Some(super::Unit::Ratio), value: super::Data::F32(MIN_COL_SCALE), doc: "How far the Backend settings table's columns shrink together before they stop shrinking and the row scrolls instead." },
        super::Entry { group: "backend_settings", name: "EDIT_FIELD_W", unit: Some(super::Unit::Px), value: super::Data::F32(EDIT_FIELD_W), doc: "The Backend settings editor's New value text field: its width." },
        super::Entry { group: "backend_settings", name: "HEADER_H", unit: Some(super::Unit::Px), value: super::Data::F32(HEADER_H), doc: "The Backend settings table's sticky header row: its height." },
        super::Entry { group: "backend_settings", name: "ROWS_MAX_H", unit: Some(super::Unit::Px), value: super::Data::F32(ROWS_MAX_H), doc: "How tall the Backend settings table's rows grow before they scroll." },
        super::Entry { group: "backend_settings", name: "ROW_H", unit: Some(super::Unit::Px), value: super::Data::F32(ROW_H), doc: "The Backend settings table's data rows: the height of one row." },
        super::Entry { group: "backend_settings", name: "W_EDIT", unit: Some(super::Unit::Px), value: super::Data::F32(W_EDIT), doc: "The Backend settings table's edit-button column: its width at full size." },
        super::Entry { group: "backend_settings", name: "W_KEY", unit: Some(super::Unit::Px), value: super::Data::F32(W_KEY), doc: "The Backend settings table's Key column: its width at full size (the columns shrink together on a narrow window)." },
        super::Entry { group: "backend_settings", name: "W_ORIGIN", unit: Some(super::Unit::Px), value: super::Data::F32(W_ORIGIN), doc: "The Backend settings table's Origin column (where the value comes from): its width at full size." },
        super::Entry { group: "backend_settings", name: "W_READ", unit: Some(super::Unit::Px), value: super::Data::F32(W_READ), doc: "The Backend settings table's Read column (whether the daemon reads the key): its width at full size." },
        super::Entry { group: "backend_settings", name: "W_VALUE", unit: Some(super::Unit::Px), value: super::Data::F32(W_VALUE), doc: "The Backend settings table's Value column: its width at full size." },
    ];
}

/// Calendar: the `[[value]]` rows of `ui-theme.toml` whose `group` is `calendar`.
pub mod calendar {
    /// A Calendar day-strip card: its widest width.
    pub const CARD_MAX_W: f32 = 320.0;
    /// A Calendar day-strip card: its least height.
    pub const CARD_MIN_H: f32 = 64.0;
    /// A Calendar day-strip card (Mon to Sun): its narrowest width.
    pub const CARD_MIN_W: f32 = 90.0;
    /// The Calendar economic table's Country column (flag and name): its width.
    pub const COL_COUNTRY_W: f32 = 150.0;
    /// The Calendar economic table's impact glyph column: its width.
    pub const COL_IMPACT_W: f32 = 24.0;
    /// The Calendar economic table's Time column: its width.
    pub const COL_TIME_W: f32 = 52.0;
    /// The Calendar toolbar's Countries button: its least width.
    pub const COUNTRIES_BTN_W: f32 = 110.0;
    /// The Calendar's Dividends table Amount column: its share of the table's width against the other columns.
    pub const DIV_WEIGHT_AMOUNT: f32 = 1.0;
    /// The Calendar's Dividends table Ex-date column: its share of the table's width against the other columns.
    pub const DIV_WEIGHT_EX_DATE: f32 = 1.1;
    /// The Calendar's Dividends table Freq column: its share of the table's width against the other columns.
    pub const DIV_WEIGHT_FREQ: f32 = 0.9;
    /// The Calendar's Dividends table Pay date column: its share of the table's width against the other columns.
    pub const DIV_WEIGHT_PAY_DATE: f32 = 1.1;
    /// The Calendar's Dividends table Symbol column: its share of the table's width against the other columns.
    pub const DIV_WEIGHT_SYMBOL: f32 = 1.0;
    /// The Calendar's Dividends table Yield column: its share of the table's width against the other columns.
    pub const DIV_WEIGHT_YIELD: f32 = 0.9;
    /// The Calendar's Earnings table EPS act. column: its share of the table's width against the other columns.
    pub const EARN_WEIGHT_EPS_ACT: f32 = 1.0;
    /// The Calendar's Earnings table EPS est. column: its share of the table's width against the other columns.
    pub const EARN_WEIGHT_EPS_EST: f32 = 1.0;
    /// The Calendar's Earnings table Surprise column: its share of the table's width against the other columns.
    pub const EARN_WEIGHT_SURPRISE: f32 = 1.0;
    /// The Calendar's Earnings table Symbol column: its share of the table's width against the other columns.
    pub const EARN_WEIGHT_SYMBOL: f32 = 1.3;
    /// The Calendar's Earnings table Time column: its share of the table's width against the other columns.
    pub const EARN_WEIGHT_TIME: f32 = 1.0;
    /// The Earnings, Dividends and IPO tables' right margin, taken off the panel width before the columns share it.
    pub const EQUITY_RIGHT_PAD: f32 = 16.0;
    /// The Calendar economic table's Event column: its narrowest width, however narrow the panel.
    pub const EVENT_MIN_W: f32 = 140.0;
    /// The drawn EU flag's centre dot: its radius.
    pub const FLAG_EU_DOT_R: f32 = 2.0;
    /// A country flag in the Calendar economic table: its size.
    pub const FLAG_SIZE: egui::Vec2 = egui::vec2(20.0, 14.0);
    /// The height of a Calendar table's header row.
    pub const HEADER_H: f32 = 20.0;
    /// The three-bar impact glyph in the Calendar economic table: its box.
    pub const IMPACT_SIZE: egui::Vec2 = egui::vec2(18.0, 14.0);
    /// The Calendar's IPO table Company column: its share of the table's width against the other columns.
    pub const IPO_WEIGHT_COMPANY: f32 = 2.2;
    /// The Calendar's IPO table Exchange column: its share of the table's width against the other columns.
    pub const IPO_WEIGHT_EXCHANGE: f32 = 1.0;
    /// The Calendar's IPO table Price column: its share of the table's width against the other columns.
    pub const IPO_WEIGHT_PRICE: f32 = 0.9;
    /// The Calendar's IPO table Shares column: its share of the table's width against the other columns.
    pub const IPO_WEIGHT_SHARES: f32 = 1.1;
    /// The Calendar's IPO table Status column: its share of the table's width against the other columns.
    pub const IPO_WEIGHT_STATUS: f32 = 0.9;
    /// The Calendar's IPO table Symbol column: its share of the table's width against the other columns.
    pub const IPO_WEIGHT_SYMBOL: f32 = 1.0;
    /// The Calendar toolbar's Local (time zone) button: its least width.
    pub const LOCAL_BTN_W: f32 = 96.0;
    /// Below the wide breakpoint, each of the Actual, Forecast and Prior columns takes this share of the panel's width.
    pub const NUM_COL_FRAC: f32 = 0.14;
    /// The narrowest the Actual, Forecast and Prior columns get on a narrow panel.
    pub const NUM_COL_MIN_W: f32 = 90.0;
    /// The Calendar economic table's Actual, Forecast and Prior columns: each one's width on a wide panel, and the most one gets on a narrow one.
    pub const NUM_COL_W: f32 = 210.0;
    /// The Calendar panel width from which the economic table's Actual, Forecast and Prior columns take their full width.
    pub const NUM_COL_WIDE_AT: f32 = 980.0;
    /// The economic table's right margin: the panel's own padding plus scrollbar clearance, so the Prior column does not collide with the window edge.
    pub const RIGHT_PAD: f32 = 52.0;
    /// The height of a Calendar table's data row.
    pub const ROW_H: f32 = 19.0;
    /// Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): the red field of the China and Hong Kong flags.
    pub const FLAG_CN_RED: egui::Color32 = egui::Color32::from_rgb(222, 41, 16);
    /// Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): Germany's black band.
    pub const FLAG_DE_BLACK: egui::Color32 = egui::Color32::from_rgb(0, 0, 0);
    /// Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): Germany's gold band.
    pub const FLAG_DE_GOLD: egui::Color32 = egui::Color32::from_rgb(255, 206, 0);
    /// Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): Germany's red band.
    pub const FLAG_DE_RED: egui::Color32 = egui::Color32::from_rgb(221, 0, 0);
    /// Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): the European Union's blue field.
    pub const FLAG_EU_BLUE: egui::Color32 = egui::Color32::from_rgb(0, 51, 153);
    /// Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): the European Union's gold dot.
    pub const FLAG_EU_GOLD: egui::Color32 = egui::Color32::from_rgb(255, 204, 0);
    /// Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): France's blue band.
    pub const FLAG_FR_BLUE: egui::Color32 = egui::Color32::from_rgb(0, 35, 149);
    /// Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): France's red band.
    pub const FLAG_FR_RED: egui::Color32 = egui::Color32::from_rgb(237, 41, 57);
    /// Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): Indonesia's red band.
    pub const FLAG_ID_RED: egui::Color32 = egui::Color32::from_rgb(231, 0, 17);
    /// Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): India's green band.
    pub const FLAG_IN_GREEN: egui::Color32 = egui::Color32::from_rgb(19, 136, 8);
    /// Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): India's saffron band.
    pub const FLAG_IN_SAFFRON: egui::Color32 = egui::Color32::from_rgb(255, 153, 51);
    /// Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): Italy's green band.
    pub const FLAG_IT_GREEN: egui::Color32 = egui::Color32::from_rgb(0, 146, 70);
    /// Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): Italy's red band.
    pub const FLAG_IT_RED: egui::Color32 = egui::Color32::from_rgb(206, 43, 55);
    /// Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): Japan's red disc.
    pub const FLAG_JP_RED: egui::Color32 = egui::Color32::from_rgb(188, 0, 45);
    /// Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): Mexico's green band.
    pub const FLAG_MX_GREEN: egui::Color32 = egui::Color32::from_rgb(0, 104, 71);
    /// Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): Mexico's red band.
    pub const FLAG_MX_RED: egui::Color32 = egui::Color32::from_rgb(206, 17, 38);
    /// Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): the navy blue field of the AU, NZ, US and GB flags.
    pub const FLAG_NAVY: egui::Color32 = egui::Color32::from_rgb(1, 33, 105);
    /// Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): the red of the CA, US, GB and CH flags.
    pub const FLAG_RED: egui::Color32 = egui::Color32::from_rgb(207, 20, 43);
    /// Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): Russia's blue band.
    pub const FLAG_RU_BLUE: egui::Color32 = egui::Color32::from_rgb(0, 57, 166);
    /// Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): Russia's red band.
    pub const FLAG_RU_RED: egui::Color32 = egui::Color32::from_rgb(213, 43, 30);
    /// Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): Saudi Arabia's green field.
    pub const FLAG_SA_GREEN: egui::Color32 = egui::Color32::from_rgb(0, 108, 53);
    /// Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): Singapore's red band.
    pub const FLAG_SG_RED: egui::Color32 = egui::Color32::from_rgb(239, 51, 64);
    /// Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): Turkey's red field.
    pub const FLAG_TR_RED: egui::Color32 = egui::Color32::from_rgb(227, 10, 23);
    /// Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): the placeholder grey block drawn for a country that has no drawn flag.
    pub const FLAG_UNKNOWN: egui::Color32 = egui::Color32::from_rgb(60, 66, 74);
    /// Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): the white band or field of the FR, IT, IN, RU, MX, CA, ID, SG, ZA, US, JP and CH flags.
    pub const FLAG_WHITE: egui::Color32 = egui::Color32::from_rgb(255, 255, 255);
    /// Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): South Africa's green band.
    pub const FLAG_ZA_GREEN: egui::Color32 = egui::Color32::from_rgb(0, 122, 77);
    /// Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): South Africa's red band.
    pub const FLAG_ZA_RED: egui::Color32 = egui::Color32::from_rgb(222, 56, 49);

    /// Every row of this group as data, in the order the TOML lists them.
    pub const ENTRIES: &[super::Entry] = &[
        super::Entry { group: "calendar", name: "CARD_MAX_W", unit: Some(super::Unit::Px), value: super::Data::F32(CARD_MAX_W), doc: "A Calendar day-strip card: its widest width." },
        super::Entry { group: "calendar", name: "CARD_MIN_H", unit: Some(super::Unit::Px), value: super::Data::F32(CARD_MIN_H), doc: "A Calendar day-strip card: its least height." },
        super::Entry { group: "calendar", name: "CARD_MIN_W", unit: Some(super::Unit::Px), value: super::Data::F32(CARD_MIN_W), doc: "A Calendar day-strip card (Mon to Sun): its narrowest width." },
        super::Entry { group: "calendar", name: "COL_COUNTRY_W", unit: Some(super::Unit::Px), value: super::Data::F32(COL_COUNTRY_W), doc: "The Calendar economic table's Country column (flag and name): its width." },
        super::Entry { group: "calendar", name: "COL_IMPACT_W", unit: Some(super::Unit::Px), value: super::Data::F32(COL_IMPACT_W), doc: "The Calendar economic table's impact glyph column: its width." },
        super::Entry { group: "calendar", name: "COL_TIME_W", unit: Some(super::Unit::Px), value: super::Data::F32(COL_TIME_W), doc: "The Calendar economic table's Time column: its width." },
        super::Entry { group: "calendar", name: "COUNTRIES_BTN_W", unit: Some(super::Unit::Px), value: super::Data::F32(COUNTRIES_BTN_W), doc: "The Calendar toolbar's Countries button: its least width." },
        super::Entry { group: "calendar", name: "DIV_WEIGHT_AMOUNT", unit: Some(super::Unit::Ratio), value: super::Data::F32(DIV_WEIGHT_AMOUNT), doc: "The Calendar's Dividends table Amount column: its share of the table's width against the other columns." },
        super::Entry { group: "calendar", name: "DIV_WEIGHT_EX_DATE", unit: Some(super::Unit::Ratio), value: super::Data::F32(DIV_WEIGHT_EX_DATE), doc: "The Calendar's Dividends table Ex-date column: its share of the table's width against the other columns." },
        super::Entry { group: "calendar", name: "DIV_WEIGHT_FREQ", unit: Some(super::Unit::Ratio), value: super::Data::F32(DIV_WEIGHT_FREQ), doc: "The Calendar's Dividends table Freq column: its share of the table's width against the other columns." },
        super::Entry { group: "calendar", name: "DIV_WEIGHT_PAY_DATE", unit: Some(super::Unit::Ratio), value: super::Data::F32(DIV_WEIGHT_PAY_DATE), doc: "The Calendar's Dividends table Pay date column: its share of the table's width against the other columns." },
        super::Entry { group: "calendar", name: "DIV_WEIGHT_SYMBOL", unit: Some(super::Unit::Ratio), value: super::Data::F32(DIV_WEIGHT_SYMBOL), doc: "The Calendar's Dividends table Symbol column: its share of the table's width against the other columns." },
        super::Entry { group: "calendar", name: "DIV_WEIGHT_YIELD", unit: Some(super::Unit::Ratio), value: super::Data::F32(DIV_WEIGHT_YIELD), doc: "The Calendar's Dividends table Yield column: its share of the table's width against the other columns." },
        super::Entry { group: "calendar", name: "EARN_WEIGHT_EPS_ACT", unit: Some(super::Unit::Ratio), value: super::Data::F32(EARN_WEIGHT_EPS_ACT), doc: "The Calendar's Earnings table EPS act. column: its share of the table's width against the other columns." },
        super::Entry { group: "calendar", name: "EARN_WEIGHT_EPS_EST", unit: Some(super::Unit::Ratio), value: super::Data::F32(EARN_WEIGHT_EPS_EST), doc: "The Calendar's Earnings table EPS est. column: its share of the table's width against the other columns." },
        super::Entry { group: "calendar", name: "EARN_WEIGHT_SURPRISE", unit: Some(super::Unit::Ratio), value: super::Data::F32(EARN_WEIGHT_SURPRISE), doc: "The Calendar's Earnings table Surprise column: its share of the table's width against the other columns." },
        super::Entry { group: "calendar", name: "EARN_WEIGHT_SYMBOL", unit: Some(super::Unit::Ratio), value: super::Data::F32(EARN_WEIGHT_SYMBOL), doc: "The Calendar's Earnings table Symbol column: its share of the table's width against the other columns." },
        super::Entry { group: "calendar", name: "EARN_WEIGHT_TIME", unit: Some(super::Unit::Ratio), value: super::Data::F32(EARN_WEIGHT_TIME), doc: "The Calendar's Earnings table Time column: its share of the table's width against the other columns." },
        super::Entry { group: "calendar", name: "EQUITY_RIGHT_PAD", unit: Some(super::Unit::Px), value: super::Data::F32(EQUITY_RIGHT_PAD), doc: "The Earnings, Dividends and IPO tables' right margin, taken off the panel width before the columns share it." },
        super::Entry { group: "calendar", name: "EVENT_MIN_W", unit: Some(super::Unit::Px), value: super::Data::F32(EVENT_MIN_W), doc: "The Calendar economic table's Event column: its narrowest width, however narrow the panel." },
        super::Entry { group: "calendar", name: "FLAG_EU_DOT_R", unit: Some(super::Unit::Px), value: super::Data::F32(FLAG_EU_DOT_R), doc: "The drawn EU flag's centre dot: its radius." },
        super::Entry { group: "calendar", name: "FLAG_SIZE", unit: Some(super::Unit::Px), value: super::Data::Vec2(FLAG_SIZE), doc: "A country flag in the Calendar economic table: its size." },
        super::Entry { group: "calendar", name: "HEADER_H", unit: Some(super::Unit::Px), value: super::Data::F32(HEADER_H), doc: "The height of a Calendar table's header row." },
        super::Entry { group: "calendar", name: "IMPACT_SIZE", unit: Some(super::Unit::Px), value: super::Data::Vec2(IMPACT_SIZE), doc: "The three-bar impact glyph in the Calendar economic table: its box." },
        super::Entry { group: "calendar", name: "IPO_WEIGHT_COMPANY", unit: Some(super::Unit::Ratio), value: super::Data::F32(IPO_WEIGHT_COMPANY), doc: "The Calendar's IPO table Company column: its share of the table's width against the other columns." },
        super::Entry { group: "calendar", name: "IPO_WEIGHT_EXCHANGE", unit: Some(super::Unit::Ratio), value: super::Data::F32(IPO_WEIGHT_EXCHANGE), doc: "The Calendar's IPO table Exchange column: its share of the table's width against the other columns." },
        super::Entry { group: "calendar", name: "IPO_WEIGHT_PRICE", unit: Some(super::Unit::Ratio), value: super::Data::F32(IPO_WEIGHT_PRICE), doc: "The Calendar's IPO table Price column: its share of the table's width against the other columns." },
        super::Entry { group: "calendar", name: "IPO_WEIGHT_SHARES", unit: Some(super::Unit::Ratio), value: super::Data::F32(IPO_WEIGHT_SHARES), doc: "The Calendar's IPO table Shares column: its share of the table's width against the other columns." },
        super::Entry { group: "calendar", name: "IPO_WEIGHT_STATUS", unit: Some(super::Unit::Ratio), value: super::Data::F32(IPO_WEIGHT_STATUS), doc: "The Calendar's IPO table Status column: its share of the table's width against the other columns." },
        super::Entry { group: "calendar", name: "IPO_WEIGHT_SYMBOL", unit: Some(super::Unit::Ratio), value: super::Data::F32(IPO_WEIGHT_SYMBOL), doc: "The Calendar's IPO table Symbol column: its share of the table's width against the other columns." },
        super::Entry { group: "calendar", name: "LOCAL_BTN_W", unit: Some(super::Unit::Px), value: super::Data::F32(LOCAL_BTN_W), doc: "The Calendar toolbar's Local (time zone) button: its least width." },
        super::Entry { group: "calendar", name: "NUM_COL_FRAC", unit: Some(super::Unit::Ratio), value: super::Data::F32(NUM_COL_FRAC), doc: "Below the wide breakpoint, each of the Actual, Forecast and Prior columns takes this share of the panel's width." },
        super::Entry { group: "calendar", name: "NUM_COL_MIN_W", unit: Some(super::Unit::Px), value: super::Data::F32(NUM_COL_MIN_W), doc: "The narrowest the Actual, Forecast and Prior columns get on a narrow panel." },
        super::Entry { group: "calendar", name: "NUM_COL_W", unit: Some(super::Unit::Px), value: super::Data::F32(NUM_COL_W), doc: "The Calendar economic table's Actual, Forecast and Prior columns: each one's width on a wide panel, and the most one gets on a narrow one." },
        super::Entry { group: "calendar", name: "NUM_COL_WIDE_AT", unit: Some(super::Unit::Px), value: super::Data::F32(NUM_COL_WIDE_AT), doc: "The Calendar panel width from which the economic table's Actual, Forecast and Prior columns take their full width." },
        super::Entry { group: "calendar", name: "RIGHT_PAD", unit: Some(super::Unit::Px), value: super::Data::F32(RIGHT_PAD), doc: "The economic table's right margin: the panel's own padding plus scrollbar clearance, so the Prior column does not collide with the window edge." },
        super::Entry { group: "calendar", name: "ROW_H", unit: Some(super::Unit::Px), value: super::Data::F32(ROW_H), doc: "The height of a Calendar table's data row." },
        super::Entry { group: "calendar", name: "FLAG_CN_RED", unit: None, value: super::Data::Colour(FLAG_CN_RED), doc: "Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): the red field of the China and Hong Kong flags." },
        super::Entry { group: "calendar", name: "FLAG_DE_BLACK", unit: None, value: super::Data::Colour(FLAG_DE_BLACK), doc: "Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): Germany's black band." },
        super::Entry { group: "calendar", name: "FLAG_DE_GOLD", unit: None, value: super::Data::Colour(FLAG_DE_GOLD), doc: "Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): Germany's gold band." },
        super::Entry { group: "calendar", name: "FLAG_DE_RED", unit: None, value: super::Data::Colour(FLAG_DE_RED), doc: "Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): Germany's red band." },
        super::Entry { group: "calendar", name: "FLAG_EU_BLUE", unit: None, value: super::Data::Colour(FLAG_EU_BLUE), doc: "Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): the European Union's blue field." },
        super::Entry { group: "calendar", name: "FLAG_EU_GOLD", unit: None, value: super::Data::Colour(FLAG_EU_GOLD), doc: "Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): the European Union's gold dot." },
        super::Entry { group: "calendar", name: "FLAG_FR_BLUE", unit: None, value: super::Data::Colour(FLAG_FR_BLUE), doc: "Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): France's blue band." },
        super::Entry { group: "calendar", name: "FLAG_FR_RED", unit: None, value: super::Data::Colour(FLAG_FR_RED), doc: "Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): France's red band." },
        super::Entry { group: "calendar", name: "FLAG_ID_RED", unit: None, value: super::Data::Colour(FLAG_ID_RED), doc: "Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): Indonesia's red band." },
        super::Entry { group: "calendar", name: "FLAG_IN_GREEN", unit: None, value: super::Data::Colour(FLAG_IN_GREEN), doc: "Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): India's green band." },
        super::Entry { group: "calendar", name: "FLAG_IN_SAFFRON", unit: None, value: super::Data::Colour(FLAG_IN_SAFFRON), doc: "Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): India's saffron band." },
        super::Entry { group: "calendar", name: "FLAG_IT_GREEN", unit: None, value: super::Data::Colour(FLAG_IT_GREEN), doc: "Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): Italy's green band." },
        super::Entry { group: "calendar", name: "FLAG_IT_RED", unit: None, value: super::Data::Colour(FLAG_IT_RED), doc: "Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): Italy's red band." },
        super::Entry { group: "calendar", name: "FLAG_JP_RED", unit: None, value: super::Data::Colour(FLAG_JP_RED), doc: "Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): Japan's red disc." },
        super::Entry { group: "calendar", name: "FLAG_MX_GREEN", unit: None, value: super::Data::Colour(FLAG_MX_GREEN), doc: "Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): Mexico's green band." },
        super::Entry { group: "calendar", name: "FLAG_MX_RED", unit: None, value: super::Data::Colour(FLAG_MX_RED), doc: "Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): Mexico's red band." },
        super::Entry { group: "calendar", name: "FLAG_NAVY", unit: None, value: super::Data::Colour(FLAG_NAVY), doc: "Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): the navy blue field of the AU, NZ, US and GB flags." },
        super::Entry { group: "calendar", name: "FLAG_RED", unit: None, value: super::Data::Colour(FLAG_RED), doc: "Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): the red of the CA, US, GB and CH flags." },
        super::Entry { group: "calendar", name: "FLAG_RU_BLUE", unit: None, value: super::Data::Colour(FLAG_RU_BLUE), doc: "Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): Russia's blue band." },
        super::Entry { group: "calendar", name: "FLAG_RU_RED", unit: None, value: super::Data::Colour(FLAG_RU_RED), doc: "Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): Russia's red band." },
        super::Entry { group: "calendar", name: "FLAG_SA_GREEN", unit: None, value: super::Data::Colour(FLAG_SA_GREEN), doc: "Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): Saudi Arabia's green field." },
        super::Entry { group: "calendar", name: "FLAG_SG_RED", unit: None, value: super::Data::Colour(FLAG_SG_RED), doc: "Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): Singapore's red band." },
        super::Entry { group: "calendar", name: "FLAG_TR_RED", unit: None, value: super::Data::Colour(FLAG_TR_RED), doc: "Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): Turkey's red field." },
        super::Entry { group: "calendar", name: "FLAG_UNKNOWN", unit: None, value: super::Data::Colour(FLAG_UNKNOWN), doc: "Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): the placeholder grey block drawn for a country that has no drawn flag." },
        super::Entry { group: "calendar", name: "FLAG_WHITE", unit: None, value: super::Data::Colour(FLAG_WHITE), doc: "Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): the white band or field of the FR, IT, IN, RU, MX, CA, ID, SG, ZA, US, JP and CH flags." },
        super::Entry { group: "calendar", name: "FLAG_ZA_GREEN", unit: None, value: super::Data::Colour(FLAG_ZA_GREEN), doc: "Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): South Africa's green band." },
        super::Entry { group: "calendar", name: "FLAG_ZA_RED", unit: None, value: super::Data::Colour(FLAG_ZA_RED), doc: "Drawn-flag fallback in the economic table's Country cell (shown until the flag image has loaded, or offline): South Africa's red band." },
    ];
}

/// Caption: the `[[value]]` rows of `ui-theme.toml` whose `group` is `caption`.
pub mod caption {
    /// How far from the caption's left edge the command palette field may start, clearing the mark, the menus and the GPU toggle.
    pub const PALETTE_LEFT_CLEAR: f32 = 190.0;
    /// How far from the caption's right edge the command palette field may end, clearing the tool launchers and the window controls.
    pub const PALETTE_RIGHT_CLEAR: f32 = 280.0;
    /// The window caption bar's height: the frameless strip across the top of the main window (the Python app's TITLEBAR_H).
    pub const CAPTION_H: f32 = 32.0;
    /// One window control's width in the caption: minimise, maximise, close.
    pub const CONTROL_W: f32 = 34.0;
    /// A tool launcher's square in the caption; its icon sits 4 px inside it, which leaves the PNGs' 18 px design size.
    pub const LAUNCHER_BOX: f32 = 26.0;
    /// The Vike mark's footprint at the caption's left, in the old V tile's size so nothing beside it moves.
    pub const MARK_SIZE: egui::Vec2 = egui::vec2(24.0, 22.0);
    /// The command palette field's height in the caption (fixed chrome, it does not follow density).
    pub const PALETTE_H: f32 = 24.0;
    /// The command palette field's widest width in the caption.
    pub const PALETTE_MAX_W: f32 = 360.0;
    /// The command palette field's narrowest width when the caption is squeezed.
    pub const PALETTE_MIN_W: f32 = 120.0;
    /// The command palette field is the gap between the launcher cluster and the window controls less this much, shared by its two sides.
    pub const PALETTE_SIDE_GAP: f32 = 12.0;

    /// Every row of this group as data, in the order the TOML lists them.
    pub const ENTRIES: &[super::Entry] = &[
        super::Entry { group: "caption", name: "PALETTE_LEFT_CLEAR", unit: Some(super::Unit::Px), value: super::Data::F32(PALETTE_LEFT_CLEAR), doc: "How far from the caption's left edge the command palette field may start, clearing the mark, the menus and the GPU toggle." },
        super::Entry { group: "caption", name: "PALETTE_RIGHT_CLEAR", unit: Some(super::Unit::Px), value: super::Data::F32(PALETTE_RIGHT_CLEAR), doc: "How far from the caption's right edge the command palette field may end, clearing the tool launchers and the window controls." },
        super::Entry { group: "caption", name: "CAPTION_H", unit: Some(super::Unit::Px), value: super::Data::F32(CAPTION_H), doc: "The window caption bar's height: the frameless strip across the top of the main window (the Python app's TITLEBAR_H)." },
        super::Entry { group: "caption", name: "CONTROL_W", unit: Some(super::Unit::Px), value: super::Data::F32(CONTROL_W), doc: "One window control's width in the caption: minimise, maximise, close." },
        super::Entry { group: "caption", name: "LAUNCHER_BOX", unit: Some(super::Unit::Px), value: super::Data::F32(LAUNCHER_BOX), doc: "A tool launcher's square in the caption; its icon sits 4 px inside it, which leaves the PNGs' 18 px design size." },
        super::Entry { group: "caption", name: "MARK_SIZE", unit: Some(super::Unit::Px), value: super::Data::Vec2(MARK_SIZE), doc: "The Vike mark's footprint at the caption's left, in the old V tile's size so nothing beside it moves." },
        super::Entry { group: "caption", name: "PALETTE_H", unit: Some(super::Unit::Px), value: super::Data::F32(PALETTE_H), doc: "The command palette field's height in the caption (fixed chrome, it does not follow density)." },
        super::Entry { group: "caption", name: "PALETTE_MAX_W", unit: Some(super::Unit::Px), value: super::Data::F32(PALETTE_MAX_W), doc: "The command palette field's widest width in the caption." },
        super::Entry { group: "caption", name: "PALETTE_MIN_W", unit: Some(super::Unit::Px), value: super::Data::F32(PALETTE_MIN_W), doc: "The command palette field's narrowest width when the caption is squeezed." },
        super::Entry { group: "caption", name: "PALETTE_SIDE_GAP", unit: Some(super::Unit::Px), value: super::Data::F32(PALETTE_SIDE_GAP), doc: "The command palette field is the gap between the launcher cluster and the window controls less this much, shared by its two sides." },
    ];
}

/// Chart: the `[[value]]` rows of `ui-theme.toml` whose `group` is `chart`.
pub mod chart {
    /// How far above the price frame's bottom edge the top of the navigation row (zoom, pan, reset view, settings) sits.
    pub const NAV_ROW_FROM_BOTTOM: f32 = 44.0;
    /// The height of the strip along the top of a chart pane where the crosshair price tag is not drawn, because the window's title-bar controls sit there.
    pub const PRICE_TAG_HIDDEN_STRIP_H: f32 = 26.0;
    /// How far below the price frame's top edge the scale-fallback hint is drawn.
    pub const SCALE_HINT_FROM_TOP: f32 = 30.0;
    /// How far above the price frame's bottom edge the top of the Lin/Log/% scale row sits.
    pub const SCALE_ROW_FROM_BOTTOM: f32 = 34.0;
    /// A candle's body: how far it reaches either side of its bar's centre, in bar widths (the body is twice this wide, so neighbouring candles keep a gutter).
    pub const CANDLE_BODY_HALF_W: f64 = 0.34;
    /// A footprint cell: how far it reaches either side of its bar's centre, in bar widths; wider than a candle body so the cells read as a near-contiguous grid, shy of 0.5 so neighbouring bars keep a thin gutter.
    pub const FOOTPRINT_CELL_HALF_W: f64 = 0.45;
    /// A histogram output in an indicator or study pane (the MACD histogram and the like): the width of one bar, in bar widths.
    pub const HISTOGRAM_BAR_W: f64 = 0.8;
    /// An OHLC bar's open tick (left of its stem) and close tick (right of it), and an HLC bar's close tick: how far each reaches from the stem, in bar widths.
    pub const OHLC_TICK_W: f64 = 0.32;
    /// The volume pane's bars: the width of one bar, in bar widths, so a thin gap is left between neighbours.
    pub const VOLUME_BAR_W: f64 = 0.8;
    /// The fill of the Area and HLC-area style icons: the up colour at this strength of itself.
    pub const AREA_ICON_FILL: f32 = 0.35;
    /// The Area style's gradient fill at the line: the opacity of the topmost band, fading to transparent at the floor.
    pub const AREA_TOP_ALPHA: u8 = 55;
    /// The strip of time labels under the bottom chart pane, and the height of the drag-to-zoom strip laid over it.
    pub const AXIS_LABEL_H: f32 = 18.0;
    /// The corner radius of the tags on the chart's axes: last price, crosshair price, indicator value and crosshair time.
    pub const AXIS_TAG_RADIUS: f32 = 3.0;
    /// The Baseline style's wash above and below the anchor line: the up and down colours at this opacity.
    pub const BASELINE_WASH_ALPHA: u8 = 28;
    /// The footprint cell's tint behind its buy and sell numbers, a faint imbalance wash; a neutral cell takes half of it.
    pub const CELL_FILL_ALPHA: u8 = 40;
    /// The Columns style's column fill: the bar's direction colour at this opacity.
    pub const COLUMN_FILL_ALPHA: u8 = 150;
    /// The dot radius of an indicator that draws as dots.
    pub const DOT_MARKER_RADIUS: f32 = 2.0;
    /// How strongly the ghost crosshair (a synced chart's pointer) is drawn: this share of the crosshair colour.
    pub const GHOST_STRENGTH: f32 = 0.4;
    /// The opacity of the scale-fallback hint: the secondary text colour, present but under the data.
    pub const HINT_ALPHA: u8 = 150;
    /// The HLC-area style's fill between high and low: the line colour at this opacity.
    pub const HLC_FILL_ALPHA: u8 = 30;
    /// The radius of the dots in the Line-with-markers style's menu icon.
    pub const ICON_DOT_RADIUS: f32 = 1.5;
    /// The dot radius of the Line-with-markers chart style.
    pub const LINE_MARKER_RADIUS: f32 = 2.0;
    /// The least plot height a chart pane (price or indicator) can be dragged down to.
    pub const MIN_PANE_PX: f32 = 44.0;
    /// The vertical pitch of the indicator legends stacked over a merged pane's plot.
    pub const PANE_OVERLAY_ROW_H: f32 = 18.0;
    /// The thickness of the draggable strip between two stacked chart panes.
    pub const PANE_SEP_H: f32 = 5.0;
    /// The radius of the up or down glyph an indicator anchors to a flagged candlestick pattern.
    pub const PATTERN_MARKER_RADIUS: f32 = 4.0;
    /// The room kept at the price frame's bottom-right for the Lin/Log/% scale control and the Auto button.
    pub const SCALE_ROW_MAX_W: f32 = 200.0;
    /// The price chart's right price-axis gutter: the same width on every pane, and the width of the drag-to-zoom strip laid over it.
    pub const Y_AXIS_GUTTER_W: f32 = 72.0;
    /// The default overbought zone wash in an oscillator pane (RSI and the like) when its fill is on: green at alpha 30; the user can recolour it.
    pub const OVERBOUGHT_FILL: egui::Color32 = egui::Color32::from_rgba_unmultiplied_const(76, 175, 80, 30);
    /// The default oversold zone wash in an oscillator pane when its fill is on: red at alpha 30; the user can recolour it.
    pub const OVERSOLD_FILL: egui::Color32 = egui::Color32::from_rgba_unmultiplied_const(239, 83, 80, 30);
    /// The point of control, amber: the volume-profile line and the 1 px outline of each footprint bar's maximum-volume cell.
    pub const POC_AMBER: egui::Color32 = egui::Color32::from_rgb(240, 180, 41);
    /// The volume-profile histogram's opacity: series colour 1 at this alpha (the bar fill and outline derive from it).
    pub const PROFILE_ALPHA: u8 = 90;
    /// Series colour 1, blue: an indicator's first output line; also the CVD pane line, a microstructure study's line and the volume-profile bars.
    pub const SERIES_1: egui::Color32 = egui::Color32::from_rgb(87, 165, 255);
    /// Series colour 2, purple: an indicator's second output line.
    pub const SERIES_2: egui::Color32 = egui::Color32::from_rgb(168, 85, 247);
    /// Series colour 3, cyan: an indicator's third output line.
    pub const SERIES_3: egui::Color32 = egui::Color32::from_rgb(38, 198, 218);
    /// Series colour 4, green: an indicator's fourth output line.
    pub const SERIES_4: egui::Color32 = egui::Color32::from_rgb(102, 187, 106);
    /// Series colour 5, pink: an indicator's fifth output line.
    pub const SERIES_5: egui::Color32 = egui::Color32::from_rgb(236, 64, 122);
    /// Series colour 6, amber: an indicator's sixth output line (the smoothing-average default).
    pub const SERIES_6: egui::Color32 = egui::Color32::from_rgb(245, 166, 35);
    /// The value-area band behind price in the volume profile: series colour 1 at this alpha, a near-invisible wash.
    pub const VALUE_AREA_ALPHA: u8 = 18;

    /// Every row of this group as data, in the order the TOML lists them.
    pub const ENTRIES: &[super::Entry] = &[
        super::Entry { group: "chart", name: "NAV_ROW_FROM_BOTTOM", unit: Some(super::Unit::Px), value: super::Data::F32(NAV_ROW_FROM_BOTTOM), doc: "How far above the price frame's bottom edge the top of the navigation row (zoom, pan, reset view, settings) sits." },
        super::Entry { group: "chart", name: "PRICE_TAG_HIDDEN_STRIP_H", unit: Some(super::Unit::Px), value: super::Data::F32(PRICE_TAG_HIDDEN_STRIP_H), doc: "The height of the strip along the top of a chart pane where the crosshair price tag is not drawn, because the window's title-bar controls sit there." },
        super::Entry { group: "chart", name: "SCALE_HINT_FROM_TOP", unit: Some(super::Unit::Px), value: super::Data::F32(SCALE_HINT_FROM_TOP), doc: "How far below the price frame's top edge the scale-fallback hint is drawn." },
        super::Entry { group: "chart", name: "SCALE_ROW_FROM_BOTTOM", unit: Some(super::Unit::Px), value: super::Data::F32(SCALE_ROW_FROM_BOTTOM), doc: "How far above the price frame's bottom edge the top of the Lin/Log/% scale row sits." },
        super::Entry { group: "chart", name: "CANDLE_BODY_HALF_W", unit: Some(super::Unit::Ratio), value: super::Data::F64(CANDLE_BODY_HALF_W), doc: "A candle's body: how far it reaches either side of its bar's centre, in bar widths (the body is twice this wide, so neighbouring candles keep a gutter)." },
        super::Entry { group: "chart", name: "FOOTPRINT_CELL_HALF_W", unit: Some(super::Unit::Ratio), value: super::Data::F64(FOOTPRINT_CELL_HALF_W), doc: "A footprint cell: how far it reaches either side of its bar's centre, in bar widths; wider than a candle body so the cells read as a near-contiguous grid, shy of 0.5 so neighbouring bars keep a thin gutter." },
        super::Entry { group: "chart", name: "HISTOGRAM_BAR_W", unit: Some(super::Unit::Ratio), value: super::Data::F64(HISTOGRAM_BAR_W), doc: "A histogram output in an indicator or study pane (the MACD histogram and the like): the width of one bar, in bar widths." },
        super::Entry { group: "chart", name: "OHLC_TICK_W", unit: Some(super::Unit::Ratio), value: super::Data::F64(OHLC_TICK_W), doc: "An OHLC bar's open tick (left of its stem) and close tick (right of it), and an HLC bar's close tick: how far each reaches from the stem, in bar widths." },
        super::Entry { group: "chart", name: "VOLUME_BAR_W", unit: Some(super::Unit::Ratio), value: super::Data::F64(VOLUME_BAR_W), doc: "The volume pane's bars: the width of one bar, in bar widths, so a thin gap is left between neighbours." },
        super::Entry { group: "chart", name: "AREA_ICON_FILL", unit: Some(super::Unit::Ratio), value: super::Data::F32(AREA_ICON_FILL), doc: "The fill of the Area and HLC-area style icons: the up colour at this strength of itself." },
        super::Entry { group: "chart", name: "AREA_TOP_ALPHA", unit: Some(super::Unit::Alpha), value: super::Data::U8(AREA_TOP_ALPHA), doc: "The Area style's gradient fill at the line: the opacity of the topmost band, fading to transparent at the floor." },
        super::Entry { group: "chart", name: "AXIS_LABEL_H", unit: Some(super::Unit::Px), value: super::Data::F32(AXIS_LABEL_H), doc: "The strip of time labels under the bottom chart pane, and the height of the drag-to-zoom strip laid over it." },
        super::Entry { group: "chart", name: "AXIS_TAG_RADIUS", unit: Some(super::Unit::Px), value: super::Data::F32(AXIS_TAG_RADIUS), doc: "The corner radius of the tags on the chart's axes: last price, crosshair price, indicator value and crosshair time." },
        super::Entry { group: "chart", name: "BASELINE_WASH_ALPHA", unit: Some(super::Unit::Alpha), value: super::Data::U8(BASELINE_WASH_ALPHA), doc: "The Baseline style's wash above and below the anchor line: the up and down colours at this opacity." },
        super::Entry { group: "chart", name: "CELL_FILL_ALPHA", unit: Some(super::Unit::Alpha), value: super::Data::U8(CELL_FILL_ALPHA), doc: "The footprint cell's tint behind its buy and sell numbers, a faint imbalance wash; a neutral cell takes half of it." },
        super::Entry { group: "chart", name: "COLUMN_FILL_ALPHA", unit: Some(super::Unit::Alpha), value: super::Data::U8(COLUMN_FILL_ALPHA), doc: "The Columns style's column fill: the bar's direction colour at this opacity." },
        super::Entry { group: "chart", name: "DOT_MARKER_RADIUS", unit: Some(super::Unit::Px), value: super::Data::F32(DOT_MARKER_RADIUS), doc: "The dot radius of an indicator that draws as dots." },
        super::Entry { group: "chart", name: "GHOST_STRENGTH", unit: Some(super::Unit::Ratio), value: super::Data::F32(GHOST_STRENGTH), doc: "How strongly the ghost crosshair (a synced chart's pointer) is drawn: this share of the crosshair colour." },
        super::Entry { group: "chart", name: "HINT_ALPHA", unit: Some(super::Unit::Alpha), value: super::Data::U8(HINT_ALPHA), doc: "The opacity of the scale-fallback hint: the secondary text colour, present but under the data." },
        super::Entry { group: "chart", name: "HLC_FILL_ALPHA", unit: Some(super::Unit::Alpha), value: super::Data::U8(HLC_FILL_ALPHA), doc: "The HLC-area style's fill between high and low: the line colour at this opacity." },
        super::Entry { group: "chart", name: "ICON_DOT_RADIUS", unit: Some(super::Unit::Px), value: super::Data::F32(ICON_DOT_RADIUS), doc: "The radius of the dots in the Line-with-markers style's menu icon." },
        super::Entry { group: "chart", name: "LINE_MARKER_RADIUS", unit: Some(super::Unit::Px), value: super::Data::F32(LINE_MARKER_RADIUS), doc: "The dot radius of the Line-with-markers chart style." },
        super::Entry { group: "chart", name: "MIN_PANE_PX", unit: Some(super::Unit::Px), value: super::Data::F32(MIN_PANE_PX), doc: "The least plot height a chart pane (price or indicator) can be dragged down to." },
        super::Entry { group: "chart", name: "PANE_OVERLAY_ROW_H", unit: Some(super::Unit::Px), value: super::Data::F32(PANE_OVERLAY_ROW_H), doc: "The vertical pitch of the indicator legends stacked over a merged pane's plot." },
        super::Entry { group: "chart", name: "PANE_SEP_H", unit: Some(super::Unit::Px), value: super::Data::F32(PANE_SEP_H), doc: "The thickness of the draggable strip between two stacked chart panes." },
        super::Entry { group: "chart", name: "PATTERN_MARKER_RADIUS", unit: Some(super::Unit::Px), value: super::Data::F32(PATTERN_MARKER_RADIUS), doc: "The radius of the up or down glyph an indicator anchors to a flagged candlestick pattern." },
        super::Entry { group: "chart", name: "SCALE_ROW_MAX_W", unit: Some(super::Unit::Px), value: super::Data::F32(SCALE_ROW_MAX_W), doc: "The room kept at the price frame's bottom-right for the Lin/Log/% scale control and the Auto button." },
        super::Entry { group: "chart", name: "Y_AXIS_GUTTER_W", unit: Some(super::Unit::Px), value: super::Data::F32(Y_AXIS_GUTTER_W), doc: "The price chart's right price-axis gutter: the same width on every pane, and the width of the drag-to-zoom strip laid over it." },
        super::Entry { group: "chart", name: "OVERBOUGHT_FILL", unit: None, value: super::Data::Colour(OVERBOUGHT_FILL), doc: "The default overbought zone wash in an oscillator pane (RSI and the like) when its fill is on: green at alpha 30; the user can recolour it." },
        super::Entry { group: "chart", name: "OVERSOLD_FILL", unit: None, value: super::Data::Colour(OVERSOLD_FILL), doc: "The default oversold zone wash in an oscillator pane when its fill is on: red at alpha 30; the user can recolour it." },
        super::Entry { group: "chart", name: "POC_AMBER", unit: None, value: super::Data::Colour(POC_AMBER), doc: "The point of control, amber: the volume-profile line and the 1 px outline of each footprint bar's maximum-volume cell." },
        super::Entry { group: "chart", name: "PROFILE_ALPHA", unit: Some(super::Unit::Alpha), value: super::Data::U8(PROFILE_ALPHA), doc: "The volume-profile histogram's opacity: series colour 1 at this alpha (the bar fill and outline derive from it)." },
        super::Entry { group: "chart", name: "SERIES_1", unit: None, value: super::Data::Colour(SERIES_1), doc: "Series colour 1, blue: an indicator's first output line; also the CVD pane line, a microstructure study's line and the volume-profile bars." },
        super::Entry { group: "chart", name: "SERIES_2", unit: None, value: super::Data::Colour(SERIES_2), doc: "Series colour 2, purple: an indicator's second output line." },
        super::Entry { group: "chart", name: "SERIES_3", unit: None, value: super::Data::Colour(SERIES_3), doc: "Series colour 3, cyan: an indicator's third output line." },
        super::Entry { group: "chart", name: "SERIES_4", unit: None, value: super::Data::Colour(SERIES_4), doc: "Series colour 4, green: an indicator's fourth output line." },
        super::Entry { group: "chart", name: "SERIES_5", unit: None, value: super::Data::Colour(SERIES_5), doc: "Series colour 5, pink: an indicator's fifth output line." },
        super::Entry { group: "chart", name: "SERIES_6", unit: None, value: super::Data::Colour(SERIES_6), doc: "Series colour 6, amber: an indicator's sixth output line (the smoothing-average default)." },
        super::Entry { group: "chart", name: "VALUE_AREA_ALPHA", unit: Some(super::Unit::Alpha), value: super::Data::U8(VALUE_AREA_ALPHA), doc: "The value-area band behind price in the volume profile: series colour 1 at this alpha, a near-invisible wash." },
    ];
}

/// Chart Dialogs: the `[[value]]` rows of `ui-theme.toml` whose `group` is `chart_dialogs`.
pub mod chart_dialogs {
    /// The Chart settings window's opening width.
    pub const CHART_SETTINGS_W: f32 = 470.0;
    /// The indicator settings window's opening width.
    pub const INDICATOR_DIALOG_W: f32 = 300.0;
    /// The width of the dash-style selector beside each line on an indicator's Style tab.
    pub const LINE_DASH_COMBO_W: f32 = 78.0;
    /// The Chart settings dialog's minimum content width.
    pub const SETTINGS_MIN_W: f32 = 300.0;
    /// The Source selector's width on an indicator's Inputs tab.
    pub const SOURCE_COMBO_W: f32 = 110.0;

    /// Every row of this group as data, in the order the TOML lists them.
    pub const ENTRIES: &[super::Entry] = &[
        super::Entry { group: "chart_dialogs", name: "CHART_SETTINGS_W", unit: Some(super::Unit::Px), value: super::Data::F32(CHART_SETTINGS_W), doc: "The Chart settings window's opening width." },
        super::Entry { group: "chart_dialogs", name: "INDICATOR_DIALOG_W", unit: Some(super::Unit::Px), value: super::Data::F32(INDICATOR_DIALOG_W), doc: "The indicator settings window's opening width." },
        super::Entry { group: "chart_dialogs", name: "LINE_DASH_COMBO_W", unit: Some(super::Unit::Px), value: super::Data::F32(LINE_DASH_COMBO_W), doc: "The width of the dash-style selector beside each line on an indicator's Style tab." },
        super::Entry { group: "chart_dialogs", name: "SETTINGS_MIN_W", unit: Some(super::Unit::Px), value: super::Data::F32(SETTINGS_MIN_W), doc: "The Chart settings dialog's minimum content width." },
        super::Entry { group: "chart_dialogs", name: "SOURCE_COMBO_W", unit: Some(super::Unit::Px), value: super::Data::F32(SOURCE_COMBO_W), doc: "The Source selector's width on an indicator's Inputs tab." },
    ];
}

/// Cockpit: the `[[value]]` rows of `ui-theme.toml` whose `group` is `cockpit`.
pub mod cockpit {
    /// The width of one window card in the Cockpit's strip.
    pub const CARD_W: f32 = 118.0;
    /// The faintest fill of a ladder depth bar: even a sliver of resting size paints this much.
    pub const DEPTH_ALPHA_FLOOR: f32 = 30.0;
    /// How much opacity a ladder depth bar gains from the floor to the largest resting size, so the size label on top stays legible.
    pub const DEPTH_ALPHA_SPAN: f32 = 120.0;
    /// The Cockpit header strip's least height; it grows only when the Hero countdown needs more.
    pub const HEADER_MIN_H: f32 = 44.0;
    /// The wash over the ladder rung under the pointer: the text colour at this opacity.
    pub const HOVER_ALPHA: u8 = 8;
    /// The tint of the inside-market rungs in the probability ladder: the up or down colour at this opacity.
    pub const INSIDE_ALPHA: u8 = 30;
    /// The width of a resting-order marker in the ladder; it sits in from the column edge.
    pub const MARKER_W: f32 = 14.0;
    /// The height of the Cockpit's strip of window cards.
    pub const RAIL_H: f32 = 92.0;
    /// The spread band in the probability ladder: the analysis line colour at this opacity, about a fifth.
    pub const SPREAD_ALPHA: u8 = 51;
    /// The dim laid over the whole ladder when its data is stale: the background colour at this opacity.
    pub const STALE_ALPHA: u8 = 150;

    /// Every row of this group as data, in the order the TOML lists them.
    pub const ENTRIES: &[super::Entry] = &[
        super::Entry { group: "cockpit", name: "CARD_W", unit: Some(super::Unit::Px), value: super::Data::F32(CARD_W), doc: "The width of one window card in the Cockpit's strip." },
        super::Entry { group: "cockpit", name: "DEPTH_ALPHA_FLOOR", unit: Some(super::Unit::Alpha), value: super::Data::F32(DEPTH_ALPHA_FLOOR), doc: "The faintest fill of a ladder depth bar: even a sliver of resting size paints this much." },
        super::Entry { group: "cockpit", name: "DEPTH_ALPHA_SPAN", unit: Some(super::Unit::Alpha), value: super::Data::F32(DEPTH_ALPHA_SPAN), doc: "How much opacity a ladder depth bar gains from the floor to the largest resting size, so the size label on top stays legible." },
        super::Entry { group: "cockpit", name: "HEADER_MIN_H", unit: Some(super::Unit::Px), value: super::Data::F32(HEADER_MIN_H), doc: "The Cockpit header strip's least height; it grows only when the Hero countdown needs more." },
        super::Entry { group: "cockpit", name: "HOVER_ALPHA", unit: Some(super::Unit::Alpha), value: super::Data::U8(HOVER_ALPHA), doc: "The wash over the ladder rung under the pointer: the text colour at this opacity." },
        super::Entry { group: "cockpit", name: "INSIDE_ALPHA", unit: Some(super::Unit::Alpha), value: super::Data::U8(INSIDE_ALPHA), doc: "The tint of the inside-market rungs in the probability ladder: the up or down colour at this opacity." },
        super::Entry { group: "cockpit", name: "MARKER_W", unit: Some(super::Unit::Px), value: super::Data::F32(MARKER_W), doc: "The width of a resting-order marker in the ladder; it sits in from the column edge." },
        super::Entry { group: "cockpit", name: "RAIL_H", unit: Some(super::Unit::Px), value: super::Data::F32(RAIL_H), doc: "The height of the Cockpit's strip of window cards." },
        super::Entry { group: "cockpit", name: "SPREAD_ALPHA", unit: Some(super::Unit::Alpha), value: super::Data::U8(SPREAD_ALPHA), doc: "The spread band in the probability ladder: the analysis line colour at this opacity, about a fifth." },
        super::Entry { group: "cockpit", name: "STALE_ALPHA", unit: Some(super::Unit::Alpha), value: super::Data::U8(STALE_ALPHA), doc: "The dim laid over the whole ladder when its data is stale: the background colour at this opacity." },
    ];
}

/// Connections: the `[[value]]` rows of `ui-theme.toml` whose `group` is `connections`.
pub mod connections {
    /// The room the right-aligned legend of Data Manager's Credentials destination's account strip leaves for rounding before it wraps.
    pub const LEGEND_FIT_SLACK: f32 = 4.0;
    /// The room the right-aligned action buttons of the ambient backend strip (drawn on Data Manager's Credentials AND Backend destinations) leave for rounding before they wrap.
    pub const STRIP_ACTIONS_FIT_SLACK: f32 = 2.0;
    /// The width of the new-account label box (the one with the ALT hint).
    pub const ALT_LABEL_INPUT_W: f32 = 160.0;
    /// The corner radius of a detail-pane badge (the Venue and feed-producer chips).
    pub const BADGE_RADIUS: f32 = 3.0;
    /// The height of one rail row: a chip, a venue-name cell and a status dot are all this tall.
    pub const CHIP_H: f32 = 16.0;
    /// The width of one rail status dot cell and of one S, D or L header letter: one value so the header and the dots line up.
    pub const DOT_W: f32 = 11.0;
    /// The width of a credential value box in the account editor.
    pub const FIELD_INPUT_W: f32 = 240.0;
    /// The key-name cell beside each credential input in the account editor.
    pub const FIELD_LABEL_CELL: egui::Vec2 = egui::vec2(160.0, 18.0);
    /// The widest a paragraph of prose is set in Data Manager's Credentials destination: a measure for words, never for a pane.
    pub const NOTE_W: f32 = 520.0;
    /// Data Manager's Credentials destination's own width from which the venue rail and the detail pane sit side by side; narrower, the rail is a wrapped row of chips.
    pub const RAIL_DETAIL_MIN_W: f32 = 820.0;
    /// The height of the rail header's cells: the Venue label and the S, D and L letters.
    pub const RAIL_HEADER_H: f32 = 14.0;
    /// The venue rail's widest width in the two-column Connections layout (the design's minmax(190px, 232px)).
    pub const RAIL_MAX_W: f32 = 232.0;
    /// The venue rail's narrowest width in the two-column Connections layout (the design's minmax(190px, 232px)).
    pub const RAIL_MIN_W: f32 = 190.0;
    /// The width of the box where an account row's id is typed to confirm removing it.
    pub const REMOVE_CONFIRM_W: f32 = 56.0;
    /// The name cell of a credential tier row (Sim, Demo, Live) in a venue's detail pane.
    pub const TIER_NAME_CELL: egui::Vec2 = egui::vec2(46.0, 16.0);
    /// The state cell of a credential tier row: the mark and its word (configured, not set).
    pub const TIER_STATE_CELL: egui::Vec2 = egui::vec2(104.0, 16.0);
    /// The outline of the detail pane's `Venue` and `feed producer` badges: the badge's own ink at this strength.
    pub const BADGE_OUTLINE_STRENGTH: f32 = 0.6;
    /// The strength of the muted grey on the marks that are not a measurement: the rail's middle dot and the detail pane's em dash for a tier the venue does not have, and the legend's `no such tier` entry.
    pub const NOT_CONFIGURABLE_DIM: f32 = 0.45;

    /// Every row of this group as data, in the order the TOML lists them.
    pub const ENTRIES: &[super::Entry] = &[
        super::Entry { group: "connections", name: "LEGEND_FIT_SLACK", unit: Some(super::Unit::Px), value: super::Data::F32(LEGEND_FIT_SLACK), doc: "The room the right-aligned legend of Data Manager's Credentials destination's account strip leaves for rounding before it wraps." },
        super::Entry { group: "connections", name: "STRIP_ACTIONS_FIT_SLACK", unit: Some(super::Unit::Px), value: super::Data::F32(STRIP_ACTIONS_FIT_SLACK), doc: "The room the right-aligned action buttons of the ambient backend strip (drawn on Data Manager's Credentials AND Backend destinations) leave for rounding before they wrap." },
        super::Entry { group: "connections", name: "ALT_LABEL_INPUT_W", unit: Some(super::Unit::Px), value: super::Data::F32(ALT_LABEL_INPUT_W), doc: "The width of the new-account label box (the one with the ALT hint)." },
        super::Entry { group: "connections", name: "BADGE_RADIUS", unit: Some(super::Unit::Px), value: super::Data::F32(BADGE_RADIUS), doc: "The corner radius of a detail-pane badge (the Venue and feed-producer chips)." },
        super::Entry { group: "connections", name: "CHIP_H", unit: Some(super::Unit::Px), value: super::Data::F32(CHIP_H), doc: "The height of one rail row: a chip, a venue-name cell and a status dot are all this tall." },
        super::Entry { group: "connections", name: "DOT_W", unit: Some(super::Unit::Px), value: super::Data::F32(DOT_W), doc: "The width of one rail status dot cell and of one S, D or L header letter: one value so the header and the dots line up." },
        super::Entry { group: "connections", name: "FIELD_INPUT_W", unit: Some(super::Unit::Px), value: super::Data::F32(FIELD_INPUT_W), doc: "The width of a credential value box in the account editor." },
        super::Entry { group: "connections", name: "FIELD_LABEL_CELL", unit: Some(super::Unit::Px), value: super::Data::Vec2(FIELD_LABEL_CELL), doc: "The key-name cell beside each credential input in the account editor." },
        super::Entry { group: "connections", name: "NOTE_W", unit: Some(super::Unit::Px), value: super::Data::F32(NOTE_W), doc: "The widest a paragraph of prose is set in Data Manager's Credentials destination: a measure for words, never for a pane." },
        super::Entry { group: "connections", name: "RAIL_DETAIL_MIN_W", unit: Some(super::Unit::Px), value: super::Data::F32(RAIL_DETAIL_MIN_W), doc: "Data Manager's Credentials destination's own width from which the venue rail and the detail pane sit side by side; narrower, the rail is a wrapped row of chips." },
        super::Entry { group: "connections", name: "RAIL_HEADER_H", unit: Some(super::Unit::Px), value: super::Data::F32(RAIL_HEADER_H), doc: "The height of the rail header's cells: the Venue label and the S, D and L letters." },
        super::Entry { group: "connections", name: "RAIL_MAX_W", unit: Some(super::Unit::Px), value: super::Data::F32(RAIL_MAX_W), doc: "The venue rail's widest width in the two-column Connections layout (the design's minmax(190px, 232px))." },
        super::Entry { group: "connections", name: "RAIL_MIN_W", unit: Some(super::Unit::Px), value: super::Data::F32(RAIL_MIN_W), doc: "The venue rail's narrowest width in the two-column Connections layout (the design's minmax(190px, 232px))." },
        super::Entry { group: "connections", name: "REMOVE_CONFIRM_W", unit: Some(super::Unit::Px), value: super::Data::F32(REMOVE_CONFIRM_W), doc: "The width of the box where an account row's id is typed to confirm removing it." },
        super::Entry { group: "connections", name: "TIER_NAME_CELL", unit: Some(super::Unit::Px), value: super::Data::Vec2(TIER_NAME_CELL), doc: "The name cell of a credential tier row (Sim, Demo, Live) in a venue's detail pane." },
        super::Entry { group: "connections", name: "TIER_STATE_CELL", unit: Some(super::Unit::Px), value: super::Data::Vec2(TIER_STATE_CELL), doc: "The state cell of a credential tier row: the mark and its word (configured, not set)." },
        super::Entry { group: "connections", name: "BADGE_OUTLINE_STRENGTH", unit: Some(super::Unit::Ratio), value: super::Data::F32(BADGE_OUTLINE_STRENGTH), doc: "The outline of the detail pane's `Venue` and `feed producer` badges: the badge's own ink at this strength." },
        super::Entry { group: "connections", name: "NOT_CONFIGURABLE_DIM", unit: Some(super::Unit::Ratio), value: super::Data::F32(NOT_CONFIGURABLE_DIM), doc: "The strength of the muted grey on the marks that are not a measurement: the rail's middle dot and the detail pane's em dash for a tier the venue does not have, and the legend's `no such tier` entry." },
    ];
}

/// Data: the `[[value]]` rows of `ui-theme.toml` whose `group` is `data`.
pub mod data {
    /// The width kept back beside the Data window's category rail for the separator line, taken off the body so rail and body fit side by side.
    pub const RAIL_SEPARATOR_RESERVE: f32 = 14.0;
    /// The Data window's body (right of the rail): its narrowest width, however narrow the window.
    pub const BODY_MIN_W: f32 = 120.0;
    /// The DataSets editor's Benchmark field: its width.
    pub const DS_BENCHMARK_FIELD_W: f32 = 360.0;
    /// The DataSets editor's Interval dropdown menu: its narrowest width.
    pub const DS_INTERVAL_MENU_MIN_W: f32 = 116.0;
    /// The DataSets editor's Interval dropdown button: its width.
    pub const DS_INTERVAL_W: f32 = 120.0;
    /// The DataSets editor form: the width of the label column (Name, Provider, Interval, Benchmark).
    pub const DS_LABEL_W: f32 = 92.0;
    /// The DataSet members table's Member column: it never gets narrower than this.
    pub const DS_MEMBERS_MEMBER_MIN_W: f32 = 90.0;
    /// The DataSet members table's Member column: its share of the width against the other column.
    pub const DS_MEMBERS_MEMBER_WEIGHT: f32 = 2.0;
    /// The DataSet members table's In this store column: it never gets narrower than this.
    pub const DS_MEMBERS_STORED_MIN_W: f32 = 80.0;
    /// The DataSet members table's In this store column: its share of the width against the other column.
    pub const DS_MEMBERS_STORED_WEIGHT: f32 = 1.0;
    /// How many rows tall the DataSets members table is: its header and five member rows.
    pub const DS_MEMBER_ROWS: u8 = 6;
    /// The DataSets editor's Name field: its width.
    pub const DS_NAME_FIELD_W: f32 = 260.0;
    /// The DataSets editor's Provider dropdown menu: its narrowest width.
    pub const DS_PROVIDER_MENU_MIN_W: f32 = 196.0;
    /// The DataSets editor's Provider dropdown button: its width.
    pub const DS_PROVIDER_W: f32 = 200.0;
    /// The DataSets screen's grouped tree (left column): its share of the screen's width, between its narrowest and widest.
    pub const DS_TREE_FRAC: f32 = 0.26;
    /// The DataSets screen's grouped tree: its widest width.
    pub const DS_TREE_MAX_W: f32 = 290.0;
    /// The DataSets screen's grouped tree: its narrowest width.
    pub const DS_TREE_MIN_W: f32 = 170.0;
    /// The Cached feeds table's Bars column: it never gets narrower than this.
    pub const FEEDS_BARS_MIN_W: f32 = 56.0;
    /// The Cached feeds table's Bars column: its share of the width against the other columns.
    pub const FEEDS_BARS_WEIGHT: f32 = 0.8;
    /// The Cached feeds table's From column: it never gets narrower than this.
    pub const FEEDS_FROM_MIN_W: f32 = 118.0;
    /// The Cached feeds table's From column: its share of the width against the other columns.
    pub const FEEDS_FROM_WEIGHT: f32 = 1.6;
    /// The Cached feeds table's Source column: it never gets narrower than this.
    pub const FEEDS_SOURCE_MIN_W: f32 = 64.0;
    /// The Cached feeds table's Source column: its share of the width against the other columns.
    pub const FEEDS_SOURCE_WEIGHT: f32 = 1.0;
    /// The Cached feeds table's Symbol column: it never gets narrower than this.
    pub const FEEDS_SYMBOL_MIN_W: f32 = 70.0;
    /// The Cached feeds table's Symbol column: its share of the width against the other columns.
    pub const FEEDS_SYMBOL_WEIGHT: f32 = 1.2;
    /// The Cached feeds table's Timeframe column: it never gets narrower than this.
    pub const FEEDS_TIMEFRAME_MIN_W: f32 = 60.0;
    /// The Cached feeds table's Timeframe column: its share of the width against the other columns.
    pub const FEEDS_TIMEFRAME_WEIGHT: f32 = 0.9;
    /// The Cached feeds table's To column: it never gets narrower than this.
    pub const FEEDS_TO_MIN_W: f32 = 118.0;
    /// The Cached feeds table's To column: its share of the width against the other columns.
    pub const FEEDS_TO_WEIGHT: f32 = 1.6;
    /// The Store screen's layout-by-kind table: its widest width, so four columns do not stretch across the whole body.
    pub const KINDS_TABLE_MAX_W: f32 = 480.0;
    /// The Activity log screen's Find field: its width.
    pub const LOG_FIND_W: f32 = 180.0;
    /// The Activity log's least height inside its scroll area, so an empty log still reads as a panel.
    pub const LOG_MIN_H: f32 = 54.0;
    /// The cross-kind partial-days list: the series name column's width.
    pub const PARTIAL_W_SERIES: f32 = 260.0;
    /// The cross-kind partial-days list: the date-span column's width.
    pub const PARTIAL_W_SPAN: f32 = 170.0;
    /// The cross-kind partial-days list: the partial-days summary column's width.
    pub const PARTIAL_W_SUMMARY: f32 = 240.0;
    /// The most of the Data window's width the left rail may take; a narrow window shrinks the rail below its full width.
    pub const RAIL_MAX_FRAC: f32 = 0.34;
    /// The Data window's left rail: its width. Wide enough for Cached feeds plus a three-digit count without eliding, and no wider: the shared grid's fixed columns already sum to about 690 px, so every pixel the rail takes is one the grid clips.
    pub const RAIL_W: f32 = 178.0;
    /// The Providers screen's source rows: the source name column's width.
    pub const SOURCE_W_NAME: f32 = 116.0;
    /// The Providers screen's source rows: the what-it-serves column's width.
    pub const SOURCE_W_SERVES: f32 = 168.0;
    /// The Store screen's layout-by-kind table, KIND column: it never gets narrower than this.
    pub const STORE_KIND_KIND_MIN_W: f32 = 80.0;
    /// The Store screen's layout-by-kind table, KIND column: its share of the width against the other columns.
    pub const STORE_KIND_KIND_WEIGHT: f32 = 1.6;
    /// The Store screen's layout-by-kind table, ROWS column: it never gets narrower than this.
    pub const STORE_KIND_ROWS_MIN_W: f32 = 64.0;
    /// The Store screen's layout-by-kind table, ROWS column: its share of the width against the other columns.
    pub const STORE_KIND_ROWS_WEIGHT: f32 = 1.0;
    /// The Store screen's layout-by-kind table, SERIES column: it never gets narrower than this.
    pub const STORE_KIND_SERIES_MIN_W: f32 = 56.0;
    /// The Store screen's layout-by-kind table, SERIES column: its share of the width against the other columns.
    pub const STORE_KIND_SERIES_WEIGHT: f32 = 1.0;
    /// The Store screen's layout-by-kind table, SIZE column: it never gets narrower than this.
    pub const STORE_KIND_SIZE_MIN_W: f32 = 64.0;
    /// The Store screen's layout-by-kind table, SIZE column: its share of the width against the other columns.
    pub const STORE_KIND_SIZE_WEIGHT: f32 = 1.0;
    /// A big-number tile on the Data window's overview screens: its narrowest width.
    pub const TILE_MIN_W: f32 = 148.0;

    /// Every row of this group as data, in the order the TOML lists them.
    pub const ENTRIES: &[super::Entry] = &[
        super::Entry { group: "data", name: "RAIL_SEPARATOR_RESERVE", unit: Some(super::Unit::Px), value: super::Data::F32(RAIL_SEPARATOR_RESERVE), doc: "The width kept back beside the Data window's category rail for the separator line, taken off the body so rail and body fit side by side." },
        super::Entry { group: "data", name: "BODY_MIN_W", unit: Some(super::Unit::Px), value: super::Data::F32(BODY_MIN_W), doc: "The Data window's body (right of the rail): its narrowest width, however narrow the window." },
        super::Entry { group: "data", name: "DS_BENCHMARK_FIELD_W", unit: Some(super::Unit::Px), value: super::Data::F32(DS_BENCHMARK_FIELD_W), doc: "The DataSets editor's Benchmark field: its width." },
        super::Entry { group: "data", name: "DS_INTERVAL_MENU_MIN_W", unit: Some(super::Unit::Px), value: super::Data::F32(DS_INTERVAL_MENU_MIN_W), doc: "The DataSets editor's Interval dropdown menu: its narrowest width." },
        super::Entry { group: "data", name: "DS_INTERVAL_W", unit: Some(super::Unit::Px), value: super::Data::F32(DS_INTERVAL_W), doc: "The DataSets editor's Interval dropdown button: its width." },
        super::Entry { group: "data", name: "DS_LABEL_W", unit: Some(super::Unit::Px), value: super::Data::F32(DS_LABEL_W), doc: "The DataSets editor form: the width of the label column (Name, Provider, Interval, Benchmark)." },
        super::Entry { group: "data", name: "DS_MEMBERS_MEMBER_MIN_W", unit: Some(super::Unit::Px), value: super::Data::F32(DS_MEMBERS_MEMBER_MIN_W), doc: "The DataSet members table's Member column: it never gets narrower than this." },
        super::Entry { group: "data", name: "DS_MEMBERS_MEMBER_WEIGHT", unit: Some(super::Unit::Ratio), value: super::Data::F32(DS_MEMBERS_MEMBER_WEIGHT), doc: "The DataSet members table's Member column: its share of the width against the other column." },
        super::Entry { group: "data", name: "DS_MEMBERS_STORED_MIN_W", unit: Some(super::Unit::Px), value: super::Data::F32(DS_MEMBERS_STORED_MIN_W), doc: "The DataSet members table's In this store column: it never gets narrower than this." },
        super::Entry { group: "data", name: "DS_MEMBERS_STORED_WEIGHT", unit: Some(super::Unit::Ratio), value: super::Data::F32(DS_MEMBERS_STORED_WEIGHT), doc: "The DataSet members table's In this store column: its share of the width against the other column." },
        super::Entry { group: "data", name: "DS_MEMBER_ROWS", unit: Some(super::Unit::Count), value: super::Data::U8(DS_MEMBER_ROWS), doc: "How many rows tall the DataSets members table is: its header and five member rows." },
        super::Entry { group: "data", name: "DS_NAME_FIELD_W", unit: Some(super::Unit::Px), value: super::Data::F32(DS_NAME_FIELD_W), doc: "The DataSets editor's Name field: its width." },
        super::Entry { group: "data", name: "DS_PROVIDER_MENU_MIN_W", unit: Some(super::Unit::Px), value: super::Data::F32(DS_PROVIDER_MENU_MIN_W), doc: "The DataSets editor's Provider dropdown menu: its narrowest width." },
        super::Entry { group: "data", name: "DS_PROVIDER_W", unit: Some(super::Unit::Px), value: super::Data::F32(DS_PROVIDER_W), doc: "The DataSets editor's Provider dropdown button: its width." },
        super::Entry { group: "data", name: "DS_TREE_FRAC", unit: Some(super::Unit::Ratio), value: super::Data::F32(DS_TREE_FRAC), doc: "The DataSets screen's grouped tree (left column): its share of the screen's width, between its narrowest and widest." },
        super::Entry { group: "data", name: "DS_TREE_MAX_W", unit: Some(super::Unit::Px), value: super::Data::F32(DS_TREE_MAX_W), doc: "The DataSets screen's grouped tree: its widest width." },
        super::Entry { group: "data", name: "DS_TREE_MIN_W", unit: Some(super::Unit::Px), value: super::Data::F32(DS_TREE_MIN_W), doc: "The DataSets screen's grouped tree: its narrowest width." },
        super::Entry { group: "data", name: "FEEDS_BARS_MIN_W", unit: Some(super::Unit::Px), value: super::Data::F32(FEEDS_BARS_MIN_W), doc: "The Cached feeds table's Bars column: it never gets narrower than this." },
        super::Entry { group: "data", name: "FEEDS_BARS_WEIGHT", unit: Some(super::Unit::Ratio), value: super::Data::F32(FEEDS_BARS_WEIGHT), doc: "The Cached feeds table's Bars column: its share of the width against the other columns." },
        super::Entry { group: "data", name: "FEEDS_FROM_MIN_W", unit: Some(super::Unit::Px), value: super::Data::F32(FEEDS_FROM_MIN_W), doc: "The Cached feeds table's From column: it never gets narrower than this." },
        super::Entry { group: "data", name: "FEEDS_FROM_WEIGHT", unit: Some(super::Unit::Ratio), value: super::Data::F32(FEEDS_FROM_WEIGHT), doc: "The Cached feeds table's From column: its share of the width against the other columns." },
        super::Entry { group: "data", name: "FEEDS_SOURCE_MIN_W", unit: Some(super::Unit::Px), value: super::Data::F32(FEEDS_SOURCE_MIN_W), doc: "The Cached feeds table's Source column: it never gets narrower than this." },
        super::Entry { group: "data", name: "FEEDS_SOURCE_WEIGHT", unit: Some(super::Unit::Ratio), value: super::Data::F32(FEEDS_SOURCE_WEIGHT), doc: "The Cached feeds table's Source column: its share of the width against the other columns." },
        super::Entry { group: "data", name: "FEEDS_SYMBOL_MIN_W", unit: Some(super::Unit::Px), value: super::Data::F32(FEEDS_SYMBOL_MIN_W), doc: "The Cached feeds table's Symbol column: it never gets narrower than this." },
        super::Entry { group: "data", name: "FEEDS_SYMBOL_WEIGHT", unit: Some(super::Unit::Ratio), value: super::Data::F32(FEEDS_SYMBOL_WEIGHT), doc: "The Cached feeds table's Symbol column: its share of the width against the other columns." },
        super::Entry { group: "data", name: "FEEDS_TIMEFRAME_MIN_W", unit: Some(super::Unit::Px), value: super::Data::F32(FEEDS_TIMEFRAME_MIN_W), doc: "The Cached feeds table's Timeframe column: it never gets narrower than this." },
        super::Entry { group: "data", name: "FEEDS_TIMEFRAME_WEIGHT", unit: Some(super::Unit::Ratio), value: super::Data::F32(FEEDS_TIMEFRAME_WEIGHT), doc: "The Cached feeds table's Timeframe column: its share of the width against the other columns." },
        super::Entry { group: "data", name: "FEEDS_TO_MIN_W", unit: Some(super::Unit::Px), value: super::Data::F32(FEEDS_TO_MIN_W), doc: "The Cached feeds table's To column: it never gets narrower than this." },
        super::Entry { group: "data", name: "FEEDS_TO_WEIGHT", unit: Some(super::Unit::Ratio), value: super::Data::F32(FEEDS_TO_WEIGHT), doc: "The Cached feeds table's To column: its share of the width against the other columns." },
        super::Entry { group: "data", name: "KINDS_TABLE_MAX_W", unit: Some(super::Unit::Px), value: super::Data::F32(KINDS_TABLE_MAX_W), doc: "The Store screen's layout-by-kind table: its widest width, so four columns do not stretch across the whole body." },
        super::Entry { group: "data", name: "LOG_FIND_W", unit: Some(super::Unit::Px), value: super::Data::F32(LOG_FIND_W), doc: "The Activity log screen's Find field: its width." },
        super::Entry { group: "data", name: "LOG_MIN_H", unit: Some(super::Unit::Px), value: super::Data::F32(LOG_MIN_H), doc: "The Activity log's least height inside its scroll area, so an empty log still reads as a panel." },
        super::Entry { group: "data", name: "PARTIAL_W_SERIES", unit: Some(super::Unit::Px), value: super::Data::F32(PARTIAL_W_SERIES), doc: "The cross-kind partial-days list: the series name column's width." },
        super::Entry { group: "data", name: "PARTIAL_W_SPAN", unit: Some(super::Unit::Px), value: super::Data::F32(PARTIAL_W_SPAN), doc: "The cross-kind partial-days list: the date-span column's width." },
        super::Entry { group: "data", name: "PARTIAL_W_SUMMARY", unit: Some(super::Unit::Px), value: super::Data::F32(PARTIAL_W_SUMMARY), doc: "The cross-kind partial-days list: the partial-days summary column's width." },
        super::Entry { group: "data", name: "RAIL_MAX_FRAC", unit: Some(super::Unit::Ratio), value: super::Data::F32(RAIL_MAX_FRAC), doc: "The most of the Data window's width the left rail may take; a narrow window shrinks the rail below its full width." },
        super::Entry { group: "data", name: "RAIL_W", unit: Some(super::Unit::Px), value: super::Data::F32(RAIL_W), doc: "The Data window's left rail: its width. Wide enough for Cached feeds plus a three-digit count without eliding, and no wider: the shared grid's fixed columns already sum to about 690 px, so every pixel the rail takes is one the grid clips." },
        super::Entry { group: "data", name: "SOURCE_W_NAME", unit: Some(super::Unit::Px), value: super::Data::F32(SOURCE_W_NAME), doc: "The Providers screen's source rows: the source name column's width." },
        super::Entry { group: "data", name: "SOURCE_W_SERVES", unit: Some(super::Unit::Px), value: super::Data::F32(SOURCE_W_SERVES), doc: "The Providers screen's source rows: the what-it-serves column's width." },
        super::Entry { group: "data", name: "STORE_KIND_KIND_MIN_W", unit: Some(super::Unit::Px), value: super::Data::F32(STORE_KIND_KIND_MIN_W), doc: "The Store screen's layout-by-kind table, KIND column: it never gets narrower than this." },
        super::Entry { group: "data", name: "STORE_KIND_KIND_WEIGHT", unit: Some(super::Unit::Ratio), value: super::Data::F32(STORE_KIND_KIND_WEIGHT), doc: "The Store screen's layout-by-kind table, KIND column: its share of the width against the other columns." },
        super::Entry { group: "data", name: "STORE_KIND_ROWS_MIN_W", unit: Some(super::Unit::Px), value: super::Data::F32(STORE_KIND_ROWS_MIN_W), doc: "The Store screen's layout-by-kind table, ROWS column: it never gets narrower than this." },
        super::Entry { group: "data", name: "STORE_KIND_ROWS_WEIGHT", unit: Some(super::Unit::Ratio), value: super::Data::F32(STORE_KIND_ROWS_WEIGHT), doc: "The Store screen's layout-by-kind table, ROWS column: its share of the width against the other columns." },
        super::Entry { group: "data", name: "STORE_KIND_SERIES_MIN_W", unit: Some(super::Unit::Px), value: super::Data::F32(STORE_KIND_SERIES_MIN_W), doc: "The Store screen's layout-by-kind table, SERIES column: it never gets narrower than this." },
        super::Entry { group: "data", name: "STORE_KIND_SERIES_WEIGHT", unit: Some(super::Unit::Ratio), value: super::Data::F32(STORE_KIND_SERIES_WEIGHT), doc: "The Store screen's layout-by-kind table, SERIES column: its share of the width against the other columns." },
        super::Entry { group: "data", name: "STORE_KIND_SIZE_MIN_W", unit: Some(super::Unit::Px), value: super::Data::F32(STORE_KIND_SIZE_MIN_W), doc: "The Store screen's layout-by-kind table, SIZE column: it never gets narrower than this." },
        super::Entry { group: "data", name: "STORE_KIND_SIZE_WEIGHT", unit: Some(super::Unit::Ratio), value: super::Data::F32(STORE_KIND_SIZE_WEIGHT), doc: "The Store screen's layout-by-kind table, SIZE column: its share of the width against the other columns." },
        super::Entry { group: "data", name: "TILE_MIN_W", unit: Some(super::Unit::Px), value: super::Data::F32(TILE_MIN_W), doc: "A big-number tile on the Data window's overview screens: its narrowest width." },
    ];
}

/// Data Manager: the `[[value]]` rows of `ui-theme.toml` whose `group` is `data_manager`.
pub mod data_manager {
    /// A coverage bar's height as a share of a table row's height.
    pub const BAR_H_RATIO: f32 = 0.5;
    /// The least width a series' coverage span is drawn at, so a very short span still shows.
    pub const BAR_MIN_W: f32 = 1.5;
    /// A coverage-legend swatch's width as a multiple of the bar's height.
    pub const LEGEND_SWATCH_ASPECT: f32 = 2.0;
    /// The Polymarket proxy box's width: a whole `socks5h://user:pass@host:port` without scrolling.
    pub const PROXY_BOX_W: f32 = 320.0;
    /// The stored-series grid's Symbol column: its width, in the header and in every row.
    pub const W_SYMBOL: f32 = 150.0;
    /// The grid's Kind column: its width.
    pub const W_KIND: f32 = 120.0;
    /// The grid's Coverage column: its width, and the coverage bar inside it is this wide less 8.
    pub const W_COVERAGE: f32 = 150.0;
    /// The cross-kind Partial column — a glyph, not text, so it stays narrow; the count and the missing kinds are in the tooltip. Shown by the rich grid only.
    pub const W_PARTIAL: f32 = 26.0;
    /// The grid's Rows column: its width.
    pub const W_ROWS: f32 = 90.0;
    /// The grid's Size column: its width.
    pub const W_SIZE: f32 = 80.0;
    /// The grid's Updated column: its width.
    pub const W_UPDATED: f32 = 100.0;

    /// Every row of this group as data, in the order the TOML lists them.
    pub const ENTRIES: &[super::Entry] = &[
        super::Entry { group: "data_manager", name: "BAR_H_RATIO", unit: Some(super::Unit::Ratio), value: super::Data::F32(BAR_H_RATIO), doc: "A coverage bar's height as a share of a table row's height." },
        super::Entry { group: "data_manager", name: "BAR_MIN_W", unit: Some(super::Unit::Px), value: super::Data::F32(BAR_MIN_W), doc: "The least width a series' coverage span is drawn at, so a very short span still shows." },
        super::Entry { group: "data_manager", name: "LEGEND_SWATCH_ASPECT", unit: Some(super::Unit::Ratio), value: super::Data::F32(LEGEND_SWATCH_ASPECT), doc: "A coverage-legend swatch's width as a multiple of the bar's height." },
        super::Entry { group: "data_manager", name: "PROXY_BOX_W", unit: Some(super::Unit::Px), value: super::Data::F32(PROXY_BOX_W), doc: "The Polymarket proxy box's width: a whole `socks5h://user:pass@host:port` without scrolling." },
        super::Entry { group: "data_manager", name: "W_SYMBOL", unit: Some(super::Unit::Px), value: super::Data::F32(W_SYMBOL), doc: "The stored-series grid's Symbol column: its width, in the header and in every row." },
        super::Entry { group: "data_manager", name: "W_KIND", unit: Some(super::Unit::Px), value: super::Data::F32(W_KIND), doc: "The grid's Kind column: its width." },
        super::Entry { group: "data_manager", name: "W_COVERAGE", unit: Some(super::Unit::Px), value: super::Data::F32(W_COVERAGE), doc: "The grid's Coverage column: its width, and the coverage bar inside it is this wide less 8." },
        super::Entry { group: "data_manager", name: "W_PARTIAL", unit: Some(super::Unit::Px), value: super::Data::F32(W_PARTIAL), doc: "The cross-kind Partial column — a glyph, not text, so it stays narrow; the count and the missing kinds are in the tooltip. Shown by the rich grid only." },
        super::Entry { group: "data_manager", name: "W_ROWS", unit: Some(super::Unit::Px), value: super::Data::F32(W_ROWS), doc: "The grid's Rows column: its width." },
        super::Entry { group: "data_manager", name: "W_SIZE", unit: Some(super::Unit::Px), value: super::Data::F32(W_SIZE), doc: "The grid's Size column: its width." },
        super::Entry { group: "data_manager", name: "W_UPDATED", unit: Some(super::Unit::Px), value: super::Data::F32(W_UPDATED), doc: "The grid's Updated column: its width." },
    ];
}

/// Desktop: the `[[value]]` rows of `ui-theme.toml` whose `group` is `desktop`.
pub mod desktop {
    /// How far in from the left edge of a chart-style menu row its label starts, after the mini chart icon.
    pub const STYLE_LABEL_X: f32 = 30.0;
    /// The chart-style brand icon in a chart window's title bar; clicking it opens the style menu.
    pub const BRAND_SIZE: egui::Vec2 = egui::vec2(22.0, 18.0);
    /// The width of the symbol entry box in the Compare popup.
    pub const CMP_ENTRY_W: f32 = 160.0;
    /// The tallest the Compare popup's search results get before they scroll.
    pub const CMP_LIST_H: f32 = 240.0;
    /// The minimum width of the Compare (Cmp) popup.
    pub const CMP_MENU_W: f32 = 250.0;
    /// The minimum width of the interval menu in a chart window's title bar.
    pub const INTERVAL_MENU_W: f32 = 70.0;
    /// The widest the minimized-window rail can be dragged to.
    pub const MIN_RAIL_MAX_W: f32 = 40.0;
    /// The narrowest the minimized-window rail can be dragged to.
    pub const MIN_RAIL_MIN_W: f32 = 24.0;
    /// The opening width of the left rail of minimized-window tabs.
    pub const MIN_RAIL_W: f32 = 30.0;
    /// The minimum width of the Orderflow (OF) popup: the CVD and Volume Profile toggles and the tick size.
    pub const OF_MENU_W: f32 = 170.0;
    /// The mini chart icon at the left of each chart-style menu row.
    pub const STYLE_ICON_SIZE: egui::Vec2 = egui::vec2(18.0, 16.0);
    /// The chart-style menu's minimum width.
    pub const STYLE_MENU_W: f32 = 186.0;
    /// A chart-style menu row: its least width (it takes the menu's full width above that) and its height.
    pub const STYLE_ROW: egui::Vec2 = egui::vec2(172.0, 22.0);
    /// The minimum width of the symbol quick-pick menu in a chart window's title bar.
    pub const SYMBOL_MENU_W: f32 = 250.0;
    /// The app window's size when it is restored down from maximized; it opens maximized.
    pub const WINDOW_RESTORED_SIZE: egui::Vec2 = egui::vec2(1400.0, 900.0);
    /// Compare overlay 1, blue: the overlay's line on the chart and its chip dot in the title-bar popup.
    pub const COMPARE_1: egui::Color32 = egui::Color32::from_rgb(87, 165, 255);
    /// Compare overlay 2, orange: the overlay's line on the chart and its chip dot.
    pub const COMPARE_2: egui::Color32 = egui::Color32::from_rgb(240, 149, 40);
    /// Compare overlay 3, violet: the overlay's line on the chart and its chip dot.
    pub const COMPARE_3: egui::Color32 = egui::Color32::from_rgb(175, 122, 255);
    /// Compare overlay 4, cyan: the overlay's line on the chart and its chip dot.
    pub const COMPARE_4: egui::Color32 = egui::Color32::from_rgb(38, 198, 218);
    /// Compare overlay 5, pink: the overlay's line on the chart and its chip dot.
    pub const COMPARE_5: egui::Color32 = egui::Color32::from_rgb(236, 64, 122);
    /// The chart title bar's sync-group dot for group 1, red: charts in one group share their crosshair and range.
    pub const SYNC_GROUP_1: egui::Color32 = egui::Color32::from_rgb(255, 0, 0);
    /// The chart title bar's sync-group dot for group 2, blue.
    pub const SYNC_GROUP_2: egui::Color32 = egui::Color32::from_rgb(0, 0, 255);
    /// The chart title bar's sync-group dot for group 3, green.
    pub const SYNC_GROUP_3: egui::Color32 = egui::Color32::from_rgb(0, 255, 0);
    /// The chart title bar's sync-group dot for group 4, yellow.
    pub const SYNC_GROUP_4: egui::Color32 = egui::Color32::from_rgb(255, 255, 0);

    /// Every row of this group as data, in the order the TOML lists them.
    pub const ENTRIES: &[super::Entry] = &[
        super::Entry { group: "desktop", name: "STYLE_LABEL_X", unit: Some(super::Unit::Px), value: super::Data::F32(STYLE_LABEL_X), doc: "How far in from the left edge of a chart-style menu row its label starts, after the mini chart icon." },
        super::Entry { group: "desktop", name: "BRAND_SIZE", unit: Some(super::Unit::Px), value: super::Data::Vec2(BRAND_SIZE), doc: "The chart-style brand icon in a chart window's title bar; clicking it opens the style menu." },
        super::Entry { group: "desktop", name: "CMP_ENTRY_W", unit: Some(super::Unit::Px), value: super::Data::F32(CMP_ENTRY_W), doc: "The width of the symbol entry box in the Compare popup." },
        super::Entry { group: "desktop", name: "CMP_LIST_H", unit: Some(super::Unit::Px), value: super::Data::F32(CMP_LIST_H), doc: "The tallest the Compare popup's search results get before they scroll." },
        super::Entry { group: "desktop", name: "CMP_MENU_W", unit: Some(super::Unit::Px), value: super::Data::F32(CMP_MENU_W), doc: "The minimum width of the Compare (Cmp) popup." },
        super::Entry { group: "desktop", name: "INTERVAL_MENU_W", unit: Some(super::Unit::Px), value: super::Data::F32(INTERVAL_MENU_W), doc: "The minimum width of the interval menu in a chart window's title bar." },
        super::Entry { group: "desktop", name: "MIN_RAIL_MAX_W", unit: Some(super::Unit::Px), value: super::Data::F32(MIN_RAIL_MAX_W), doc: "The widest the minimized-window rail can be dragged to." },
        super::Entry { group: "desktop", name: "MIN_RAIL_MIN_W", unit: Some(super::Unit::Px), value: super::Data::F32(MIN_RAIL_MIN_W), doc: "The narrowest the minimized-window rail can be dragged to." },
        super::Entry { group: "desktop", name: "MIN_RAIL_W", unit: Some(super::Unit::Px), value: super::Data::F32(MIN_RAIL_W), doc: "The opening width of the left rail of minimized-window tabs." },
        super::Entry { group: "desktop", name: "OF_MENU_W", unit: Some(super::Unit::Px), value: super::Data::F32(OF_MENU_W), doc: "The minimum width of the Orderflow (OF) popup: the CVD and Volume Profile toggles and the tick size." },
        super::Entry { group: "desktop", name: "STYLE_ICON_SIZE", unit: Some(super::Unit::Px), value: super::Data::Vec2(STYLE_ICON_SIZE), doc: "The mini chart icon at the left of each chart-style menu row." },
        super::Entry { group: "desktop", name: "STYLE_MENU_W", unit: Some(super::Unit::Px), value: super::Data::F32(STYLE_MENU_W), doc: "The chart-style menu's minimum width." },
        super::Entry { group: "desktop", name: "STYLE_ROW", unit: Some(super::Unit::Px), value: super::Data::Vec2(STYLE_ROW), doc: "A chart-style menu row: its least width (it takes the menu's full width above that) and its height." },
        super::Entry { group: "desktop", name: "SYMBOL_MENU_W", unit: Some(super::Unit::Px), value: super::Data::F32(SYMBOL_MENU_W), doc: "The minimum width of the symbol quick-pick menu in a chart window's title bar." },
        super::Entry { group: "desktop", name: "WINDOW_RESTORED_SIZE", unit: Some(super::Unit::Px), value: super::Data::Vec2(WINDOW_RESTORED_SIZE), doc: "The app window's size when it is restored down from maximized; it opens maximized." },
        super::Entry { group: "desktop", name: "COMPARE_1", unit: None, value: super::Data::Colour(COMPARE_1), doc: "Compare overlay 1, blue: the overlay's line on the chart and its chip dot in the title-bar popup." },
        super::Entry { group: "desktop", name: "COMPARE_2", unit: None, value: super::Data::Colour(COMPARE_2), doc: "Compare overlay 2, orange: the overlay's line on the chart and its chip dot." },
        super::Entry { group: "desktop", name: "COMPARE_3", unit: None, value: super::Data::Colour(COMPARE_3), doc: "Compare overlay 3, violet: the overlay's line on the chart and its chip dot." },
        super::Entry { group: "desktop", name: "COMPARE_4", unit: None, value: super::Data::Colour(COMPARE_4), doc: "Compare overlay 4, cyan: the overlay's line on the chart and its chip dot." },
        super::Entry { group: "desktop", name: "COMPARE_5", unit: None, value: super::Data::Colour(COMPARE_5), doc: "Compare overlay 5, pink: the overlay's line on the chart and its chip dot." },
        super::Entry { group: "desktop", name: "SYNC_GROUP_1", unit: None, value: super::Data::Colour(SYNC_GROUP_1), doc: "The chart title bar's sync-group dot for group 1, red: charts in one group share their crosshair and range." },
        super::Entry { group: "desktop", name: "SYNC_GROUP_2", unit: None, value: super::Data::Colour(SYNC_GROUP_2), doc: "The chart title bar's sync-group dot for group 2, blue." },
        super::Entry { group: "desktop", name: "SYNC_GROUP_3", unit: None, value: super::Data::Colour(SYNC_GROUP_3), doc: "The chart title bar's sync-group dot for group 3, green." },
        super::Entry { group: "desktop", name: "SYNC_GROUP_4", unit: None, value: super::Data::Colour(SYNC_GROUP_4), doc: "The chart title bar's sync-group dot for group 4, yellow." },
    ];
}

/// Fx Picker: the `[[value]]` rows of `ui-theme.toml` whose `group` is `fx_picker`.
pub mod fx_picker {
    /// The Indicators picker's indicator list: how tall it grows before it scrolls.
    pub const LIST_MAX_H: f32 = 320.0;
    /// The Indicators picker window (the fx picker): its width.
    pub const POPUP_W: f32 = 300.0;
    /// The Indicators picker's search field: its width.
    pub const SEARCH_W: f32 = 210.0;
    /// The per-study source symbol menu's result list: how tall it grows before it scrolls.
    pub const SOURCE_LIST_MAX_H: f32 = 260.0;
    /// The per-study source symbol menu in the Indicators picker: its narrowest width.
    pub const SOURCE_MENU_MIN_W: f32 = 240.0;
    /// The per-study source symbol menu's Search symbol field: its width.
    pub const SOURCE_SEARCH_W: f32 = 224.0;

    /// Every row of this group as data, in the order the TOML lists them.
    pub const ENTRIES: &[super::Entry] = &[
        super::Entry { group: "fx_picker", name: "LIST_MAX_H", unit: Some(super::Unit::Px), value: super::Data::F32(LIST_MAX_H), doc: "The Indicators picker's indicator list: how tall it grows before it scrolls." },
        super::Entry { group: "fx_picker", name: "POPUP_W", unit: Some(super::Unit::Px), value: super::Data::F32(POPUP_W), doc: "The Indicators picker window (the fx picker): its width." },
        super::Entry { group: "fx_picker", name: "SEARCH_W", unit: Some(super::Unit::Px), value: super::Data::F32(SEARCH_W), doc: "The Indicators picker's search field: its width." },
        super::Entry { group: "fx_picker", name: "SOURCE_LIST_MAX_H", unit: Some(super::Unit::Px), value: super::Data::F32(SOURCE_LIST_MAX_H), doc: "The per-study source symbol menu's result list: how tall it grows before it scrolls." },
        super::Entry { group: "fx_picker", name: "SOURCE_MENU_MIN_W", unit: Some(super::Unit::Px), value: super::Data::F32(SOURCE_MENU_MIN_W), doc: "The per-study source symbol menu in the Indicators picker: its narrowest width." },
        super::Entry { group: "fx_picker", name: "SOURCE_SEARCH_W", unit: Some(super::Unit::Px), value: super::Data::F32(SOURCE_SEARCH_W), doc: "The per-study source symbol menu's Search symbol field: its width." },
    ];
}

/// Instruments: the `[[value]]` rows of `ui-theme.toml` whose `group` is `instruments`.
pub mod instruments {
    /// How wide the Status cell of the Data Manager's Instruments table grows before its text wraps.
    pub const STATUS_MAX_WIDTH: f32 = 420.0;

    /// Every row of this group as data, in the order the TOML lists them.
    pub const ENTRIES: &[super::Entry] = &[
        super::Entry { group: "instruments", name: "STATUS_MAX_WIDTH", unit: Some(super::Unit::Px), value: super::Data::F32(STATUS_MAX_WIDTH), doc: "How wide the Status cell of the Data Manager's Instruments table grows before its text wraps." },
    ];
}

/// News: the `[[value]]` rows of `ui-theme.toml` whose `group` is `news`.
pub mod news {
    /// The width a headline card's content gives up inside its frame: the list width less this is the room for the card's text.
    pub const CARD_INNER_RESERVE: f32 = 18.0;
    /// The width kept free between the headline list and the reader pane, so the two sit side by side without overflowing the window.
    pub const READER_WIDTH_RESERVE: f32 = 22.0;
    /// The News window: the size of the letter in a fallback source avatar, as a share of the avatar's size.
    pub const AVATAR_LETTER_FRAC: f32 = 0.5;
    /// The News window: a source avatar's corner radius as a share of its size.
    pub const AVATAR_RADIUS_FRAC: f32 = 0.28;
    /// The News window: the square of a headline's source avatar, in the list and in the reader.
    pub const AVATAR_SIZE: f32 = 30.0;
    /// The News toolbar's filter pill menus (Market, Category, Provider): their narrowest width.
    pub const FILTER_MENU_MIN_W: f32 = 170.0;
    /// The News window with the reader open: the headline list's share of the window's width.
    pub const LIST_FRAC: f32 = 0.66;
    /// The News window with the reader open: the headline list never takes the last this-much of the width, which the reader keeps.
    pub const LIST_LEAVES_READER_W: f32 = 170.0;
    /// The News window with the reader open: the headline list's narrowest width.
    pub const LIST_MIN_W: f32 = 240.0;
    /// The News reader pane: its narrowest width.
    pub const READER_MIN_W: f32 = 150.0;
    /// The News toolbar's Search headlines field: its width.
    pub const SEARCH_W: f32 = 220.0;
    /// The News window: the least height of the headline list and reader panes.
    pub const SPLIT_MIN_H: f32 = 120.0;
    /// The News toolbar's filter pills and Follow chart button: their least height.
    pub const TOOLBAR_BTN_H: f32 = 34.0;
    /// Fallback avatar of a news source with no favicon: background colour 1 of 8 (green); the colour is picked by hashing the source name, so the order of the eight is the contract.
    pub const AVATAR_1: egui::Color32 = egui::Color32::from_rgb(63, 224, 138);
    /// Fallback avatar of a news source with no favicon: background colour 2 of 8 (amber), in hash order.
    pub const AVATAR_2: egui::Color32 = egui::Color32::from_rgb(240, 169, 63);
    /// Fallback avatar of a news source with no favicon: background colour 3 of 8 (blue), in hash order.
    pub const AVATAR_3: egui::Color32 = egui::Color32::from_rgb(63, 155, 224);
    /// Fallback avatar of a news source with no favicon: background colour 4 of 8 (orange), in hash order.
    pub const AVATAR_4: egui::Color32 = egui::Color32::from_rgb(224, 100, 63);
    /// Fallback avatar of a news source with no favicon: background colour 5 of 8 (violet), in hash order.
    pub const AVATAR_5: egui::Color32 = egui::Color32::from_rgb(176, 111, 224);
    /// Fallback avatar of a news source with no favicon: background colour 6 of 8 (teal), in hash order.
    pub const AVATAR_6: egui::Color32 = egui::Color32::from_rgb(63, 224, 200);
    /// Fallback avatar of a news source with no favicon: background colour 7 of 8 (pink), in hash order.
    pub const AVATAR_7: egui::Color32 = egui::Color32::from_rgb(224, 63, 138);
    /// Fallback avatar of a news source with no favicon: background colour 8 of 8 (lime), in hash order.
    pub const AVATAR_8: egui::Color32 = egui::Color32::from_rgb(155, 224, 63);
    /// The near-white rounded plate behind a source's real favicon in the News list, so a dark or transparent logo stays legible in every theme.
    pub const LOGO_PLATE: egui::Color32 = egui::Color32::from_rgb(244, 244, 244);

    /// Every row of this group as data, in the order the TOML lists them.
    pub const ENTRIES: &[super::Entry] = &[
        super::Entry { group: "news", name: "CARD_INNER_RESERVE", unit: Some(super::Unit::Px), value: super::Data::F32(CARD_INNER_RESERVE), doc: "The width a headline card's content gives up inside its frame: the list width less this is the room for the card's text." },
        super::Entry { group: "news", name: "READER_WIDTH_RESERVE", unit: Some(super::Unit::Px), value: super::Data::F32(READER_WIDTH_RESERVE), doc: "The width kept free between the headline list and the reader pane, so the two sit side by side without overflowing the window." },
        super::Entry { group: "news", name: "AVATAR_LETTER_FRAC", unit: Some(super::Unit::Ratio), value: super::Data::F32(AVATAR_LETTER_FRAC), doc: "The News window: the size of the letter in a fallback source avatar, as a share of the avatar's size." },
        super::Entry { group: "news", name: "AVATAR_RADIUS_FRAC", unit: Some(super::Unit::Ratio), value: super::Data::F32(AVATAR_RADIUS_FRAC), doc: "The News window: a source avatar's corner radius as a share of its size." },
        super::Entry { group: "news", name: "AVATAR_SIZE", unit: Some(super::Unit::Px), value: super::Data::F32(AVATAR_SIZE), doc: "The News window: the square of a headline's source avatar, in the list and in the reader." },
        super::Entry { group: "news", name: "FILTER_MENU_MIN_W", unit: Some(super::Unit::Px), value: super::Data::F32(FILTER_MENU_MIN_W), doc: "The News toolbar's filter pill menus (Market, Category, Provider): their narrowest width." },
        super::Entry { group: "news", name: "LIST_FRAC", unit: Some(super::Unit::Ratio), value: super::Data::F32(LIST_FRAC), doc: "The News window with the reader open: the headline list's share of the window's width." },
        super::Entry { group: "news", name: "LIST_LEAVES_READER_W", unit: Some(super::Unit::Px), value: super::Data::F32(LIST_LEAVES_READER_W), doc: "The News window with the reader open: the headline list never takes the last this-much of the width, which the reader keeps." },
        super::Entry { group: "news", name: "LIST_MIN_W", unit: Some(super::Unit::Px), value: super::Data::F32(LIST_MIN_W), doc: "The News window with the reader open: the headline list's narrowest width." },
        super::Entry { group: "news", name: "READER_MIN_W", unit: Some(super::Unit::Px), value: super::Data::F32(READER_MIN_W), doc: "The News reader pane: its narrowest width." },
        super::Entry { group: "news", name: "SEARCH_W", unit: Some(super::Unit::Px), value: super::Data::F32(SEARCH_W), doc: "The News toolbar's Search headlines field: its width." },
        super::Entry { group: "news", name: "SPLIT_MIN_H", unit: Some(super::Unit::Px), value: super::Data::F32(SPLIT_MIN_H), doc: "The News window: the least height of the headline list and reader panes." },
        super::Entry { group: "news", name: "TOOLBAR_BTN_H", unit: Some(super::Unit::Px), value: super::Data::F32(TOOLBAR_BTN_H), doc: "The News toolbar's filter pills and Follow chart button: their least height." },
        super::Entry { group: "news", name: "AVATAR_1", unit: None, value: super::Data::Colour(AVATAR_1), doc: "Fallback avatar of a news source with no favicon: background colour 1 of 8 (green); the colour is picked by hashing the source name, so the order of the eight is the contract." },
        super::Entry { group: "news", name: "AVATAR_2", unit: None, value: super::Data::Colour(AVATAR_2), doc: "Fallback avatar of a news source with no favicon: background colour 2 of 8 (amber), in hash order." },
        super::Entry { group: "news", name: "AVATAR_3", unit: None, value: super::Data::Colour(AVATAR_3), doc: "Fallback avatar of a news source with no favicon: background colour 3 of 8 (blue), in hash order." },
        super::Entry { group: "news", name: "AVATAR_4", unit: None, value: super::Data::Colour(AVATAR_4), doc: "Fallback avatar of a news source with no favicon: background colour 4 of 8 (orange), in hash order." },
        super::Entry { group: "news", name: "AVATAR_5", unit: None, value: super::Data::Colour(AVATAR_5), doc: "Fallback avatar of a news source with no favicon: background colour 5 of 8 (violet), in hash order." },
        super::Entry { group: "news", name: "AVATAR_6", unit: None, value: super::Data::Colour(AVATAR_6), doc: "Fallback avatar of a news source with no favicon: background colour 6 of 8 (teal), in hash order." },
        super::Entry { group: "news", name: "AVATAR_7", unit: None, value: super::Data::Colour(AVATAR_7), doc: "Fallback avatar of a news source with no favicon: background colour 7 of 8 (pink), in hash order." },
        super::Entry { group: "news", name: "AVATAR_8", unit: None, value: super::Data::Colour(AVATAR_8), doc: "Fallback avatar of a news source with no favicon: background colour 8 of 8 (lime), in hash order." },
        super::Entry { group: "news", name: "LOGO_PLATE", unit: None, value: super::Data::Colour(LOGO_PLATE), doc: "The near-white rounded plate behind a source's real favicon in the News list, so a dark or transparent logo stays legible in every theme." },
    ];
}

/// Options Chain: the `[[value]]` rows of `ui-theme.toml` whose `group` is `options_chain`.
pub mod options_chain {
    /// How far in from the left edge of its side's first column a chain row's position and working-order badges start.
    pub const BADGE_START_OFFSET: f32 = 3.0;
    /// How far each diagonal of the in-the-money hatch leans sideways while it climbs a row; the first stroke starts this far left of the cell so its top end reaches the cell's edge.
    pub const HATCH_LEAN: f32 = 26.0;
    /// The gap between a chain row's position badge and its working-order badge.
    pub const MARKER_GAP: f32 = 3.0;
    /// The side-tinted fill under the pointer on a tradeable bid or ask cell.
    pub const CELL_HOVER_ALPHA: u8 = 28;
    /// The dimmed diagonal hatch on an in-the-money cell that has a quote.
    pub const ITM_HATCH_ALPHA: u8 = 70;
    /// The height of a position badge and of a working-order marker, centred on the row.
    pub const MARKER_H: f32 = 16.0;
    /// The corner radius of a position badge and of a working-order marker.
    pub const MARKER_RADIUS: f32 = 3.0;
    /// A working-order marker's fill while the pointer is not on it.
    pub const ORDER_FILL_ALPHA: u8 = 45;
    /// A working-order marker's fill while the pointer is on it.
    pub const ORDER_FILL_HOT_ALPHA: u8 = 95;
    /// A position badge's fill, behind its own outline.
    pub const POSITION_FILL_ALPHA: u8 = 55;
    /// The volume magnitude bar under a row's Volume cell.
    pub const VOLUME_BAR_ALPHA: u8 = 120;
    /// The height of the volume magnitude bar in a row's Volume cell.
    pub const VOLUME_BAR_H: f32 = 3.0;

    /// Every row of this group as data, in the order the TOML lists them.
    pub const ENTRIES: &[super::Entry] = &[
        super::Entry { group: "options_chain", name: "BADGE_START_OFFSET", unit: Some(super::Unit::Px), value: super::Data::F32(BADGE_START_OFFSET), doc: "How far in from the left edge of its side's first column a chain row's position and working-order badges start." },
        super::Entry { group: "options_chain", name: "HATCH_LEAN", unit: Some(super::Unit::Px), value: super::Data::F32(HATCH_LEAN), doc: "How far each diagonal of the in-the-money hatch leans sideways while it climbs a row; the first stroke starts this far left of the cell so its top end reaches the cell's edge." },
        super::Entry { group: "options_chain", name: "MARKER_GAP", unit: Some(super::Unit::Px), value: super::Data::F32(MARKER_GAP), doc: "The gap between a chain row's position badge and its working-order badge." },
        super::Entry { group: "options_chain", name: "CELL_HOVER_ALPHA", unit: Some(super::Unit::Alpha), value: super::Data::U8(CELL_HOVER_ALPHA), doc: "The side-tinted fill under the pointer on a tradeable bid or ask cell." },
        super::Entry { group: "options_chain", name: "ITM_HATCH_ALPHA", unit: Some(super::Unit::Alpha), value: super::Data::U8(ITM_HATCH_ALPHA), doc: "The dimmed diagonal hatch on an in-the-money cell that has a quote." },
        super::Entry { group: "options_chain", name: "MARKER_H", unit: Some(super::Unit::Px), value: super::Data::F32(MARKER_H), doc: "The height of a position badge and of a working-order marker, centred on the row." },
        super::Entry { group: "options_chain", name: "MARKER_RADIUS", unit: Some(super::Unit::Px), value: super::Data::F32(MARKER_RADIUS), doc: "The corner radius of a position badge and of a working-order marker." },
        super::Entry { group: "options_chain", name: "ORDER_FILL_ALPHA", unit: Some(super::Unit::Alpha), value: super::Data::U8(ORDER_FILL_ALPHA), doc: "A working-order marker's fill while the pointer is not on it." },
        super::Entry { group: "options_chain", name: "ORDER_FILL_HOT_ALPHA", unit: Some(super::Unit::Alpha), value: super::Data::U8(ORDER_FILL_HOT_ALPHA), doc: "A working-order marker's fill while the pointer is on it." },
        super::Entry { group: "options_chain", name: "POSITION_FILL_ALPHA", unit: Some(super::Unit::Alpha), value: super::Data::U8(POSITION_FILL_ALPHA), doc: "A position badge's fill, behind its own outline." },
        super::Entry { group: "options_chain", name: "VOLUME_BAR_ALPHA", unit: Some(super::Unit::Alpha), value: super::Data::U8(VOLUME_BAR_ALPHA), doc: "The volume magnitude bar under a row's Volume cell." },
        super::Entry { group: "options_chain", name: "VOLUME_BAR_H", unit: Some(super::Unit::Px), value: super::Data::F32(VOLUME_BAR_H), doc: "The height of the volume magnitude bar in a row's Volume cell." },
    ];
}

/// Settings: the `[[value]]` rows of `ui-theme.toml` whose `group` is `settings`.
pub mod settings {
    /// The Settings window: the height of the theme or market name under each preview.
    pub const NAME_H: f32 = 18.0;

    /// Every row of this group as data, in the order the TOML lists them.
    pub const ENTRIES: &[super::Entry] = &[
        super::Entry { group: "settings", name: "NAME_H", unit: Some(super::Unit::Px), value: super::Data::F32(NAME_H), doc: "The Settings window: the height of the theme or market name under each preview." },
    ];
}

/// Status: the `[[value]]` rows of `ui-theme.toml` whose `group` is `status`.
pub mod status {
    /// Every row of this group as data, in the order the TOML lists them.
    pub const ENTRIES: &[super::Entry] = &[
    ];
}

/// Status Bar: the `[[value]]` rows of `ui-theme.toml` whose `group` is `status_bar`.
pub mod status_bar {
    /// The bottom status bar strip's height; it does not follow density.
    pub const STATUS_BAR_H: f32 = 22.0;

    /// Every row of this group as data, in the order the TOML lists them.
    pub const ENTRIES: &[super::Entry] = &[
        super::Entry { group: "status_bar", name: "STATUS_BAR_H", unit: Some(super::Unit::Px), value: super::Data::F32(STATUS_BAR_H), doc: "The bottom status bar strip's height; it does not follow density." },
    ];
}

/// Stored: the `[[value]]` rows of `ui-theme.toml` whose `group` is `stored`.
pub mod stored {
    /// The width kept free beside the stored-data inspector, taken off the grid so grid and inspector sit side by side without overflowing.
    pub const INSPECTOR_RESERVE: f32 = 10.0;
    /// The Stored window's series grid: its narrowest width beside the inspector.
    pub const GRID_MIN_W: f32 = 200.0;
    /// The Stored window's series inspector (right pane): its share of the window's width, up to its widest.
    pub const INSPECTOR_FRAC: f32 = 0.3;
    /// The Stored window's inspector: the box each label (COVERS, ROWS, SIZE, PARTS) sits in, left of its value.
    pub const INSPECTOR_KEY_SIZE: egui::Vec2 = egui::vec2(78.0, 14.0);
    /// The Stored window's series inspector: its widest width.
    pub const INSPECTOR_MAX_W: f32 = 330.0;
    /// The Stored window's series inspector: a window too narrow to give it this much hides it.
    pub const INSPECTOR_MIN_W: f32 = 120.0;

    /// Every row of this group as data, in the order the TOML lists them.
    pub const ENTRIES: &[super::Entry] = &[
        super::Entry { group: "stored", name: "INSPECTOR_RESERVE", unit: Some(super::Unit::Px), value: super::Data::F32(INSPECTOR_RESERVE), doc: "The width kept free beside the stored-data inspector, taken off the grid so grid and inspector sit side by side without overflowing." },
        super::Entry { group: "stored", name: "GRID_MIN_W", unit: Some(super::Unit::Px), value: super::Data::F32(GRID_MIN_W), doc: "The Stored window's series grid: its narrowest width beside the inspector." },
        super::Entry { group: "stored", name: "INSPECTOR_FRAC", unit: Some(super::Unit::Ratio), value: super::Data::F32(INSPECTOR_FRAC), doc: "The Stored window's series inspector (right pane): its share of the window's width, up to its widest." },
        super::Entry { group: "stored", name: "INSPECTOR_KEY_SIZE", unit: Some(super::Unit::Px), value: super::Data::Vec2(INSPECTOR_KEY_SIZE), doc: "The Stored window's inspector: the box each label (COVERS, ROWS, SIZE, PARTS) sits in, left of its value." },
        super::Entry { group: "stored", name: "INSPECTOR_MAX_W", unit: Some(super::Unit::Px), value: super::Data::F32(INSPECTOR_MAX_W), doc: "The Stored window's series inspector: its widest width." },
        super::Entry { group: "stored", name: "INSPECTOR_MIN_W", unit: Some(super::Unit::Px), value: super::Data::F32(INSPECTOR_MIN_W), doc: "The Stored window's series inspector: a window too narrow to give it this much hides it." },
    ];
}

/// Studio: the `[[value]]` rows of `ui-theme.toml` whose `group` is `studio`.
pub mod studio {
    /// The width the Sweep template drop-down leaves free on its row for the button beside it.
    pub const COMBO_BESIDE_BUTTON_RESERVE: f32 = 64.0;
    /// The width reserved for the Save button on the Saved strategies name row; the name field takes whatever is left.
    pub const SAVE_BUTTON_W: f32 = 64.0;
    /// The width of the compute daemon address box (host:port) for the Remote and Named backends.
    pub const BACKEND_ADDR_W: f32 = 160.0;
    /// The width of the Builder service address box.
    pub const BUILDER_ADDR_W: f32 = 140.0;
    /// The tallest the assistant's Review changes diff gets before it scrolls.
    pub const CHAT_DIFF_MAX_H: f32 = 180.0;
    /// The tallest the assistant chat history gets before it scrolls.
    pub const CHAT_HISTORY_MAX_H: f32 = 220.0;
    /// The tallest the strategy compare table gets before it scrolls.
    pub const COMPARE_SCROLL_MAX_H: f32 = 280.0;
    /// The Data pane's minimum content width: narrower, the catalog grid scrolls sideways instead of squeezing its cells.
    pub const DATA_BROWSER_MIN_W: f32 = 700.0;
    /// The width of the data-slice selector in the Studio's data picker.
    pub const DATA_SLICE_COMBO_W: f32 = 230.0;
    /// The expanded editor panel's opening width; the user can drag it.
    pub const EDITOR_DEFAULT_W: f32 = 460.0;
    /// The Studio code editor's font size, which also sets how many rows fill its panel.
    pub const EDITOR_FONT_SIZE: f32 = 13.0;
    /// The width of the collapsed editor panel: just the expand button.
    pub const EDITOR_MIN_W: f32 = 28.0;
    /// The box for extra tick symbols to replay alongside the selected one, shown beside a tick data pick.
    pub const EXTRA_SYMBOLS_BOX: egui::Vec2 = egui::vec2(140.0, 20.0);
    /// The name column of the sweep grid's script parameters; it truncates, so a long name cannot push the value field out.
    pub const GRID_NAME_W: f32 = 96.0;
    /// The tallest the indicator list gets before it scrolls.
    pub const INDICATOR_LIST_MAX_H: f32 = 220.0;
    /// The height of the indicator preview plot.
    pub const INDICATOR_PREVIEW_H: f32 = 160.0;
    /// The narrowest an indicator parameter slider gets.
    pub const INDICATOR_SLIDER_MIN_W: f32 = 80.0;
    /// The room kept free beside each indicator parameter slider for its trailing label.
    pub const INDICATOR_SLIDER_RESERVE: f32 = 150.0;
    /// The height of a native strategy parameter's key and value boxes.
    pub const PARAM_FIELD_H: f32 = 18.0;
    /// The key box of a native strategy parameter row.
    pub const PARAM_KEY_W: f32 = 96.0;
    /// The width of the Studio tool rail down the window's right edge; its buttons are this less 8.
    pub const RAIL_WIDTH: f32 = 40.0;
    /// The tallest the research runs list gets before it scrolls.
    pub const RUN_LIST_MAX_H: f32 = 220.0;
    /// The height of the Run backtest row in the Studio's centre panel.
    pub const RUN_ROW_H: f32 = 28.0;
    /// The widest the Run backtest row is laid out; a wider panel leaves the rest empty.
    pub const RUN_ROW_MAX_W: f32 = 280.0;
    /// The tallest the saved-strategies list gets before it scrolls.
    pub const SAVED_LIST_MAX_H: f32 = 200.0;
    /// The equity sparkline in each row of the strategy compare table.
    pub const SPARKLINE_SIZE: egui::Vec2 = egui::vec2(64.0, 18.0);
    /// The tallest the research studies list gets before it scrolls.
    pub const STUDY_LIST_MAX_H: f32 = 150.0;
    /// The tallest the template gallery gets before it scrolls.
    pub const TEMPLATE_GALLERY_MAX_H: f32 = 260.0;
    /// The width of the Studio tools panel beside the tool rail.
    pub const TOOLS_PANEL_W: f32 = 340.0;
    /// The height of the walk-forward out-of-sample equity plot.
    pub const WF_PLOT_H: f32 = 200.0;

    /// Every row of this group as data, in the order the TOML lists them.
    pub const ENTRIES: &[super::Entry] = &[
        super::Entry { group: "studio", name: "COMBO_BESIDE_BUTTON_RESERVE", unit: Some(super::Unit::Px), value: super::Data::F32(COMBO_BESIDE_BUTTON_RESERVE), doc: "The width the Sweep template drop-down leaves free on its row for the button beside it." },
        super::Entry { group: "studio", name: "SAVE_BUTTON_W", unit: Some(super::Unit::Px), value: super::Data::F32(SAVE_BUTTON_W), doc: "The width reserved for the Save button on the Saved strategies name row; the name field takes whatever is left." },
        super::Entry { group: "studio", name: "BACKEND_ADDR_W", unit: Some(super::Unit::Px), value: super::Data::F32(BACKEND_ADDR_W), doc: "The width of the compute daemon address box (host:port) for the Remote and Named backends." },
        super::Entry { group: "studio", name: "BUILDER_ADDR_W", unit: Some(super::Unit::Px), value: super::Data::F32(BUILDER_ADDR_W), doc: "The width of the Builder service address box." },
        super::Entry { group: "studio", name: "CHAT_DIFF_MAX_H", unit: Some(super::Unit::Px), value: super::Data::F32(CHAT_DIFF_MAX_H), doc: "The tallest the assistant's Review changes diff gets before it scrolls." },
        super::Entry { group: "studio", name: "CHAT_HISTORY_MAX_H", unit: Some(super::Unit::Px), value: super::Data::F32(CHAT_HISTORY_MAX_H), doc: "The tallest the assistant chat history gets before it scrolls." },
        super::Entry { group: "studio", name: "COMPARE_SCROLL_MAX_H", unit: Some(super::Unit::Px), value: super::Data::F32(COMPARE_SCROLL_MAX_H), doc: "The tallest the strategy compare table gets before it scrolls." },
        super::Entry { group: "studio", name: "DATA_BROWSER_MIN_W", unit: Some(super::Unit::Px), value: super::Data::F32(DATA_BROWSER_MIN_W), doc: "The Data pane's minimum content width: narrower, the catalog grid scrolls sideways instead of squeezing its cells." },
        super::Entry { group: "studio", name: "DATA_SLICE_COMBO_W", unit: Some(super::Unit::Px), value: super::Data::F32(DATA_SLICE_COMBO_W), doc: "The width of the data-slice selector in the Studio's data picker." },
        super::Entry { group: "studio", name: "EDITOR_DEFAULT_W", unit: Some(super::Unit::Px), value: super::Data::F32(EDITOR_DEFAULT_W), doc: "The expanded editor panel's opening width; the user can drag it." },
        super::Entry { group: "studio", name: "EDITOR_FONT_SIZE", unit: Some(super::Unit::Px), value: super::Data::F32(EDITOR_FONT_SIZE), doc: "The Studio code editor's font size, which also sets how many rows fill its panel." },
        super::Entry { group: "studio", name: "EDITOR_MIN_W", unit: Some(super::Unit::Px), value: super::Data::F32(EDITOR_MIN_W), doc: "The width of the collapsed editor panel: just the expand button." },
        super::Entry { group: "studio", name: "EXTRA_SYMBOLS_BOX", unit: Some(super::Unit::Px), value: super::Data::Vec2(EXTRA_SYMBOLS_BOX), doc: "The box for extra tick symbols to replay alongside the selected one, shown beside a tick data pick." },
        super::Entry { group: "studio", name: "GRID_NAME_W", unit: Some(super::Unit::Px), value: super::Data::F32(GRID_NAME_W), doc: "The name column of the sweep grid's script parameters; it truncates, so a long name cannot push the value field out." },
        super::Entry { group: "studio", name: "INDICATOR_LIST_MAX_H", unit: Some(super::Unit::Px), value: super::Data::F32(INDICATOR_LIST_MAX_H), doc: "The tallest the indicator list gets before it scrolls." },
        super::Entry { group: "studio", name: "INDICATOR_PREVIEW_H", unit: Some(super::Unit::Px), value: super::Data::F32(INDICATOR_PREVIEW_H), doc: "The height of the indicator preview plot." },
        super::Entry { group: "studio", name: "INDICATOR_SLIDER_MIN_W", unit: Some(super::Unit::Px), value: super::Data::F32(INDICATOR_SLIDER_MIN_W), doc: "The narrowest an indicator parameter slider gets." },
        super::Entry { group: "studio", name: "INDICATOR_SLIDER_RESERVE", unit: Some(super::Unit::Px), value: super::Data::F32(INDICATOR_SLIDER_RESERVE), doc: "The room kept free beside each indicator parameter slider for its trailing label." },
        super::Entry { group: "studio", name: "PARAM_FIELD_H", unit: Some(super::Unit::Px), value: super::Data::F32(PARAM_FIELD_H), doc: "The height of a native strategy parameter's key and value boxes." },
        super::Entry { group: "studio", name: "PARAM_KEY_W", unit: Some(super::Unit::Px), value: super::Data::F32(PARAM_KEY_W), doc: "The key box of a native strategy parameter row." },
        super::Entry { group: "studio", name: "RAIL_WIDTH", unit: Some(super::Unit::Px), value: super::Data::F32(RAIL_WIDTH), doc: "The width of the Studio tool rail down the window's right edge; its buttons are this less 8." },
        super::Entry { group: "studio", name: "RUN_LIST_MAX_H", unit: Some(super::Unit::Px), value: super::Data::F32(RUN_LIST_MAX_H), doc: "The tallest the research runs list gets before it scrolls." },
        super::Entry { group: "studio", name: "RUN_ROW_H", unit: Some(super::Unit::Px), value: super::Data::F32(RUN_ROW_H), doc: "The height of the Run backtest row in the Studio's centre panel." },
        super::Entry { group: "studio", name: "RUN_ROW_MAX_W", unit: Some(super::Unit::Px), value: super::Data::F32(RUN_ROW_MAX_W), doc: "The widest the Run backtest row is laid out; a wider panel leaves the rest empty." },
        super::Entry { group: "studio", name: "SAVED_LIST_MAX_H", unit: Some(super::Unit::Px), value: super::Data::F32(SAVED_LIST_MAX_H), doc: "The tallest the saved-strategies list gets before it scrolls." },
        super::Entry { group: "studio", name: "SPARKLINE_SIZE", unit: Some(super::Unit::Px), value: super::Data::Vec2(SPARKLINE_SIZE), doc: "The equity sparkline in each row of the strategy compare table." },
        super::Entry { group: "studio", name: "STUDY_LIST_MAX_H", unit: Some(super::Unit::Px), value: super::Data::F32(STUDY_LIST_MAX_H), doc: "The tallest the research studies list gets before it scrolls." },
        super::Entry { group: "studio", name: "TEMPLATE_GALLERY_MAX_H", unit: Some(super::Unit::Px), value: super::Data::F32(TEMPLATE_GALLERY_MAX_H), doc: "The tallest the template gallery gets before it scrolls." },
        super::Entry { group: "studio", name: "TOOLS_PANEL_W", unit: Some(super::Unit::Px), value: super::Data::F32(TOOLS_PANEL_W), doc: "The width of the Studio tools panel beside the tool rail." },
        super::Entry { group: "studio", name: "WF_PLOT_H", unit: Some(super::Unit::Px), value: super::Data::F32(WF_PLOT_H), doc: "The height of the walk-forward out-of-sample equity plot." },
    ];
}

/// Symbol Picker: the `[[value]]` rows of `ui-theme.toml` whose `group` is `symbol_picker`.
pub mod symbol_picker {
    /// How far from the row's left edge an instrument search-result row's description column starts.
    pub const DESC_INSET: f32 = 110.0;
    /// How tall the symbol picker's result list grows before it scrolls.
    pub const LIST_H: f32 = 300.0;
    /// The symbol picker's width: its search field and its result lines take all of it.
    pub const PICKER_W: f32 = 470.0;
    /// The height of one instrument search-result row.
    pub const ROW_H: f32 = 22.0;
    /// The narrowest an instrument search-result row is allowed to be (it otherwise takes the full available width).
    pub const ROW_MIN_W: f32 = 230.0;

    /// Every row of this group as data, in the order the TOML lists them.
    pub const ENTRIES: &[super::Entry] = &[
        super::Entry { group: "symbol_picker", name: "DESC_INSET", unit: Some(super::Unit::Px), value: super::Data::F32(DESC_INSET), doc: "How far from the row's left edge an instrument search-result row's description column starts." },
        super::Entry { group: "symbol_picker", name: "LIST_H", unit: Some(super::Unit::Px), value: super::Data::F32(LIST_H), doc: "How tall the symbol picker's result list grows before it scrolls." },
        super::Entry { group: "symbol_picker", name: "PICKER_W", unit: Some(super::Unit::Px), value: super::Data::F32(PICKER_W), doc: "The symbol picker's width: its search field and its result lines take all of it." },
        super::Entry { group: "symbol_picker", name: "ROW_H", unit: Some(super::Unit::Px), value: super::Data::F32(ROW_H), doc: "The height of one instrument search-result row." },
        super::Entry { group: "symbol_picker", name: "ROW_MIN_W", unit: Some(super::Unit::Px), value: super::Data::F32(ROW_MIN_W), doc: "The narrowest an instrument search-result row is allowed to be (it otherwise takes the full available width)." },
    ];
}

/// Title Bar: the `[[value]]` rows of `ui-theme.toml` whose `group` is `title_bar`.
pub mod title_bar {
    /// The tab chips' height in a tool window's title bar, seated on the bar's bottom edge so the selected one's missing fourth edge is the break in the hairline under the bar.
    pub const TAB_H: f32 = 24.0;
    /// Below this title bar width a tabbed window's title drops and its segments tighten; a tool window also opens at this width, so the narrow shape is the ordinary one.
    pub const TITLE_DROP_W: f32 = 560.0;

    /// Every row of this group as data, in the order the TOML lists them.
    pub const ENTRIES: &[super::Entry] = &[
        super::Entry { group: "title_bar", name: "TAB_H", unit: Some(super::Unit::Px), value: super::Data::F32(TAB_H), doc: "The tab chips' height in a tool window's title bar, seated on the bar's bottom edge so the selected one's missing fourth edge is the break in the hairline under the bar." },
        super::Entry { group: "title_bar", name: "TITLE_DROP_W", unit: Some(super::Unit::Px), value: super::Data::F32(TITLE_DROP_W), doc: "Below this title bar width a tabbed window's title drops and its segments tighten; a tool window also opens at this width, so the narrow shape is the ordinary one." },
    ];
}

/// Trade: the `[[value]]` rows of `ui-theme.toml` whose `group` is `trade`.
pub mod trade {
    /// The line width of the ladder toolbar's crosshair glyph (its circle and four ticks): 1.3 pt, the v3 design's own.
    pub const CROSS_GLYPH_W: f32 = 1.3;
    /// The line width of the accent ring round an order marker while it is dragged: 1.6 pt, the v3 design's own (not a step of the stroke scale; the ring still stands off the marker by one LINE).
    pub const DRAG_RING_W: f32 = 1.6;
    /// The line width of the ladder toolbar's minus and plus glyphs (Group - and +): 1.28 pt, the v3 design's own.
    pub const PLUS_MINUS_GLYPH_W: f32 = 1.28;
    /// The room each side of the hairline that sets the two pane toggles apart from the two layout toggles in the title bar (the design's `.tsep`): 5 pt, the v3 design's own number (not a step of the scale).
    pub const VIEW_GROUP_GAP: f32 = 5.0;
    /// How far a Vol bar sits inside its row, at the top and the bottom, in the ladder's Vol column: 5 pt, the v3 design's own number (not a step of the scale).
    pub const VOL_BAR_INSET: f32 = 5.0;
    /// How far a size may run past its column in the Trade window's ladder before it is printed to fewer decimals.
    pub const FIT_SLACK: f32 = 1.0;
    /// What the Trade window's instrument bar leaves unspent when it measures how many rows its pickers and chip need.
    pub const SPARE: f32 = 4.0;
    /// The account picker popup's narrowest width.
    pub const ACCOUNT_PICKER_W: f32 = 356.0;
    /// The Trade window with the ticket beside the ladder (spec 3.9): 600 x 560.
    pub const BESIDE_SIZE: egui::Vec2 = egui::vec2(600.0, 560.0);
    /// How opaque a Trades bubble is (0 to 1), so two prints on one row both read.
    pub const BUBBLE_ALPHA: f32 = 0.7;
    /// The radius of the largest bubble on the tick chart's Trades layer, in multiples of the row height.
    pub const BUBBLE_MAX_ROWS: f32 = 0.5;
    /// The radius of the smallest bubble on the tick chart's Trades layer, in multiples of the row height.
    pub const BUBBLE_MIN_ROWS: f32 = 0.18;
    /// The tick-chart pane's width beside the ladder (the v3 design's: its window is 301 wider with the chart on); a window made wider gives the extra to the ladder, one too narrow for both gives the chart up first.
    pub const CHART_W: f32 = 301.0;
    /// The design's width of the ladder's Bid column and of its Ask column, in every layout.
    pub const COL_BID_ASK_W: f32 = 64.0;
    /// The design's width of the ladder's Buy column and of its Sell column with the ticket under the ladder, where there is no Vol column and they are wider.
    pub const COL_BUY_SELL_UNDER_W: f32 = 48.0;
    /// The design's width of the ladder's Buy column and of its Sell column beside the ticket (the columns scale to the width the ladder has).
    pub const COL_BUY_SELL_W: f32 = 44.0;
    /// The design's width of the ladder's Price column; with the Vol column off it takes Vol's width as well.
    pub const COL_PRICE_W: f32 = 76.0;
    /// The design's width of the ladder's Vol column (shown only beside the ticket, when the toolbar's first button turns it on).
    pub const COL_VOL_W: f32 = 30.0;
    /// The window holding only the compact ticket (panel under, no ladder, no chart): the design's 320 pt column; its height is counted from the ticket's rows.
    pub const COMPACT_ALONE_W: f32 = 320.0;
    /// The dash of a stop order's pill edge: 3.5 pt on, as the design's browser draws a one-point dashed border.
    pub const DASH: f32 = 3.5;
    /// The gap between two dashes of a stop order's pill edge: 1.5 pt off.
    pub const DASH_GAP: f32 = 1.5;
    /// The round cap of a ladder toolbar glyph's strokes: the corner radius of the thin rounded rectangle that stands in for a round-capped line.
    pub const GLYPH_CAP_RADIUS: u8 = 1;
    /// The least width of the box holding the grouping's number, so 1 and 10 do not move the buttons beside it.
    pub const GROUP_NUM_W: f32 = 16.0;
    /// How much shorter than a control the ladder toolbar's icon buttons are drawn (24 x 20 at Normal density).
    pub const ICON_SHRINK: f32 = 4.0;
    /// The width a ladder toolbar icon button (Vol, Recenter) is drawn; the click target keeps the whole control height.
    pub const ICON_W: f32 = 24.0;
    /// How tall the symbol picker's list of matches grows before it scrolls.
    pub const MATCHES_H: f32 = 260.0;
    /// The narrowest tick-chart pane drawn; any narrower and the chart is left out, like a ladder under its own minimum.
    pub const MIN_CHART_W: f32 = 160.0;
    /// The narrowest a number field in the order ticket is drawn, however crowded its row.
    pub const MIN_FIELD_W: f32 = 48.0;
    /// The narrowest ladder drawn beside the ticket; any narrower and the ticket takes the whole body.
    pub const MIN_LADDER_W: f32 = 200.0;
    /// Where the bookless ladder's message block starts: this far above the middle of its rect, about half the block's own height, so it reads centred.
    pub const NO_BOOK_RISE: f32 = 46.0;
    /// The one-point edge of a limit order's pill that the venue can reprice: its side's colour at 140 / 255 (the design's .55).
    pub const PILL_EDGE_ALPHA: u8 = 140;
    /// The fill of a limit order's pill on the ladder: its side's colour at 56 / 255 (the design's rgba(..., .22)).
    pub const PILL_FILL_ALPHA: u8 = 56;
    /// The wash a pill takes under the pointer: the text colour at 36 / 255 (the design's brightness(1.25)).
    pub const PILL_LIFT_ALPHA: u8 = 36;
    /// The corner radius of an own-order pill on the ladder: the design's 3 px, tighter than the kit's 4.
    pub const PILL_RADIUS: u8 = 3;
    /// How opaque the price labels down the tick chart's right edge are (0 to 1): the muted text colour at this strength.
    pub const PRICE_LABEL_ALPHA: f32 = 0.9;
    /// How much smaller than a control the grouping's minus and plus squares are drawn.
    pub const SQUARE_SHRINK: f32 = 6.0;
    /// The least diameter of the status strip's dot, at any text size.
    pub const STATUS_DOT_MIN: f32 = 5.0;
    /// The status strip's dot diameter as a share of the Caption text size (a little over half), rounded to whole points.
    pub const STATUS_DOT_RATIO: f32 = 0.6;
    /// The symbol picker popup's width: its search field and its lines (a chip for every venue) take all of it; wider than the window can be.
    pub const SYMBOL_PICKER_W: f32 = 380.0;
    /// Below this width the instrument bar takes three rows (symbol picker and chip, account picker, price group), measured at Normal density and Small text; a looser or larger look grows it.
    pub const THREE_ROWS_BELOW: f32 = 388.0;
    /// The height of the window holding only the ticket (the design's 280 x 560).
    pub const TICKET_ONLY_H: f32 = 560.0;
    /// How much wider than the ticket the window holding only the ticket is: the pad round it (the design's 280 x 560 over a 260 ticket).
    pub const TICKET_ONLY_PAD: f32 = 20.0;
    /// The order ticket's width beside the ladder at Small text, the design's own; the ticket grows it with the text size (286 at Standard, 338 at Large).
    pub const TICKET_W: f32 = 260.0;
    /// Below this width the instrument bar takes two rows (the pickers and the chip, then the price group), measured at Normal density and Small text; a looser or larger look grows it.
    pub const TWO_ROWS_BELOW: f32 = 470.0;
    /// How much shorter the ticket-under window is with the chart and the ladder side by side over the ticket than the plain 680: the design's 621 x 659.
    pub const UNDER_BOTH_LESS_H: f32 = 21.0;
    /// The Trade window with the ticket under the ladder (spec 3.9): 320 x 680.
    pub const UNDER_SIZE: egui::Vec2 = egui::vec2(320.0, 680.0);
    /// One of the four view toggles in the Trade window's title bar (the design's `.ib`): 24 x 20 at every density.
    pub const VIEW_BUTTON: egui::Vec2 = egui::vec2(24.0, 20.0);
    /// The traded-volume bar in the ladder's Vol column: the grey text colour at 71 / 255 (the design's rgba(..., .28)).
    pub const VOL_BAR_ALPHA: u8 = 71;
    /// The wash the ladder cell under the pointer takes: the text colour at 13 / 255 (the design's .05).
    pub const WASH_ALPHA: u8 = 13;

    /// Every row of this group as data, in the order the TOML lists them.
    pub const ENTRIES: &[super::Entry] = &[
        super::Entry { group: "trade", name: "CROSS_GLYPH_W", unit: Some(super::Unit::Px), value: super::Data::F32(CROSS_GLYPH_W), doc: "The line width of the ladder toolbar's crosshair glyph (its circle and four ticks): 1.3 pt, the v3 design's own." },
        super::Entry { group: "trade", name: "DRAG_RING_W", unit: Some(super::Unit::Px), value: super::Data::F32(DRAG_RING_W), doc: "The line width of the accent ring round an order marker while it is dragged: 1.6 pt, the v3 design's own (not a step of the stroke scale; the ring still stands off the marker by one LINE)." },
        super::Entry { group: "trade", name: "PLUS_MINUS_GLYPH_W", unit: Some(super::Unit::Px), value: super::Data::F32(PLUS_MINUS_GLYPH_W), doc: "The line width of the ladder toolbar's minus and plus glyphs (Group - and +): 1.28 pt, the v3 design's own." },
        super::Entry { group: "trade", name: "VIEW_GROUP_GAP", unit: Some(super::Unit::Px), value: super::Data::F32(VIEW_GROUP_GAP), doc: "The room each side of the hairline that sets the two pane toggles apart from the two layout toggles in the title bar (the design's `.tsep`): 5 pt, the v3 design's own number (not a step of the scale)." },
        super::Entry { group: "trade", name: "VOL_BAR_INSET", unit: Some(super::Unit::Px), value: super::Data::F32(VOL_BAR_INSET), doc: "How far a Vol bar sits inside its row, at the top and the bottom, in the ladder's Vol column: 5 pt, the v3 design's own number (not a step of the scale)." },
        super::Entry { group: "trade", name: "FIT_SLACK", unit: Some(super::Unit::Px), value: super::Data::F32(FIT_SLACK), doc: "How far a size may run past its column in the Trade window's ladder before it is printed to fewer decimals." },
        super::Entry { group: "trade", name: "SPARE", unit: Some(super::Unit::Px), value: super::Data::F32(SPARE), doc: "What the Trade window's instrument bar leaves unspent when it measures how many rows its pickers and chip need." },
        super::Entry { group: "trade", name: "ACCOUNT_PICKER_W", unit: Some(super::Unit::Px), value: super::Data::F32(ACCOUNT_PICKER_W), doc: "The account picker popup's narrowest width." },
        super::Entry { group: "trade", name: "BESIDE_SIZE", unit: Some(super::Unit::Px), value: super::Data::Vec2(BESIDE_SIZE), doc: "The Trade window with the ticket beside the ladder (spec 3.9): 600 x 560." },
        super::Entry { group: "trade", name: "BUBBLE_ALPHA", unit: Some(super::Unit::Ratio), value: super::Data::F32(BUBBLE_ALPHA), doc: "How opaque a Trades bubble is (0 to 1), so two prints on one row both read." },
        super::Entry { group: "trade", name: "BUBBLE_MAX_ROWS", unit: Some(super::Unit::Ratio), value: super::Data::F32(BUBBLE_MAX_ROWS), doc: "The radius of the largest bubble on the tick chart's Trades layer, in multiples of the row height." },
        super::Entry { group: "trade", name: "BUBBLE_MIN_ROWS", unit: Some(super::Unit::Ratio), value: super::Data::F32(BUBBLE_MIN_ROWS), doc: "The radius of the smallest bubble on the tick chart's Trades layer, in multiples of the row height." },
        super::Entry { group: "trade", name: "CHART_W", unit: Some(super::Unit::Px), value: super::Data::F32(CHART_W), doc: "The tick-chart pane's width beside the ladder (the v3 design's: its window is 301 wider with the chart on); a window made wider gives the extra to the ladder, one too narrow for both gives the chart up first." },
        super::Entry { group: "trade", name: "COL_BID_ASK_W", unit: Some(super::Unit::Px), value: super::Data::F32(COL_BID_ASK_W), doc: "The design's width of the ladder's Bid column and of its Ask column, in every layout." },
        super::Entry { group: "trade", name: "COL_BUY_SELL_UNDER_W", unit: Some(super::Unit::Px), value: super::Data::F32(COL_BUY_SELL_UNDER_W), doc: "The design's width of the ladder's Buy column and of its Sell column with the ticket under the ladder, where there is no Vol column and they are wider." },
        super::Entry { group: "trade", name: "COL_BUY_SELL_W", unit: Some(super::Unit::Px), value: super::Data::F32(COL_BUY_SELL_W), doc: "The design's width of the ladder's Buy column and of its Sell column beside the ticket (the columns scale to the width the ladder has)." },
        super::Entry { group: "trade", name: "COL_PRICE_W", unit: Some(super::Unit::Px), value: super::Data::F32(COL_PRICE_W), doc: "The design's width of the ladder's Price column; with the Vol column off it takes Vol's width as well." },
        super::Entry { group: "trade", name: "COL_VOL_W", unit: Some(super::Unit::Px), value: super::Data::F32(COL_VOL_W), doc: "The design's width of the ladder's Vol column (shown only beside the ticket, when the toolbar's first button turns it on)." },
        super::Entry { group: "trade", name: "COMPACT_ALONE_W", unit: Some(super::Unit::Px), value: super::Data::F32(COMPACT_ALONE_W), doc: "The window holding only the compact ticket (panel under, no ladder, no chart): the design's 320 pt column; its height is counted from the ticket's rows." },
        super::Entry { group: "trade", name: "DASH", unit: Some(super::Unit::Px), value: super::Data::F32(DASH), doc: "The dash of a stop order's pill edge: 3.5 pt on, as the design's browser draws a one-point dashed border." },
        super::Entry { group: "trade", name: "DASH_GAP", unit: Some(super::Unit::Px), value: super::Data::F32(DASH_GAP), doc: "The gap between two dashes of a stop order's pill edge: 1.5 pt off." },
        super::Entry { group: "trade", name: "GLYPH_CAP_RADIUS", unit: Some(super::Unit::Px), value: super::Data::U8(GLYPH_CAP_RADIUS), doc: "The round cap of a ladder toolbar glyph's strokes: the corner radius of the thin rounded rectangle that stands in for a round-capped line." },
        super::Entry { group: "trade", name: "GROUP_NUM_W", unit: Some(super::Unit::Px), value: super::Data::F32(GROUP_NUM_W), doc: "The least width of the box holding the grouping's number, so 1 and 10 do not move the buttons beside it." },
        super::Entry { group: "trade", name: "ICON_SHRINK", unit: Some(super::Unit::Px), value: super::Data::F32(ICON_SHRINK), doc: "How much shorter than a control the ladder toolbar's icon buttons are drawn (24 x 20 at Normal density)." },
        super::Entry { group: "trade", name: "ICON_W", unit: Some(super::Unit::Px), value: super::Data::F32(ICON_W), doc: "The width a ladder toolbar icon button (Vol, Recenter) is drawn; the click target keeps the whole control height." },
        super::Entry { group: "trade", name: "MATCHES_H", unit: Some(super::Unit::Px), value: super::Data::F32(MATCHES_H), doc: "How tall the symbol picker's list of matches grows before it scrolls." },
        super::Entry { group: "trade", name: "MIN_CHART_W", unit: Some(super::Unit::Px), value: super::Data::F32(MIN_CHART_W), doc: "The narrowest tick-chart pane drawn; any narrower and the chart is left out, like a ladder under its own minimum." },
        super::Entry { group: "trade", name: "MIN_FIELD_W", unit: Some(super::Unit::Px), value: super::Data::F32(MIN_FIELD_W), doc: "The narrowest a number field in the order ticket is drawn, however crowded its row." },
        super::Entry { group: "trade", name: "MIN_LADDER_W", unit: Some(super::Unit::Px), value: super::Data::F32(MIN_LADDER_W), doc: "The narrowest ladder drawn beside the ticket; any narrower and the ticket takes the whole body." },
        super::Entry { group: "trade", name: "NO_BOOK_RISE", unit: Some(super::Unit::Px), value: super::Data::F32(NO_BOOK_RISE), doc: "Where the bookless ladder's message block starts: this far above the middle of its rect, about half the block's own height, so it reads centred." },
        super::Entry { group: "trade", name: "PILL_EDGE_ALPHA", unit: Some(super::Unit::Alpha), value: super::Data::U8(PILL_EDGE_ALPHA), doc: "The one-point edge of a limit order's pill that the venue can reprice: its side's colour at 140 / 255 (the design's .55)." },
        super::Entry { group: "trade", name: "PILL_FILL_ALPHA", unit: Some(super::Unit::Alpha), value: super::Data::U8(PILL_FILL_ALPHA), doc: "The fill of a limit order's pill on the ladder: its side's colour at 56 / 255 (the design's rgba(..., .22))." },
        super::Entry { group: "trade", name: "PILL_LIFT_ALPHA", unit: Some(super::Unit::Alpha), value: super::Data::U8(PILL_LIFT_ALPHA), doc: "The wash a pill takes under the pointer: the text colour at 36 / 255 (the design's brightness(1.25))." },
        super::Entry { group: "trade", name: "PILL_RADIUS", unit: Some(super::Unit::Px), value: super::Data::U8(PILL_RADIUS), doc: "The corner radius of an own-order pill on the ladder: the design's 3 px, tighter than the kit's 4." },
        super::Entry { group: "trade", name: "PRICE_LABEL_ALPHA", unit: Some(super::Unit::Ratio), value: super::Data::F32(PRICE_LABEL_ALPHA), doc: "How opaque the price labels down the tick chart's right edge are (0 to 1): the muted text colour at this strength." },
        super::Entry { group: "trade", name: "SQUARE_SHRINK", unit: Some(super::Unit::Px), value: super::Data::F32(SQUARE_SHRINK), doc: "How much smaller than a control the grouping's minus and plus squares are drawn." },
        super::Entry { group: "trade", name: "STATUS_DOT_MIN", unit: Some(super::Unit::Px), value: super::Data::F32(STATUS_DOT_MIN), doc: "The least diameter of the status strip's dot, at any text size." },
        super::Entry { group: "trade", name: "STATUS_DOT_RATIO", unit: Some(super::Unit::Ratio), value: super::Data::F32(STATUS_DOT_RATIO), doc: "The status strip's dot diameter as a share of the Caption text size (a little over half), rounded to whole points." },
        super::Entry { group: "trade", name: "SYMBOL_PICKER_W", unit: Some(super::Unit::Px), value: super::Data::F32(SYMBOL_PICKER_W), doc: "The symbol picker popup's width: its search field and its lines (a chip for every venue) take all of it; wider than the window can be." },
        super::Entry { group: "trade", name: "THREE_ROWS_BELOW", unit: Some(super::Unit::Px), value: super::Data::F32(THREE_ROWS_BELOW), doc: "Below this width the instrument bar takes three rows (symbol picker and chip, account picker, price group), measured at Normal density and Small text; a looser or larger look grows it." },
        super::Entry { group: "trade", name: "TICKET_ONLY_H", unit: Some(super::Unit::Px), value: super::Data::F32(TICKET_ONLY_H), doc: "The height of the window holding only the ticket (the design's 280 x 560)." },
        super::Entry { group: "trade", name: "TICKET_ONLY_PAD", unit: Some(super::Unit::Px), value: super::Data::F32(TICKET_ONLY_PAD), doc: "How much wider than the ticket the window holding only the ticket is: the pad round it (the design's 280 x 560 over a 260 ticket)." },
        super::Entry { group: "trade", name: "TICKET_W", unit: Some(super::Unit::Px), value: super::Data::F32(TICKET_W), doc: "The order ticket's width beside the ladder at Small text, the design's own; the ticket grows it with the text size (286 at Standard, 338 at Large)." },
        super::Entry { group: "trade", name: "TWO_ROWS_BELOW", unit: Some(super::Unit::Px), value: super::Data::F32(TWO_ROWS_BELOW), doc: "Below this width the instrument bar takes two rows (the pickers and the chip, then the price group), measured at Normal density and Small text; a looser or larger look grows it." },
        super::Entry { group: "trade", name: "UNDER_BOTH_LESS_H", unit: Some(super::Unit::Px), value: super::Data::F32(UNDER_BOTH_LESS_H), doc: "How much shorter the ticket-under window is with the chart and the ladder side by side over the ticket than the plain 680: the design's 621 x 659." },
        super::Entry { group: "trade", name: "UNDER_SIZE", unit: Some(super::Unit::Px), value: super::Data::Vec2(UNDER_SIZE), doc: "The Trade window with the ticket under the ladder (spec 3.9): 320 x 680." },
        super::Entry { group: "trade", name: "VIEW_BUTTON", unit: Some(super::Unit::Px), value: super::Data::Vec2(VIEW_BUTTON), doc: "One of the four view toggles in the Trade window's title bar (the design's `.ib`): 24 x 20 at every density." },
        super::Entry { group: "trade", name: "VOL_BAR_ALPHA", unit: Some(super::Unit::Alpha), value: super::Data::U8(VOL_BAR_ALPHA), doc: "The traded-volume bar in the ladder's Vol column: the grey text colour at 71 / 255 (the design's rgba(..., .28))." },
        super::Entry { group: "trade", name: "WASH_ALPHA", unit: Some(super::Unit::Alpha), value: super::Data::U8(WASH_ALPHA), doc: "The wash the ladder cell under the pointer takes: the text colour at 13 / 255 (the design's .05)." },
    ];
}

/// Workspace: the `[[value]]` rows of `ui-theme.toml` whose `group` is `workspace`.
pub mod workspace {
    /// How far below a window's top the resize bands on its left and right edges start, clearing the title-bar controls.
    pub const TITLE_CLEAR: f32 = 36.0;
    /// Window > Arrange > Cascade: each window's width and height as a share of the desktop area.
    pub const ARRANGE_CASCADE_FRAC: f32 = 0.6;
    /// Window > Arrange > Cascade: the shortest a cascaded window is made.
    pub const ARRANGE_CASCADE_MIN_H: f32 = 180.0;
    /// Window > Arrange > Cascade: the narrowest a cascaded window is made.
    pub const ARRANGE_CASCADE_MIN_W: f32 = 260.0;
    /// Window > Arrange > Cascade: how far each window steps down and right from the one before it.
    pub const ARRANGE_CASCADE_STEP: f32 = 30.0;
    /// Where the first new tool or chart window opens: its offset from the desktop's top-left corner.
    pub const CASCADE_ORIGIN: egui::Vec2 = egui::vec2(50.0, 40.0);
    /// Where the first window opened from Window > New window lands: its offset from the desktop's top-left corner.
    pub const CASCADE_ORIGIN_NEW_WINDOW: egui::Vec2 = egui::vec2(40.0, 30.0);
    /// How far each successive new window steps down and right from the previous one.
    pub const CASCADE_STEP: f32 = 28.0;
    /// The size a chart window opens at from the command palette or the Chart launcher.
    pub const CHART_WINDOW_SIZE: egui::Vec2 = egui::vec2(640.0, 420.0);
    /// The size a cloned chart window opens at.
    pub const CLONE_CHART_SIZE: egui::Vec2 = egui::vec2(620.0, 380.0);
    /// The shortest a window can be dragged or arranged to; also the floor when the arena is smaller.
    pub const MINH: f32 = 160.0;
    /// The narrowest a window can be dragged or arranged to; also the floor when the arena is smaller.
    pub const MINW: f32 = 240.0;
    /// How much of a window's height must stay reachable inside the desktop when it is dragged toward the bottom edge.
    pub const MIN_VISIBLE_H: f32 = 40.0;
    /// How much of a window's width must stay reachable inside the desktop when it is dragged toward the right edge.
    pub const MIN_VISIBLE_W: f32 = 80.0;
    /// The size a chart window opens at from Window > New window.
    pub const NEW_WINDOW_CHART_SIZE: egui::Vec2 = egui::vec2(700.0, 440.0);
    /// The size the Polymarket cockpit window opens at: narrow and tall.
    pub const COCKPIT_WINDOW_SIZE: egui::Vec2 = egui::vec2(340.0, 640.0);
    /// The left rail's vertical tabs: the widest a tab is.
    pub const RAIL_TAB_MAX_W: f32 = 28.0;
    /// The left rail's vertical tabs (one per minimized window): the narrowest a tab is.
    pub const RAIL_TAB_MIN_W: f32 = 20.0;
    /// The left rail's vertical tabs: the room added to the rotated label's length to make the tab's height.
    pub const RAIL_TAB_PAD: f32 = 18.0;
    /// The Save layout as dialog's width.
    pub const SAVE_LAYOUT_W: f32 = 280.0;
    /// The size the Settings window opens at: a row of four previews and room below them.
    pub const SETTINGS_WINDOW_SIZE: egui::Vec2 = egui::vec2(640.0, 600.0);
    /// The size a chart window opened from a stored series opens at.
    pub const STORED_CHART_SIZE: egui::Vec2 = egui::vec2(700.0, 440.0);
    /// The size the test-symbol chart window opens at.
    pub const TEST_CHART_SIZE: egui::Vec2 = egui::vec2(700.0, 440.0);
    /// The size a plain tool window (Account, Options, Greeks, News, Calendar, Data, Studio, Connections, Tearsheet) opens at.
    pub const TOOL_WINDOW_SIZE: egui::Vec2 = egui::vec2(560.0, 400.0);

    /// Every row of this group as data, in the order the TOML lists them.
    pub const ENTRIES: &[super::Entry] = &[
        super::Entry { group: "workspace", name: "TITLE_CLEAR", unit: Some(super::Unit::Px), value: super::Data::F32(TITLE_CLEAR), doc: "How far below a window's top the resize bands on its left and right edges start, clearing the title-bar controls." },
        super::Entry { group: "workspace", name: "ARRANGE_CASCADE_FRAC", unit: Some(super::Unit::Ratio), value: super::Data::F32(ARRANGE_CASCADE_FRAC), doc: "Window > Arrange > Cascade: each window's width and height as a share of the desktop area." },
        super::Entry { group: "workspace", name: "ARRANGE_CASCADE_MIN_H", unit: Some(super::Unit::Px), value: super::Data::F32(ARRANGE_CASCADE_MIN_H), doc: "Window > Arrange > Cascade: the shortest a cascaded window is made." },
        super::Entry { group: "workspace", name: "ARRANGE_CASCADE_MIN_W", unit: Some(super::Unit::Px), value: super::Data::F32(ARRANGE_CASCADE_MIN_W), doc: "Window > Arrange > Cascade: the narrowest a cascaded window is made." },
        super::Entry { group: "workspace", name: "ARRANGE_CASCADE_STEP", unit: Some(super::Unit::Px), value: super::Data::F32(ARRANGE_CASCADE_STEP), doc: "Window > Arrange > Cascade: how far each window steps down and right from the one before it." },
        super::Entry { group: "workspace", name: "CASCADE_ORIGIN", unit: Some(super::Unit::Px), value: super::Data::Vec2(CASCADE_ORIGIN), doc: "Where the first new tool or chart window opens: its offset from the desktop's top-left corner." },
        super::Entry { group: "workspace", name: "CASCADE_ORIGIN_NEW_WINDOW", unit: Some(super::Unit::Px), value: super::Data::Vec2(CASCADE_ORIGIN_NEW_WINDOW), doc: "Where the first window opened from Window > New window lands: its offset from the desktop's top-left corner." },
        super::Entry { group: "workspace", name: "CASCADE_STEP", unit: Some(super::Unit::Px), value: super::Data::F32(CASCADE_STEP), doc: "How far each successive new window steps down and right from the previous one." },
        super::Entry { group: "workspace", name: "CHART_WINDOW_SIZE", unit: Some(super::Unit::Px), value: super::Data::Vec2(CHART_WINDOW_SIZE), doc: "The size a chart window opens at from the command palette or the Chart launcher." },
        super::Entry { group: "workspace", name: "CLONE_CHART_SIZE", unit: Some(super::Unit::Px), value: super::Data::Vec2(CLONE_CHART_SIZE), doc: "The size a cloned chart window opens at." },
        super::Entry { group: "workspace", name: "MINH", unit: Some(super::Unit::Px), value: super::Data::F32(MINH), doc: "The shortest a window can be dragged or arranged to; also the floor when the arena is smaller." },
        super::Entry { group: "workspace", name: "MINW", unit: Some(super::Unit::Px), value: super::Data::F32(MINW), doc: "The narrowest a window can be dragged or arranged to; also the floor when the arena is smaller." },
        super::Entry { group: "workspace", name: "MIN_VISIBLE_H", unit: Some(super::Unit::Px), value: super::Data::F32(MIN_VISIBLE_H), doc: "How much of a window's height must stay reachable inside the desktop when it is dragged toward the bottom edge." },
        super::Entry { group: "workspace", name: "MIN_VISIBLE_W", unit: Some(super::Unit::Px), value: super::Data::F32(MIN_VISIBLE_W), doc: "How much of a window's width must stay reachable inside the desktop when it is dragged toward the right edge." },
        super::Entry { group: "workspace", name: "NEW_WINDOW_CHART_SIZE", unit: Some(super::Unit::Px), value: super::Data::Vec2(NEW_WINDOW_CHART_SIZE), doc: "The size a chart window opens at from Window > New window." },
        super::Entry { group: "workspace", name: "COCKPIT_WINDOW_SIZE", unit: Some(super::Unit::Px), value: super::Data::Vec2(COCKPIT_WINDOW_SIZE), doc: "The size the Polymarket cockpit window opens at: narrow and tall." },
        super::Entry { group: "workspace", name: "RAIL_TAB_MAX_W", unit: Some(super::Unit::Px), value: super::Data::F32(RAIL_TAB_MAX_W), doc: "The left rail's vertical tabs: the widest a tab is." },
        super::Entry { group: "workspace", name: "RAIL_TAB_MIN_W", unit: Some(super::Unit::Px), value: super::Data::F32(RAIL_TAB_MIN_W), doc: "The left rail's vertical tabs (one per minimized window): the narrowest a tab is." },
        super::Entry { group: "workspace", name: "RAIL_TAB_PAD", unit: Some(super::Unit::Px), value: super::Data::F32(RAIL_TAB_PAD), doc: "The left rail's vertical tabs: the room added to the rotated label's length to make the tab's height." },
        super::Entry { group: "workspace", name: "SAVE_LAYOUT_W", unit: Some(super::Unit::Px), value: super::Data::F32(SAVE_LAYOUT_W), doc: "The Save layout as dialog's width." },
        super::Entry { group: "workspace", name: "SETTINGS_WINDOW_SIZE", unit: Some(super::Unit::Px), value: super::Data::Vec2(SETTINGS_WINDOW_SIZE), doc: "The size the Settings window opens at: a row of four previews and room below them." },
        super::Entry { group: "workspace", name: "STORED_CHART_SIZE", unit: Some(super::Unit::Px), value: super::Data::Vec2(STORED_CHART_SIZE), doc: "The size a chart window opened from a stored series opens at." },
        super::Entry { group: "workspace", name: "TEST_CHART_SIZE", unit: Some(super::Unit::Px), value: super::Data::Vec2(TEST_CHART_SIZE), doc: "The size the test-symbol chart window opens at." },
        super::Entry { group: "workspace", name: "TOOL_WINDOW_SIZE", unit: Some(super::Unit::Px), value: super::Data::Vec2(TOOL_WINDOW_SIZE), doc: "The size a plain tool window (Account, Options, Greeks, News, Calendar, Data, Studio, Connections, Tearsheet) opens at." },
    ];
}

/// Every group, alphabetical, with its rows: what the brand book and the tests iterate.
pub const GROUPS: &[Group] = &[
    Group { name: "account", entries: account::ENTRIES },
    Group { name: "backend_settings", entries: backend_settings::ENTRIES },
    Group { name: "calendar", entries: calendar::ENTRIES },
    Group { name: "caption", entries: caption::ENTRIES },
    Group { name: "chart", entries: chart::ENTRIES },
    Group { name: "chart_dialogs", entries: chart_dialogs::ENTRIES },
    Group { name: "cockpit", entries: cockpit::ENTRIES },
    Group { name: "connections", entries: connections::ENTRIES },
    Group { name: "data", entries: data::ENTRIES },
    Group { name: "data_manager", entries: data_manager::ENTRIES },
    Group { name: "desktop", entries: desktop::ENTRIES },
    Group { name: "fx_picker", entries: fx_picker::ENTRIES },
    Group { name: "instruments", entries: instruments::ENTRIES },
    Group { name: "news", entries: news::ENTRIES },
    Group { name: "options_chain", entries: options_chain::ENTRIES },
    Group { name: "settings", entries: settings::ENTRIES },
    Group { name: "status", entries: status::ENTRIES },
    Group { name: "status_bar", entries: status_bar::ENTRIES },
    Group { name: "stored", entries: stored::ENTRIES },
    Group { name: "studio", entries: studio::ENTRIES },
    Group { name: "symbol_picker", entries: symbol_picker::ENTRIES },
    Group { name: "title_bar", entries: title_bar::ENTRIES },
    Group { name: "trade", entries: trade::ENTRIES },
    Group { name: "workspace", entries: workspace::ENTRIES },
];
