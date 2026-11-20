use indexmap::IndexMap;
use serde::{Deserialize, Deserializer};

use crate::error::Result;

/// Number that accepts JSON numbers or numeric strings (old layouts contain both).
#[derive(Debug, Clone, Copy, Default)]
pub struct Num(pub f64);

impl<'de> Deserialize<'de> for Num {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        let n = match v {
            serde_json::Value::Number(n) => n.as_f64().unwrap_or(0.0),
            serde_json::Value::String(s) => s.trim().replace(',', ".").parse().unwrap_or(0.0),
            _ => 0.0,
        };
        Ok(Num(n))
    }
}

/// Boolean-ish flag: accepts true/false, "0"/"1", 0/1.
#[derive(Debug, Clone, Copy, Default)]
pub struct Flag(pub bool);

impl<'de> Deserialize<'de> for Flag {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        let b = match v {
            serde_json::Value::Bool(b) => b,
            serde_json::Value::Number(n) => n.as_f64().unwrap_or(0.0) != 0.0,
            serde_json::Value::String(s) => {
                matches!(s.trim(), "1" | "true" | "True" | "yes" | "on")
            }
            _ => false,
        };
        Ok(Flag(b))
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct Rect {
    #[serde(default)]
    pub left: Num,
    #[serde(default)]
    pub top: Num,
    #[serde(default)]
    pub width: Num,
    #[serde(default)]
    pub height: Num,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Font {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub size: Num,
    #[serde(default)]
    pub bold: Flag,
    #[serde(default)]
    pub italic: Flag,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SendAction {
    #[serde(default, rename = "Delay")]
    pub delay: Num,
    #[serde(default, rename = "RequestName")]
    pub request: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Presend {
    #[serde(default, rename = "Delay")]
    pub delay: Num,
    #[serde(default, rename = "RequestName")]
    pub request: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DisplayDef {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub request: String,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub fontcolor: String,
    #[serde(default)]
    pub width: Num,
    #[serde(default)]
    pub rect: Rect,
    #[serde(default)]
    pub font: Font,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct InputDef {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub request: String,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub fontcolor: String,
    #[serde(default)]
    pub width: Num,
    #[serde(default)]
    pub rect: Rect,
    #[serde(default)]
    pub font: Font,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ButtonDef {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub uniquename: String,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub width: Num,
    #[serde(default)]
    pub rect: Rect,
    #[serde(default)]
    pub font: Font,
    #[serde(default)]
    pub messages: Vec<String>,
    #[serde(default)]
    pub send: Vec<SendAction>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct LabelDef {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub fontcolor: String,
    #[serde(default)]
    pub bbox: Rect,
    #[serde(default)]
    pub font: Font,
    #[serde(default)]
    pub alignment: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Screen {
    pub width: Num,
    pub height: Num,
    pub color: String,
    pub presend: Vec<Presend>,
    pub displays: Vec<DisplayDef>,
    pub inputs: Vec<InputDef>,
    pub buttons: Vec<ButtonDef>,
    pub labels: Vec<LabelDef>,
}

impl Default for Screen {
    fn default() -> Self {
        Self {
            width: Num(8000.0),
            height: Num(8000.0),
            color: "rgb(0,0,30)".into(),
            presend: Vec::new(),
            displays: Vec::new(),
            inputs: Vec::new(),
            buttons: Vec::new(),
            labels: Vec::new(),
        }
    }
}

impl Screen {
    pub fn width(&self) -> f64 {
        if self.width.0 > 0.0 {
            self.width.0
        } else {
            8000.0
        }
    }
    pub fn height(&self) -> f64 {
        if self.height.0 > 0.0 {
            self.height.0
        } else {
            8000.0
        }
    }
    pub fn button_key(button: &ButtonDef) -> String {
        if button.uniquename.is_empty() {
            button.text.clone()
        } else {
            button.uniquename.clone()
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct LayoutSpec {
    #[serde(default)]
    pub categories: IndexMap<String, Vec<String>>,
    #[serde(default)]
    pub screens: IndexMap<String, Screen>,
}

#[derive(Debug, Clone, Default)]
pub struct EcuLayout {
    pub categories: IndexMap<String, Vec<String>>,
    pub screens: IndexMap<String, Screen>,
}

impl EcuLayout {
    pub fn from_json(text: &str) -> Result<Self> {
        let spec: LayoutSpec = serde_json::from_str(text)?;
        Ok(Self {
            categories: spec.categories,
            screens: spec.screens,
        })
    }

    pub fn screen(&self, name: &str) -> Option<&Screen> {
        self.screens.get(name)
    }

    /// All screens in display order: categories first, then uncategorized screens.
    pub fn ordered_screens(&self) -> Vec<(Option<String>, String)> {
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for (category, screens) in &self.categories {
            for screen in screens {
                if self.screens.contains_key(screen) && seen.insert(screen.clone()) {
                    out.push((Some(category.clone()), screen.clone()));
                }
            }
        }
        for screen in self.screens.keys() {
            if seen.insert(screen.clone()) {
                out.push((None, screen.clone()));
            }
        }
        out
    }

    pub fn categories_for(&self, screen: &str) -> Vec<&str> {
        self.categories
            .iter()
            .filter(|(_, screens)| screens.iter().any(|s| s == screen))
            .map(|(c, _)| c.as_str())
            .collect()
    }
}

/// Extract the graphics name from "::pic:path\\to\\image" or "::BTN:|path".
pub fn graphic_name(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let rest = if lower.starts_with("::pic:") {
        &text[6..]
    } else if lower.starts_with("::btn:") {
        let mut rest = &text[6..];
        rest = rest.trim_start_matches('|');
        for dir in ["DOWN|", "UP|", "LEFT|", "RIGHT|"] {
            if rest.to_ascii_uppercase().starts_with(dir) {
                rest = &rest[dir.len()..];
                break;
            }
        }
        rest.trim_start_matches('|')
    } else {
        return None;
    };
    let name = rest.trim().replace('\\', "/");
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// Parse "rgb(r,g,b)", "#rrggbb" or "r,g,b" into an RGB triple.
pub fn parse_color(s: &str) -> Option<[u8; 3]> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(rest) = s.strip_prefix("rgb(").or_else(|| s.strip_prefix("rgba(")) {
        let parts: Vec<_> = rest.trim_end_matches(')').split(',').collect();
        if parts.len() >= 3 {
            let r = parts[0].trim().parse().ok()?;
            let g = parts[1].trim().parse().ok()?;
            let b = parts[2].trim().parse().ok()?;
            return Some([r, g, b]);
        }
    }
    if let Some(hex) = s.strip_prefix('#') {
        if hex.len() == 6 {
            return Some([
                u8::from_str_radix(&hex[0..2], 16).ok()?,
                u8::from_str_radix(&hex[2..4], 16).ok()?,
                u8::from_str_radix(&hex[4..6], 16).ok()?,
            ]);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_graphics() {
        assert_eq!(
            graphic_name("::pic:misc\\gauge").as_deref(),
            Some("misc/gauge")
        );
        assert_eq!(
            graphic_name("::BTN:|radio/play").as_deref(),
            Some("radio/play")
        );
    }

    #[test]
    fn parses_colors() {
        assert_eq!(parse_color("rgb(1,2,3)"), Some([1, 2, 3]));
        assert_eq!(parse_color("#0A0B0C"), Some([10, 11, 12]));
    }
}
