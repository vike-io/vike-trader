//! **Egress** for every live Polymarket connection: the SOCKS proxy resolution, the proxy-aware
//! `ureq` agents, the keyless [`get_json`] — re-homed here from `exec` for the feeds/exec seam
//! (split-plane Phase 5: the feed pumps dial through the same tunnel and must compile without the
//! exec plane) — plus the **egress guard** (polymarket-workstreams spec §0.2).
//!
//! ## Where the proxy config comes from — the composition root DECLARES it
//! Decision 0095 (`docs/decisions/0095-venues-read-no-environment-and-live-means-mainnet.md`): no
//! environment variable configures a venue, and this module opens no store. The five egress settings
//! are `venue.polymarket.{socks_proxy, proxy_enabled, proxy_host, proxy_port, ws_proxy_enabled}`
//! rows in the settings database; each daemon that dials Polymarket READS them once and hands them,
//! with the credential map it holds, to [`declare_from_rows`] before its first client exists. That is
//! the ONE entry every root calls: it owns the precedence (per field, the row; else the credential
//! map's legacy name, as a TEMPORARY fallback that warns; else the built-in default) and the one
//! store-error policy, and it declares the result through [`declare_egress`]. A process that declares
//! nothing resolves the built-in default, proxy ON at `socks5h://127.0.0.1:1080` — the answer a box
//! with no rows gets too.
//!
//! ⚠ What this replaced, because it is why the rule reads the way it does: the resolver used to
//! read the PROCESS environment first and then the credential store, which it opened itself from a
//! walk that ignored `VIKE_SETTINGS_DIR` — so a daemon's egress depended on where its working
//! directory sat, and an `Environment=` line on a unit beat the database. The ENVIRONMENT layer is
//! gone for good, and a set `POLY_*` proxy variable refuses startup (`vike_config::REMOVED_ENV`). The
//! credential layer is back only as the temporary fallback above, and the ROOT supplies it (a map it
//! already holds, or a scoped read of exactly the five names): this module still opens no store of
//! its own. It exists for a box that has not moved the five names into rows — one that never ran
//! `vike-cli secrets move-venue-config`, or keeps a file store — where reading `venue_setting` alone
//! would silently switch its egress to the default proxy. The release that turns such a credential
//! row into a startup refusal deletes it.
//!
//! ## The egress guard
//!
//! ⚠ **This said the standing constraint was that "all Polymarket traffic" leaves via Dublin
//! (arbdub), and that is WIDER than the rule.** The owner narrowed it on 2026-09-22: Dublin is
//! needed for TRADING — signing and orders — *exactly and only for that*. So the constraint, as it
//! actually stands, has two halves and they are not the same claim:
//!
//! * **REQUIRES the Dublin egress — the EXEC plane.** Order placement and the EIP-712 signing
//!   behind it, the `/auth/derive-api-key` L2 credential derivation a live mount performs, and
//!   reconcile, which authenticates as the account.
//! * **DOES NOT require it — the TAPE.** The keyless CLOB market channel, RTDS, the Gamma catalog
//!   reads, and whatever a recorder writes out of them. This crate had already MEASURED that half
//!   and written it down elsewhere: `crates/bridges/polymarket/CLAUDE.md`'s "Local traps" records
//!   that from a restricted egress the feeds stream and every authenticated READ comes back green
//!   — a submit is the one call that 403s. The retired sentence contradicted its own crate's page.
//!
//! **The wide wording had a measured cost, which is why this is not pedantry.** A session read
//! "all traffic", found a recorder on a tunnel-less box subscribed to a Polymarket family, and
//! raised a false alarm that the box was egressing in violation. It was not: that box records the
//! tape and trades nothing. Anybody reading the old sentence reaches the same wrong conclusion.
//!
//! ⚠ **The narrowing is about what is REQUIRED, never about what is ROUTED — do not read it as a
//! licence to take a feed lane off the tunnel.** [`proxy_url`] is deliberately ONE switch for every
//! Polymarket connection and its default is ON, and that default rests on a second argument this
//! ruling leaves untouched: in some regions the CLOB host does not RESOLVE direct at all, so there
//! the tape needs the tunnel for REACHABILITY rather than for permission ([`proxy_url`]'s own doc
//! carries it). Nothing about the resolution changes here; only the claim about what must hold.
//!
//! **Why the guard exists at all.** Nothing in the tree ever CHECKED the egress. When the tunnel is
//! down a live smoke either self-skips on credential derivation
//! (`crates/bridges/polymarket/tests/polymarket_reconcile_smoke.rs`: *"skip: could not derive L2
//! creds (proxy down / geo-blocked?)"*) or fails as an ordinary network error. Neither says *"you
//! are not egressing via Dublin"*, so a misrouted run can look like an ordinary flake — or, worse,
//! quietly test the WRONG path.
//!
//! This module is the explicit assertion, and it is **opt-in and inert**: with no expected country
//! (`None`), [`check_expected_egress`] performs **no network call at all** and returns
//! [`EgressCheck::NotConfigured`]. The CALLER declares the expectation — it is a parameter, not a
//! setting (decision 0095 retired the variable that used to carry it). **Assert it BEFORE
//! TRADING**: the live smokes that place orders pass [`DUBLIN_EGRESS_COUNTRY`], which turns a
//! misroute into a LOUD failure naming the observed IP/country instead of a limp. A feeds-only,
//! read-only or recorder process has no such expectation to declare, and passing `None` there is
//! the correct call rather than an omission — which is why the parameter is an `Option` rather
//! than a default, and why asserting the region always would have been wrong.
//!
//! ⚠ **MEASURED on the CI box, 2026-09-22 — and it is what makes the wording load-bearing rather than
//! tidy.** That box's `policy.venues.polymarket` ceiling is `live` and the exec credentials are all
//! in its store (`POLY_PRIVATE_KEY`, `POLY_FUNDER`, the relayer pair), so the ONE thing keeping it
//! from trading is `flags.poly_exec`/`flags.poly_reconcile` being `false` — a single settings row — while
//! no tunnel exists at either end: nothing listens on the local SOCKS port, and the Dublin host
//! answers on nothing but sshd. A box in that state is CORRECT while it records the tape and would
//! be in violation the moment it trades, and only the narrow rule can tell those two states apart.
//! Under the old sentence the recording box read as already in breach, which is the false alarm
//! above; under this one the real hazard — an exec mount with no tunnel under it — is what stands
//! out. ⚠ What catches it in a daemon is NOT [`check_expected_egress`]: no composition root calls
//! that (the order-placing smokes do, with [`DUBLIN_EGRESS_COUNTRY`]). The exec mount's own
//! pre-flight is `crates/bridges/polymarket/src/exec_plane/mount.rs`'s `geoblock_preflight`, over
//! [`check_order_placement_geo`] — the venue's answer to whether THIS egress may place orders, which
//! refuses a blocked region rather than asserting the Dublin one.
//!
//! **What the probe proves, precisely.** It rides [`agent`] — the same proxy-aware `ureq` agent
//! every CLOB REST call uses — so it measures the HTTP lane's real egress. The WS lane
//! ([`ws_proxy`]) is not separately probed because, by construction, it can only use the SAME
//! SOCKS endpoint ([`proxy_url`]); [`Egress::ws_lane`] reports whether the WS gate is on so the
//! disclosure is honest about which lanes the observed egress covers.

use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use vike_secrets::DbError;
use vike_secrets::venue_setting::{SettingTier, VenueSettings, venue_setting_names};

/// **Polymarket's egress settings** — the five `venue.polymarket.*` proxy fields, as the
/// composition root read them from the settings database
/// (`docs/decisions/0095-venues-read-no-environment-and-live-means-mainnet.md`).
///
/// Plain data: building one opens no store and reads no environment. A root does not build one
/// itself: it hands the rows it read (and its credential map) to [`declare_from_rows`], which builds
/// this with [`EgressSettings::from_lookup`] and [`declare_egress`]es it once, before the first
/// Polymarket client exists. `Default` is "nothing configured": the built-in proxy at
/// `socks5h://127.0.0.1:1080`.
///
/// ⚠ `Debug` is written by hand: `socks_proxy` may carry `user:password@` (a declared SECRET field),
/// so it prints whether that field is set, never its value.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct EgressSettings {
    socks_proxy: Option<String>,
    proxy_enabled: Option<String>,
    proxy_host: Option<String>,
    proxy_port: Option<String>,
    ws_proxy_enabled: Option<String>,
}

impl EgressSettings {
    /// Build from a lookup that answers one declared field name (`"proxy_host"`, …) with its value,
    /// `None` when there is none. [`declare_from_rows`] is the production caller (the row first, then
    /// the temporary credential fallback); a test builds one from a plain map.
    #[must_use]
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
        EgressSettings {
            socks_proxy: get("socks_proxy"),
            proxy_enabled: get("proxy_enabled"),
            proxy_host: get("proxy_host"),
            proxy_port: get("proxy_port"),
            ws_proxy_enabled: get("ws_proxy_enabled"),
        }
    }

    /// One field's value — its FIRST token only: a row carried over from the old `.env` may still
    /// hold a trailing `# comment`, and every one of these fields is single-token.
    fn field(&self, name: &str) -> Option<String> {
        let value = match name {
            "socks_proxy" => &self.socks_proxy,
            "proxy_enabled" => &self.proxy_enabled,
            "proxy_host" => &self.proxy_host,
            "proxy_port" => &self.proxy_port,
            "ws_proxy_enabled" => &self.ws_proxy_enabled,
            _ => return None,
        };
        value.as_deref().map(|v| super::config::first_token(v).to_string())
    }
}

impl std::fmt::Debug for EgressSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EgressSettings")
            .field("socks_proxy", &self.socks_proxy.as_ref().map(|_| "<secret>"))
            .field("proxy_enabled", &self.proxy_enabled)
            .field("proxy_host", &self.proxy_host)
            .field("proxy_port", &self.proxy_port)
            .field("ws_proxy_enabled", &self.ws_proxy_enabled)
            .finish()
    }
}

/// The ONE declaration this process's egress resolves from — see [`declare_egress`].
static DECLARED: OnceLock<EgressSettings> = OnceLock::new();

/// Set when egress was resolved while nothing was declared, so a late declaration can say that the
/// connections made before it used the defaults.
static RESOLVED_UNDECLARED: AtomicBool = AtomicBool::new(false);

/// **Hand this crate the egress the composition root read from the settings database.** Call it
/// once, from a root, before the first Polymarket client — a feed, a catalog read, a mount.
///
/// The precedent is `vike_bridge_core::halt::declare_project_state_dir`, and so are the failure
/// rules, REPORTED rather than swallowed: a second, DIFFERENT declaration is an `Err` (the first
/// stands — one process has one egress), and a first declaration made after egress was already
/// resolved is an `Err` naming the default those earlier connections used. The same declaration
/// twice is `Ok`. The caller logs the error; neither message carries a value.
///
/// A process that never declares — a test, a tool — resolves the built-in default, which is what a
/// box with no rows resolves too.
pub fn declare_egress(settings: EgressSettings) -> Result<(), String> {
    declare_in(&DECLARED, &RESOLVED_UNDECLARED, settings)
}

/// [`declare_egress`] over caller-owned cells, so its rule is testable without the process-wide one.
fn declare_in(
    cell: &OnceLock<EgressSettings>,
    resolved_undeclared: &AtomicBool,
    settings: EgressSettings,
) -> Result<(), String> {
    let mut first = false;
    let declared = cell.get_or_init(|| {
        first = true;
        settings.clone()
    });
    if *declared != settings {
        return Err("Polymarket egress was already declared with different settings; this second \
                    declaration is IGNORED — one process has one egress"
            .to_string());
    }
    if first && resolved_undeclared.load(Ordering::Relaxed) {
        return Err(
            "Polymarket egress was resolved BEFORE it was declared, so the connections made \
                    until now used the built-in default (a SOCKS proxy at 127.0.0.1:1080) — \
                    declare it before the first Polymarket client is built"
                .to_string(),
        );
    }
    Ok(())
}

/// One declared field, or `None` — the built-in default — when nothing was declared.
fn declared_field(field: &str) -> Option<String> {
    match DECLARED.get() {
        Some(settings) => settings.field(field),
        None => {
            RESOLVED_UNDECLARED.store(true, Ordering::Relaxed);
            None
        }
    }
}

// --- the composition root's ONE entry: rows (+ a temporary credential fallback) -> declaration -------
//
// Every root that dials Polymarket does the same three things — read the settings rows, decide what
// to do when the store will not open, and declare — and until decision 0095's review each spelled
// them itself (five copies, two store-error policies). This is the one spelling. It is DATA IN,
// DATA OUT: the root READS the rows (`vike_secrets::venue_setting::load_venue_settings`) and its
// credential map, and hands both here; this crate still opens no store and reads no environment.

/// The five egress fields — the ONLY names a root's rows or credential map are consulted for, in
/// the order [`EgressSettings::from_lookup`] reads them. `egress_fields_are_exactly_what_the_settings_read`
/// holds it equal to what [`EgressSettings`] really reads, so a sixth field cannot join one list
/// and not the other.
const EGRESS_FIELDS: [&str; 5] =
    ["socks_proxy", "proxy_enabled", "proxy_host", "proxy_port", "ws_proxy_enabled"];

/// The credential-store name the DELETED resolver read `field` under — rendered by the store's own
/// renderer (`vike_secrets::venue_setting::venue_setting_names`), never spelled here, so it is the
/// very name the settings store's fold writes a `venue_setting` row under and the very name
/// `vike-cli secrets move-venue-config` moves. `None` only if the renderer answered nothing, which
/// `each_egress_field_has_exactly_one_legacy_name` holds it never does.
fn legacy_credential_name(field: &str) -> Option<String> {
    venue_setting_names("polymarket", None, &field.to_ascii_uppercase()).into_iter().next()
}

/// **The credential-store names of the five egress fields** — for a root that has no credential
/// map of its own to hand [`declare_from_rows`] and must read exactly these names to build
/// one (the data server, whose boot loads none): declare them as the `KeyScope` of a scoped read
/// and nothing else is materialised.
///
/// ⚠ TEMPORARY, with the fallback it serves — see [`declare_from_rows`].
#[must_use]
pub fn egress_legacy_names() -> Vec<String> {
    EGRESS_FIELDS.iter().filter_map(|f| legacy_credential_name(f)).collect()
}

/// What [`resolve_egress`] decided.
struct ResolvedEgress {
    settings: EgressSettings,
    /// The legacy credential NAMES — never their values — that supplied a field no `venue_setting`
    /// row backs. Empty in the healthy case, and the whole reason for the warning.
    from_credentials: Vec<String>,
}

/// **Per field, in this order: (1) the `venue_setting` row, (2) the value the credential map holds
/// under the legacy name, (3) nothing — the built-in default.** The row is authoritative: the
/// credential map ALREADY carries every `venue_setting` row folded in under its legacy name, and on
/// a collision the credential value wins IN THE MAP, so consulting the map first would let a stale
/// credential row outrank the new home.
///
/// ⚠ **Accepted residual, with no code to close it:** when BOTH homes hold a DIFFERENT value for one
/// of the five names (a row written while a credential row of the same name stayed, or a credential
/// row written beside an existing row — `vike-cli secrets set` still admits these names; nothing
/// REFUSES it and the dry run does not show it, though the trading daemon's credential load logs the
/// colliding names), the release before this one followed the CREDENTIAL value (the store fold keeps
/// it), this function
/// follows the ROW, and `vike-cli secrets move-venue-config` then upserts the credential's value over
/// the row and deletes the credential row — so the egress can change twice. The operator narrative,
/// and the check that makes the two homes agree before the deploy, is `docs/ops/upgrading.md`'s
/// step 1.
///
/// An empty credential value is a value, not an absence: the deleted resolver read
/// `POLY_SOCKS_PROXY=` as "connect direct", and that meaning survives the move.
fn resolve_egress(
    rows: Option<&VenueSettings>,
    credentials: Option<&HashMap<String, String>>,
) -> ResolvedEgress {
    let mut values: HashMap<&'static str, String> = HashMap::new();
    let mut from_credentials = Vec::new();
    for field in EGRESS_FIELDS {
        if let Some(v) = rows.and_then(|r| r.get(SettingTier::Any, field)) {
            values.insert(field, v.to_string());
        } else if let Some(name) = legacy_credential_name(field)
            && let Some(v) = credentials.and_then(|c| c.get(&name))
        {
            values.insert(field, v.clone());
            from_credentials.push(name);
        }
    }
    ResolvedEgress {
        settings: EgressSettings::from_lookup(|field| values.get(field).cloned()),
        from_credentials,
    }
}

/// The warning for a fallback that supplied a value: the NAMES, how to file them, and that the
/// honouring is temporary. Never a value — `POLY_SOCKS_PROXY` can embed `user:password@`.
fn legacy_credential_warning(names: &[String]) -> String {
    format!(
        "polymarket egress: the credential store still holds {} with no venue_setting row behind \
         {} (values are never logged). They are honoured for now, so a box that has not moved them \
         keeps the egress it had — but nothing else reads them, and a later release refuses to \
         start on a credential row like this. File them as rows with `vike-cli secrets \
         move-venue-config` (`--dry-run` first; `vike-cli secrets migrate` first on a box with no \
         settings database).",
        names.join(", "),
        if names.len() == 1 { "it" } else { "them" },
    )
}

/// **The one entry every composition root calls to give this process its Polymarket egress** —
/// from the `venue_setting` rows it read, with a TEMPORARY fallback to the credential names those
/// settings used to live under. It builds the [`EgressSettings`], LOGS what it decided and
/// [`declare_egress`]es it; it returns the `polymarket` rows so a root that also needs another field
/// (the data server's `ws_tokens_per_socket`) does not load them twice.
///
/// * `loaded` is what the root's `vike_secrets::venue_setting::load_venue_settings` returned for the
///   boot's settings directory (`None`: the boot found no directory, so there is nothing to read).
/// * `credentials` is the root's credential map, or `None` when it holds none.
///
/// # Precedence, per field
/// 1. the `venue_setting` row — the new home, authoritative;
/// 2. else the value `credentials` holds under the field's legacy name (`POLY_PROXY_HOST`, …), and
///    then ONE `warn!` names those legacy NAMES (never a value) and `vike-cli secrets
///    move-venue-config`;
/// 3. else nothing: the built-in default, a SOCKS proxy at `127.0.0.1:1080`.
///
/// ⚠ **Step 2 is temporary and exists for one reason: a box that has not run the move.** The old
/// resolver honoured `POLY_PROXY_*` credential rows; the roots read `venue_setting` only. Without
/// the fallback a box that never ran `vike-cli secrets move-venue-config`, or keeps a file store
/// (its five names are still `credential` rows, and stay there until the verb or a `config set`
/// files them), would silently start dialling the default proxy — the egress change this whole plan
/// exists to prevent, and one that stops a recorder on a box that reaches the venue directly. Not
/// every box is such a box: whether a given one is cannot be read off the code or off `vike-cli
/// secrets list` (the store's read side folds a row into the credential NAMES, so the two homes look
/// alike there) — only the tables say, which is why the fallback is unconditional rather than
/// guessed. It ends with the release that turns a stranded credential row into a startup refusal
/// (`docs/superpowers/plans/2026-09-28-venue-settings-live-in-sqlite.md`, Task 7), which deletes
/// step 2 and this function's `credentials` parameter with it.
///
/// # ⚠ The store-error policy — ONE rule, for every root
/// A settings database that exists and cannot be READ (`Some(Err(_))`) is logged at `error!` and read
/// as NO ROWS: step 2 and then the built-in default apply, and the result is declared anyway.
/// * *Not a refusal*, because a venue connection is a capability, not a precondition
///   (`docs/decisions/0013-degrade-vs-refuse.md`): the process still serves and records everything
///   that does not need this venue's egress, and the boot has already refused the failures that
///   should stop a process (an unopenable settings tree).
/// * *Declared rather than left undeclared*, so the process has one deliberate egress and the log
///   says which — a later contradictory declaration is REPORTED by [`declare_egress`], not honoured
///   by whichever connection happens to resolve first.
/// * *The cost is stated in the error line itself*: the default is a SOCKS proxy at
///   `127.0.0.1:1080`, so a box that reaches the venue directly fails every Polymarket dial until
///   the store reads again.
///
/// Before this existed the trading daemon declared NOTHING on a store error and the data server
/// declared the default; the two are the same egress and were two policies.
pub fn declare_from_rows(
    loaded: Option<Result<BTreeMap<String, VenueSettings>, DbError>>,
    credentials: Option<&HashMap<String, String>>,
) -> Option<VenueSettings> {
    declare_from_rows_with(loaded, credentials, declare_egress)
}

/// [`declare_from_rows`] over an injectable declaration, so its rule is testable without the
/// process-wide cell — the same seam [`declare_in`] gives [`declare_egress`].
fn declare_from_rows_with(
    loaded: Option<Result<BTreeMap<String, VenueSettings>, DbError>>,
    credentials: Option<&HashMap<String, String>>,
    declare: impl FnOnce(EgressSettings) -> Result<(), String>,
) -> Option<VenueSettings> {
    let rows = match loaded {
        None => None,
        Some(Ok(mut all)) => all.remove("polymarket"),
        Some(Err(e)) => {
            tracing::error!(
                error = %e,
                "the venue_setting table could not be read; Polymarket egress takes the credential \
                 store's legacy names if it has them and the built-in default otherwise (a SOCKS \
                 proxy at 127.0.0.1:1080) — a box that reaches the venue directly will fail every \
                 Polymarket dial until the store reads again"
            );
            None
        }
    };
    let ResolvedEgress { settings, from_credentials } = resolve_egress(rows.as_ref(), credentials);
    if !from_credentials.is_empty() {
        tracing::warn!("{}", legacy_credential_warning(&from_credentials));
    }
    tracing::info!(egress = ?settings, "polymarket egress declared");
    if let Err(e) = declare(settings) {
        tracing::error!("{e}");
    }
    rows
}

/// Resolve the Polymarket SOCKS proxy URL, or `None` for a direct connection.
///
/// Polymarket is geo/DNS-blocked in some regions (e.g. UA — `clob` resolves to the local router),
/// so the proxy is **ON by default** at `socks5h://127.0.0.1:1080` (`socks5h` = remote DNS through
/// the tunnel; plain `socks5` resolves locally and hits the block). Configure it via:
/// - `venue.polymarket.proxy_enabled` = `false` (or `0`/`no`/`off`) → direct, no proxy
/// - `venue.polymarket.proxy_host` / `proxy_port` → the SOCKS endpoint (default `127.0.0.1` / `1080`)
/// - `venue.polymarket.socks_proxy` = `<full url>` → explicit override (or `none`/`direct` → disable)
///
/// ⚠ **The default-ON is a REACHABILITY decision, and it SURVIVES the 2026-09-22 narrowing of the
/// Dublin rule** (module doc): only the exec plane is REQUIRED to egress via Dublin, but in a
/// region where `clob` does not resolve, the keyless reads do not work direct either. So this stays
/// ONE switch for every lane and stays ON — "the tape needs no Dublin" is a statement about what
/// the venue PERMITS, not about what this box can reach.
///
/// Read from the declaration ([`declare_egress`]); nothing declared means these defaults.
pub fn proxy_url() -> Option<String> {
    proxy_url_with(declared_field)
}

/// [`proxy_url`]'s resolution logic over an injectable field lookup — the testable core. `get` is
/// [`declared_field`] in production; the tests pass a map-backed closure so the resolution can be
/// proven without touching the process-wide declaration.
fn proxy_url_with(get: impl Fn(&str) -> Option<String>) -> Option<String> {
    if let Some(u) = get("socks_proxy") {
        let u = u.trim();
        if u.is_empty() || u.eq_ignore_ascii_case("none") || u.eq_ignore_ascii_case("direct") {
            return None;
        }
        return Some(u.to_string());
    }
    let enabled = get("proxy_enabled")
        .map(|v| !matches!(v.trim().to_ascii_lowercase().as_str(), "false" | "0" | "no" | "off"))
        .unwrap_or(true); // default ON — reachability, not only the Dublin trading rule
    if !enabled {
        return None;
    }
    let host =
        get("proxy_host").filter(|s| !s.is_empty()).unwrap_or_else(|| "127.0.0.1".to_string());
    let port = get("proxy_port").filter(|s| !s.is_empty()).unwrap_or_else(|| "1080".to_string());
    Some(format!("socks5h://{host}:{port}"))
}

/// The SOCKS5 endpoint the **WebSocket** lanes (CLOB market feed, CLOB user channel, RTDS) dial
/// through, or `None` for a direct connection.
///
/// **One unified switch.** WHERE the tunnel is comes from exactly [`proxy_url`] —
/// `venue.polymarket.socks_proxy` / `proxy_host` / `proxy_port` — so HTTP and WS can never disagree
/// about the endpoint. And WHETHER the WS lane uses it now **inherits the HTTP lane's decision by
/// default**: the single control `venue.polymarket.proxy_enabled` (+ the endpoint keys) governs
/// EVERY Polymarket connection — order REST, on-chain settlement, Gamma reads, market feed, RTDS,
/// and the user channel — so an operator turns the proxy on or off in ONE place. (This is the
/// "collapse the WS gate into `proxy_url`" the previous two-gate rollout was staged toward.)
///
/// `venue.polymarket.ws_proxy_enabled` is retained as an OPTIONAL per-lane OVERRIDE for the rare
/// case where the WS lanes must differ from the HTTP lane:
/// - falsey (`0`/`false`/`no`/`off`) → force the WS lanes DIRECT while the HTTP lane still proxies
///   (e.g. order placement is geo-blocked but the keyless feeds are reachable direct — precisely
///   the exec/tape split the module doc's Dublin rule draws, which this override predates and was
///   the tree's first written evidence for);
/// - truthy (`1`/`true`/`yes`/`on`) → force the WS lanes onto the tunnel (a no-op when the HTTP
///   lane already proxies);
/// - UNSET (the default) → inherit the HTTP lane.
///
/// The master OFF still wins over everything: `venue.polymarket.proxy_enabled = false` or
/// `socks_proxy = none` disables BOTH lanes regardless of the WS override (there is no endpoint to
/// dial).
///
/// Read from the declaration ([`declare_egress`]); nothing declared means these defaults.
pub fn ws_proxy() -> Option<vike_bridge_core::ws_proxy::WsProxy> {
    ws_proxy_with(declared_field)
}

/// [`ws_proxy`]'s logic over an injectable key lookup — the testable core (see [`proxy_url_with`]).
fn ws_proxy_with(
    get: impl Fn(&str) -> Option<String>,
) -> Option<vike_bridge_core::ws_proxy::WsProxy> {
    // UNIFIED switch: the WS lane INHERITS the HTTP lane's decision ([`proxy_url_with`]) by
    // default, so ONE control governs every Polymarket connection. `ws_proxy_enabled` is
    // now an OPTIONAL per-lane OVERRIDE — a falsey value forces the WS lanes DIRECT while the HTTP
    // lane still proxies; a truthy value is a no-op when the HTTP lane already proxies; UNSET
    // inherits. The master OFF (`proxy_enabled = false` / `socks_proxy = none`) still
    // disables BOTH lanes, via `proxy_url_with` below.
    if let Some(v) = get("ws_proxy_enabled") {
        let on = matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on");
        if !on {
            return None; // explicit WS-off override (the HTTP lane may still proxy)
        }
    }
    let url = proxy_url_with(&get)?; // `proxy_enabled = false` / `socks_proxy = none` disables both lanes
    match vike_bridge_core::ws_proxy::WsProxy::parse(&url) {
        Ok(p) => Some(p),
        Err(e) => {
            // LOUD, not silent: falling back to a direct dial would hit the very geo-block the
            // proxy exists to clear. The message carries the parse error, never the URL's
            // userinfo — an assertion this comment used to make on its own, while two of
            // `WsProxy::parse`'s arms echoed the raw url straight into `e`. It is now backed by
            // `vike_bridge_core::ws_proxy`'s `redact_userinfo` and gated by that module's
            // `a_parse_error_never_carries_the_proxy_credentials`.
            tracing::error!(venue = "polymarket", error = %e, "Polymarket proxy url is unusable — WS lanes stay DIRECT");
            None
        }
    }
}

/// The global timeout [`agent`] carries — the ORDER-PATH budget, sized for a submit/cancel round
/// trip through the SOCKS tunnel on a venue that is geo-blocked from most of our boxes.
///
/// It is deliberately NOT the feed path's budget. A caller on a live feed thread spends this window
/// out of `crates/vike-datahub/src/recorder.rs`'s `FEED_STOP_BUDGET_SECS`, which is 12 s
/// in total, so it takes [`agent_with_timeout`] and a shorter ceiling instead — see
/// `crates/bridges/polymarket/src/market_feed.rs`'s `FEED_WARMUP_TIMEOUT`.
pub(crate) const REST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// A proxy-aware agent (see [`proxy_url`]) with an explicit global timeout — the rung that exists so
/// a caller whose thread has a stop budget can say what its own ceiling is instead of inheriting the
/// order path's [`REST_TIMEOUT`].
///
/// Every other property is identical for both ceilings on purpose: the proxy arm, the
/// `http_status_as_error(false)` contract (4xx/5xx come back as responses, so the callers' body
/// parsers stay the error surface) and the user agent. Splitting the timeout out is the whole change
/// — a second hand-built agent would be a second thing to forget to route through the tunnel, which
/// is exactly the bug the `with_agent` comment in `market_feed.rs`'s `shard_main` records.
pub(crate) fn agent_with_timeout(global: std::time::Duration) -> ureq::Agent {
    let mut b = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(global))
        .user_agent("vike-trader-rust");
    if let Some(url) = proxy_url()
        && let Ok(proxy) = ureq::Proxy::new(&url)
    {
        b = b.proxy(Some(proxy));
    }
    b.build().new_agent()
}

/// A proxy-aware agent (see [`proxy_url`]). Shared by the REST reads, the L1 derive, and
/// submit/cancel, so every call made through it routes through the tunnel when one is enabled —
/// deliberately ONE agent rather than a second, unrouted one for the read half.
///
/// ⚠ That is a statement about this AGENT, not about the standing constraint. Only the exec plane
/// is REQUIRED to leave via Dublin (module doc); the reads ride the same tunnel because sharing one
/// agent is how the two lanes are kept from disagreeing about the endpoint, not because the tape
/// owes anybody a region.
pub(crate) fn agent() -> ureq::Agent {
    agent_with_timeout(REST_TIMEOUT)
}

/// Proxy-aware unauthenticated GET → parsed JSON (book/midpoint/markets reads through the tunnel).
pub fn get_json(base: &str, path: &str, query: &str) -> Result<serde_json::Value, String> {
    let url =
        if query.is_empty() { format!("{base}{path}") } else { format!("{base}{path}?{query}") };
    let mut resp = agent().get(&url).call().map_err(|e| format!("network: {e}"))?;
    let text = resp.body_mut().read_to_string().map_err(|e| format!("network: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("bad json: {e}"))
}

/// The IP/geo probe the smokes hand [`check_expected_egress`] — any endpoint returning a JSON object
/// with an `ip` field and, ideally, `country` works as the `probe_url` argument.
pub const DEFAULT_EGRESS_PROBE: &str = "https://ipinfo.io/json";

/// The region the Dublin SOCKS tunnel exits from — what an order-placing run asserts before it
/// trades (`check_expected_egress(Some(DUBLIN_EGRESS_COUNTRY), DEFAULT_EGRESS_PROBE)`).
pub const DUBLIN_EGRESS_COUNTRY: &str = "IE";

/// The observed public egress of this process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Egress {
    /// The source IP the internet sees.
    pub ip: String,
    /// ISO-3166 alpha-2 country, when the probe reports one (`IE` for the Dublin host).
    pub country: Option<String>,
    /// City, when the probe reports one (purely for the disclosure line).
    pub city: Option<String>,
    /// Whether the WS lanes are ALSO tunnelled (`venue.polymarket.ws_proxy_enabled`) — the probe
    /// itself only measures the HTTP lane, so this makes the coverage of the claim explicit.
    pub ws_lane: bool,
}

impl std::fmt::Display for Egress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.ip)?;
        if let Some(c) = &self.country {
            write!(f, " [{c}")?;
            if let Some(city) = &self.city {
                write!(f, "/{city}")?;
            }
            write!(f, "]")?;
        }
        write!(f, " (ws lane {})", if self.ws_lane { "tunnelled" } else { "DIRECT" })
    }
}

/// Outcome of [`check_expected_egress`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EgressCheck {
    /// No expectation declared — nothing was probed.
    NotConfigured,
    /// The observed egress matches the declared expectation.
    Ok(Egress),
    /// The observed egress does NOT match — a misrouted run.
    Mismatch { expected: String, observed: Egress },
}

impl EgressCheck {
    /// Collapse to a `Result` a caller can `?`/`expect` on: a mismatch becomes a descriptive
    /// `Err`, an unchecked run becomes `Ok(None)`. This is what the live smokes assert against.
    pub fn into_result(self) -> Result<Option<Egress>, String> {
        match self {
            EgressCheck::NotConfigured => Ok(None),
            EgressCheck::Ok(e) => Ok(Some(e)),
            EgressCheck::Mismatch { expected, observed } => Err(format!(
                "egress is {observed} but the expected egress country is {expected} — this run is \
                 NOT routed through the expected region (tunnel down? proxy disabled?)"
            )),
        }
    }
}

/// Pure parse of a probe response (`ipinfo.io`-shaped: `{"ip":…,"city":…,"country":…}`). Split out
/// from the network call so it is fixture-tested with zero I/O, like every other parser here.
pub fn parse_egress(body: &str, ws_lane: bool) -> Result<Egress, String> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("egress probe: bad json: {e}"))?;
    let ip = v
        .get("ip")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "egress probe: response has no `ip`".to_string())?
        .to_string();
    let field = |k: &str| {
        v.get(k).and_then(serde_json::Value::as_str).filter(|s| !s.is_empty()).map(str::to_string)
    };
    Ok(Egress { ip, country: field("country"), city: field("city"), ws_lane })
}

/// Probe the live egress at `probe_url` through the proxy-aware agent. Network call — only reached
/// when an expectation is declared (see [`check_expected_egress`]) or a caller asks explicitly.
pub fn observe_egress(probe_url: &str) -> Result<Egress, String> {
    let mut resp =
        agent().get(probe_url).call().map_err(|e| format!("egress probe {probe_url}: {e}"))?;
    let text =
        resp.body_mut().read_to_string().map_err(|e| format!("egress probe {probe_url}: {e}"))?;
    parse_egress(&text, ws_proxy().is_some())
}

/// Assert the process egresses from `expected` (case-insensitive ISO alpha-2, e.g.
/// [`DUBLIN_EGRESS_COUNTRY`]), probing `probe_url`. **Inert for `None` or a blank value** — returns
/// [`EgressCheck::NotConfigured`] without touching the network. The CALLER decides: an order-placing
/// smoke passes the tunnel's region, a read-only one passes `None` (decision 0095).
pub fn check_expected_egress(
    expected: Option<&str>,
    probe_url: &str,
) -> Result<EgressCheck, String> {
    let Some(expected) = expected.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(EgressCheck::NotConfigured);
    };
    let observed = observe_egress(probe_url)?;
    match &observed.country {
        Some(c) if c.eq_ignore_ascii_case(expected) => Ok(EgressCheck::Ok(observed)),
        _ => Ok(EgressCheck::Mismatch { expected: expected.to_string(), observed }),
    }
}

// ---------------------------------------------------------------------------------------------
// THE VENUE'S OWN ORDER-PLACEMENT GEOBLOCK
// ---------------------------------------------------------------------------------------------
//
// The guard above answers "where am I egressing from"; this one answers the question that actually
// costs money — "will this venue let me place an order from here". They are NOT the same question,
// and the difference is the whole reason this exists: Polymarket restricts ORDER PLACEMENT by
// region while leaving market data AND authenticated reads working. Measured through a German exit
// (2026-08-23): balance, positions, orders and fills all green, then a submit refused with a 403
// whose body reads "Trading restricted in your region".
//
// So a wrong-region session looks completely healthy — it mounts, streams, reconciles positions and
// shows balances — and discovers the refusal one order at a time, on the exec path. That is the
// same defect class as a SOCKS tunnel that silently failed to bind, and the cure is the same: ask
// BEFORE, through the SAME egress the orders will use.

/// The venue's own keyless, free pre-flight for order placement.
///
/// ⚠ It is served by **`polymarket.com`**, NOT the CLOB API host ([`crate::config::CLOB_BASE`]) —
/// the web app owns this route and the trading API does not. Verified live returning exactly
/// `{"blocked":true,"ip":"2a01:…","country":"DE","region":"SN"}`.
///
/// The venue's published tiers are finer than this one bit — Germany is "close-only on frontend
/// AND API" (no new orders) while Ireland is "close-only on frontend" only, with the API
/// unrestricted — so `blocked` is read as exactly what it is: THIS endpoint's answer for THIS
/// egress, not a reconstruction of the tier table.
pub const GEOBLOCK_URL: &str = "https://polymarket.com/api/geoblock";

/// The ceiling [`observe_geoblock`] dials under — deliberately shorter than [`REST_TIMEOUT`], and
/// for the same reason `crates/bridges/polymarket/src/market_feed.rs`'s `FEED_WARMUP_TIMEOUT` is:
/// this probe runs on the MOUNT path, ahead of the L2 handshake. Its entire contract is that
/// failing to reach it changes nothing, so it must not be able to stall a mount for the order
/// path's budget either.
const GEOBLOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The venue's answer about order placement from this egress — the parsed `/api/geoblock` body.
///
/// Only `blocked` is required; the geo fields are a courtesy of an undocumented web-app route. They
/// are carried anyway because a refusal that cannot name WHERE it is refusing from tells an
/// operator nothing they can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Geoblock {
    /// The venue's verdict — ⚠ **this is the FRONTEND's policy, not the API's**, and reading it as
    /// "order placement is refused" is wrong. See [`api_placement_permitted`]: measured
    /// 2026-08-23, Ireland returns `blocked: true` here while the CLOB API accepts orders from it.
    pub blocked: bool,
    /// The source IP the venue saw, when it reports one.
    pub ip: Option<String>,
    /// ISO-3166 alpha-2 country, when the venue reports one (`DE` on the measured refusal).
    pub country: Option<String>,
    /// Sub-national region, when the venue reports one (`SN` on the measured refusal).
    pub region: Option<String>,
}

/// `?` rather than an empty gap for a field the endpoint did not report — a refusal message that
/// renders `country=` and stops is worse than one that says it does not know.
fn or_unknown(field: &Option<String>) -> &str {
    field.as_deref().unwrap_or("?")
}

impl std::fmt::Display for Geoblock {
    /// The LOCATION only. The verdict itself is [`Geoblock::blocked`] (and, at the call sites that
    /// matter, the [`GeoblockVerdict`] variant), so rendering it here too would make every message
    /// that already names the outcome say it twice.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "country={} region={} ip={}",
            or_unknown(&self.country),
            or_unknown(&self.region),
            or_unknown(&self.ip)
        )
    }
}

/// Pure parse of an `/api/geoblock` body — split from the network call so the tolerance rules below
/// are fixture-tested with zero I/O, exactly like [`parse_egress`].
///
/// **The tolerance is stated precisely because a mount REFUSAL hangs off it.** `blocked` is the one
/// required field, and it is honoured only where a boolean can honestly be read out of it (`true`/
/// `false`, or those two spelled as a string — this is a web-app route, not a contracted API).
/// Anything else — the field absent, a number, a nested object, a non-object body, not JSON at all
/// — is an `Err`, **never** `blocked: false`: "we could not tell" and "the venue says you may
/// trade" are different answers, and rendering the first as the second turns a pre-flight into a
/// false all-clear. Every other field is optional, a non-string value reads as absent, and no input
/// can panic.
pub fn parse_geoblock(body: &str) -> Result<Geoblock, String> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("geoblock: bad json: {e}"))?;
    let blocked = match v.get("blocked") {
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::String(s)) if s.eq_ignore_ascii_case("true") => true,
        Some(serde_json::Value::String(s)) if s.eq_ignore_ascii_case("false") => false,
        _ => return Err("geoblock: response carries no usable `blocked` field".to_string()),
    };
    let field = |k: &str| {
        v.get(k).and_then(serde_json::Value::as_str).filter(|s| !s.is_empty()).map(str::to_string)
    };
    Ok(Geoblock { blocked, ip: field("ip"), country: field("country"), region: field("region") })
}

/// Ask the venue, through [`agent_with_timeout`] — i.e. through the SAME resolved proxy
/// ([`proxy_url`]) every CLOB REST call and every WS dial uses.
///
/// That sharing is the whole point of homing this beside the proxy resolution rather than building
/// a second agent: a pre-flight that measured a DIFFERENT egress than the one the orders leave by
/// would answer a question nobody asked.
pub fn observe_geoblock() -> Result<Geoblock, String> {
    let fail = |e: String| format!("geoblock probe {GEOBLOCK_URL}: {e}");
    let agent = agent_with_timeout(GEOBLOCK_TIMEOUT);
    let mut resp = agent.get(GEOBLOCK_URL).call().map_err(|e| fail(e.to_string()))?;
    let text = resp.body_mut().read_to_string().map_err(|e| fail(e.to_string()))?;
    parse_geoblock(&text)
}

/// What the venue said about order placement from this egress, in the three shapes a caller must
/// treat differently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeoblockVerdict {
    /// The venue answered, and this egress may place orders.
    Allowed(Geoblock),
    /// The venue answered, and order placement is REFUSED from this egress. Reads are unaffected.
    Blocked(Geoblock),
    /// The venue did not answer, or answered something unreadable — carrying the reason.
    ///
    /// ⚠ This is NOT a block, and must never be collapsed into one. An unreachable pre-flight is
    /// evidence of nothing, and a venue outage that became a mount failure would be a worse defect
    /// than the one this whole probe exists to catch.
    Unknown(String),
}

/// Classify an observation. PURE — the network half stops at this function's argument, which is
/// what makes "an unreachable probe is not a block" a unit test instead of a claim.
/// ⚠ The classification asks [`api_placement_permitted`], NOT `g.blocked`. The endpoint reports the
/// FRONTEND's policy, and for four jurisdictions the API's policy differs — Ireland reports
/// `blocked: true` here and accepts orders. Keying the verdict straight off `blocked` refused the
/// one region in this workspace where trading works, which is what shipping and then measuring
/// found.
pub fn geoblock_verdict(observed: Result<Geoblock, String>) -> GeoblockVerdict {
    match observed {
        Ok(g) if !api_placement_permitted(&g) => GeoblockVerdict::Blocked(g),
        Ok(g) => GeoblockVerdict::Allowed(g),
        Err(why) => GeoblockVerdict::Unknown(why),
    }
}

/// The probe and its classification in one call — what a live exec mount asks.
/// `crates/bridges/polymarket/src/exec_plane/mount.rs`'s `geoblock_action` decides what to DO about it.
pub fn check_order_placement_geo() -> GeoblockVerdict {
    geoblock_verdict(observe_geoblock())
}

#[path = "egress_tests.rs"]
#[cfg(test)]
pub(crate) mod egress_tests;

/// The four jurisdictions Polymarket documents as **frontend-only** restrictions — close-only on
/// polymarket.com while *"the API itself is not restricted"*.
///
/// ⚠ This list exists because `/api/geoblock` CANNOT answer the question the exec mount is asking.
/// That route lives on `polymarket.com` and reports the **site's** policy; the CLOB API's policy is
/// different, and the two disagree for exactly these countries. Measured 2026-08-23, both halves:
///
/// | egress | `/api/geoblock` | a real order |
/// |---|---|---|
/// | Germany (`DE`) | `blocked: true` | **403** `Trading restricted in your region` |
/// | Ireland (`IE`) | `blocked: true` | **ACCEPTED** — went `live`, then cancelled clean |
///
/// So `blocked: true` alone would refuse Ireland, which is one of only four regions where trading
/// works at all — and is where this workspace's own Dublin exit lives. Reconstructing the tier is
/// not gold-plating here; it is the difference between the gate refusing the wrong half of the
/// world and refusing the right one.
const API_PERMITTED_WHEN_FRONTEND_BLOCKED: [&str; 4] = ["IE", "JP", "NL", "MT"];

/// Does the CLOB **API** permit order placement from this egress, given the frontend's verdict?
///
/// `blocked: false` is unambiguous — neither surface restricts it. `blocked: true` is ambiguous by
/// construction, and is resolved by country: the four frontend-only jurisdictions still trade
/// through the API, everything else does not. An unreported country under `blocked: true` is
/// treated as NOT permitted, because the measured majority of that tier really is API-restricted
/// and a false refusal is recoverable (the override) where a false permit is a 403 on the exec path.
#[must_use]
pub fn api_placement_permitted(g: &Geoblock) -> bool {
    if !g.blocked {
        return true;
    }
    g.country
        .as_deref()
        .map(|c| API_PERMITTED_WHEN_FRONTEND_BLOCKED.iter().any(|p| p.eq_ignore_ascii_case(c)))
        .unwrap_or(false)
}

#[cfg(test)]
mod api_placement_tests {
    use super::*;

    fn geo(blocked: bool, country: Option<&str>) -> Geoblock {
        Geoblock { blocked, ip: None, country: country.map(str::to_string), region: None }
    }

    /// The measurement that forced this function to exist: both countries report `blocked: true`,
    /// and only one of them can actually trade.
    #[test]
    fn ireland_trades_and_germany_does_not_although_both_report_blocked() {
        assert!(api_placement_permitted(&geo(true, Some("IE"))), "IE placed a live order");
        assert!(!api_placement_permitted(&geo(true, Some("DE"))), "DE was refused 403 on submit");
    }

    /// An unrestricted frontend means an unrestricted API — no country lookup needed.
    #[test]
    fn an_unblocked_frontend_permits_regardless_of_country() {
        assert!(api_placement_permitted(&geo(false, Some("DE"))));
        assert!(api_placement_permitted(&geo(false, None)));
    }

    /// All four documented frontend-only jurisdictions, case-insensitively.
    #[test]
    fn every_frontend_only_jurisdiction_still_trades() {
        for c in ["IE", "JP", "NL", "MT", "ie", "Nl"] {
            assert!(api_placement_permitted(&geo(true, Some(c))), "{c} is frontend-only");
        }
    }

    /// Blocked with no country is the one case that must fail CLOSED: a false refusal is undone by
    /// the override, a false permit is a 403 on the exec path.
    #[test]
    fn blocked_without_a_country_is_refused() {
        assert!(!api_placement_permitted(&geo(true, None)));
        assert!(!api_placement_permitted(&geo(true, Some(""))));
    }
}
