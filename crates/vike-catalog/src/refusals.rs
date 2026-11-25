//! The refusals this crate words for a person, declared once (`vike_utility::refusal_enum!`, decision 0301): each
//! variant carries its ONE text and its id, published in the workspace's `refusals.json`.
//!
//! [`SymbolRefusal`] is every "no" to a NAME: a typed symbol's shape and kept row (`crate::door_user_symbol` and
//! its siblings), a kept row's user name (`crate::spell_naming_of_row`, `crate::kept_row_of_user_name`) and a
//! polymarket recorder family (`crate::polymarket_family_minutes`). [`SessionListRefusal`] is a held-session venue's
//! list that is not held (`crate::held_list`). The operator's own lists refuse through
//! `crate::InstrumentListError`, declared the same way beside its check.
//!
//! The functions that refuse keep their `String` answers: each is the variant's BARE text (`to_string()`), because
//! every caller (the tradehub's control door, the CLI, the datahub, the gatewayhub, a bridge) forwards it or holds it
//! as a hole of its own sentence, and a kept row STORES its `Unnamed` words in the kept file. Decide on the variant,
//! never on its words.

vike_utility::refusal_enum! {
    /// A refusal of a name: this module's doc.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum SymbolRefusal with ids SymbolRefusalId {
        /// hyperliquid's `@107`: the venue's wire name of a spot pair, typed where a user name goes.
        WireNameTyped { raw: String } => "catalog.symbol.wire_name_typed":
            "`{raw}` is not a symbol: that is the venue's wire name of a spot pair; write the pair's name, base and \
             quote (`HYPEUSDC`, decision 0290)",
        /// A text of no symbol's shape: blank, over 64 characters, or a character the grammar never uses.
        NotASymbol { raw: String } => "catalog.symbol.not_a_symbol":
            "`{raw}` is not a symbol (letters and digits, e.g. `BTCUSDT.P`, `EURUSD`, `AAPL`; decision 0290's grammar)",
        /// An option written as `venue` writes one, on a venue that trades none.
        OptionNotTraded { raw: String, venue: String } => "catalog.symbol.option_not_traded":
            "`{raw}` is an option and options are not traded on {venue}: the engine has no option payoff or \
             short-option margin model (decision 0290's O-3: deribit is the one venue that trades options)",
        /// A deribit option's shape whose base, expiry or strike does not hold.
        NotADeribitOption { raw: String } => "catalog.symbol.not_a_deribit_option":
            "`{raw}` is not a deribit option name (a USDC option is `BASE_USDC-DDMMMYY-STRIKE-C`, \
             `ETH_USDC-27MAR26-2500-C`)",
        /// A coin-settled deribit option, its USDC-settled twin named.
        CoinSettledOption { raw: String, base: String, expiry: String, strike: String, cp: String }
            => "catalog.symbol.coin_settled_option":
            "`{raw}` is a coin-settled option: its premium is quoted in {base}, so its notional and PnL are coin \
             amounts that an account's one-currency ledger would book as dollars. Trade the USDC-settled options \
             of the same underlying (`{base}_USDC-{expiry}-{strike}-{cp}`)",
        /// `venue` keeps no list a name could be read off; `fix` is the refresh or the operator's row.
        NoKeptList { venue: String, fix: String } => "catalog.symbol.no_kept_list":
            "no {venue} kept list: {fix} (the datahub keeps it as `<project>/instruments/{venue}.json`; a name is never \
             read for an instrument without its row, decision 0290)",
        /// A venue no rule declares a spelling for.
        NoSpelling { raw: String, venue: String } => "catalog.symbol.no_spelling":
            "`{raw}` cannot be read as a {venue} symbol: no spelling is declared for {venue}",
        /// The control door's reading with no list of `venue` here; `no_list` is [`SymbolRefusal::NoKeptList`]'s words.
        NoListHere { raw: String, venue: String, no_list: String } => "catalog.symbol.no_list_here":
            "`{raw}` names no {venue} instrument here: {no_list}",
        /// A deribit spot pair at the door: the venue reports no position for one.
        DeribitSpotPair { raw: String } => "catalog.symbol.deribit_spot_pair":
            "`{raw}` is a spot pair: Deribit reports no position for a spot pair (a holding is only a currency \
             balance, the same wallet that is the collateral of the coin-margined contracts), so the node could not \
             reconcile one",
        /// A name taken as written that a kept row spells another way.
        Respelled { raw: String, canonical: String, venue: String } => "catalog.symbol.respelled":
            "`{raw}` is written `{canonical}` on {venue} (its user name, the control door's, decision 0290): one \
             instrument has one name, so write it `{canonical}` here and wherever the strategy names it",
        /// A polymarket symbol that is no outcome token id.
        NotAnOutcomeToken { raw: String } => "catalog.symbol.not_an_outcome_token":
            "`{raw}` is not a Polymarket outcome token id (a number of 20 or more digits); a market name \
             (`market-slug:Yes`, or the market's polymarket.com address) is looked up by the gatewayhub, \
             which this surface did not reach",
        /// A kept row that does not state what its class's grammar spells (`what`: a base and quote, an expiry, ...).
        Unnamed { venue: String, raw_symbol: String, what: &'static str } => "catalog.symbol.unnamed":
            "{venue} `{raw_symbol}` has no user name: its kept row states no {what} (decision 0290's grammar)",
        /// An option row of a venue other than deribit.
        OptionOffDeribit { venue: String, raw_symbol: String } => "catalog.symbol.option_off_deribit":
            "{venue} `{raw_symbol}` is an option: options are named (and traded) on deribit only (decision 0290's O-3)",
        /// A row of a class nothing trades (an index, a bond, a fund, a warrant, a spot commodity, a combo).
        ClassUnnamed { venue: String, raw_symbol: String, class: &'static str } => "catalog.symbol.class_unnamed":
            "{venue} `{raw_symbol}` is a {class} and that class has no user name here: nothing of it is traded \
             (decision 0290)",
        /// A kept list written before rows stored their names.
        PredatesNames { venue: String } => "catalog.symbol.list_predates_names":
            "the {venue} list predates symbol names: run `vike-cli data catalog refresh --venue {venue}` (decision 0290)",
        /// A lookup with no list of `venue`; `no_list` is [`SymbolRefusal::NoKeptList`]'s words.
        NoList { name: String, venue: String, no_list: String } => "catalog.symbol.no_list":
            "`{name}` names no {venue} instrument: {no_list}",
        /// A lookup in a list that predates stored names; `predates` is [`SymbolRefusal::PredatesNames`]' words.
        NoNamesYet { name: String, venue: String, predates: String } => "catalog.symbol.no_names_yet":
            "`{name}` names no {venue} instrument yet: {predates}",
        /// No kept row answers the name (`which`: `user` or `store`).
        NoRow { name: String, which: &'static str, venue: String } => "catalog.symbol.no_row":
            "`{name}` is no {which} name of a {venue} instrument: the kept list (`<project>/instruments/{venue}.json`) \
             carries no row it names (decision 0290's grammar: FX `USDJPY`, spot `BTCUSDT`, perpetual `BTCUSDT.P`, \
             dated `BTCUSDT_261225`, stock `AAPL`, stock CFD `AAPL.CFD`)",
        /// Two kept rows answer the name, so it names none.
        TwoRows { name: String, venue: String, first: String, second: String, which: &'static str }
            => "catalog.symbol.two_rows":
            "`{name}` names two {venue} instruments, `{first}` and `{second}`: one {which} name must name one row, so \
             it names none (decision 0290)",
        /// A polymarket recorder family with no trailing `-<N>m` window.
        FamilyWithoutInterval { family: String } => "catalog.symbol.family_without_interval":
            "polymarket family `{family}` does not end in an interval like `-5m` — a rolling family \
             is named after its window slugs (`btc-updown-5m` ⇒ `btc-updown-5m-<unix>`), and the \
             interval is what says how long a window lasts",
    }
}

vike_utility::refusal_enum! {
    /// Why a held-session venue's list (`crate::held_list`) is not held: this module's doc.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum SessionListRefusal with ids SessionListRefusalId {
        /// The running account's read answered no rows.
        Empty { venue: String } => "catalog.session_list.empty":
            "`{venue}`: the running account's list came back empty, so nothing is held from it",
        /// No running account of `venue` has handed its list over since this process started.
        NotHeld { venue: String } => "catalog.session_list.not_held":
            "no running `{venue}` account has handed this gatewayhub its list since it started, so it holds none \
             (a login, or the account's own instrument read, hands it over; nothing is asked of the venue for it)",
    }
}
