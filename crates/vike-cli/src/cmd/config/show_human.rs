//! The human printers: the header, the settings table, the ceilings and the profile-risk block.

use vike_config::Description;
// The FILES-half row builder — MOVED to `vike_config::show` (one authority; the tradehub
// `SettingsShow` wire verb consumes the same builder), re-consumed here by the printers.
use vike_config::show::FileRow;

use super::resolve::{Resolved, UnknownKeys};
use super::show_env::{print_env_table, wrap};
use super::{RunProfileRows, Section, StoreStatus};

/// An empty cell renders as `-` in the human tables, so an empty override is visibly a value rather
/// than a formatting gap. The `--json` view keeps the raw `""` — a tool must not have to un-invent
/// this placeholder.
pub(super) fn dash(s: &str) -> &str {
    if s.is_empty() { "-" } else { s }
}

/// The whole human view: the header (which project am I reading?), then the requested halves.
///
/// ⚠ One parameter per THING RENDERED, past clippy's threshold since decision 0057's Phase 2 added
/// the mirrored `[risk]` rows. Bundling them into a struct is the obvious cure and is deliberately
/// not taken: every one of these is resolved from a DIFFERENT store by `execute` above — the files,
/// the process environment, the credential store, the settings database — and a struct would put
/// four independently-failing reads behind one name, which is the thing this command exists to
/// stop doing. The cost is a long signature at four call sites, all of them in this file.
pub(super) fn print_human(
    section: Section,
    filter: Option<&str>,
    d: &Description,
    secrets: &StoreStatus,
    files: &[FileRow],
    envs: &[Resolved],
    unknown: &UnknownKeys,
    profile_risk: &RunProfileRows,
    venue: &[crate::cmd::config::venue::VenueRow],
) {
    print_header(d, secrets);
    if section.files() {
        println!();
        print_file_table(files, &d.settings.warnings, d.store_refusal.as_deref());

        // ⚠ NOT gated on `--changed-only`, and not on the settings table being non-empty. The
        // whole finding is that a pre-trade ceiling can be in force from a file this command does
        // not read, so "nothing matched above" is precisely when an operator most needs to be told
        // the other file exists.
        print_ceilings(filter, profile_risk.source.profiles().unwrap_or_default());
        // ...and, since decision 0057's Phase 2, the VALUES — when somebody has mirrored them.
        print_profile_risk(filter, profile_risk);
        // ...and, since decision 0095, the declared venue-settings catalog.
        crate::cmd::config::venue::print_venue_table(venue);
    }
    if section.env() {
        println!();
        print_env_table(envs, secrets, unknown);
    }
}

/// **Which project am I reading?** — the question behind every other question here, and the one
/// nothing used to answer. Prints the resolved settings directory, then the credential store's
/// presence, so "my write did nothing" resolves to a stated fact on a line rather than to a guess.
///
/// ⚠ **This used to also print each of the four settings files' presence and key count.**
/// `docs/decisions/0086` deletes the files outright — there is nothing left on disk for this header
/// to describe, so the settings half of this question is answered entirely by `print_file_table`'s
/// row table below, keyed off the settings DATABASE alone.
fn print_header(d: &Description, secrets: &StoreStatus) {
    let Some(dir) = &d.settings_dir else {
        println!(
            "settings directory: NONE — no project above the working directory, and no explicit \
             override."
        );
        println!("  NO settings row could be read: every setting below is a compiled-in default.");
        // The credential loader has one more rung than the settings walk — a CWD-RELATIVE
        // `settings/db/vike.db` when no project resolves. Saying nothing here would let this
        // command report "no settings directory" while the env half below shows `database` rows,
        // which reads as a contradiction rather than as the two different fallbacks it is.
        if secrets.keys > 0 {
            println!(
                "  ...yet the credential loader found {} key(s): it also tries a \
                 working-directory-relative `{}/` store, which this command did not resolve and \
                 so cannot name.",
                secrets.keys,
                vike_model::paths::state_path::PROJECT_SETTINGS_DIR
            );
        }
        return;
    };
    println!("settings directory: {}", dir.display());

    let width = secrets.label().len();
    // ⚠ **The STORE THAT ANSWERED, labelled with ITS OWN file name** — `vike.db`. Key COUNT only —
    // names are `vike-cli secrets list`'s job, values are nobody's.
    let state = if secrets.present {
        format!("present, {} key(s) — names: `vike-cli secrets list`", secrets.keys)
    } else {
        "absent".to_string()
    };
    println!("  {:<width$}  {state}", secrets.label());
    // Says DATABASE, deliberately, and for the reason `config check`'s twin row states: 0054's
    // constraint 2 is that an operator reads the store with `cat` and `sqlite3` is not installed on
    // the live box, so a `.db` path in a sentence that says "file" hands them something they cannot
    // open and no hint why.
    if matches!(secrets.backend, vike_secrets::Backend::Database(_)) {
        println!("  {:<width$}  (the settings DATABASE, not a text file)", "");
    }
}

/// The settings half: one row per typed setting, its effective value, and the layer that set it.
fn print_file_table(rows: &[FileRow], warnings: &[String], store_refusal: Option<&str>) {
    println!("-- settings -----------------------------------------------------------------");
    // RENDERED from `vike_config::PRECEDENCE`, never typed here. This line named a per-project
    // override file for two months while every composition root passed `None` for the project
    // directory, so the layer was implemented, tested, advertised — and read by nothing. That layer
    // is now REMOVED, which is the second way a hand-written header goes false: it would still be
    // naming the file today. A header derived from the loader's own layer list can only name layers
    // that exist, in both directions, and the two reachability gates
    // (`crates/vike-config/tests/layers_are_reachable.rs`,
    // `crates/vike-cli/tests/settings_layers_reachable.rs`) hold each of them to a proof of effect.
    //
    // ⚠ **This used to lead with a `source: files|db` line that named WHICH of two sources
    // answered.** `docs/decisions/0086` deletes the second source outright — every key resolves
    // from the settings database or from its compiled-in default, unconditionally — so there is no
    // longer a fact for that line to state that `precedence_line` does not already carry.
    println!("{}", vike_config::precedence_line());
    // ⚠ And the refusal LEADS the values rather than trailing them. A table printed under a store
    // nobody could open is not what a daemon on this box would resolve, and printing it without
    // saying so is the "positive confirmation of something false" this whole command exists to
    // prevent.
    if let Some(why) = store_refusal {
        println!();
        println!("⚠ SOURCE UNREADABLE — these values are NOT what a daemon would resolve.");
        println!("  {why}");
    }
    println!();

    if rows.is_empty() {
        println!("(no settings matched)");
        return;
    }

    let (mut wk, mut wv, mut wo, mut wd) =
        ("SETTING".len(), "VALUE".len(), "ORIGIN".len(), "DEFAULT".len());
    for r in rows {
        wk = wk.max(r.key.len());
        wv = wv.max(dash(&r.value).len());
        wo = wo.max(r.origin.len() + usize::from(r.adjusted));
        wd = wd.max(dash(&r.default).len());
    }

    println!("{:<wk$}  {:<wv$}  {:<wo$}  {:<wd$}  READ", "SETTING", "VALUE", "ORIGIN", "DEFAULT");
    let mut adjusted = false;
    for r in rows {
        let origin = if r.adjusted {
            adjusted = true;
            format!("{}*", r.origin)
        } else {
            r.origin.clone()
        };
        println!(
            "{:<wk$}  {:<wv$}  {:<wo$}  {:<wd$}  {}",
            r.key,
            dash(&r.value),
            origin,
            dash(&r.default),
            r.read_cell()
        );
    }
    println!();
    let configured = rows.iter().filter(|r| r.origin_kind != "default").count();
    println!("{} setting(s) shown, {configured} configured (origin != default)", rows.len());
    if adjusted {
        println!("* the effective value is not what that layer holds — a later rule moved it:");
    }
    // The loader's non-fatal resolutions — DATA it returns rather than logs, so somebody has to
    // print them, and a settings-disclosure command is exactly that somebody.
    for w in warnings {
        println!("  {w}");
    }
    print_read_scope(rows);
    print_unread(rows);
}

/// The `READ` column's legend — printed only when a shown row names a BINARY, so it costs nothing on
/// a filtered view that has none.
///
/// The column used to be binary in both senses: `yes` or `NO`, saying nothing about WHERE. On a
/// headless tradehub or recorder box, `config.state_dir`, `config.store_root` and
/// `preferences.chart_style` all said `yes` while their only reader was `vike-app` — measured:
/// setting `config.state_dir` on a daemon box did nothing, which was correct behaviour (it was
/// vike-app's strategy-state SIDECAR, not the `settings/state/` root the README names, and it is
/// deleted since) reported as though it had worked. A column that says "something reads this" is
/// not much use to somebody deciding whether to set it HERE.
fn print_read_scope(rows: &[FileRow]) {
    let mut binaries: Vec<&str> = rows.iter().filter_map(|r| r.read_by).collect();
    binaries.sort_unstable();
    binaries.dedup();
    if binaries.is_empty() {
        return;
    }
    println!();
    println!(
        "READ names the BINARY that reads each setting ({}) — that program and no other. A key \
         read only by `app` does nothing on a headless box, and vice versa; `yes` means a LIBRARY \
         reads it, so every binary linking it does.",
        binaries.join(", ")
    );
}

/// The `READ = NO` follow-up: which of the shown settings nothing reads, loudest first.
///
/// Split in two on purpose, because the two cases are not equally urgent. A key an operator
/// actually CONFIGURED and that nothing reads is the defect this column exists for — they wrote a
/// value, this command told them where it came from, and the program ignores it — so each one gets
/// its own paragraph naming what reads the variable instead. A key nobody configured is merely
/// declared-and-unwired; it gets a count and a pointer, because printing twenty-five paragraphs
/// nobody asked for would bury the one that matters.
fn print_unread(rows: &[FileRow]) {
    let unread: Vec<&FileRow> = rows.iter().filter(|r| !r.consumed).collect();
    if unread.is_empty() {
        return;
    }
    let (set, unset): (Vec<&&FileRow>, Vec<&&FileRow>) =
        unread.iter().partition(|r| r.origin_kind != "default");

    if !set.is_empty() {
        println!();
        println!(
            "⚠ {} setting(s) below are CONFIGURED and read by NOTHING — the value has no effect:",
            set.len()
        );
        for r in &set {
            println!("  {} = {}  (from {})", r.key, dash(&r.value), r.origin);
            // ⚠ The VERDICT first, then the paragraph — and the order is the fix rather than
            // formatting. The paragraph alone named a library that reads the variable without
            // saying whether anything calls that library, so six keys' worth of it read as "export
            // the variable instead" while exporting it did nothing. An operator who stops after
            // one line must still get the true answer.
            if let Some(verdict) = r.unread_verdict {
                for line in wrap(verdict, 92) {
                    println!("      {line}");
                }
            }
            if let Some(why) = r.why_unread {
                for line in wrap(why, 92) {
                    println!("      {line}");
                }
            }
        }
    }
    if !unset.is_empty() {
        println!();
        println!(
            "{} further setting(s) are declared but read by nothing yet (none of them configured \
             here); `--json` carries each one's reason.",
            unset.len()
        );
    }
}

/// **The pre-trade ceilings, and the ACT each one judges** — rendered from
/// [`vike_config::ceilings::PRE_TRADE_CEILINGS`], which is the authority.
///
/// # Why this block exists at all
///
/// The table above covers the four settings sections. A run profile is not one of them, and pre-trade
/// ceilings live in one: MEASURED on a live box, `[risk] max_total_exposure = 500.0` was enforced,
/// mandatory for the mount to start live, and present in **no** payload this command printed —
/// while `Policy::max_total_exposure` had been DELETED from `policy.toml` for the opposite defect
/// (declared, displayed, read by nothing). The same box carried `max_notional_per_order` in BOTH
/// files, kept in step by a `# mirrors settings/policy.toml` comment, and the two do not judge the
/// same act: the policy key guards three EDGE surfaces and never reaches `vike_model::RiskLimits`,
/// while the profile key judges every order the core admits.
///
/// So this block answers the question the settings table structurally cannot: *what else can refuse
/// my order, and where do I write it?*
///
/// # What it deliberately does not print, and the half decision 0057 Phase 2 changed
///
/// VALUES for the run-profile rows, **from the FILE**. Resolving them there means parsing a
/// `RunProfile`, and `vike-cli` links neither `vike-core` nor `vike-exec` on purpose — the
/// `light-consumers` CI lane exists to hold it out of that closure. Inventing a second `[risk]`
/// parser here is exactly the drift this workspace forbids. Naming the ceiling, its act, its
/// absence semantics and the file that holds it is the honest shape; the ORIGIN line says which
/// rows this command read and which it did not, so a blank is never mistaken for an unset ceiling.
///
/// What CAN be printed is a value an operator has STORED in the settings database —
/// `vike-cli config bootstrap-run` (the run profile's only writer since decision 0111). That is why
/// this function takes the stored profiles:
/// the `VALUE SHOWN` cell says *yes, below* for a key some stored body carries and names the repair
/// for one nothing does, instead of the flat "this command does not read that file" that was the
/// only true answer before. The values themselves are printed by [`print_profile_risk`], which is
/// also where the sentence that matters lives — and ⚠ that sentence is now CONDITIONAL: a stored
/// body is readable and inert until `config activate` selects it, at which point the daemon builds
/// its `vike_model::ProfileRisk` from it and the numbers judge every order.
pub(super) fn print_ceilings(filter: Option<&str>, profiles: &[vike_secrets::StoredProfileRisk]) {
    let rows: Vec<&vike_config::Ceiling> = vike_config::ceilings::PRE_TRADE_CEILINGS
        .iter()
        .filter(|c| match filter {
            None => true,
            Some(f) => {
                let f = f.to_lowercase();
                c.name.to_lowercase().contains(&f)
                    || c.home.label().to_lowercase().contains(&f)
                    || "ceiling".contains(f.as_str())
            }
        })
        .collect();
    if rows.is_empty() {
        return;
    }

    println!();
    println!("-- pre-trade ceilings -----------------------------------------------------");
    for line in wrap(
        "Every ceiling an operator of this deployment can write, and the ACT each judges. Two \
         ceilings can share a NAME and judge different acts; nothing compares them.",
        92,
    ) {
        println!("{line}");
    }
    println!();

    let wn = rows.iter().map(|c| c.name.len()).max().unwrap_or(0).max("CEILING".len());
    let wh = rows.iter().map(|c| c.home.label().len()).max().unwrap_or(0).max("LIVES IN".len());
    println!("{:<wn$}  {:<wh$}  VALUE SHOWN", "CEILING", "LIVES IN");
    for c in &rows {
        // Three answers, not two. The third is the one Phase 2 added: a run-profile ceiling whose
        // value this command CAN print, because an operator stored the profile it lives in.
        let mirrored: Vec<&str> = profiles
            .iter()
            .filter(|p| p.rows.iter().any(|r| r.key == c.name))
            .map(|p| p.profile.as_str())
            .collect();
        let shown = if c.home.value_shown_by_config_show() {
            "yes — in the settings table above".to_string()
        } else if !mirrored.is_empty() {
            format!("yes — from the stored {} below", mirrored.join(" / "))
        } else {
            "no — no stored run profile carries it; `config bootstrap-run` writes one".to_string()
        };
        println!("{:<wn$}  {:<wh$}  {shown}", c.name, c.home.label());
    }

    // The label rides the FIRST line of each wrapped paragraph and the rest hangs under it. The
    // obvious alternative — repeating the label on every line — was tried and reads as several
    // separate claims rather than one sentence, which is the opposite of what this block is for.
    let para = |label: &str, text: &str| {
        for (i, line) in wrap(text, 84).into_iter().enumerate() {
            if i == 0 {
                println!("    {label:<9} {line}");
            } else {
                println!("    {:<9} {line}", "");
            }
        }
    };
    for c in &rows {
        println!();
        println!("{} — {}", c.name, c.home.label());
        para("guards:", c.guards);
        if c.enforced_at.is_empty() {
            para("refuses:", "NOTHING refuses on this key from this file.");
        } else {
            for s in c.enforced_at {
                para("refuses:", &format!("{} — {}", s.what, s.file));
            }
        }
        para("if unset:", c.absent_means);
    }

    // The relationship itself, DERIVED from the table rather than typed here — add a second home
    // for a key and this paragraph appears with no edit. It is the last thing printed because it is
    // the thing an operator most needs and least expects.
    let shared: Vec<&str> = vike_config::shared_names()
        .into_iter()
        .filter(|n| rows.iter().any(|c| c.name == *n))
        .collect();
    for name in shared {
        let homes: Vec<&str> = vike_config::ceilings_named(name).map(|c| c.home.label()).collect();
        println!();
        for line in wrap(
            &format!(
                "⚠ `{name}` is written in {} places ({}) and they are NOT one ceiling. Nothing \
                 compares the two numbers — not at load, not at mount, not at submit — and neither \
                 refusal mentions the other. Read each row's `guards` above before assuming the \
                 stricter one is in force for the act you care about.",
                homes.len(),
                homes.join(" and "),
            ),
            92,
        ) {
            println!("  {line}");
        }
    }
}

/// **The stored run-profile `[risk]` values** — the answer to the one question [`print_ceilings`]
/// above could previously only pose.
///
/// # Why this block is worth a screen
///
/// The ceilings block names two keys that REFUSE a live mount when absent and judge every order the
/// core admits — and it could not print their numbers, because they live in a run profile and this
/// command links neither of the crates that parse one. So an operator could read *what can refuse
/// my order* and never *at what size*. Storing the profile's body in the settings database
/// (`config bootstrap-run`) puts the values somewhere a light binary CAN read, **with the daemon
/// down and with no `sqlite3` on the box** — which is 0054's read-back requirement, reaching a file
/// 0054 had not opened.
///
/// # ⚠ THE SENTENCE THIS BLOCK USED TO PRINT IS NOW CONDITIONAL, AND THAT IS THE WHOLE CHANGE
///
/// It said, flatly: *"A mirrored row is READABLE, not ENFORCEABLE. Nothing on the mount path reads
/// one."* That was true of `profile_risk`, the Phase-2 disclosure mirror, and it is FALSE of the
/// profile BODY plane these rows come off now: `crates/vike-tradehub/src/tradehub_cli.rs` reads the
/// ACTIVE `run` body and builds its `vike_model::ProfileRisk` from it — and from nothing else since
/// decision 0111.
///
/// So the block names WHICH RUNG WINS instead of asserting one half as if universal. An ACTIVE row
/// is marked, and the paragraph says outright that the marked body is what judges an order. A
/// stored-but-inactive body keeps the old sentence, which is still exactly right for it. Printing
/// the old sentence over an active row would be positive confirmation of something false about a
/// live pre-trade ceiling — the failure this whole disclosure exists to prevent.
///
/// # …and the one it must print when it has nothing
///
/// A box with no stored bodies, a store that predates the profile tables, and a stored profile that
/// genuinely sets no ceiling are three different facts. So the empty case prints the store's own
/// account of itself rather than nothing at all; a silent block would read as "no ceilings", which
/// is the worst thing this command could say about a live box.
pub(super) fn print_profile_risk(filter: Option<&str>, view: &RunProfileRows) {
    let matches = |hay: &str| match filter {
        None => true,
        Some(f) => hay.to_lowercase().contains(&f.to_lowercase()),
    };
    // The block's own words are filterable like every other, so `--filter risk` reaches it.
    let block_matched = filter.is_none_or(|f| {
        let f = f.to_lowercase();
        "run profile [risk]".contains(f.as_str()) || "ceiling".contains(f.as_str())
    });

    let source = &view.source;
    let profiles: Vec<&vike_secrets::StoredProfileRisk> = source
        .profiles()
        .unwrap_or_default()
        .iter()
        .filter(|p| block_matched || matches(&p.profile) || p.rows.iter().any(|r| matches(&r.key)))
        .collect();
    if profiles.is_empty() && !block_matched {
        return;
    }

    println!();
    println!("-- run profile [risk], as stored ------------------------------------------");
    for line in wrap(
        "The live pre-trade ceilings, read from the settings database rather than from the profile \
         file — so this box can answer with the daemon down.",
        92,
    ) {
        println!("{line}");
    }
    match &view.active {
        Some(name) => {
            for line in wrap(
                &format!(
                    "⚠ ENFORCEABLE: `{name}` is the ACTIVE run profile, so the daemon builds its \
                     `vike_model::ProfileRisk` from THAT body — the only place it reads a run \
                     profile from (decision 0111). Any OTHER profile below is stored and inert. \
                     `vike-cli config deactivate run` leaves NO run profile (a live mount then \
                     refuses to start); a running daemon holds what it BOOTED with either way.",
                ),
                92,
            ) {
                println!("{line}");
            }
        }
        None => {
            for line in wrap(
                "READABLE, NOT ENFORCEABLE on this box: no `run` profile row is ACTIVE, so the \
                 daemon runs with NO run profile — its paper mount has no operator [risk] budget \
                 and a live mount refuses to start. Nothing below binds until a body is ACTIVE: \
                 `vike-cli config bootstrap-run` writes and activates one, `vike-cli config \
                 activate run <name> --proves <file>` activates a stored one.",
                92,
            ) {
                println!("{line}");
            }
        }
    }
    println!();

    if profiles.is_empty() {
        // Never silence. See this function's doc: "nothing mirrored" and "no ceilings" are
        // different facts and only one of them is safe to read as an absence.
        println!("  {source}");
        println!("  `vike-cli config bootstrap-run` is what puts a run profile's ceilings here.");
        return;
    }

    for p in profiles {
        // ⚠ The ACTIVE marker rides the profile's own heading rather than a separate legend,
        // because a legend is what a reader skips: the one fact that decides whether the numbers
        // below judge an order has to be on the same line as the name.
        if view.active.as_deref() == Some(p.profile.as_str()) {
            println!("{}  ⚠ ACTIVE — the daemon builds its ceilings from THIS body", p.profile);
        } else {
            println!("{}  (stored, not selected)", p.profile);
        }
        let rows: Vec<&vike_secrets::ProfileRiskRow> = p
            .rows
            .iter()
            .filter(|r| block_matched || matches(&r.key) || matches(&p.profile))
            .collect();
        if rows.is_empty() {
            println!("  (this profile sets no [risk] key that matches the filter)");
        } else {
            let wk = rows.iter().map(|r| r.key.len()).max().unwrap_or(0).max("KEY".len());
            let wv = rows.iter().map(|r| r.value.len()).max().unwrap_or(0).max("VALUE".len());
            println!("  {:<wk$}  {:<wv$}  BOUNDS", "KEY", "VALUE");
            for r in &rows {
                match vike_config::risk_key_bounds(&r.key) {
                    Some(what) => {
                        for (i, line) in wrap(what, 60).into_iter().enumerate() {
                            if i == 0 {
                                println!("  {:<wk$}  {:<wv$}  {line}", r.key, r.value);
                            } else {
                                println!("  {:<wk$}  {:<wv$}  {line}", "", "");
                            }
                        }
                    }
                    // A row whose key is in no `[risk]` schema. No verb in this tree can write
                    // one, so it arrived by a hand `INSERT`, a restored backup or a migration
                    // written elsewhere — and it configures NOTHING. Saying so is the read half of
                    // the roster's refusal; rendering it as a ceiling would be the defect a
                    // database introduces that a file does not.
                    None => println!(
                        "  {:<wk$}  {:<wv$}  ⚠ UNKNOWN KEY — no `[risk]` field goes by this name, \
                         so it bounds nothing",
                        r.key, r.value
                    ),
                }
            }
        }

        // The unset half, and the two that are not merely unset.
        let missing = vike_config::missing_keys(p);
        if !missing.is_empty() {
            for line in wrap(&format!("not set here: {}", missing.join(", ")), 88) {
                println!("  {line}");
            }
        }
        for name in &missing {
            // The flag comes from the ceilings table, which is its one authority — see
            // `vike_config::ceiling_for`, which is the join rather than a second copy.
            if vike_config::ceiling_for(name).is_some_and(|c| c.refuses_live_mount_when_absent) {
                for line in wrap(
                    &format!(
                        "⚠ `{}` is UNSET in this profile, and a LIVE venue mount REFUSES TO START \
                         without it. On a paper or backtest mount it is simply no ceiling.",
                        name
                    ),
                    88,
                ) {
                    println!("  {line}");
                }
            }
        }
        println!();
    }
}
