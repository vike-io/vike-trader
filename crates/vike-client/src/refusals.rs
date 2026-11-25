//! The refusals both command-line programs (`vike-cli` and `vike-mcp`) make alike, declared once
//! (`vike_utility::refusal_enum!`, decision 0301) and shown through the macro's `rendered()`. Every
//! `OrderGateRefusal` is NOT SENT: nothing left this box, and sending it again changes nothing until the input does.

vike_utility::refusal_enum! {
    /// A backtest call both programs refuse before any run starts.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum BacktestCallRefusal with ids BacktestCallRefusalId {
        /// An inline strategy SOURCE (MCP's `script`, the CLI's `--script`): a strategy is a FILE under the
        /// strategies/ folder, named by the profile's `[strategy] file`. The source used to ride the profile as
        /// `[strategy.params].src`, which the run drops unread (`crates/vike-backtest/src/run/strategy_builder.rs`'s
        /// `rhai_overrides`), so the run went ahead on the file and said nothing. `argument` is the surface's own
        /// spelling of it.
        ScriptUnsupported { argument: &'static str } => "backtest.script_unsupported":
            "{argument} is no longer supported: put the strategy in a file under strategies/ and name it with \
             `[strategy] file`",
    }
}

vike_utility::refusal_enum! {
    /// An argument or flag a call names under the spelling it had before the 2026-11-24 naming pass (the strategy
    /// file is `strategy` / `--strategy` on every door, MCP's mount id `mount_id`). REFUSED naming the new one: never
    /// read as the new one (a symbol has one name, root `CLAUDE.md`) and never read as absent, where an explicit
    /// mount id would become the derived one in silence. Shown through the macro's `rendered()`.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum RenamedArgumentRefusal with ids RenamedArgumentRefusalId {
        /// `old` and `new` are the surface's own spellings (`--file` / `--strategy`, `file` / `strategy`).
        Renamed { old: &'static str, new: &'static str } => "call.argument_renamed":
            "`{old}` is now `{new}`: send it under its new name",
    }
}

vike_utility::refusal_enum! {
    /// An order or a node command THIS BOX refuses before its first byte: NOT SENT (decision 0303). Decided once,
    /// by `crate::order_gate::judge` (and, for a pinned id, `crate::verbs::order_client_id_refusal`), for both
    /// doors; each door adds only its own tail after the rendered line.
    #[derive(Debug, Clone, PartialEq)]
    pub enum OrderGateRefusal with ids OrderGateRefusalId {
        OverMaxNotional { notional: f64, max: f64 } => "order.edge.over_max_notional":
            "not sent: notional {notional} is over this box's policy.max_notional_per_order {max}; nothing went to \
             the node",
        VenueBlank => "order.edge.venue_blank":
            "not sent: the command names no venue, so the node would apply it to whichever book it runs first",
        VenueNotMounted { venue: String, mounted: String } => "order.edge.venue_not_mounted":
            "not sent: venue `{venue}` is not mounted on the node, which mounts: {mounted}",
        AccountNotMounted { venue: String, account: String, mounted: String } => "order.edge.account_not_mounted":
            "not sent: account `{account}` of venue `{venue}` is not mounted on the node, which mounts: {mounted}",
        /// `sentence` is `vike_tradehub_client::wire::names_no_account_refusal`'s, the node's and the desktop's
        /// one wording (decision 0254).
        BareVenueOfSeveralAccounts { sentence: String } => "order.edge.bare_venue_of_several_accounts":
            "{sentence}",
        /// `why` is `vike_tradehub_client::venue_routing::venue_routing_verdict`'s reason, the desktop's too.
        NodeDoesNotRoute { why: String } => "order.edge.node_does_not_route":
            "not sent: {why}",
        BareSubmitToOlderNode { venue: String } => "order.edge.bare_submit_to_older_node":
            "not sent: this node predates the one-account rule (no `one-account-venue`); on venue `{venue}` of \
             several accounts it would answer accepted and place nothing. Name the account",
        /// `charset` is always `crate::verbs::ORDER_CLIENT_ID_CHARSET` (a template holds only field holes).
        OrderClientIdCharset { id: String, charset: &'static str } => "order.edge.order_client_id_charset":
            "{id:?} is not a usable order_client_id: it must be {charset} (the strictest charset every venue \
             accepts)",
        /// A bracket's pinned id that is no group stem (`vike_model::orders::client_id::OrderClientIdStem::given`): the
        /// core would refuse the bracket whole only after the node's answer.
        BracketStemShape { id: String } => "order.edge.bracket_stem_shape":
            "{id:?} is not a bracket's group stem: a bracket's order client id is the stem its first send showed \
             (a v2 order client id through its unique part, `group=<stem>` in the CLI's preview), from which the \
             node derives the three legs' ids",
    }
}

vike_utility::refusal_enum! {
    /// A built submit's terms, refused by the one rule both doors judge a submit by after
    /// `crate::verbs::unsizeable_submit_refusal` (`crate::verbs::submit_rules_refusal`, its variants in the order they
    /// bite), or its peg anchor's word (`crate::verbs::peg_of_word`, the last variant). Each `why` is the `vike_model` rule's own words, the rule the
    /// node's door applies again. NOT SENT.
    #[derive(Debug, Clone, PartialEq)]
    pub enum SubmitRuleRefusal with ids SubmitRuleRefusalId {
        /// A trail on a kind that does not trail: the node's paper book would lower it as a trailing stop.
        TrailOffKind { kind: String, trail: f64 } => "order.submit.trail_off_kind":
            "a trail belongs to a trailing, trailing_limit or trailing_take_profit order; this {kind} order states \
             {trail}",
        /// Its prices for its kind (`vike_model::order_terms_refusal`).
        Terms { why: String } => "order.submit.terms": "{why}",
        /// Its display quantity (`vike_model::display_refusal`).
        Display { why: String } => "order.submit.display": "{why}",
        /// A limit's attributes (`vike_model::attributes_refusal`).
        Attributes { why: String } => "order.submit.attributes": "{why}",
        /// A stop's adjustment (`vike_model::stop_adjust_refusal`).
        StopAdjust { why: String } => "order.submit.stop_adjust": "{why}",
        /// Its lifetime and a `gtd` expiry (`vike_model::lifetime_refusal`).
        Lifetime { why: String } => "order.submit.lifetime": "{why}",
        /// Its release moment, with that lifetime (`vike_model::lifetime_refusal` over the release).
        Release { why: String } => "order.submit.release": "{why}",
        /// A peg anchor word that names none of the four, in any case. `accepted` is
        /// `vike_model::OrderPegAnchor::accepted_words`.
        PegAnchor { word: String, accepted: String } => "order.submit.peg_anchor":
            "{word:?} is not a peg anchor: expected one of {accepted} (any case)",
    }
}

vike_utility::refusal_enum! {
    /// A modify both doors refuse before anything is sent (`crate::verbs::modify_of`). NOT SENT.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ModifyRefusal with ids ModifyRefusalId {
        /// No new qty, price or trigger price: a modify that changes nothing is an authoring mistake, not a command.
        NothingToChange => "order.modify.nothing_to_change":
            "nothing to change: a modify names a new qty, a new price or a new trigger price, at least one",
    }
}

vike_utility::refusal_enum! {
    /// No tradehub to dial: no `--node` and no `config.node_addr` row (`crate::resolved::node_dial_address`, every
    /// node-facing door's one ladder). NOT SENT: nothing was dialled.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum NodeAddressRefusal with ids NodeAddressRefusalId {
        Unset => "dial.node.unset":
            "no vike-tradehub to dial: no --node <host:port> was given and this box's config.node_addr row is not \
             set. `vike-cli backend connect` attaches this box to a node and writes the row (or `vike-cli config \
             set config.node_addr HOST:PORT`)",
    }
}

vike_utility::refusal_enum! {
    /// An order list whose protective leg is not named exactly one way, refused by the one builder both doors call
    /// (`crate::verbs`' `oco_list`, `oto_list`, `ladder_list`) before anything is sent. A trailing leg trails by a
    /// distance in price from the moment it is live and takes no limit: the list leg carries no limit offset.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum OrderListRefusal with ids OrderListRefusalId {
        OcoStopLeg => "order.list.oco_stop_leg":
            "an OCO's second leg is exactly one of a stop at its trigger (a stop-limit with its limit too) or a \
             trailing stop by its trail; a trailing stop takes no limit",
        OtoOneExit => "order.list.oto_one_exit":
            "an OTO names exactly one exit: a stop at its trigger, a limit at its price or a trailing stop by its trail",
        LadderStopLeg => "order.list.ladder_stop_leg":
            "a ladder names exactly one protective leg for its whole size: a stop-loss at its trigger or a trailing \
             stop by its trail",
    }
}

vike_utility::refusal_enum! {
    /// A tool name the MCP door served before the 2026-11-24 naming pass (`crates/vike-mcp/src/refusals.rs`'s
    /// `RENAMED_TOOLS`: `modify` is `modify_order`, `market_exit` is `close_all`). REFUSED naming the new one, on a
    /// call and on `--deny-tool` alike: never an alias, and never "unknown tool", which would tell an agent the
    /// operation is gone. Declared here, beside the other door's spellings, so the workspace's `refusals.json`
    /// walks it.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum RenamedToolRefusal with ids RenamedToolRefusalId {
        /// `was` is the name the call used, `now` the name the server serves that tool under.
        Renamed { was: &'static str, now: &'static str } => "mcp.tool.renamed":
            "the tool `{was}` is now `{now}`: use the new name",
    }
}

vike_utility::refusal_enum! {
    /// A `profile_template` call (the MCP door's read of the published backtest-profile schema) naming no table of
    /// the profile. Declared here, beside the other door's refusals, so the workspace's `refusals.json` walks it.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum ProfileTemplateRefusal with ids ProfileTemplateRefusalId {
        /// `tables` is every table's name, in the schema's order, the top level spelled `""`.
        UnknownTable { table: String, tables: String } => "mcp.profile_template.unknown_table":
            "the backtest profile has no table `{table}`; its tables are: {tables}",
    }
}

vike_utility::refusal_enum! {
    /// A background run of the MCP door: a run tool called with `background: true`, then `run_status` and `cancel_run`
    /// (`crates/vike-mcp/src/background_runs.rs`). The CLI has none (a command runs in its shell); declared here, beside
    /// `RenamedToolRefusal`, so the workspace's `refusals.json` walks it.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum BackgroundRunRefusal with ids BackgroundRunRefusalId {
        /// `background` is no boolean; `got` is the JSON value the call sent.
        NotABoolean { got: String } => "mcp.run.background_not_a_boolean":
            "`background` must be true or false, got {got}",
        /// `run_status` or `cancel_run` (`tool`) named no run: no `run_id`, or one that is no text.
        NoRunId { tool: &'static str } => "mcp.run.no_run_id":
            "{tool} requires a `run_id` string argument: the id a `background: true` call answered",
        /// No run of this process has the id: another process's, or an ended run past the newest `kept`.
        Unknown { run_id: String, kept: usize } => "mcp.run.unknown":
            "no background run {run_id:?} in this session: a run id lasts as long as the vike-mcp process that \
             started it, and only the newest {kept} ended runs are kept",
        /// `cancel_run` on a run that already ended (`state`): nothing is left to stop.
        AlreadyEnded { run_id: String, state: &'static str } => "mcp.run.already_ended":
            "background run {run_id:?} already ended ({state}): there is nothing to cancel, and run_status reads \
             what it answered",
        /// A background start while `limit` runs are running. Nothing was started.
        TooManyRunning { limit: usize } => "mcp.run.too_many_running":
            "{limit} background runs are already running in this session: wait for one to end (run_status) or \
             stop one (cancel_run), then start this one again. Nothing was started.",
    }
}

vike_utility::refusal_enum! {
    /// Why a hub dial has nothing to sign with: this box's device key was not read (decision 0288 item 8). Every door
    /// answers it from `crate::hubkeys::HubKeyring::absent_refusal` (`vike-cli` after its command's opening line,
    /// `vike-mcp` whole, the Telegram channel in its log), shown through the macro's `rendered()`.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum DeviceKeyRefusal with ids DeviceKeyRefusalId {
        /// No project above the working directory, so no settings database was read at all: the key may well
        /// exist, so the cure is where the program starts, never a new key.
        NoSettingsDirectory => "dial.device_key.no_settings_directory":
            "this box's device key could not be read: this program resolved no settings directory (no project \
             above its working directory), so it has no settings database to read. Start it from inside the \
             project folder, or set VIKE_SETTINGS_DIR.",
        /// The settings database holds no `device_key` row. `store` is its path, `state` whether the file is there,
        /// `cure` the hub protocol's one cure for where the node is (`vike_hub_protocol::conn::no_device_key_cure`).
        NotInStore { store: String, state: &'static str, cure: &'static str } => "dial.device_key.not_in_store":
            "this box's device key is not in its settings database ({store}: {state}). {cure}.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The id is the contract and the text names the surface's own spelling.
    #[test]
    fn the_script_refusal_names_its_argument_under_its_id() {
        let refusal = BacktestCallRefusal::ScriptUnsupported { argument: "--script" };
        assert_eq!(refusal.id(), BacktestCallRefusalId::ScriptUnsupported);
        assert_eq!(
            refusal.rendered(),
            "error [backtest.script_unsupported]: --script is no longer supported: put the strategy in \
             a file under strategies/ and name it with `[strategy] file`"
        );
    }

    /// A renamed spelling is refused under its one id, naming both spellings.
    #[test]
    fn a_renamed_argument_names_its_new_spelling_under_its_id() {
        let refusal = RenamedArgumentRefusal::Renamed { old: "--file", new: "--strategy" };
        assert_eq!(refusal.id(), RenamedArgumentRefusalId::Renamed);
        assert_eq!(
            vike_utility::errors::render_refusal(refusal.id().word(), &refusal),
            "error [call.argument_renamed]: `--file` is now `--strategy`: send it under its new name"
        );
    }
}
