//! The test-side account classification (the seam `vike_bridge_core::credentials::classify_credential_name` fills in production), the node-key predicates and the fake secret values.
//!
//! ⚠ **One table-driven classifier replaces ten hand-built ones.** This crate cannot link the crate
//! that owns the production classification (`vike-bridge-core` is rank 25 where this crate's tier
//! rule is *nothing above rank 10*, and it declares `vike-secrets` itself, so the edge is a cycle
//! as well as a band violation), so every test binary spelled a deliberately SIMPLER rule of its
//! own — ten functions, ten bodies, each re-spelling `Classification { placement: …, field: …,
//! secret: true, recognised: true, pending_move: None }`. What differed between them was never the
//! shape; it was a handful of facts — which PREFIX owns a name, which venue and tier that prefix
//! stands for, whether two accounts share a venue and tier (the discriminator), and the odd
//! venue-scoped key. Those facts are a [`Rule`] each, and a test states only its own:
//!
//! ```ignore
//! const RULES: &[Rule] = &[
//!     Rule::prefix("BYBIT_DEMO_").account("bybit", "demo"),
//!     Rule::exact("CTRADER_CLIENT_ID").venue("ctrader").field("CLIENT_ID"),
//! ];
//! fn classify(name: &str) -> Classification { support::classify_by(RULES, name) }
//! ```
//!
//! **First match wins, in table order**, which is how the hand-built `for`/`if let` chains read, so
//! a table is the old function with the boilerplate taken out and nothing else. A name no rule
//! claims is `Classification::unrecognised(name)`, exactly as every hand-built variant fell
//! through. A rule can only ever produce what ALL ten did — `secret: true`, `recognised: true`,
//! `label: None` — and that is deliberate: **a label is NEVER classified in a fixture** (the owner
//! refused the provisional `DEMO1`/`DEMO2` spellings at the spec's signature — *labels are
//! informative and optional, `id` is the identity*), so a fixture writes what the migration
//! writes: NULL. The one other degree of freedom any variant had, a pending-move table keyed on
//! the field, is [`Rule::pending_moves`].
//!
//! All the builders are `const fn` over `&'static str`, so a table is an ordinary `const`.

use vike_secrets::{AccountKey, Classification, PendingMove, Placement};

/// No node keys in this fixture — the `node_key` namespace is 0051's and nothing here touches it.
/// The answer for every fixture that plants no node file; a fixture that DOES plant one names its
/// own with [`node_keys_in`].
pub fn is_node_key(_key: &str) -> bool {
    false
}

/// **A node-key predicate over a fixed list of names** — the shape `database_migration.rs` and
/// `venue_setting_fold.rs` each hand-wrote (`LIVE_NODE_KEYS.contains(&key)` and `key == NODE_KEY`).
///
/// ⚠ Spelled over the fixture's OWN names rather than over `vike_model::credential_keys::
/// is_platform_key`, on purpose and by ruling (2026-09-26, cited on `vike_secrets::migrate`): the
/// test answers about the box it MEASURED, so it can still disagree with the production table.
pub fn node_keys_in<'a>(names: &'a [&'a str]) -> impl Fn(&str) -> bool + 'a {
    move |key| names.contains(&key)
}

/// A fake value that is stable per key, so an assertion can prove a value did NOT come back — or
/// that a round trip DID return the right one — without any real credential existing anywhere near
/// a test file.
pub fn fake_value(key: &str) -> String {
    format!("value-for-{key}")
}

/// **[`fake_value`] with the legacy `MAINNET` tier spelling folded onto `LIVE`** — the one other
/// body any file had (`scoped_read.rs`).
///
/// ⚠ **The two TIER SPELLINGS of one credential share a value, and that is load-bearing.** The
/// reshape files a legacy spelling as an ALIAS only when both names carry the SAME value; two
/// spellings that DISAGREE are refused and the row is dropped instead. Without this,
/// `ASTER_MAINNET_PRIVATE_KEY` never reaches the database and a scoped read would be compared
/// against a whole-table read over a store with no alias in it — green, and proving nothing about
/// the predicate it exists for. MEASURED: it failed exactly that way first.
pub fn fake_value_folding_mainnet(key: &str) -> String {
    format!("value-for-{}", key.replace("_MAINNET_", "_LIVE_"))
}

/// **The pending-move table's two rows**, spelled here only so the report path they feed is
/// exercised end to end: a `SERVER` field is machine- or tier-scoped configuration that moves to
/// `venue_setting`, and an `ACCOUNT_ID` field IS the book and folds into
/// `account.venue_account_id`. `crates/vike-bridge-core/src/credentials.rs` is where the real table
/// lives and is held — this is the fixture's copy of its shape, not a mirror of its rows.
pub fn pending_move_of_field(field: &str) -> Option<PendingMove> {
    match field {
        "SERVER" => Some(PendingMove::VenueSetting),
        "ACCOUNT_ID" => Some(PendingMove::BookIdentifier),
        _ => None,
    }
}

/// How a rule recognises a name — and so what its `field` is by default.
#[derive(Clone, Copy, Debug)]
enum Matcher {
    /// `name.strip_prefix(prefix)`; the field is what is left.
    Prefix(&'static str),
    /// `name.split_once(token)`; the field is the TAIL, the head can name the venue.
    Split(&'static str),
    /// `name == exact`; the field is the whole name unless [`Rule::field`] says otherwise.
    Exact(&'static str),
    /// Every name; the field is the whole name.
    Any,
}

/// Where a rule's venue comes from.
#[derive(Clone, Copy, Debug)]
enum VenueOf {
    Named(&'static str),
    /// The text before a [`Matcher::Split`] token, ASCII-lowercased (`BINANCE` → `binance`).
    Head,
}

/// Where a rule's tier comes from.
#[derive(Clone, Copy, Debug)]
enum TierOf {
    Literal(&'static str),
    /// `vike_secrets::account_tier_of_key_token(token)` — the PRODUCTION map, for the tests whose
    /// subject is the tier word itself and so must not re-spell it.
    KeyToken(&'static str),
}

#[derive(Clone, Copy, Debug)]
enum Filed {
    Account { venue: VenueOf, tier: TierOf, discriminator: Option<&'static str> },
    Venue(&'static str),
}

/// **The first half of a [`Rule`]: how it recognises a name.** It becomes a rule when it says
/// what the name IS — [`Matching::account`], [`Matching::account_of_head`] or [`Matching::venue`].
#[derive(Clone, Copy, Debug)]
pub struct Matching(Matcher);

/// **One fact a test classifier knows**: a name shape and what it is filed as. See the module doc.
#[derive(Clone, Copy, Debug)]
pub struct Rule {
    matcher: Matcher,
    filed: Filed,
    field: Option<&'static str>,
    pending: bool,
}

impl Rule {
    /// Names starting with `prefix`; the field is the rest (`OKX_DEMO_API_KEY` → `API_KEY`).
    pub const fn prefix(prefix: &'static str) -> Matching {
        Matching(Matcher::Prefix(prefix))
    }

    /// Names containing `token`, split at its FIRST occurrence; the field is the tail, and
    /// [`Matching::account_of_head`] takes the venue from the head (`BINANCE_SIM_API_KEY` at
    /// `_SIM_` → venue `binance`, field `API_KEY`).
    pub const fn split(token: &'static str) -> Matching {
        Matching(Matcher::Split(token))
    }

    /// Exactly this name; the field is the whole name unless [`Rule::field`] overrides it.
    pub const fn exact(name: &'static str) -> Matching {
        Matching(Matcher::Exact(name))
    }

    /// Every name. Only useful as the LAST rule, or as the whole table of a classifier that is
    /// broken on purpose (every row filed under one invalid tier).
    pub const fn any() -> Matching {
        Matching(Matcher::Any)
    }

    /// **Two accounts of one venue and tier** (dukascopy's `DEMO1`/`DEMO2`): the discriminator
    /// reaches no column — it is how the classifier says *these are two* without the index token
    /// becoming an identity again, which is the defect §1 is about. Account rules only.
    pub const fn discriminator(mut self, discriminator: &'static str) -> Rule {
        match self.filed {
            Filed::Account { venue, tier, .. } => {
                self.filed = Filed::Account { venue, tier, discriminator: Some(discriminator) };
            }
            Filed::Venue(_) => panic!("a venue-scoped rule has no discriminator"),
        }
        self
    }

    /// Take the tier from the PRODUCTION key-token map instead of a literal. Account rules only.
    pub const fn tier_token(mut self, token: &'static str) -> Rule {
        match self.filed {
            Filed::Account { venue, discriminator, .. } => {
                self.filed = Filed::Account { venue, tier: TierOf::KeyToken(token), discriminator };
            }
            Filed::Venue(_) => panic!("a venue-scoped rule has no tier"),
        }
        self
    }

    /// The field, when it is not what the matcher leaves (`CTRADER_CLIENT_ID` → `CLIENT_ID`).
    pub const fn field(mut self, field: &'static str) -> Rule {
        self.field = Some(field);
        self
    }

    /// Mark the rows this rule files with [`pending_move_of_field`]'s answer for their field.
    pub const fn pending_moves(mut self) -> Rule {
        self.pending = true;
        self
    }

    fn classify(&self, name: &str) -> Option<Classification> {
        let (rest, head) = match self.matcher {
            Matcher::Prefix(prefix) => (name.strip_prefix(prefix)?, None),
            Matcher::Split(token) => {
                let (head, tail) = name.split_once(token)?;
                (tail, Some(head))
            }
            Matcher::Exact(exact) if name == exact => (name, None),
            Matcher::Exact(_) => return None,
            Matcher::Any => (name, None),
        };
        let field = self.field.unwrap_or(rest).to_string();
        let placement = match self.filed {
            Filed::Account { venue, tier, discriminator } => Placement::Account(AccountKey {
                venue: match venue {
                    VenueOf::Named(venue) => venue.to_string(),
                    VenueOf::Head => head.expect("a head needs a split rule").to_ascii_lowercase(),
                },
                tier: match tier {
                    TierOf::Literal(tier) => tier.to_string(),
                    TierOf::KeyToken(token) => vike_secrets::account_tier_of_key_token(token),
                },
                label: None,
                discriminator: discriminator.map(str::to_string),
            }),
            Filed::Venue(venue) => Placement::Venue(venue.to_string()),
        };
        let pending_move = if self.pending { pending_move_of_field(&field) } else { None };
        Some(Classification { placement, field, secret: true, recognised: true, pending_move })
    }
}

impl Matching {
    /// Filed as an ACCOUNT's credential: `venue` at `tier`, no label.
    pub const fn account(self, venue: &'static str, tier: &'static str) -> Rule {
        Rule {
            matcher: self.0,
            filed: Filed::Account {
                venue: VenueOf::Named(venue),
                tier: TierOf::Literal(tier),
                discriminator: None,
            },
            field: None,
            pending: false,
        }
    }

    /// Filed as an account's credential whose VENUE is the lowercased head of a [`Rule::split`]
    /// match, at `tier`.
    pub const fn account_of_head(self, tier: &'static str) -> Rule {
        match self.0 {
            Matcher::Split(_) => {}
            _ => panic!("`account_of_head` needs a `Rule::split` matcher: nothing else has a head"),
        }
        Rule {
            matcher: self.0,
            filed: Filed::Account {
                venue: VenueOf::Head,
                tier: TierOf::Literal(tier),
                discriminator: None,
            },
            field: None,
            pending: false,
        }
    }

    /// Filed as an APPLICATION credential shared by every account of `venue`
    /// (`Placement::Venue`) — cTrader's OAuth client id.
    pub const fn venue(self, venue: &'static str) -> Rule {
        Rule { matcher: self.0, filed: Filed::Venue(venue), field: None, pending: false }
    }
}

/// **Classify `name` by the first rule that claims it**, or as `Classification::unrecognised` —
/// the engine behind every table. A plain function over a slice, so a `const` table needs only a
/// one-line `fn classify(name: &str) -> Classification` around it to keep every `&classify` call
/// site as it was.
pub fn classify_by(rules: &[Rule], name: &str) -> Classification {
    classify_over(rules, name, Classification::unrecognised)
}

/// [`classify_by`] with a different fallback than `unrecognised` — a LAYER over another
/// classifier: the rules answer for the one or two names a test adds, everything else goes to
/// `otherwise` (usually [`classify`]).
pub fn classify_over(
    rules: &[Rule],
    name: &str,
    otherwise: impl FnOnce(&str) -> Classification,
) -> Classification {
    rules.iter().find_map(|rule| rule.classify(name)).unwrap_or_else(|| otherwise(name))
}

/// [`classify_by`] as a closure that OWNS its rules — for an inline table (`classifier([Rule::…])`),
/// where a `const` would be a name for something used once, and where a borrowed array would not
/// outlive the statement that builds it. Coerces to the `&dyn Fn(&str) -> Classification` that
/// `migrate`/`preview` take.
pub fn classifier(rules: impl IntoIterator<Item = Rule>) -> impl Fn(&str) -> Classification {
    let rules: Vec<Rule> = rules.into_iter().collect();
    move |name| classify_by(&rules, name)
}

/// [`classify_over`] as a closure that owns its rules: `classifier_over([Rule::…], support::classify)`
/// is "the shared classifier, plus these names".
pub fn classifier_over(
    rules: impl IntoIterator<Item = Rule>,
    otherwise: impl Fn(&str) -> Classification,
) -> impl Fn(&str) -> Classification {
    let rules: Vec<Rule> = rules.into_iter().collect();
    move |name| classify_over(&rules, name, &otherwise)
}

/// The names [`classify`] knows.
const FIXTURE_RULES: &[Rule] = &[
    Rule::prefix("DUKASCOPY_DEMO1_").account("dukascopy", "demo").discriminator("DEMO1"),
    Rule::prefix("BINANCE_DEMO_").account("binance", "demo"),
    Rule::prefix("BINANCE_LIVE_").account("binance", "live"),
];

/// The account classification the production caller passes
/// (`vike_bridge_core::credentials::classify_credential_name`), spelled here because this crate
/// cannot link the crate that owns it — the same seam, and the same reason, as
/// `tests/account_book.rs`' own copy. It is the shared fixture's rule table (`FIXTURE_KEYS` is
/// what it classifies), and what a test with a different fixture states for itself with [`Rule`]
/// rather than by writing another one of these.
pub fn classify(name: &str) -> Classification {
    classify_by(FIXTURE_RULES, name)
}
