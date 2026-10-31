//! `credential_schema` — **every venue's credential FIELDS as ONE typed table**: which fields an
//! account of the venue holds, which the venue holds once for every account, which are required,
//! which are secret, and which the GUI's form writes.
//!
//! # A credential belongs to its ACCOUNT, and a field is all a reader names
//!
//! Decision 0156 item 6: the store's `credential.account_id` + `field` address an account's
//! credential (`credential_one_live_value` admits one live row per pair), and `credential.venue_id` +
//! `field` a venue's application credential (cTrader's OAuth client). A bridge asks for "this
//! account's `API_KEY`" and never composes a NAME: no credential name carries a tier, and no reader
//! derives a tier from one. The tier is the account row's (`account.tier`), written by the operator
//! and nowhere else.
//!
//! So this table carries no tier, no token, no name grammar and no placeholder: an account has ONE
//! tier, and its fields are the same whichever it is. What it does carry is the one statement of
//! each venue's field set that the bridges' loaders, the GUI's credential form and its status dots
//! all read, so a form can no longer write a field no loader reads.
//!
//! # `secret` is the GUI's masking rule
//!
//! [`KeyField::secret`] is whether the GUI masks the value (possessing it lets you act as the
//! account). The store's `credential.secret` column is the writer's own rule; a value wrongly marked
//! non-secret is the worse error there.
//!
//! # Key FIELDS only
//!
//! Nothing here reads, holds, forwards or logs a credential VALUE (`credential_keys`' rule).

/// Who holds a field: each ACCOUNT of the venue, or the VENUE once for every account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldOwner {
    /// `credential.account_id` set: the account's own credential (an API key, a login).
    Account,
    /// `credential.venue_id` set and no account: an APPLICATION credential every account of the
    /// venue shares — cTrader's OAuth client registration.
    Venue,
}

/// One credential field of one venue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyField {
    /// The store's `credential.field` word — `API_KEY`, `LOGIN`.
    pub field: &'static str,
    /// The GUI form's label for the field, or a plain description for a field no form writes.
    pub label: &'static str,
    /// The venue's loader returns no credentials (the live gate) without it.
    pub required: bool,
    /// Masked in the GUI.
    pub secret: bool,
    /// Who holds it.
    pub owner: FieldOwner,
    /// The GUI's credential form writes this field. `false` for a field a loader reads that no form
    /// offers (cTrader's optional `ACCOUNT_ID`, discovered at connect; IBKR's defaulted client ids;
    /// polymarket's `FUNDER` fallback and `SIGNATURE_TYPE`).
    pub form: bool,
}

impl KeyField {
    const fn account(
        field: &'static str,
        label: &'static str,
        required: bool,
        secret: bool,
    ) -> Self {
        KeyField { field, label, required, secret, owner: FieldOwner::Account, form: true }
    }

    const fn venue(field: &'static str, label: &'static str, required: bool, secret: bool) -> Self {
        KeyField { field, label, required, secret, owner: FieldOwner::Venue, form: true }
    }

    /// The same field, read by the loader and written by no form.
    const fn read_only(self) -> Self {
        KeyField { form: false, ..self }
    }
}

/// One venue's credential fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VenueKeySchema {
    /// The [`vike_model::VENUES`] id.
    pub venue: &'static str,
    /// The fields, in the GUI form's order (a read-only field sits where its loader reads it).
    pub fields: &'static [KeyField],
}

/// The generic HMAC trio with the passphrase optional (the GUI labels it optional for every
/// venue), built from `credential_keys`' own field constants.
const GRID_FIELDS: [KeyField; 3] = [
    KeyField::account(crate::credential_keys::API_KEY, "API Key", true, true),
    KeyField::account(crate::credential_keys::API_SECRET, "API Secret", true, true),
    KeyField::account(crate::credential_keys::API_PASSPHRASE, "Passphrase (optional)", false, true),
];

/// [`GRID_FIELDS`] for a venue whose signer REQUIRES the passphrase (OKX) —
/// `vike_secrets::venue_passphrase`'s `Required` row.
const GRID_FIELDS_PASSPHRASE_REQUIRED: [KeyField; 3] = [
    GRID_FIELDS[0],
    GRID_FIELDS[1],
    KeyField::account(crate::credential_keys::API_PASSPHRASE, "Passphrase", true, true),
];

impl VenueKeySchema {
    /// The generic `API_KEY` / `API_SECRET` / `API_PASSPHRASE` account set —
    /// `vike_secrets::credentials::api_credentials`', read by every venue that signs with an HMAC
    /// pair.
    #[must_use]
    pub const fn grid(venue: &'static str, passphrase_required: bool) -> VenueKeySchema {
        VenueKeySchema {
            venue,
            fields: if passphrase_required {
                &GRID_FIELDS_PASSPHRASE_REQUIRED
            } else {
                &GRID_FIELDS
            },
        }
    }

    /// The field whose word is `field`.
    #[must_use]
    pub fn field(&self, field: &str) -> Option<&'static KeyField> {
        self.fields.iter().find(|f| f.field == field)
    }

    /// The fields one ACCOUNT holds, in table order.
    pub fn account_fields(&self) -> impl Iterator<Item = &'static KeyField> + '_ {
        self.fields.iter().filter(|f| f.owner == FieldOwner::Account)
    }

    /// The fields the VENUE holds once for every account, in table order.
    pub fn venue_fields(&self) -> impl Iterator<Item = &'static KeyField> + '_ {
        self.fields.iter().filter(|f| f.owner == FieldOwner::Venue)
    }

    /// The REQUIRED fields of `owner`, in table order — the set a loader refuses without.
    #[must_use]
    pub fn required(&self, owner: FieldOwner) -> Vec<&'static str> {
        self.fields.iter().filter(|f| f.owner == owner && f.required).map(|f| f.field).collect()
    }

    /// The GUI credential form of `owner`'s fields, in the form's order.
    #[must_use]
    pub fn form(&self, owner: FieldOwner) -> Vec<&'static KeyField> {
        self.fields.iter().filter(|f| f.owner == owner && f.form).collect()
    }
}

/// **The table: one row per [`vike_model::VENUES`] venue.** Each row's facts were read off that
/// venue's loader (cited in the row's comment) and are held by
/// `crates/vike-venues/tests/credential_key_names.rs`.
pub static VENUE_KEY_SCHEMAS: &[VenueKeySchema] = &[
    // `vike_secrets::credentials::api_credentials` — the generic HMAC set.
    VenueKeySchema::grid("binance", false),
    VenueKeySchema::grid("bybit", false),
    // OKX's signer sends the passphrase on every request: `venue_passphrase` says `Required`.
    VenueKeySchema::grid("okx", true),
    VenueKeySchema::grid("deribit", false),
    // `vike_oanda::config`'s loader.
    VenueKeySchema {
        venue: "oanda",
        fields: &[
            KeyField::account("API_KEY", "API Key", true, true),
            KeyField::account("ACCOUNT_ID", "Account ID", true, false),
        ],
    },
    // `vike_ig::config`'s loader.
    VenueKeySchema {
        venue: "ig",
        fields: &[
            KeyField::account("API_KEY", "API Key", true, true),
            KeyField::account("IDENTIFIER", "Identifier", true, false),
            KeyField::account("PASSWORD", "Password", true, true),
        ],
    },
    // `vike_fxcm::config`'s loader.
    VenueKeySchema {
        venue: "fxcm",
        fields: &[
            KeyField::account("USER", "User", true, false),
            KeyField::account("PASSWORD", "Password", true, true),
        ],
    },
    // `vike_dukascopy::config`'s loader: one JForex login per account. The owner's two demo
    // accounts are two `account` rows at tier `demo` with two labels (decision 0156 item 6).
    VenueKeySchema {
        venue: "dukascopy",
        fields: &[
            KeyField::account("LOGIN", "Login", true, false),
            KeyField::account("PASSWORD", "Password", true, true),
        ],
    },
    // `vike_polymarket::config`'s loader and `exec_plane::recon_client`'s signature type. Required:
    // the L1 key alone; the L2 trio is derived at connect.
    VenueKeySchema {
        venue: "polymarket",
        fields: &[
            KeyField::account("PRIVATE_KEY", "Private Key", true, true),
            KeyField::account("ADDRESS", "Funder Address (optional)", false, false),
            KeyField::account("FUNDER", "Funder (fallback)", false, true).read_only(),
            KeyField::account("API_KEY", "API Key (L2, optional)", false, true),
            KeyField::account("SECRET", "API Secret (L2, optional)", false, true),
            KeyField::account("PASSPHRASE", "API Passphrase (L2, optional)", false, true),
            KeyField::account("RELAYER_API_KEY", "Relayer API Key (optional)", false, true),
            KeyField::account(
                "RELAYER_API_KEY_ADDRESS",
                "Relayer Address (optional)",
                false,
                false,
            ),
            KeyField::account("SIGNATURE_TYPE", "Signature Type", false, true).read_only(),
        ],
    },
    // `vike_ibkr::config`'s loader: the account number is the only required field; every other has
    // a default.
    VenueKeySchema {
        venue: "ibkr",
        fields: &[
            KeyField::account("ACCOUNT", "Account", true, false),
            KeyField::account("CLIENT_ID", "Client ID", false, false).read_only(),
            KeyField::account("DATA_CLIENT_ID", "Data Client ID", false, false).read_only(),
            KeyField::account("RECON_CLIENT_ID", "Recon Client ID", false, false).read_only(),
            KeyField::account("MKTDATA_TYPE", "Market Data Type", false, true).read_only(),
        ],
    },
    // `vike_ctrader::config`'s loader (and `token_store`, which writes the rotated grant back to the
    // same two account fields). The OAuth APPLICATION pair is the venue's: one registration serves
    // every account.
    VenueKeySchema {
        venue: "ctrader",
        fields: &[
            KeyField::account("ACCESS_TOKEN", "Access Token", true, true),
            KeyField::account("REFRESH_TOKEN", "Refresh Token", true, true),
            KeyField::account("ACCOUNT_ID", "Account ID", false, false).read_only(),
            KeyField::venue("CLIENT_ID", "Client ID (app)", true, false),
            KeyField::venue("CLIENT_SECRET", "Client Secret (app)", true, true),
        ],
    },
    // `vike_alpaca::config`'s loader.
    VenueKeySchema {
        venue: "alpaca",
        fields: &[
            KeyField::account("CLIENT_ID", "Client ID", true, false),
            KeyField::account("CLIENT_SECRET", "Client Secret", true, true),
            KeyField::account("ACCOUNT_ID", "Account ID", true, false),
        ],
    },
    // `vike_aster::signing`'s loader: `SIGNER` is the optional signer ADDRESS, derived from the key
    // when absent.
    VenueKeySchema {
        venue: "aster",
        fields: &[
            KeyField::account("USER", "User", true, false),
            KeyField::account("PRIVATE_KEY", "Private Key", true, true),
            KeyField::account("SIGNER", "Signer (optional)", false, false),
        ],
    },
    // vike:new-venue:row // TODO(new-venue: {venue}): read the fields off the venue's own loader. The
    // vike:new-venue:row // generic HMAC set below is a CLAIM that it reads `API_KEY`/`API_SECRET`, not a default.
    // vike:new-venue:row VenueKeySchema::grid("{venue}", false),
    // `vike_hyperliquid::base::config`'s loader: the master `ACCOUNT_ADDRESS` is optional
    // (agent-wallet mode).
    VenueKeySchema {
        venue: "hyperliquid",
        fields: &[
            KeyField::account("PRIVATE_KEY", "Private Key", true, true),
            KeyField::account("ACCOUNT_ADDRESS", "Account Address (optional)", false, false),
        ],
    },
];

/// The row for `venue` (a roster id), or `None` for a venue the table does not carry.
#[must_use]
pub fn schema_for(venue: &str) -> Option<&'static VenueKeySchema> {
    VENUE_KEY_SCHEMAS.iter().find(|s| s.venue == venue)
}
