//! The factory file format: `.fbfactory` files, "Export factory JSON" and
//! share codes all use it.
//!
//! It's plain, versioned JSON that doesn't depend on the editor's internals:
//! nodes are listed in order and wires refer to them by index, item ports by
//! their position on the node (item ports only), plus the bubble (`slot`) the
//! wire uses on its output. Recipes and items use the game's class names.
//!
//! ```json
//! { "format": "factory-builder", "version": 1, "name": "Steel",
//!   "plan": { "products": [{"item": "Desc_SteelPlate_C", "rate": 20}], ... },
//!   "nodes": [{"kind": "recipe", "class": "Recipe_SteelPlate_C", "machines": 2, "clock": 100, "x": 40, "y": 40}],
//!   "wires": [{"from": 0, "output": 0, "to": 1, "input": 0, "slot": 0}] }
//! ```
//!
//! A share code is that JSON, deflate-compressed and base64url-encoded,
//! after "FB1-".

use base64::Engine;
use fb_model::{Objective, Target};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const FORMAT: &str = "factory-builder";
pub const VERSION: u32 = 1;
pub const SHARE_PREFIX: &str = "FB1-";
const MAX_NODES: usize = 10_000;
const MAX_WIRES: usize = 100_000;
const MAX_JSON: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactoryFile {
    pub format: String,
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub plan: Plan,
    #[serde(default)]
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub wires: Vec<Wire>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Plan {
    #[serde(default)]
    pub products: Vec<Target>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub available: Vec<Target>,
    #[serde(default)]
    pub optimize: Objective,
    /// Alternate recipes turned on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternates_on: Vec<String>,
    /// Shorter form when most alternates are on: all of them except these.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub all_alternates: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternates_off: Vec<String>,
    /// Standard recipes turned off.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub standard_off: Vec<String>,
    /// Resource limits that differ from the whole-map defaults.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub limits: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Recipe,
    Resource,
    Input,
    Output,
    Sink,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub kind: Kind,
    /// Recipe class for machines, item class for the rest.
    pub class: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machines: Option<f64>,
    /// Clock speed in percent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock: Option<f64>,
    /// Per minute, for resources, inputs and outputs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate: Option<f64>,
    /// Header colour label, "#RRGGBB".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Wire {
    pub from: usize,
    pub output: usize,
    pub to: usize,
    pub input: usize,
    /// Which bubble on the output the wire leaves from.
    #[serde(default)]
    pub slot: usize,
}

pub fn color_to_hex([r, g, b]: [u8; 3]) -> String {
    format!("#{r:02X}{g:02X}{b:02X}")
}

pub fn color_from_hex(s: &str) -> Option<[u8; 3]> {
    let h = s.strip_prefix('#')?;
    if h.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

impl FactoryFile {
    pub fn new(name: Option<String>) -> Self {
        Self { format: FORMAT.into(), version: VERSION, name, plan: Plan::default(), nodes: Vec::new(), wires: Vec::new() }
    }

    pub fn to_json(&self, pretty: bool) -> String {
        if pretty {
            serde_json::to_string_pretty(self).unwrap_or_default()
        } else {
            serde_json::to_string(self).unwrap_or_default()
        }
    }

    /// Reads and sanity-checks a factory file.
    pub fn parse(json: &str) -> Result<FactoryFile, String> {
        if json.len() > MAX_JSON {
            return Err("That's too big to be a factory.".into());
        }
        let f: FactoryFile = serde_json::from_str(json).map_err(|_| "That isn't a Factory Builder factory.".to_string())?;
        if f.format != FORMAT {
            return Err("That isn't a Factory Builder factory.".into());
        }
        if f.version > VERSION {
            return Err("That factory was made by a newer Factory Builder. Update to open it.".into());
        }
        if f.nodes.len() > MAX_NODES || f.wires.len() > MAX_WIRES {
            return Err("That factory is too big to open.".into());
        }
        if f.nodes.iter().any(|n| !n.x.is_finite() || !n.y.is_finite()) {
            return Err("That factory has broken positions.".into());
        }
        Ok(f)
    }

    pub fn to_share_code(&self) -> String {
        let packed = miniz_oxide::deflate::compress_to_vec(self.to_json(false).as_bytes(), 9);
        format!("{SHARE_PREFIX}{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(packed))
    }

    pub fn from_share_code(code: &str) -> Result<FactoryFile, String> {
        let clean: String = code.chars().filter(|c| !c.is_whitespace()).collect();
        let data = clean.strip_prefix(SHARE_PREFIX).ok_or("That isn't a Factory Builder share code. Share codes start with FB1-.")?;
        let packed = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(data)
            .map_err(|_| "That share code is damaged (it may have been cut off when copying).".to_string())?;
        let json = miniz_oxide::inflate::decompress_to_vec_with_limit(&packed, MAX_JSON)
            .map_err(|_| "That share code is damaged (it may have been cut off when copying).".to_string())?;
        let json = String::from_utf8(json).map_err(|_| "That share code is damaged.".to_string())?;
        FactoryFile::parse(&json)
    }

    /// Checks indices before anything is built from the file.
    pub fn valid_wires(&self) -> impl Iterator<Item = &Wire> {
        let n = self.nodes.len();
        self.wires.iter().filter(move |w| w.from < n && w.to < n && w.from != w.to)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> FactoryFile {
        let mut f = FactoryFile::new(Some("Steel".into()));
        f.plan.products.push(Target { item: "Desc_SteelPlate_C".into(), rate: 20.0 });
        f.nodes.push(Node { kind: Kind::Resource, class: "Desc_OreIron_C".into(), machines: None, clock: None, rate: Some(60.0), color: None, x: 0.0, y: 0.0 });
        f.nodes.push(Node { kind: Kind::Recipe, class: "Recipe_IngotIron_C".into(), machines: Some(2.0), clock: Some(100.0), rate: None, color: Some("#FA9549".into()), x: 380.0, y: 0.0 });
        f.wires.push(Wire { from: 0, output: 0, to: 1, input: 0, slot: 0 });
        f
    }

    #[test]
    fn json_round_trip() {
        let f = sample();
        assert_eq!(FactoryFile::parse(&f.to_json(true)).unwrap(), f);
        assert_eq!(FactoryFile::parse(&f.to_json(false)).unwrap(), f);
    }

    #[test]
    fn share_codes_round_trip_and_survive_line_breaks() {
        let f = sample();
        let code = f.to_share_code();
        assert!(code.starts_with("FB1-"));
        let wrapped: String = code.chars().enumerate().flat_map(|(i, c)| if i % 40 == 39 { vec![c, '\n'] } else { vec![c] }).collect();
        assert_eq!(FactoryFile::from_share_code(&wrapped).unwrap(), f);
    }

    #[test]
    fn bad_input_gets_a_plain_message() {
        assert!(FactoryFile::from_share_code("hello").unwrap_err().contains("FB1-"));
        let cut = &sample().to_share_code()[..20];
        assert!(FactoryFile::from_share_code(cut).unwrap_err().contains("damaged"));
        assert!(FactoryFile::parse("{\"format\":\"other\",\"version\":1}").is_err());
        assert!(FactoryFile::parse("{\"format\":\"factory-builder\",\"version\":99}").unwrap_err().contains("newer"));
    }

    #[test]
    fn colors_round_trip() {
        assert_eq!(color_from_hex(&color_to_hex([0xFA, 0x95, 0x49])), Some([0xFA, 0x95, 0x49]));
        assert_eq!(color_from_hex("#12"), None);
    }
}
