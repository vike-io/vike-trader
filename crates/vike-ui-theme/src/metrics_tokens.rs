// @generated from crates/vike-ui-theme/ui-theme.toml by crates/vike-ui-theme/tests/tokens_gen — do not edit; change the TOML and regenerate (see tests/brand_assets.rs).

/// Every rounded corner in the app.
pub const RADIUS: u8 = 4;

const COMPACT: Metrics = Metrics { control_h: 20.0, row_h: 16.0, pad: 6.0, gap: 4.0, header_h: 22.0 };
const NORMAL: Metrics = Metrics { control_h: 24.0, row_h: 18.0, pad: 8.0, gap: 6.0, header_h: 24.0 };
const COMFORTABLE: Metrics = Metrics { control_h: 28.0, row_h: 22.0, pad: 10.0, gap: 8.0, header_h: 28.0 };

/// Spacing: `ui-theme.toml`'s `[[space]]`.
pub mod space {
    /// The finest step: the padding an icon button leaves round its glyph, and how far the focus ring stands off its widget.
    pub const XS: f32 = 2.0;
    /// A small gap and the vertical padding of a button; the most-used step.
    pub const SM: f32 = 4.0;
    /// A medium gap; Normal density's gap between controls.
    pub const MD: f32 = 6.0;
    /// A large gap; Normal density's cell padding.
    pub const LG: f32 = 8.0;
    /// The largest step the windows already use.
    pub const XL: f32 = 12.0;
}

/// Strokes: `ui-theme.toml`'s `[[stroke]]`.
pub mod stroke {
    /// A control's edge, a card's border, a rule: every kit widget draws its outline at this width.
    pub const HAIRLINE: f32 = 1.0;
    /// A mark that must read at a glance, such as the venue chip's ring.
    pub const LINE: f32 = 1.5;
    /// The selected edge: the underline of the chosen tab.
    pub const EDGE: f32 = 2.0;
}

/// Strengths: `ui-theme.toml`'s `[[alpha]]`.
pub mod alpha {
    /// How far a filled button moves toward the text colour while it is hovered or pressed.
    pub const LIFT: f32 = 0.12;
    /// How far a chip's fill is mixed into the ground behind it (`Mode::wash`): enough to read as a coloured band on every theme's near-black background, and little enough that what is written on it still reads. On LIVE's accent wash the least of the three inks is the accent itself as text on Dusk, 5.58:1; the theme text clears 12.21:1 and the warning amber 7.53:1 on every theme, Carbon's amber wash included (8.25:1).
    pub const WASH: f32 = 0.14;
}
