//! The header's extraction from a file's text, its two serde helpers, and the header's own checks
//! (a declaration that contradicts itself is refused when the file is read, never when a profile
//! first meets it).

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer};
use vike_model::params::ParamValue;

use super::{
    ParamType, RouteKind, StrategyFileHeader, StrategyFileHeaderParam, StrategyFileNeed,
    StrategyFileRoutes,
};

/// What a file with no header is told.
const NO_HEADER: &str = "no header: a strategy file opens with comment lines (`//`, `///` or \
                         `//!`) holding a ```toml block whose first table is [strategy]";

/// What a file whose header block is not closed is told.
const UNCLOSED: &str =
    "the header's ```toml block is not closed by a ``` line in the file's leading comment lines";

impl StrategyFileHeader {
    /// The header of a whole file's text: the first ```` ```toml ```` block in its leading comment
    /// lines (the module doc's format), parsed and checked. `Err` is the one-line reason the file
    /// is listed broken with.
    pub fn parse(source: &str) -> Result<Self, String> {
        Self::from_toml(&extract(source)?)
    }

    /// The header from the text of its `toml` block, parsed and checked.
    pub fn from_toml(text: &str) -> Result<Self, String> {
        let header: StrategyFileHeader =
            toml::from_str(text).map_err(|e| format!("the header does not parse: {e}"))?;
        header.check()?;
        Ok(header)
    }

    /// The declaration of `key`, or `None` when the header does not declare it.
    pub fn param(&self, key: &str) -> Option<&StrategyFileHeaderParam> {
        self.params.iter().find(|(k, _)| k == key).map(|(_, p)| p)
    }

    /// The header's own consistency, every finding at once.
    fn check(&self) -> Result<(), String> {
        let mut bad = Vec::new();
        let multi_leg = self.strategy.routes == StrategyFileRoutes::MultiLeg;
        match (multi_leg, self.strategy.multi_leg_why.as_deref().map(str::trim)) {
            (true, None | Some("")) => bad.push(
                "`routes = \"multi_leg\"` needs `multi_leg_why`: why its route keys name legs that \
                 are not the mount's own"
                    .to_string(),
            ),
            (false, Some(_)) => bad.push(
                "`multi_leg_why` is given but `routes` is not \"multi_leg\"".to_string(),
            ),
            _ => {}
        }
        for (key, p) in &self.params {
            let param_type = p.r#type;
            if let Some(v) = &p.default
                && !param_type.accepts(v)
            {
                bad.push(format!(
                    "`{key}`'s default wants {}, got {}",
                    param_type.expected(),
                    v.type_str()
                ));
            }
            if let Some([low, high]) = p.range {
                if !matches!(param_type, ParamType::Number | ParamType::Integer) {
                    bad.push(format!("`{key}` has a range but is neither a number nor an integer"));
                } else if low.is_nan() || high.is_nan() || low > high {
                    bad.push(format!("`{key}`'s range [{low}, {high}] holds no value"));
                } else if let Some(d) = p.default.as_ref().and_then(ParamValue::as_f64)
                    && !(low..=high).contains(&d)
                {
                    bad.push(format!("`{key}`'s default {d} is outside its range [{low}, {high}]"));
                }
            }
            for other in p.needs.keys() {
                if other == key {
                    bad.push(format!("`{key}` needs itself"));
                } else if self.param(other).is_none() {
                    bad.push(format!("`{key}` needs `{other}`, which the header does not declare"));
                }
            }
            let route_type = match p.route {
                Some(RouteKind::Symbol | RouteKind::Venue) => Some(ParamType::Text),
                Some(RouteKind::VenueMap) => Some(ParamType::Table),
                None => None,
            };
            if let Some(wanted) = route_type
                && wanted != param_type
            {
                bad.push(format!(
                    "`{key}` is a route, so its type is {} (a `symbol` or `venue` route is \
                     \"text\", a `venue_map` route \"table\")",
                    wanted.expected()
                ));
            }
        }
        if bad.is_empty() {
            Ok(())
        } else {
            Err(format!("the header is refused: {}", bad.join("; ")))
        }
    }
}

/// The text of the first ```` ```toml ```` block in `source`'s leading comment lines, each line's
/// comment marker and one space after it stripped.
pub(super) fn extract(source: &str) -> Result<String, String> {
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let mut block: Option<Vec<&str>> = None;
    for line in source.lines() {
        let trimmed = line.trim_start();
        let Some(text) = ["//!", "///", "//"].iter().find_map(|m| trimmed.strip_prefix(m)) else {
            if trimmed.is_empty() {
                continue;
            }
            break;
        };
        let text = text.strip_prefix(' ').unwrap_or(text);
        if let Some(lines) = block.as_mut() {
            if text.trim() == "```" {
                return Ok(lines.join("\n"));
            }
            lines.push(text);
        } else if text.trim() == "```toml" {
            block = Some(Vec::new());
        }
    }
    Err(if block.is_some() { UNCLOSED } else { NO_HEADER }.to_string())
}

/// `[params]` in the order the header wrote it: the toml table arrives sorted by key, so each key
/// carries its span and the declarations are put back in the order of their spans.
pub(super) fn in_document_order<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Vec<(String, StrategyFileHeaderParam)>, D::Error> {
    let by_key = BTreeMap::<toml::Spanned<String>, StrategyFileHeaderParam>::deserialize(d)?;
    let mut params: Vec<_> = by_key.into_iter().collect();
    params.sort_by_key(|(key, _)| key.span().start);
    Ok(params.into_iter().map(|(key, p)| (key.into_inner(), p)).collect())
}

/// A `needs` entry as written: a list of values, or the word `positive`.
#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum NeedText {
    Values(Vec<String>),
    Word(String),
}

impl TryFrom<NeedText> for StrategyFileNeed {
    type Error = String;

    fn try_from(text: NeedText) -> Result<Self, String> {
        match text {
            NeedText::Values(values) if !values.is_empty() => Ok(StrategyFileNeed::OneOf(values)),
            NeedText::Values(_) => Err("a `needs` list names at least one value".to_string()),
            NeedText::Word(word) if word == "positive" => Ok(StrategyFileNeed::Positive),
            NeedText::Word(word) => {
                Err(format!("a `needs` entry is a list of values or \"positive\", got {word:?}"))
            }
        }
    }
}
