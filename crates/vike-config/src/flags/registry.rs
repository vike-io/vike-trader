//! Stewardship: one owner + review date + expected disposition row per `Flags` field.

use serde::Serialize;

#[cfg(doc)]
use super::Flags;
use super::{
    ALLOW_WITHDRAW_KEYS_ENV, CANCEL_ORDERS_ON_SHUTDOWN_ENV, HL_OUTCOME_ENV, HYPERLIQUID_HIP3_ENV,
    OCO_CANCEL_SIBLING_ON_DEAD_EXIT_ENV, PM_RESOLVE_ENV, POLY_AUTO_REDEEM_ENV, POLY_EXEC_ENV,
    POLY_HEARTBEAT_ENV, POLY_RECONCILE_ENV, POLY_REDEEM_HALT_ENV, PREFLIGHT_SKIP_ENV,
    RECONCILE_BALANCE_ENV, RECONCILE_ENV, RECONCILE_GENERATE_MISSING_ENV, RECONCILE_OFF_ENV,
    RECORD_CHAINS_ENV, RECORD_PROPERTIES_ENV, TELEGRAM_CONTROL_ENV, TRADEHUB_ALLOW_PUBLIC_BIND_ENV,
    TRADEHUB_CONTROL_ENV, TRADEHUB_LIVE_ENV, VENUE_CATALOG_OFF_ENV,
};

// -------------------------------------------------------------------------------------------
// Stewardship: owner + review date + expected disposition, one row per field
// -------------------------------------------------------------------------------------------

/// What a flag's review is expected to CONCLUDE. The date forces the conversation; this records
/// which conversation it is, so the review is not re-derived from scratch every time.
///
/// Deliberately three coarse verdicts rather than a free-text note: a verdict a reader can act on
/// beats a paragraph they have to interpret, and "keep" being explicit is what stops the list from
/// reading as a backlog of unfinished work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Disposition {
    /// Stays a flag. This is genuinely a per-run operator decision — a real-money arm, a kill
    /// switch, a safety override — and a file that quietly carries it would be worse, not better.
    Keep,
    /// Should GRADUATE out of [`Flags`] into [`crate::Config`] (or [`crate::Policy`]): it is not
    /// really a per-run decision, it is how this deployment is set up, and it usually already has
    /// non-boolean siblings (`*_CADENCE_MS`, `*_INTERVAL_MS`, a policy name) sitting in config.
    Graduate,
    /// Should be DELETED, with the behaviour it gates becoming unconditional (or dropped). The
    /// flag exists because something was unproven; once it is proven, an option nobody should turn
    /// off is a branch nobody tests.
    Retire,
}

/// One flag's stewardship record — the machine-readable twin of the owner/review line in each
/// [`Flags`] field's doc comment, and the AUTHORITY when the two disagree.
///
/// Kept as a `&'static [FlagMeta]` rather than attributes on the fields for the reason the
/// `vike_model::VENUES` roster and `vike_ops::settings::SETTINGS` are tables: a table can be
/// iterated by a test, and a doc comment cannot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FlagMeta {
    /// The [`Flags`] field name — identical to the `flags.<field>` row key, which
    /// `tests/flag_registry.rs` proves rather than assumes.
    pub field: &'static str,
    /// The environment variable that overrides it — or, where [`FlagMeta::reads_env`] is `false`,
    /// the variable decision 0095 retired, kept so the refusal and the daemon's credential-map fold
    /// can name it.
    pub env: &'static str,
    /// WHO decides this flag's fate. Never blank — that is the one thing the gate exists to
    /// enforce, because a flag with no owner is a flag nobody will ever delete.
    ///
    /// Every row names the same handle today, because the repository has one maintainer and
    /// inventing team names would be fiction. The field exists so ownership can diverge later
    /// without a schema change — which is the whole reason it is a column and not a comment.
    pub owner: &'static str,
    /// ISO-8601 `YYYY-MM-DD`: when this flag's [`Disposition`] is due to be revisited.
    ///
    /// ⚠ A passed date does NOT fail the gate — see the module doc for why the calendar must not
    /// become a merge blocker on unrelated PRs.
    pub review: &'static str,
    /// What that review is expected to conclude.
    pub disposition: Disposition,
}

impl FlagMeta {
    /// Whether [`FlagMeta::env`] is still an environment layer. `false` for a variable decision
    /// 0095 RETIRED: it is refused at startup ([`crate::REMOVED_ENV`]), [`Flags`] has no environment
    /// arm for it, and the settings row alone sets the flag.
    #[must_use]
    pub fn reads_env(&self) -> bool {
        !crate::REMOVED_ENV.iter().any(|r| r.var == self.env)
    }
}

/// Every flag's owner and review date, one row per [`Flags`] field.
///
/// `crates/vike-config/tests/flag_registry.rs` gates this table BOTH ways — a field with no row
/// and a row with no field both fail — and additionally drives each row through the real env and
/// file layers, so a row wired to the wrong field fails too. Adding a `Flags` field without adding
/// a row here therefore fails CI; it does not depend on a reviewer noticing.
///
/// The dates cluster on two dates on purpose: `2026-11-01` for anything touching a live order
/// path, a remote write surface or an unproven-and-therefore-flagged behaviour, and `2027-02-01`
/// for the diagnostics, recorders and long-lived escape hatches. A per-flag date invented to look
/// precise would be noise.
pub const FLAG_REGISTRY: &[FlagMeta] = &[
    // --- reconciliation ---------------------------------------------------------------------
    FlagMeta {
        field: "reconcile",
        env: RECONCILE_ENV,
        owner: "@AlexSokhanych",
        review: "2026-11-01",
        // Named by the design as a permanent flag dressed as a temporary one. The rest of the
        // `VIKE_RECONCILE_*` family (interval, lookback, policy) is already config-shaped.
        disposition: Disposition::Graduate,
    },
    FlagMeta {
        field: "reconcile_generate_missing",
        env: RECONCILE_GENERATE_MISSING_ENV,
        owner: "@AlexSokhanych",
        review: "2027-02-01",
        // A reconcile POLICY choice, and `VIKE_RECONCILE_POLICY` is already a config string.
        disposition: Disposition::Graduate,
    },
    FlagMeta {
        field: "reconcile_balance",
        env: RECONCILE_BALANCE_ENV,
        owner: "@AlexSokhanych",
        review: "2027-02-01",
        // Ships with `*_TOL_ABS`/`*_TOL_REL` siblings that are plainly config; the trio moves
        // together or not at all.
        disposition: Disposition::Graduate,
    },
    FlagMeta {
        field: "oco_cancel_sibling_on_dead_exit",
        env: OCO_CANCEL_SIBLING_ON_DEAD_EXIT_ENV,
        owner: "@AlexSokhanych",
        review: "2026-11-01",
        disposition: Disposition::Retire,
    },
    // --- the headless daemon ----------------------------------------------------------------
    FlagMeta {
        field: "tradehub_live",
        env: TRADEHUB_LIVE_ENV,
        owner: "@AlexSokhanych",
        review: "2026-11-01",
        disposition: Disposition::Keep,
    },
    FlagMeta {
        field: "tradehub_control",
        env: TRADEHUB_CONTROL_ENV,
        owner: "@AlexSokhanych",
        review: "2026-11-01",
        // Also named by the design. It already has `*_KEY` and `*_RATE` config siblings; the
        // listen ADDRESS being present is the honest gate, not a separate boolean.
        disposition: Disposition::Graduate,
    },
    FlagMeta {
        field: "tradehub_allow_public_bind",
        env: TRADEHUB_ALLOW_PUBLIC_BIND_ENV,
        owner: "@AlexSokhanych",
        review: "2026-11-01",
        // KEEP. Unlike its siblings this is a safety OVERRIDE, not a feature switch: it has no
        // config-shaped answer to graduate INTO (the address it guards is already
        // `config.tradehub_addr`, and letting that address imply its own consent is exactly the
        // failure being closed), and retiring it would mean either refusing every LAN deployment
        // or silently publishing a plaintext order surface again.
        disposition: Disposition::Keep,
    },
    FlagMeta {
        field: "telegram_control",
        env: TELEGRAM_CONTROL_ENV,
        owner: "@AlexSokhanych",
        review: "2026-11-01",
        disposition: Disposition::Keep,
    },
    FlagMeta {
        field: "cancel_orders_on_shutdown",
        env: CANCEL_ORDERS_ON_SHUTDOWN_ENV,
        owner: "@AlexSokhanych",
        review: "2026-11-01",
        // A standing operator preference about what a stop does to the book — there is no end
        // state in which this stops being a choice, so it is not on its way anywhere.
        disposition: Disposition::Keep,
    },
    // --- Polymarket -------------------------------------------------------------------------
    FlagMeta {
        field: "poly_exec",
        env: POLY_EXEC_ENV,
        owner: "@AlexSokhanych",
        review: "2026-11-01",
        // The third flag the design names by hand.
        disposition: Disposition::Graduate,
    },
    FlagMeta {
        field: "poly_reconcile",
        env: POLY_RECONCILE_ENV,
        owner: "@AlexSokhanych",
        review: "2026-11-01",
        disposition: Disposition::Graduate,
    },
    FlagMeta {
        field: "poly_heartbeat",
        env: POLY_HEARTBEAT_ENV,
        owner: "@AlexSokhanych",
        review: "2026-11-01",
        disposition: Disposition::Retire,
    },
    FlagMeta {
        field: "poly_auto_redeem",
        env: POLY_AUTO_REDEEM_ENV,
        owner: "@AlexSokhanych",
        review: "2026-11-01",
        disposition: Disposition::Keep,
    },
    FlagMeta {
        field: "poly_redeem_halt",
        env: POLY_REDEEM_HALT_ENV,
        owner: "@AlexSokhanych",
        review: "2027-02-01",
        disposition: Disposition::Keep,
    },
    // --- settlement pollers -----------------------------------------------------------------
    FlagMeta {
        field: "pm_resolve",
        env: PM_RESOLVE_ENV,
        owner: "@AlexSokhanych",
        review: "2027-02-01",
        disposition: Disposition::Keep,
    },
    FlagMeta {
        field: "hl_outcome",
        env: HL_OUTCOME_ENV,
        owner: "@AlexSokhanych",
        review: "2027-02-01",
        disposition: Disposition::Keep,
    },
    // --- venue execution / feed opt-ins -----------------------------------------------------
    FlagMeta {
        field: "hyperliquid_hip3",
        env: HYPERLIQUID_HIP3_ENV,
        owner: "@AlexSokhanych",
        review: "2027-02-01",
        disposition: Disposition::Graduate,
    },
    // --- recorders --------------------------------------------------------------------------
    FlagMeta {
        field: "record_properties",
        env: RECORD_PROPERTIES_ENV,
        owner: "@AlexSokhanych",
        review: "2027-02-01",
        disposition: Disposition::Graduate,
    },
    FlagMeta {
        field: "record_chains",
        env: RECORD_CHAINS_ENV,
        owner: "@AlexSokhanych",
        review: "2027-02-01",
        disposition: Disposition::Graduate,
    },
    // ⚠ `record_dvol` has no row: [`DEAD_FLAG_KEYS`] names the two surviving symbols and what a
    // root must supply. A review date on a key nothing reads is paperwork; a refusal an operator
    // SEES is evidence.
    // --- safety overrides -------------------------------------------------------------------
    FlagMeta {
        field: "allow_withdraw_keys",
        env: ALLOW_WITHDRAW_KEYS_ENV,
        owner: "@AlexSokhanych",
        review: "2027-02-01",
        disposition: Disposition::Keep,
    },
    FlagMeta {
        field: "preflight_skip",
        env: PREFLIGHT_SKIP_ENV,
        owner: "@AlexSokhanych",
        review: "2027-02-01",
        disposition: Disposition::Keep,
    },
    FlagMeta {
        field: "reconcile_off",
        env: RECONCILE_OFF_ENV,
        owner: "@AlexSokhanych",
        review: "2027-02-01",
        // KEEP, not GRADUATE, and for the reason the safety-override siblings above give: this is
        // an escape hatch from a DEFAULT-ON safety behaviour, not a piece of configuration on its
        // way to `Config`. It should stay a flag for exactly as long as the default exists.
        disposition: Disposition::Keep,
    },
    FlagMeta {
        field: "venue_catalog_off",
        env: VENUE_CATALOG_OFF_ENV,
        owner: "@AlexSokhanych",
        review: "2027-02-01",
        // KEEP, for `reconcile_off`'s reason exactly: the refusal of a default-on behaviour is a
        // standing operator hatch. `docs/decisions/0066`'s decision 2 is where the argument is —
        // cost survives as the reason a refusal must EXIST, never as a reason for the default.
        disposition: Disposition::Keep,
    },
];

/// The row for one [`Flags`] field, by field name. `None` for an unknown name.
///
/// Exists so a `config show`-style dump can print "who owns this and when is it reviewed" next to
/// an effective value, which is the operator-facing point of the whole table.
#[must_use]
pub fn flag_meta(field: &str) -> Option<&'static FlagMeta> {
    FLAG_REGISTRY.iter().find(|m| m.field == field)
}
