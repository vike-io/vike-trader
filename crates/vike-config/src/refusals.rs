//! The refusals this crate gives a person or an agent for their SETTINGS, declared once
//! (`vike_utility::refusal_enum!`, decision 0301): each variant carries its ONE text and its id, and
//! `crates/vike-backend/tests/refusal_catalog.rs` publishes every id in `refusals.json`.
//!
//! - [`ConfigRefusal`] is everything a [`crate::ConfigError`] says: the loader's refusals of a stored
//!   row or a written value (`config set`, `config unset`, the desktop's settings Saves, a hub's
//!   boot), ids `config.<family>.<what>` (a shared shape is `config.value.<what>`, a one-key rule
//!   `config.<key>.<what>`). Its text is what follows the error's LOCATOR (`<file>: <key> = `), so
//!   one id covers every key that is refused alike.
//! - [`SettingsWriteRefusal`] is the write planner's own (`crate::write`): a key the grammar refuses,
//!   a credential-shaped name, and each validator verdict (`config.write.<what>`,
//!   `config.unset.<what>`). Its text is the whole sentence.
//!
//! A surface SHOWS one through `rendered()` (`error [<id>]: <text>`): [`crate::ConfigError::rendered`]
//! puts the id before the locator, [`crate::RowPlanError`]'s `Display` renders its own, and a
//! validator verdict that wraps a resolve error carries the inner error's rendered form in its hole,
//! so a person reads both ids. `Display` stays the bare text. Decide on the variant or the id, never
//! on the words: they are reworded freely, the id is published and stays.

vike_utility::refusal_enum! {
    /// What a [`crate::ConfigError`] says after its locator: this module's doc.
    #[derive(Debug, Clone, PartialEq)]
    pub enum ConfigRefusal with ids ConfigRefusalId {
        // -- the load ---------------------------------------------------------------------------
        /// A settings source exists and could not be read.
        Unreadable { error: String } => "config.load.unreadable": "cannot be read: {error}",
        /// The parser's own message (serde's or toml's), verbatim: a wrong type, an unknown field.
        NotParsed { message: String } => "config.load.not_parsed": "{message}",
        /// A `<project>/vike.toml` that IS there (`crate::removed::RemovedFileProbe::Present`).
        RemovedProjectFilePresent { path: String }
            => "config.load.removed_project_file.present":
               "{path} is present, but the per-project override layer is NO LONGER READ — removed \
                because a fifth settings file above <project>/settings/ reintroduces the `which file \
                won?` question that directory exists to answer. There are no settings files to move \
                its keys into any more either (`docs/decisions/0086`): write each \
                `[config]`/`[preferences]` key it holds with `vike-cli config set config.<key> \
                <value>` / `vike-cli config set preferences.<key> <value>`, then delete {path}.\n\
                [policy] and [flags] tables were never accepted in it; write those with `vike-cli \
                config set policy.<key> <value>` / `vike-cli config set flags.<key> <value>`.\n\
                Nothing has been moved or deleted for you: it is your file.",
        /// A `<project>/vike.toml` whose absence could not be established
        /// (`crate::removed::RemovedFileProbe::Unestablished`).
        RemovedProjectFileUnestablished { path: String, error: String, project: String }
            => "config.load.removed_project_file.unestablished":
               "{path} could not be STATTED: {error} — this is NOT the same as absent, and it is \
                NOT evidence that the file is there. The per-project override layer is NO LONGER \
                READ, and a path whose absence cannot be ESTABLISHED refuses rather than being \
                assumed empty: a file nobody can see is precisely the one an operator would believe \
                is in force.\n\
                ⚠ It may not exist at all. Under a systemd unit the likeliest cause is the sandbox \
                rather than a leftover file: ProtectHome=yes makes /home, /root and /run/user \
                unreachable, so a stat below them fails with `Permission denied` whether or not \
                anything is there, and ProtectSystem=strict does the same outside what \
                ReadWritePaths= / ReadOnlyPaths= name. Give the unit access to the project \
                directory {project}, relax ProtectHome= to read-only, or name a project the process \
                can see with VIKE_SETTINGS_DIR. Otherwise it is ordinary permissions: a parent \
                directory this process cannot search (no `x` bit) fails identically.\n\
                If {path} really is there, it is the removed layer: write its \
                `[config]`/`[preferences]` keys with `vike-cli config set config.<key> <value>` / \
                `vike-cli config set preferences.<key> <value>`, then delete it.\n\
                Nothing has been moved or deleted for you.",

        // -- the store's rows (`crate::mirror`) ------------------------------------------------
        /// A JSON integer `serde_json` demoted to a float: no writer of this tree produces one.
        IntegerOutOfRange { key: String, raw: String }
            => "config.store.integer_out_of_range":
               "`{key}` is an INTEGER outside the range this store can hold ({raw}). `serde_json` \
                reads it as a float, so a ceiling written this way would resolve to an enormous \
                finite number and pass every positivity check — the operator would believe they \
                filed a cap and have none. No writer in this tree can produce this: a hand \
                `INSERT`, a botched migration or a restored backup did.",
        /// A row whose value is JSON `null`.
        NullRow { key: String }
            => "config.store.null_row":
               "`{key}` is JSON `null`, and a settings row may not be. Every field of every patch \
                type is an `Option`, so `null` deserializes as \"this key is UNSET\" — a key an \
                operator believes they filed would silently resolve to its default, and \
                `deny_unknown_fields` cannot catch it because the FIELD NAME is known. No writer in \
                this tree can produce this: a hand `INSERT`, a botched migration or a restored \
                backup did.",
        /// Two rows of one section claim one place (`a` beside `a.b`).
        RowCollision { key: String }
            => "config.store.row_collision":
               "`{key}` collides with another row of this section — a key and a path THROUGH that \
                key cannot both be set. Nothing has been read.",
        /// The seal's row count is off (`crate::mirror::adoption_integrity`): the erase detector.
        SealCountChanged { sealed: usize, now: usize }
            => "config.store.seal_count_changed":
               "the settings rows have CHANGED since this store was last written to: it was sealed \
                on {sealed} setting row(s) and now carries {now}. Every sanctioned write updates \
                that count in the same transaction that changes the table, so rows arrived or left \
                by some other route — a hand `INSERT`/`DELETE`, a restored backup, or a \
                partially-applied migration. These rows are the ONLY settings layer, so the missing \
                ones are not defaults an operator chose: they are a ceiling that has silently \
                stopped being applied. There is no repair command for this — `vike-cli config \
                unset` refuses while the count is off, because moving it would hide the erase: \
                restore the settings database from the box's nightly backup.",

        // -- a command-line override -----------------------------------------------------------
        /// An address flag that is not `host:port`.
        CliNotHostPort => "config.cli.not_host_port": "expected a host:port address",

        // -- shapes several keys share ---------------------------------------------------------
        /// A number that is not finite.
        NotFinite { value: f64 } => "config.value.not_finite": "{value} is not a finite number",
        /// A text value that is blank.
        Blank => "config.value.blank": "\"\" is blank — omit the key to keep the default",
        /// A word outside its list (the value is not echoed).
        NotOneOf { words: String } => "config.value.not_one_of": "… must be one of: {words}",
        /// A word outside its list, matched exactly (the value is not echoed).
        NotOneSpellingOf { words: String }
            => "config.value.not_one_spelling_of":
               "… must be one of: {words} (lower case, one spelling each)",
        /// An address that is not `host:port`.
        NotHostPort { value: String }
            => "config.value.not_host_port": "{value:?} is not a host:port address",
        /// A whole number outside its closed range (`crate::ConfigError::out_of_range`).
        OutsideRange { value: u32, min: u32, max: u32, why: &'static str }
            => "config.value.outside_range":
               "{value} is outside the allowed range {min}..={max} — {why}",
        /// A whole number outside its range (`crate::config`'s `check_between`).
        NotBetween { value: u32, start: u32, end: u32, why: &'static str }
            => "config.value.not_between":
               "{value} is outside the allowed range {start} to {end} — {why}",
        /// A whole number under its floor (`crate::config`'s `check_min_max`).
        BelowMinimum { value: u32, min: u32, why: &'static str }
            => "config.value.below_minimum": "{value} is below the allowed minimum {min} — {why}",
        /// A whole number over its ceiling (`crate::config`'s `check_at_most`).
        AboveMaximum { value: u32, max: u32, why: &'static str }
            => "config.value.above_maximum": "{value} exceeds the allowed maximum {max} — {why}",
        /// A `0` the reader would read as "unset" (`crate::config`'s `check_at_least_one`).
        ZeroNotUsable
            => "config.value.zero_not_usable":
               "0 is not a usable value — it must be at least 1; omit the key for the default",
        /// A money tolerance below zero or not finite.
        NegativeOrNotFinite { value: f64 }
            => "config.value.negative_or_not_finite": "{value} must be a finite number at or above 0",
        /// A cadence outside its range and not its `0` off word.
        CadenceOutsideRange { value: u32, min: u32, max: u32, why: &'static str }
            => "config.value.cadence_outside_range":
               "{value} is outside the allowed range {min}..={max} ms (or 0 for off) — {why}",
        /// A free-space floor outside `1..=max` MiB.
        DiskFloorOutsideRange { value: u32, max: u32 }
            => "config.value.disk_floor_outside_range":
               "{value} is outside the allowed range 1..={max} MiB — the floor is a free-space \
                figure in MiB, and 0 can never be crossed",

        // -- `policy` rules ------------------------------------------------------------------
        /// `policy.max_leverage` under 1.
        LeverageCeilingBelowOne { value: f64 }
            => "config.max_leverage.below_one":
               "{value} is below the minimum leverage 1 (1 = no leverage; a ceiling under 1x is not \
                representable, and 0 would divide by zero deriving the margin requirement)",
        /// A money ceiling at or under 0.
        CeilingNotPositive { value: f64 }
            => "config.ceiling.not_positive":
               "{value} must be greater than 0 (a ceiling of 0 denies every order — omit the key to \
                leave it uncapped)",
        /// `policy.market_slippage` above the widest compiled band.
        MarketSlippageAboveMaximum { value: f64, max: f64 }
            => "config.market_slippage.above_maximum":
               "{value} exceeds the allowed maximum {max} (a band is a FRACTION: 0.002 = 0.2%). The \
                maximum is the widest band already compiled in, so this key can only tighten a \
                venue, never widen one",
        /// `policy.market_slippage` under the tightest marketable band.
        MarketSlippageBelowMinimum { value: f64, min: f64 }
            => "config.market_slippage.below_minimum":
               "{value} is below the allowed minimum {min} — a band this tight is not marketable, so \
                the emulated order cancels unfilled, which on a tripped stop is a protective exit \
                that did not exit",
        /// `policy.deadman_timeout_ms` armed under a second.
        DeadmanTimeoutBelowMinimum { value: u64, min: u64, off: u64 }
            => "config.deadman_timeout_ms.below_minimum":
               "{value} is below the minimum armed timeout {min} ms (one second). The switch trips on \
                SILENCE across the whole core's ingest, and a live instrument goes sub-second \
                stretches without a tick routinely — a timeout this short is a false halt on any \
                quiet second. Write {off} to disable the switch outright, or {min} or more to arm it",
        /// `policy.deadman_timeout_ms` over a day.
        DeadmanTimeoutAboveMaximum { value: u64, max: u64 }
            => "config.deadman_timeout_ms.above_maximum":
               "{value} exceeds the maximum {max} ms (one day). A dead-man that waits longer than a \
                day is not a dead-man — a feed silent that long has left a book resting through an \
                entire session, which is what the switch exists to prevent. This is usually a unit \
                slip (seconds written as milliseconds); the value is in MILLISECONDS",
        /// `policy.link_deadman_grace_ms` armed under one ordinary reconnect.
        LinkDeadmanGraceBelowMinimum { value: u64, min: u64, off: u64 }
            => "config.link_deadman_grace_ms.below_minimum":
               "{value} is below the minimum armed grace {min} ms (thirty seconds). An ORDINARY \
                reconnect of the slowest lane that reports a dead link — the L2 depth driver — \
                takes up to 23 s (vike_bridge_core::depth's CONNECT_10S dial, the venue's REST book \
                seed and its 3 s DEPTH_BACKOFF), so a grace shorter than this cancels the book on a \
                re-dial rather than on a dead link. Write {off} to disable the switch outright, or \
                {min} or more to arm it",
        /// `policy.link_deadman_grace_ms` over an hour.
        LinkDeadmanGraceAboveMaximum { value: u64, max: u64 }
            => "config.link_deadman_grace_ms.above_maximum":
               "{value} exceeds the maximum {max} ms (one hour). A link the bridge has reported DOWN \
                for an hour, with orders resting behind it, is exactly what this switch exists to \
                end — a grace longer than that is a switch that will never fire. This is usually a \
                unit slip (seconds written as milliseconds); the value is in MILLISECONDS",
        /// `policy.max_mount_loss` at or under 0.
        MountLossNotPositive { value: f64 }
            => "config.max_mount_loss.not_positive":
               "{value} must be greater than 0 (a budget of 0 would latch every mount at its first \
                loss; omit the key to leave mounts without a budget)",
        /// `policy.max_mount_leverage` under 1.
        MountLeverageBelowOne { value: f64 }
            => "config.max_mount_leverage.below_one":
               "{value} is below the minimum leverage 1 (a mount's gross notional over its account's \
                sizing equity; a cap under 1x is not a leverage — omit the key for no cap)",

        // -- retired keys (each names the `config unset` that removes a stored row) ------------
        /// `policy.max_total_exposure`, read by nothing.
        RetiredMaxTotalExposure { value: f64, removal: String }
            => "config.retired.max_total_exposure":
               "{value} is no longer a policy key — it was read by NOTHING here, so it capped \
                nothing. Two ceilings replace it, and WHICH ONE YOU WANT DEPENDS ON WHAT YOU MEANT. \
                Per INSTRUMENT: `max_total_exposure` in your RUN PROFILE's `[risk]` table \
                (vike_model::ProfileRisk -> RiskLimits), which RiskGate evaluates on every order and \
                which a live mount already refuses to start without — it caps ONE SYMBOL at ONE \
                VENUE, so size it per instrument. Per ACCOUNT — the aggregate this key's NAME reads \
                as: the `policy.max_account_exposure` row (`vike-cli config set`), which caps the \
                whole account's projected open notional across every symbol and denies with \
                `over-account-exposure`. {removal}.",
        /// `policy.rate.max_utilization`, which clamped a preference nothing read.
        RetiredRateMaxUtilization { value: f64, removal: String }
            => "config.retired.rate_max_utilization":
               "{value} is no longer a policy key — it clamped `preferences.rate_utilization`, which \
                was read by NOTHING, so this ceiling bounded nothing. Every pacer takes its target \
                fraction from the compiled-in vike_model::rate_limits::DEFAULT_UTILIZATION instead \
                (a `preferences.rate_utilization` row has the same problem). {removal}. The knob \
                returns, per-venue, when vike_model::RateLimitConfig can reach a pacer",
        /// `preferences.rate_utilization`, read by nothing.
        RetiredRateUtilization { value: f64, removal: String }
            => "config.retired.rate_utilization":
               "{value} is no longer a preference — NOTHING read it. Every pacer takes its target \
                fraction from the compiled-in vike_model::rate_limits::DEFAULT_UTILIZATION, so this \
                key validated, displayed as effective, and changed nothing (a \
                `policy.rate.max_utilization` row has the same problem). {removal}. The knob \
                returns, per-venue, when vike_model::RateLimitConfig can reach a pacer",
        /// `config.state_dir`, read by nothing.
        RetiredStateDir { path: String, removal: String }
            => "config.retired.state_dir":
               "{path} is no longer a setting — NOTHING read it. It named the DESKTOP's \
                strategy-state sidecar directory, and the reader went with the desktop's local core. \
                It is NOT the state ROOT (that is <settings>/state, a different directory). {removal}",
        /// A retired flag key whose variable decision 0095 moved to a settings row.
        RetiredFlagMovedToRow {
            written: bool,
            var: &'static str,
            home: &'static str,
            removal: String,
            switch: &'static str,
        } => "config.retired.flag.moved_to_row":
               "{written} is no longer a flag — NOTHING read the resolved value; this key only ever \
                mirrored {var} into a field with no consumer. {var} was retired too (decision \
                0095): the setting is `{home}` in the settings database. {removal}; `vike-cli \
                config set {home} {switch}` writes the replacement",
        /// A retired flag key whose variable was retired with no new home.
        RetiredFlagNoHome { written: bool, var: &'static str, why: &'static str, removal: String }
            => "config.retired.flag.no_home":
               "{written} is no longer a flag — NOTHING read the resolved value; this key only ever \
                mirrored {var} into a field with no consumer. {var} was retired too: {why}. \
                {removal}",
        /// A retired flag key whose variable a venue adapter still reads.
        RetiredFlagAdapterReadsVariable { written: bool, var: &'static str, removal: String }
            => "config.retired.flag.adapter_reads_variable":
               "{written} is no longer a flag — NOTHING read the resolved value. The behaviour is \
                still there and still reachable: it is gated by the venue adapter's own read of \
                {var}, which this key only ever mirrored into a field with no consumer. {removal}, \
                and export {var}=1 instead",
        /// A dead flag key (`crate::DEAD_FLAG_KEYS`): its variable is retired too.
        DeadFlag { written: bool, var: &'static str, survives: &'static str, removal: String }
            => "config.retired.flag.dead":
               "{written} is no longer a flag — NOTHING read it, on EITHER spelling, and {var} is \
                retired too: a set one refuses startup. The FEATURE is unmounted rather than \
                removed — {survives}. {removal}",

        // -- `config` rules ------------------------------------------------------------------
        /// `config.tradehub_control_rate` not a positive finite rate.
        NotARate { value: f64 }
            => "config.tradehub_control_rate.not_a_rate":
               "{value} is not a rate: it must be a finite number of commands per second above 0 \
                (the reader treats anything else as unset and runs the default 20)",
        /// `config.datahub_options_focus_underlyings` refused by its grammar
        /// (`crate::config::parse_options_focus_underlyings`, whose words are the hole).
        OptionsFocusUnderlyingsRefused { why: String }
            => "config.datahub_options_focus_underlyings.refused": "{why}",
        /// The options focus pair names more instruments than one ticker feed may.
        OptionsFocusOverCap {
            count: usize,
            underlyings: String,
            strikes: u32,
            per_strike: u32,
            instruments: u64,
            cap: u32,
        } => "config.datahub_options_focus.over_cap":
               "{count} underlyings ({underlyings}) × {strikes} strikes each side × {per_strike} \
                instruments per strike is {instruments} instruments, above the {cap} one ticker \
                feed may name: the gatewayhub would refuse the feed and the options lane would \
                stream no live bid/ask at all. Name fewer underlyings \
                (datahub_options_focus_underlyings) or fewer strikes \
                (datahub_options_focus_strikes)",
        /// `config.paper_slippage` not a fraction of price in range.
        NotAPaperSlippage { value: f64, max: f64 }
            => "config.paper_slippage.not_a_slippage":
               "{value} is not a slippage: it must be a finite fraction of price from 0 to {max} \
                (0.0002 is 2 basis points); omit the key for the default",
        /// `config.paper_slippage_table` refused by the table's own check (its words are the hole).
        NotAPaperSlippageTable { why: String }
            => "config.paper_slippage_table.not_a_table":
               "… is not a paper slippage table: {why}. Write the table's lines, one key per line \
                (model = \"market_impact\", then base, gamma, eta, exec_time, window; or model = \
                \"percent\" with value, \"ticks\" with ticks and tick, \"none\"), or omit the key \
                for the flat paper_slippage. The row prices paper books only: a backtest's \
                slippage is the strategy parameter strategy.params.slippage",
        /// Both paper slippage rows set.
        PaperSlippageStatedTwice
            => "config.paper_slippage_table.stated_twice":
               "… and paper_slippage both state a paper fill's slippage: keep one (the table's \
                model = \"percent\" with its value is the flat figure; unset config.paper_slippage \
                first to move to a table)",
        /// A written `preflight_disk_warn_mib` at or under the fail floor.
        DiskWarnNotAboveFail { warn: u32, fail: u32, default_warn: u32, default_fail: u32 }
            => "config.preflight_disk.warn_not_above_fail":
               "{warn} is not above preflight_disk_fail_mib ({fail}): the warn floor must be the \
                higher of the two (a key not written is at its default, {default_warn} and \
                {default_fail} MiB), so move the pair one key at a time in the order that keeps \
                warn above fail",
        /// A written `preflight_disk_fail_mib` at or over the warn floor.
        DiskFailNotBelowWarn { fail: u32, warn: u32, default_warn: u32, default_fail: u32 }
            => "config.preflight_disk.fail_not_below_warn":
               "{fail} is not below preflight_disk_warn_mib ({warn}): the warn floor must be the \
                higher of the two (a key not written is at its default, {default_warn} and \
                {default_fail} MiB), so move the pair one key at a time in the order that keeps \
                warn above fail",
        /// `config.recorder_retention_days_by_kind` refused by its grammar
        /// (`crate::retention::parse_retention_days_by_kind`, whose words are the hole).
        RetentionRefused { why: String }
            => "config.recorder_retention_days_by_kind.refused": "{why}",
        /// `config.datahub_sec_user_agent` refused by `crate::config::check_sec_user_agent` (its
        /// words are the hole).
        SecUserAgentRefused { value: String, why: String }
            => "config.datahub_sec_user_agent.refused": "{value:?} {why}",
        /// `config.instance_origin` refused by `vike_model::OrderClientIdOrigin::parse` (its words
        /// are the hole).
        InvalidOrigin { error: String } => "config.instance_origin.invalid": "{error}",
        /// A tool feed's poll cadence under its first retry.
        FeedPollBelowFirstRetry { value: u32, min: u32 }
            => "config.feed_poll_secs.below_first_retry":
               "{value} is below the minimum {min} — a failed poll is retried after {min} seconds \
                and the retry doubles up to the cadence, so the cadence cannot be shorter than that \
                first retry",
        /// A tool feed's poll cadence over a day.
        FeedPollAboveOneDay { value: u32, max: u32 }
            => "config.feed_poll_secs.above_one_day":
               "{value} exceeds the allowed maximum {max} (one day) — a feed polled less often than \
                that is stale before it is asked again",

        // -- `preferences` rules -------------------------------------------------------------
        /// `preferences.sweep_threads` of 0.
        SweepThreadsZero
            => "config.sweep_threads.zero":
               "0 would install a pool that never runs a point — omit the key for the default \
                min(4, available_parallelism())",
        /// `preferences.max_order_qty` not a positive finite cap.
        NotAQuantityCap { value: f64 }
            => "config.max_order_qty.not_a_cap":
               "{value} is not a quantity cap: it must be a finite number above 0 — omit the key for \
                no cap",
        /// `preferences.export_dir` blank.
        ExportDirBlank
            => "config.export_dir.blank":
               "\"\" is blank — omit the key to keep the default <exe_dir>/exports",
        /// A link-patience window under its built-in value (raise-only).
        PatienceBelowBuiltIn { value: u32, floor: u32 }
            => "config.link_patience.below_built_in":
               "{value} is below the minimum {floor}: this window can only be RAISED above its \
                built-in value (a shorter wait tears healthy links down); omit the key for the \
                default",
        /// A link-patience window over its ceiling.
        PatienceAboveMaximum { value: u32, max: u32 }
            => "config.link_patience.above_maximum":
               "{value} exceeds the allowed maximum {max} (milliseconds)",
        /// An `ai_*_model` value not shaped like a model id (the value is not echoed).
        NotAModelId { max: usize, punctuation: &'static str }
            => "config.ai_model.not_a_model_id":
               "… is not a model id: it must be 1 to {max} characters, each an ASCII letter or \
                digit or one of `{punctuation}` — omit the key for the default",
    }
}

vike_utility::refusal_enum! {
    /// A refusal of the settings write planner (`crate::write`): this module's doc.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum SettingsWriteRefusal with ids SettingsWriteRefusalId {
        // -- the key grammar (`crate::RowPlanError::BadKey`) -------------------------------------
        /// A first segment that names none of the four sections.
        UnknownSection { key: String, name: String }
            => "config.write.unknown_section":
               "bad settings key {key:?}: unknown settings section {name:?} — every key starts with \
                one of the four `config show` groups its rows under: policy.<key>, config.<key>, \
                preferences.<key> or flags.<key>",
        /// A key with an empty segment.
        EmptySegment { key: String }
            => "config.write.empty_segment": "bad settings key {key:?}: empty key segment",
        /// A key whose first segment is not its section's word.
        WrongFirstSegment { key: String, section: &'static str, first: String }
            => "config.write.wrong_first_segment":
               "bad settings key {key:?}: a {section} key is spelled `{section}.<key>` (the same \
                dotted form `config show` renders), got first segment `{first}`",
        /// A key that names a whole section.
        WholeSection { key: String, section: &'static str }
            => "config.write.whole_section":
               "bad settings key {key:?}: it names the whole {section} section — a write sets one \
                key inside it",
        /// A value with no JSON scalar form.
        NotAScalar { key: String, shape: &'static str }
            => "config.write.not_a_scalar":
               "bad settings key {key:?}: `{key}` would be {shape}, and the settings store holds \
                one JSON scalar per row",
        /// A retired flag row (`crate::flags`' `RETIRED_FLAG_ROWS`): it loads and is read by nothing.
        RetiredFlag { key: String, why: &'static str }
            => "config.write.retired_flag":
               "bad settings key {key:?}: `{key}` is retired and read by nothing: {why}",
        /// A credential-shaped key (`crate::write::refuse_credential_key`). The VALUE is never carried.
        CredentialKey { key: String }
            => "config.write.credential_key":
               "`{key}` has a credential-shaped name and no settings write will take one.\n  \
                If you meant a CREDENTIAL: they live in the store, not in the settings database — \
                `vike-cli secrets set <KEY>` is the writer, it takes the value on stdin rather than \
                on the command line, and `vike-cli secrets path` prints which file it opens.\n  \
                If you meant a SETTING: this is not a key the loader knows (no settings field is \
                credential-shaped) — `vike-cli config show` prints every settings key, in the \
                spelling a write takes.",

        // -- `crate::write`'s `validate_row_write` -------------------------------------------
        /// The current store does not boot clean, so a write would re-bless it.
        CurrentStoreRefused { why: String }
            => "config.write.current_store_refused":
               "the current settings store does not boot clean, so this write refuses to re-bless \
                it: {why}",
        /// The current rows do not resolve (a write's and a removal's first look).
        CurrentRowsUnresolved { error: String }
            => "config.write.current_rows_unresolved":
               "the current settings rows do not resolve: {error}",
        /// The written value does not resolve; `error` is the loader's rendered refusal.
        ValueUnresolved { error: String }
            => "config.write.value_unresolved": "this value does not resolve: {error}",
        /// The candidate store would not boot; `why` is the loader's mark.
        WriteWouldNotBoot { why: String }
            => "config.write.would_not_boot":
               "this write would leave the settings store unable to boot: {why}",
        /// The write would resolve another key differently.
        WriteChangesAnotherKey { key: String, before: String, after: String }
            => "config.write.changes_another_key":
               "this write would also change `{key}` (`{before}` -> `{after}`), and a one-row \
                write may only change the key it names",

        // -- `crate::write`'s `validate_row_unset` -------------------------------------------
        /// The seal's row count is already off; `error` is the integrity check's rendered refusal.
        SealUnsound { error: String }
            => "config.unset.seal_unsound":
               "this settings store's seal is already unsound, and removing a row cannot repair a \
                row count that is off — it would only move the count and hide the erase: {error}",
        /// The store without the row does not resolve.
        WithoutRowUnresolved { error: String }
            => "config.unset.without_row_unresolved":
               "the store without this row does not resolve: {error}",
        /// The store without the row would not boot.
        RemovalWouldNotBoot { why: String }
            => "config.unset.would_not_boot":
               "this removal would leave the settings store unable to boot: {why}",
        /// The removal would resolve another key differently.
        RemovalChangesAnotherKey { key: String, before: String, after: String }
            => "config.unset.changes_another_key":
               "this removal would also change `{key}` (`{before}` -> `{after}`), and a one-row \
                removal may only change the key it names",
    }
}

#[path = "refusals_tests.rs"]
#[cfg(test)]
mod refusals_tests;
