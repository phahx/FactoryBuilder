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
//!
//! A project (`.fbproject`, share codes after "FBP1-") holds several
//! factories in that same form, where each sits on the master graph, and the
//! links between them (one factory's output item feeding another's input):
//!
//! ```json
//! { "format": "factory-builder-project", "version": 1, "name": "Main base",
//!   "factories": [{"id": 1, "x": 0, "y": 0, "factory": { "format": "factory-builder", ... }}],
//!   "links": [{"from": 1, "to": 2, "item": "Desc_IronIngot_C", "slot": 0}] }
//! ```

use base64::Engine;
use fb_model::{Objective, Target};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const FORMAT: &str = "factory-builder";
pub const VERSION: u32 = 1;
pub const SHARE_PREFIX: &str = "FB1-";
pub const PROJECT_FORMAT: &str = "factory-builder-project";
pub const PROJECT_VERSION: u32 = 1;
pub const PROJECT_SHARE_PREFIX: &str = "FBP1-";
const MAX_FACTORIES: usize = 1_000;
const MAX_LINKS: usize = 100_000;
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
    /// The plan builds its own generators instead of using the grid.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub generate_power: bool,
    /// Generators allowed, when they differ from the default (Coal-Powered
    /// Generator burning Coal).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generators: Option<Vec<String>>,
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
    /// Somersloops per machine, when any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sloops: Option<u32>,
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
        f.check()?;
        Ok(f)
    }

    fn check(&self) -> Result<(), String> {
        if self.format != FORMAT {
            return Err("That isn't a Factory Builder factory.".into());
        }
        if self.version > VERSION {
            return Err("That factory was made by a newer Factory Builder. Update to open it.".into());
        }
        if self.nodes.len() > MAX_NODES || self.wires.len() > MAX_WIRES {
            return Err("That factory is too big to open.".into());
        }
        if self.nodes.iter().any(|n| !n.x.is_finite() || !n.y.is_finite()) {
            return Err("That factory has broken positions.".into());
        }
        Ok(())
    }

    pub fn to_share_code(&self) -> String {
        pack(SHARE_PREFIX, &self.to_json(false))
    }

    pub fn from_share_code(code: &str) -> Result<FactoryFile, String> {
        match Shared::from_share_code(code)? {
            Shared::Factory(f) => Ok(f),
            Shared::Project(_) => Err("That's the share code of a whole project, not one factory. Open it with File › Open share code.".into()),
        }
    }

    /// Checks indices before anything is built from the file.
    pub fn valid_wires(&self) -> impl Iterator<Item = &Wire> {
        let n = self.nodes.len();
        self.wires.iter().filter(move |w| w.from < n && w.to < n && w.from != w.to)
    }
}

fn pack(prefix: &str, json: &str) -> String {
    let packed = miniz_oxide::deflate::compress_to_vec(json.as_bytes(), 9);
    format!("{prefix}{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(packed))
}

fn unpack(data: &str) -> Result<String, String> {
    let damaged = || "That share code is damaged (it may have been cut off when copying).".to_string();
    let packed = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(data).map_err(|_| damaged())?;
    let json = miniz_oxide::inflate::decompress_to_vec_with_limit(&packed, MAX_JSON).map_err(|_| damaged())?;
    String::from_utf8(json).map_err(|_| "That share code is damaged.".to_string())
}

/// A project: several factories, where they sit on the master graph, and
/// the links between them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectFile {
    pub format: String,
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub factories: Vec<ProjectFactory>,
    #[serde(default)]
    pub links: Vec<Link>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectFactory {
    /// Links refer to factories by this.
    pub id: u32,
    /// Its block on the master graph.
    pub x: f32,
    pub y: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// The factory itself (its name is the factory's name).
    pub factory: FactoryFile,
}

/// One factory's output item feeding another factory's input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Link {
    pub from: u32,
    pub to: u32,
    pub item: String,
    /// Which bubble on the output the link leaves from.
    #[serde(default)]
    pub slot: usize,
}

impl ProjectFile {
    pub fn new(name: Option<String>) -> Self {
        Self { format: PROJECT_FORMAT.into(), version: PROJECT_VERSION, name, factories: Vec::new(), links: Vec::new() }
    }

    pub fn to_json(&self, pretty: bool) -> String {
        if pretty {
            serde_json::to_string_pretty(self).unwrap_or_default()
        } else {
            serde_json::to_string(self).unwrap_or_default()
        }
    }

    pub fn to_share_code(&self) -> String {
        pack(PROJECT_SHARE_PREFIX, &self.to_json(false))
    }

    pub fn parse(json: &str) -> Result<ProjectFile, String> {
        if json.len() > MAX_JSON {
            return Err("That's too big to be a project.".into());
        }
        let p: ProjectFile = serde_json::from_str(json).map_err(|_| "That isn't a Factory Builder project.".to_string())?;
        if p.format != PROJECT_FORMAT {
            return Err("That isn't a Factory Builder project.".into());
        }
        if p.version > PROJECT_VERSION {
            return Err("That project was made by a newer Factory Builder. Update to open it.".into());
        }
        if p.factories.len() > MAX_FACTORIES || p.links.len() > MAX_LINKS {
            return Err("That project is too big to open.".into());
        }
        let nodes: usize = p.factories.iter().map(|f| f.factory.nodes.len()).sum();
        if nodes > MAX_NODES * 10 {
            return Err("That project is too big to open.".into());
        }
        let mut ids = std::collections::BTreeSet::new();
        for f in &p.factories {
            if !ids.insert(f.id) {
                return Err("That project lists a factory twice.".into());
            }
            if !f.x.is_finite() || !f.y.is_finite() {
                return Err("That project has broken positions.".into());
            }
            f.factory.check()?;
        }
        Ok(p)
    }

    /// Links between factories that exist (never a factory to itself).
    pub fn valid_links(&self) -> impl Iterator<Item = &Link> {
        let ids: std::collections::BTreeSet<u32> = self.factories.iter().map(|f| f.id).collect();
        self.links.iter().filter(move |l| l.from != l.to && ids.contains(&l.from) && ids.contains(&l.to))
    }
}

/// What a file or share code holds.
#[derive(Debug, Clone, PartialEq)]
pub enum Shared {
    Factory(FactoryFile),
    Project(ProjectFile),
}

impl Shared {
    /// A factory or a project file's text.
    pub fn parse(json: &str) -> Result<Shared, String> {
        let format = serde_json::from_str::<serde_json::Value>(json).ok().and_then(|v| v.get("format").and_then(|f| f.as_str()).map(str::to_string));
        match format.as_deref() {
            Some(PROJECT_FORMAT) => ProjectFile::parse(json).map(Shared::Project),
            _ => FactoryFile::parse(json).map(Shared::Factory),
        }
    }

    pub fn from_share_code(code: &str) -> Result<Shared, String> {
        let clean: String = code.chars().filter(|c| !c.is_whitespace()).collect();
        if let Some(data) = clean.strip_prefix(PROJECT_SHARE_PREFIX) {
            return ProjectFile::parse(&unpack(data)?).map(Shared::Project);
        }
        let data = clean.strip_prefix(SHARE_PREFIX).ok_or("That isn't a Factory Builder share code. Share codes start with FB1- (a factory) or FBP1- (a project).")?;
        FactoryFile::parse(&unpack(data)?).map(Shared::Factory)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> FactoryFile {
        let mut f = FactoryFile::new(Some("Steel".into()));
        f.plan.products.push(Target { item: "Desc_SteelPlate_C".into(), rate: 20.0 });
        f.nodes.push(Node { kind: Kind::Resource, class: "Desc_OreIron_C".into(), machines: None, clock: None, sloops: None, rate: Some(60.0), color: None, x: 0.0, y: 0.0 });
        f.nodes.push(Node { kind: Kind::Recipe, class: "Recipe_IngotIron_C".into(), machines: Some(2.0), clock: Some(100.0), sloops: Some(1), rate: None, color: Some("#FA9549".into()), x: 380.0, y: 0.0 });
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
    fn projects_round_trip_and_are_checked() {
        let mut p = ProjectFile::new(Some("Base".into()));
        p.factories.push(ProjectFactory { id: 1, x: 0.0, y: 0.0, color: None, factory: sample() });
        p.factories.push(ProjectFactory { id: 4, x: 500.0, y: 0.0, color: Some("#FA9549".into()), factory: FactoryFile::new(Some("Rods".into())) });
        p.links.push(Link { from: 1, to: 4, item: "Desc_IronIngot_C".into(), slot: 0 });
        p.links.push(Link { from: 1, to: 9, item: "Desc_IronIngot_C".into(), slot: 0 });
        assert_eq!(ProjectFile::parse(&p.to_json(true)).unwrap(), p);
        assert_eq!(Shared::parse(&p.to_json(false)).unwrap(), Shared::Project(p.clone()));
        assert_eq!(Shared::from_share_code(&p.to_share_code()).unwrap(), Shared::Project(p.clone()));
        assert_eq!(p.valid_links().count(), 1, "links to missing factories are dropped");
        assert!(FactoryFile::from_share_code(&p.to_share_code()).unwrap_err().contains("project"));
        assert_eq!(Shared::parse(&sample().to_json(false)).unwrap(), Shared::Factory(sample()));
        let mut twice = p.clone();
        twice.factories[1].id = 1;
        assert!(ProjectFile::parse(&twice.to_json(false)).unwrap_err().contains("twice"));
        let mut newer = p.clone();
        newer.version = 99;
        assert!(Shared::parse(&newer.to_json(false)).unwrap_err().contains("newer"));
    }

    #[test]
    fn colors_round_trip() {
        assert_eq!(color_from_hex(&color_to_hex([0xFA, 0x95, 0x49])), Some([0xFA, 0x95, 0x49]));
        assert_eq!(color_from_hex("#12"), None);
    }
}
