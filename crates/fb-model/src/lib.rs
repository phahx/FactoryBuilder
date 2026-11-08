//! Types shared by the Tauri backend (native) and the egui frontend (wasm).
//! Everything crosses the IPC boundary as JSON, so it is all serde.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Item/recipe/building ids are the game's own class names, e.g.
/// `Desc_IronIngot_C`, `Recipe_IngotIron_C`, `Build_SmelterMk1_C`.
pub type ClassName = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Form {
    Solid,
    Liquid,
    Gas,
}

impl Form {
    pub fn is_fluid(self) -> bool {
        !matches!(self, Form::Solid)
    }
    /// Unit used for rates: items/min or m³/min.
    pub fn unit(self) -> &'static str {
        if self.is_fluid() {
            "m³/min"
        } else {
            "/min"
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub class: ClassName,
    pub name: String,
    pub description: String,
    pub form: Form,
    pub sink_points: i64,
    /// True for things that come out of the ground (FGResourceDescriptor).
    pub raw: bool,
    /// RGBA from mFluidColor; used to colour fluid connections.
    pub color: [u8; 4],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Amount {
    pub item: ClassName,
    /// Per craft cycle; fluids already converted to m³.
    pub amount: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recipe {
    pub class: ClassName,
    pub name: String,
    pub alternate: bool,
    pub duration_s: f64,
    pub ingredients: Vec<Amount>,
    pub products: Vec<Amount>,
    pub building: ClassName,
    /// Recipes in variable-power buildings (Particle Accelerator, Converter,
    /// Quantum Encoder) set their own power draw: average MW.
    pub variable_power_mw: Option<f64>,
    /// Power one machine generates at 100% clock speed, in MW. Only the
    /// synthesized generator recipes (see [`GENERATOR_PREFIX`]) have it.
    #[serde(default)]
    pub generates_mw: f64,
}

/// Class-name prefix of the synthesized generator recipes:
/// `Generator__<building>__<fuel>`, e.g.
/// `Generator__Build_GeneratorCoal_C__Desc_Coal_C`. They burn fuel (and
/// water) instead of making items; they never appear in recipe lists and
/// are only planned when the plan generates its own power.
pub const GENERATOR_PREFIX: &str = "Generator__";

impl Recipe {
    /// Items (or m³) per minute, per machine at 100% clock.
    pub fn per_min(&self, a: &Amount) -> f64 {
        a.amount * 60.0 / self.duration_s
    }

    /// A synthesized power generator (building + fuel), not a crafting recipe.
    pub fn is_generator(&self) -> bool {
        self.generates_mw > 0.0
    }

    /// Item whose icon stands for this recipe: the main product, or the
    /// fuel for generators (some have no product at all).
    pub fn icon_item(&self) -> Option<&str> {
        let list = if self.is_generator() { &self.ingredients } else { &self.products };
        list.first().map(|a| a.item.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Building {
    pub class: ClassName,
    pub name: String,
    pub power_mw: f64,
    pub power_exponent: f64,
    /// Descriptor class whose icon represents this building (`Desc_*_C`).
    pub icon: ClassName,
    /// Somersloop slots (production amplification); 0 = can't be boosted.
    #[serde(default)]
    pub sloop_slots: u32,
    /// Output boost per Somersloop (1.0 = +100%).
    #[serde(default)]
    pub sloop_boost: f64,
    /// Power grows by (1 + boost)^this.
    #[serde(default = "default_sloop_power_exponent")]
    pub sloop_power_exponent: f64,
}

fn default_sloop_power_exponent() -> f64 {
    2.0
}

/// Clock speed one Power Shard adds (Docs: `Desc_CrystalShard_C.mExtraPotential`).
pub const POWER_SHARD_CLOCK: f64 = 0.5;
/// A machine takes at most this many Power Shards (250% clock speed).
pub const MAX_POWER_SHARDS: u32 = 3;
pub const POWER_SHARD: &str = "Desc_CrystalShard_C";
pub const SOMERSLOOP: &str = "Desc_WAT1_C";

/// Power Shards one machine needs for `clock` (1.0 = 100%).
pub fn power_shards_for(clock: f64) -> u32 {
    if clock <= 1.0 + 1e-9 {
        0
    } else {
        (((clock - 1.0) / POWER_SHARD_CLOCK) - 1e-9).ceil().min(MAX_POWER_SHARDS as f64) as u32
    }
}

impl Building {
    /// Power draw of one machine running at `clock` (1.0 = 100%).
    pub fn power_at(&self, base_mw: f64, clock: f64) -> f64 {
        base_mw * clock.max(0.0).powf(self.power_exponent)
    }

    /// Output multiplier with `sloops` Somersloops in one machine.
    pub fn amplification(&self, sloops: u32) -> f64 {
        1.0 + sloops.min(self.sloop_slots) as f64 * self.sloop_boost
    }

    /// One machine's power at `clock` with `sloops` Somersloops:
    /// base x clock^exponent x (1 + boost)^sloop exponent.
    pub fn power_with(&self, base_mw: f64, clock: f64, sloops: u32) -> f64 {
        self.power_at(base_mw, clock) * self.amplification(sloops).powf(self.sloop_power_exponent)
    }
}

/// How a recipe node is really built: whole machines at one clock speed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MachineSetup {
    /// Whole machines to build.
    pub count: u32,
    /// Clock speed of each (1.0 = 100%).
    pub clock: f64,
    pub shards_each: u32,
    pub sloops_each: u32,
}

impl MachineSetup {
    /// `machines` (may be fractional) at `clock`: the node's throughput spread
    /// over whole machines, keeping the number of machines the user asked for.
    pub fn new(machines: f64, clock: f64, sloops_each: u32) -> Self {
        let throughput = machines.max(0.0) * clock.max(0.0);
        if throughput <= 1e-9 {
            return Self { count: 0, clock: 0.0, shards_each: 0, sloops_each: 0 };
        }
        let count = (machines - 1e-6).ceil().max(1.0);
        let clock = throughput / count;
        Self { count: count as u32, clock, shards_each: power_shards_for(clock), sloops_each }
    }
    pub fn shards(&self) -> u32 {
        self.count * self.shards_each
    }
    pub fn sloops(&self) -> u32 {
        self.count * self.sloops_each
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GameData {
    pub items: BTreeMap<ClassName, Item>,
    pub recipes: BTreeMap<ClassName, Recipe>,
    pub buildings: BTreeMap<ClassName, Building>,
    /// Default extraction limits per minute (editable in the planner).
    pub resource_limits: BTreeMap<ClassName, f64>,
    /// What unlocks recipes and buildings: HUB milestones, MAM research,
    /// alternates (hard drives) and so on.
    #[serde(default)]
    pub schematics: BTreeMap<ClassName, Schematic>,
    /// Extractors (miners, pumps, well extractors) and belt speeds.
    #[serde(default)]
    pub extraction: ExtractionData,
    /// Building class (`Build_*_C`) -> what the Build Gun costs to place one.
    #[serde(default)]
    pub build_costs: BTreeMap<ClassName, Vec<Amount>>,
}

/// Resource node purity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Purity {
    Impure,
    #[default]
    Normal,
    Pure,
}

impl Purity {
    pub const ALL: [Purity; 3] = [Purity::Impure, Purity::Normal, Purity::Pure];
    /// Output multiplier of a node. Not in the Docs; from
    /// https://satisfactory.wiki.gg/wiki/Miner ("Impure = 0.5, Normal = 1,
    /// Pure = 2") and https://satisfactory.wiki.gg/wiki/Resource_well
    /// (30 / 60 / 120 m³/min per Resource Well Extractor).
    pub fn multiplier(self) -> f64 {
        match self {
            Purity::Impure => 0.5,
            Purity::Normal => 1.0,
            Purity::Pure => 2.0,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Purity::Impure => "Impure",
            Purity::Normal => "Normal",
            Purity::Pure => "Pure",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExtractorKind {
    /// Miner, Oil Extractor: sits on a resource node; output scales with purity.
    Node,
    /// Water Extractor: sits in water; no purity.
    Water,
    /// Resource Well Extractor: on a satellite node of a pressurised well.
    Well,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Extractor {
    pub class: ClassName,
    pub name: String,
    pub kind: ExtractorKind,
    /// `mExtractCycleTime`
    pub cycle_s: f64,
    /// `mItemsPerCycle`; fluids already converted to m³.
    pub per_cycle: f64,
    /// Which resources it can take: all of the listed forms, or only
    /// `resources` when that is non-empty.
    pub forms: Vec<Form>,
    pub resources: Vec<ClassName>,
    pub power_mw: f64,
    pub power_exponent: f64,
    /// Descriptor class whose icon represents this building.
    pub icon: ClassName,
}

impl Extractor {
    /// Items (or m³) per minute at 100% clock on a node with multiplier 1.
    pub fn base_per_min(&self) -> f64 {
        self.per_cycle * 60.0 / self.cycle_s
    }
    /// Per minute on a node of `purity` at `clock` (1.0 = 100%).
    pub fn per_min(&self, purity: Purity, clock: f64) -> f64 {
        let p = if self.kind == ExtractorKind::Water { 1.0 } else { purity.multiplier() };
        self.base_per_min() * p * clock
    }
    pub fn can_extract(&self, item: &str, form: Form) -> bool {
        self.forms.contains(&form) && (self.resources.is_empty() || self.resources.iter().any(|r| r == item))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Belt {
    pub name: String,
    /// Items per minute (`mSpeed` / 2).
    pub per_min: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExtractionData {
    /// Sorted by output at 100% on a normal node, slowest first.
    pub extractors: Vec<Extractor>,
    /// Slowest first.
    pub belts: Vec<Belt>,
    /// Pipelines, slowest first (`per_min` in m³/min).
    #[serde(default)]
    pub pipes: Vec<Belt>,
}

/// What carries a wire's flow: the slowest belt (or pipe) that's fast
/// enough, or several of the fastest.
#[derive(Debug, Clone)]
pub struct Carrier<'a> {
    pub kind: &'a Belt,
    /// Parallel lines needed (1 when one is enough).
    pub lines: u32,
}

impl ExtractionData {
    pub fn carrier(&self, fluid: bool, per_min: f64) -> Option<Carrier<'_>> {
        let list = if fluid { &self.pipes } else { &self.belts };
        if per_min <= 1e-9 {
            return None;
        }
        if let Some(b) = list.iter().find(|b| b.per_min >= per_min - 1e-6) {
            return Some(Carrier { kind: b, lines: 1 });
        }
        let fastest = list.last()?;
        Some(Carrier { kind: fastest, lines: (per_min / fastest.per_min - 1e-9).ceil() as u32 })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SchematicKind {
    /// HUB milestone (`EST_Milestone`).
    Milestone,
    /// MAM research (`EST_MAM`).
    Mam,
    /// Alternate recipe from a hard drive (`EST_Alternate`).
    Alternate,
    /// Tutorial (onboarding) step.
    Tutorial,
    /// Starting recipes and the game's internal unlock bundles (`EST_Custom`).
    Custom,
    /// AWESOME Shop, customization, events and anything else.
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schematic {
    pub class: ClassName,
    pub name: String,
    pub kind: SchematicKind,
    /// HUB tier (meaningful for milestones and the custom bundles).
    pub tier: u32,
    /// Production recipes it unlocks (classes in `GameData::recipes`).
    pub recipes: Vec<ClassName>,
    /// Buildings whose build recipe it unlocks (`Build_..._C`).
    pub buildings: Vec<ClassName>,
}

impl GameData {
    pub fn item_name<'a>(&'a self, class: &'a str) -> &'a str {
        self.items.get(class).map(|i| i.name.as_str()).unwrap_or(class)
    }
    pub fn item_form(&self, class: &str) -> Form {
        self.items.get(class).map(|i| i.form).unwrap_or(Form::Solid)
    }
    /// Power of one machine running this recipe at 100%.
    pub fn recipe_power_mw(&self, recipe: &Recipe) -> f64 {
        recipe.variable_power_mw.unwrap_or_else(|| {
            self.buildings.get(&recipe.building).map(|b| b.power_mw).unwrap_or(0.0)
        })
    }
}

// ---------------------------------------------------------------- planner IO

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Objective {
    /// Weighted raw resource use (scarce resources cost more).
    #[default]
    Resources,
    Power,
    Buildings,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Target {
    pub item: ClassName,
    pub rate: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SolveRequest {
    pub targets: Vec<Target>,
    /// Items already available (e.g. from another factory), per minute.
    pub inputs: Vec<Target>,
    /// Crafting recipes the plan may use (generator recipes listed here are
    /// ignored; they come from `power`).
    pub enabled_recipes: Vec<ClassName>,
    pub resource_limits: BTreeMap<ClassName, f64>,
    pub objective: Objective,
    #[serde(default)]
    pub power: PowerSettings,
}

/// Where the factory's power comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PowerMode {
    /// Bought from the existing power grid: the plan only reports the MW.
    #[default]
    Grid,
    /// The plan builds generators, and their fuel chain, for its own MW.
    Generate,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PowerSettings {
    #[serde(default)]
    pub mode: PowerMode,
    /// Generator recipes (`Generator__...`) the plan may build in Generate mode.
    #[serde(default = "default_generators")]
    pub generators: Vec<ClassName>,
}

/// The generator offered by default: the first one with automated fuel
/// delivery in the game (Coal-Powered Generator burning Coal).
pub const DEFAULT_GENERATOR: &str = "Generator__Build_GeneratorCoal_C__Desc_Coal_C";

fn default_generators() -> Vec<ClassName> {
    vec![DEFAULT_GENERATOR.to_string()]
}

impl Default for PowerSettings {
    fn default() -> Self {
        Self { mode: PowerMode::Grid, generators: default_generators() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum PlanNodeKind {
    /// Machines running `recipe`; `machines` is in 100%-clock equivalents.
    Recipe { recipe: ClassName, machines: f64 },
    /// Raw resource extraction.
    Resource { item: ClassName, rate: f64 },
    /// Supplied from outside this factory.
    Input { item: ClassName, rate: f64 },
    /// A requested product.
    Output { item: ClassName, rate: f64 },
    /// Leftover byproduct (sink it or store it).
    Surplus { item: ClassName, rate: f64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanNode {
    pub kind: PlanNodeKind,
    /// Suggested canvas position from the layered layout.
    pub pos: [f32; 2],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanEdge {
    pub from: usize,
    pub to: usize,
    pub item: ClassName,
    pub rate: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Plan {
    pub nodes: Vec<PlanNode>,
    pub edges: Vec<PlanEdge>,
    /// Consumed by the planned machines at 100% clock speed.
    pub power_mw: f64,
    /// Produced by the planned generators (0 when power comes from the grid).
    #[serde(default)]
    pub generated_mw: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SolveOutcome {
    Solved(Plan),
    /// No plan exists; the strings say why, for the user.
    Infeasible(Vec<String>),
}

// ------------------------------------------------------------------ settings

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Settings {
    /// Satisfactory install folder (the one containing CommunityResources).
    pub game_dir: String,
    /// Folder of extracted icons (`<Class>.png` + manifest.json).
    pub icons_dir: String,
    /// Name shown under your cursor in rooms.
    #[serde(default)]
    pub user_name: String,
    /// TCP port a hosted room listens on.
    #[serde(default = "default_room_port")]
    pub room_port: u16,
    /// Recently opened or saved factory files, newest first.
    #[serde(default)]
    pub recent: Vec<String>,
    /// First-launch setup was finished in this profile.
    #[serde(default)]
    pub setup_done: bool,
    /// Fingerprint of the game files the icons were extracted from; when the
    /// game updates it no longer matches and the app offers to extract again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icons_stamp: Option<String>,
}

pub const DEFAULT_ROOM_PORT: u16 = 47860;

fn default_room_port() -> u16 {
    DEFAULT_ROOM_PORT
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Status {
    pub settings: Settings,
    pub icons_found: usize,
    /// Game font + UI icon files found in `<icons_dir>/ui`.
    #[serde(default)]
    pub ui_assets_found: usize,
    /// `<icons_dir>/map/map.json` exists (the world map was extracted).
    #[serde(default)]
    pub map_found: bool,
    pub projects_dir: String,
    /// The game changed since the icons were extracted.
    #[serde(default)]
    pub game_changed: bool,
    #[serde(default)]
    pub setup: setup::InstallInfo,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            game_dir: String::new(),
            icons_dir: String::new(),
            user_name: String::new(),
            room_port: DEFAULT_ROOM_PORT,
            recent: Vec::new(),
            setup_done: false,
            icons_stamp: None,
        }
    }
}

// --------------------------------------------------------------------- world

/// The 2D world map: the game's map art, resource nodes, and what a save has
/// built. World coordinates are Unreal centimetres (X east, Y south).
pub mod world {
    use crate::Purity;
    use serde::{Deserialize, Serialize};

    /// The game's map image and the world area it covers, plus every resource
    /// node site. Written by the icon helper into `<icons>/map/map.json`.
    #[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
    pub struct MapInfo {
        /// World X and Y (cm) of the image's top-left corner.
        pub x_min: f64,
        pub y_min: f64,
        /// World width and height (cm) the square image covers.
        pub size: f64,
        /// Pixels per side of one tile.
        pub tile_px: u32,
        /// Zoom levels: level z has 2^z x 2^z tiles (`map/<z>/<x>_<y>.png`).
        pub levels: u32,
        pub nodes: Vec<MapNode>,
    }

    impl MapInfo {
        /// 0..1 across the image for a world position.
        pub fn to_unit(&self, x: f64, y: f64) -> (f64, f64) {
            ((x - self.x_min) / self.size, (y - self.y_min) / self.size)
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub enum SiteKind {
        /// Ore/oil node for miners and oil extractors.
        Node,
        Geyser,
        /// Resource Well satellite node (Resource Well Extractor).
        WellSatellite,
        /// Resource Well Pressurizer site.
        WellCore,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
    pub struct MapNode {
        /// The level actor's name, e.g. "BP_ResourceNode100" (saves refer to
        /// nodes by this name).
        pub id: String,
        pub kind: SiteKind,
        /// Item class (`Desc_..._C`); None for geysers.
        pub resource: Option<String>,
        pub purity: Purity,
        pub x: f64,
        pub y: f64,
        pub z: f64,
    }

    /// One session's saves, newest first.
    #[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
    pub struct Session {
        pub session: String,
        pub saves: Vec<crate::saves::SaveSummary>,
    }

    /// What a save has built, reduced to what the map and the importer need.
    #[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
    pub struct World {
        pub save: crate::saves::SaveSummary,
        /// Production buildings, extractors and generators.
        pub machines: Vec<Machine>,
        /// Belts, lifts and pipes, as lines to draw.
        pub carriers: Vec<Carrier>,
        /// Item flow between machines, resolved through belts, pipes,
        /// splitters, mergers, valves and pumps.
        pub links: Vec<Link>,
        /// Other buildings worth a mark on the map (storage, splitters...).
        pub structures: Vec<Structure>,
        /// Resource nodes the save changed (1.2 game modes).
        pub node_overrides: Vec<NodeOverride>,
        /// Objects that couldn't be read fully (shown as a note, never fatal).
        pub skipped: usize,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
    pub struct Machine {
        /// Index in `World::machines` (links refer to it).
        pub id: u32,
        /// Building class, e.g. "Build_ConstructorMk1_C".
        pub class: String,
        /// Recipe class, e.g. "Recipe_Wire_C" (None for extractors/idle).
        pub recipe: Option<String>,
        /// Clock speed, 1.0 = 100%.
        pub clock: f32,
        /// Production boost multiplier from Somersloops, 1.0 = none.
        pub boost: f32,
        pub x: f32,
        pub y: f32,
        pub z: f32,
        /// Facing, degrees.
        pub yaw: f32,
        /// For extractors: the resource node they sit on (`MapNode::id`).
        pub node: Option<String>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
    pub struct Carrier {
        /// "Build_ConveyorBeltMk3_C", "Build_PipelineMK2_C", lifts...
        pub class: String,
        pub fluid: bool,
        /// World X/Y points along it (spline points, or the two ends).
        pub points: Vec<[f32; 2]>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
    pub struct Link {
        pub from: u32,
        pub to: u32,
        /// The item carried, when it can be told from the recipes.
        pub item: Option<String>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
    pub struct Structure {
        pub class: String,
        pub x: f32,
        pub y: f32,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
    pub struct NodeOverride {
        pub id: String,
        pub resource: Option<String>,
        pub purity: Option<Purity>,
    }
}

// --------------------------------------------------------------------- saves

/// Reading progress from the player's save files.
pub mod saves {
    use serde::{Deserialize, Serialize};

    /// A save file, from its header only.
    #[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
    pub struct SaveSummary {
        pub path: String,
        pub session: String,
        /// The save's own name (newer saves); otherwise the file name.
        pub name: String,
        /// Unix seconds (UTC) when it was saved.
        pub saved_unix: i64,
        pub play_seconds: i64,
        pub header_version: i32,
        pub save_version: i32,
    }

    /// The schematics bought in a save (milestones, research, alternates...).
    #[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
    pub struct SaveUnlocks {
        pub save: SaveSummary,
        pub schematics: Vec<String>,
    }
}

// --------------------------------------------------------------------- setup

/// First-launch setup: installing, finding the game, extracting icons.
pub mod setup {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
    pub struct InstallInfo {
        /// "Factory Builder", or "Factory Builder (Test)" in a test profile.
        pub product: String,
        pub version: String,
        /// This app is running from its installed folder.
        pub installed_copy: bool,
        /// The installed copy, if there is one (it may be this one).
        pub installed: Option<InstalledCopy>,
        /// Where a new install goes unless the user picks another folder.
        pub default_dir: String,
        /// The icon helper is next to the app.
        pub helper_found: bool,
        /// Development build: Developer settings are shown.
        pub dev: bool,
        /// Running in a throwaway test profile (development): its folder.
        pub test_profile: Option<String>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
    pub struct InstalledCopy {
        pub dir: String,
        pub version: String,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
    pub struct InstallReport {
        pub dir: String,
        /// What was done, in order, in plain words.
        pub done: Vec<String>,
        /// Parts that didn't work (the install still counts).
        pub problems: Vec<String>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
    pub struct GameCheck {
        pub dir: String,
        pub recipes: usize,
        pub items: usize,
        pub buildings: usize,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
    pub struct ExtractProgress {
        pub running: bool,
        /// "starting", "mount", "icons" or "ui".
        pub stage: String,
        pub done: u32,
        pub total: u32,
        /// The last run ended (successfully when `error` is None).
        pub finished: bool,
        pub error: Option<String>,
        pub icons: usize,
        pub ui: usize,
    }

    impl ExtractProgress {
        /// 0..=1 for a progress bar; mounting counts as the first 10%.
        pub fn fraction(&self) -> f32 {
            match self.stage.as_str() {
                _ if self.finished && self.error.is_none() => 1.0,
                "icons" | "ui" if self.total > 0 => 0.1 + 0.9 * (self.done as f32 / self.total as f32).min(1.0),
                "mount" => 0.05,
                _ => 0.0,
            }
        }
    }

    /// Compares "1.2.3"-style versions; missing parts count as 0.
    pub fn version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
        let parse = |v: &str| -> Vec<u64> { v.split('.').map(|p| p.trim().parse().unwrap_or(0)).collect() };
        let (mut x, mut y) = (parse(a), parse(b));
        let n = x.len().max(y.len());
        x.resize(n, 0);
        y.resize(n, 0);
        x.cmp(&y)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::cmp::Ordering::*;

        #[test]
        fn versions_compare_numerically() {
            assert_eq!(version_cmp("0.10.0", "0.9.9"), Greater);
            assert_eq!(version_cmp("1.0", "1.0.0"), Equal);
            assert_eq!(version_cmp("0.1.0", "0.2.0"), Less);
        }

        #[test]
        fn progress_fraction() {
            let mut p = ExtractProgress { stage: "icons".into(), done: 382, total: 764, ..Default::default() };
            assert!((p.fraction() - 0.55).abs() < 1e-6);
            p.finished = true;
            assert_eq!(p.fraction(), 1.0);
        }
    }
}

// --------------------------------------------------------------------- rooms
//
// A host's app runs a WebSocket server (TCP). One room code holds the host's
// local-network IPv4 address, their public IPv4 address (0.0.0.0 if unknown)
// and the port: 80 bits, written as 16 Crockford base-32 characters
// ("XXXX-XXXX-XXXX-XXXX"). A guest tries both addresses and keeps whichever
// answers, so the same code works on the host's network and over the
// internet. Messages are JSON `Wire` values.

pub mod room {
    use serde::{Deserialize, Serialize};
    use std::net::{Ipv4Addr, SocketAddrV4};

    /// Crockford base 32: no I, L, O or U, so codes are easy to read out.
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct RoomAddress {
        pub local: Ipv4Addr,
        pub public: Option<Ipv4Addr>,
        pub port: u16,
    }

    impl RoomAddress {
        /// Addresses to try, local first.
        pub fn candidates(&self) -> Vec<SocketAddrV4> {
            let mut v = vec![SocketAddrV4::new(self.local, self.port)];
            if let Some(p) = self.public {
                if p != self.local {
                    v.push(SocketAddrV4::new(p, self.port));
                }
            }
            v
        }
    }

    pub fn encode_code(addr: RoomAddress) -> String {
        let public = addr.public.map(u32::from).unwrap_or(0) as u128;
        let mut n: u128 = (u32::from(addr.local) as u128) << 48 | public << 16 | addr.port as u128;
        let mut chars = [0u8; 16];
        for c in chars.iter_mut().rev() {
            *c = ALPHABET[(n & 31) as usize];
            n >>= 5;
        }
        let s = String::from_utf8(chars.to_vec()).unwrap();
        format!("{}-{}-{}-{}", &s[0..4], &s[4..8], &s[8..12], &s[12..16])
    }

    pub fn decode_code(code: &str) -> Option<RoomAddress> {
        let mut n: u128 = 0;
        let mut count = 0;
        for ch in code.chars().filter(|c| !c.is_whitespace() && *c != '-') {
            let c = match ch.to_ascii_uppercase() {
                'O' => '0',
                'I' | 'L' => '1',
                c => c,
            };
            let v = ALPHABET.iter().position(|&a| a as char == c)? as u128;
            n = n << 5 | v;
            count += 1;
        }
        if count != 16 {
            return None;
        }
        let port = (n & 0xFFFF) as u16;
        let public = Ipv4Addr::from(((n >> 16) & 0xFFFF_FFFF) as u32);
        let local = Ipv4Addr::from(((n >> 48) & 0xFFFF_FFFF) as u32);
        if port == 0 || local.is_unspecified() {
            return None;
        }
        Some(RoomAddress { local, public: (!public.is_unspecified()).then_some(public), port })
    }

    /// Private, CGNAT (100.64/10) or other non-routable: an address the
    /// internet can't reach directly.
    pub fn is_shared_or_private(ip: Ipv4Addr) -> bool {
        let [a, b, ..] = ip.octets();
        ip.is_private() || ip.is_loopback() || ip.is_link_local() || ip.is_unspecified() || (a == 100 && (64..128).contains(&b))
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Peer {
        pub id: u32,
        pub name: String,
        pub color: [u8; 3],
    }

    /// Everything that travels between the host and guests.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(tag = "t")]
    pub enum Wire {
        /// Guest -> host, first message.
        Hello { name: String, version: u32 },
        /// Host -> new guest: who they are, who is here, the current document.
        Welcome { you: u32, peers: Vec<Peer>, doc: Option<String>, rev: u64 },
        /// Host -> guest when the host can't take them.
        Denied { reason: String },
        Joined { peer: Peer },
        Left { id: u32 },
        /// A cursor in graph coordinates (unzoomed, unpanned).
        Cursor { id: u32, x: f32, y: f32 },
        /// The shared document after an edit. Guests send it with `rev` = the
        /// revision they edited; the host stamps a new one and relays it.
        Doc { from: u32, rev: u64, json: String },
    }

    pub const PROTOCOL_VERSION: u32 = 1;

    /// What the UI sees, from `room_poll`.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub enum RoomEvent {
        Message(Wire),
        /// The connection ended (or never opened); the text says why.
        Closed(String),
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct HostInfo {
        pub code: String,
        /// This PC's address on the local network.
        pub local_ip: String,
        pub port: u16,
        /// The address the internet sees, if found.
        pub public_ip: Option<String>,
        /// Where `public_ip` came from: "router" or the service's host name.
        pub public_source: Option<String>,
        /// The router forwarded the port for us (UPnP).
        pub forwarded: bool,
        /// Why automatic forwarding didn't happen, in plain words.
        pub forward_problem: Option<String>,
        /// The router's own internet address is a shared/private one (for
        /// example carrier-grade NAT), so forwarding on it may not be enough.
        pub shared_address: bool,
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn codes_round_trip() {
            let cases = [
                ("192.168.1.23", Some("86.12.200.7"), 47860),
                ("10.0.0.5", None, 1),
                ("255.255.255.255", Some("255.255.255.255"), 65535),
            ];
            for (local, public, port) in cases {
                let a = RoomAddress { local: local.parse().unwrap(), public: public.map(|p| p.parse().unwrap()), port };
                let code = encode_code(a);
                assert_eq!(code.len(), 19, "{code}");
                assert_eq!(decode_code(&code), Some(a), "{code}");
                assert_eq!(decode_code(&code.to_lowercase().replace('-', " ")), Some(a));
            }
        }

        #[test]
        fn candidates_try_local_then_public() {
            let a = RoomAddress { local: "192.168.1.23".parse().unwrap(), public: Some("86.12.200.7".parse().unwrap()), port: 47860 };
            assert_eq!(a.candidates(), vec!["192.168.1.23:47860".parse().unwrap(), "86.12.200.7:47860".parse().unwrap()]);
        }

        #[test]
        fn shared_addresses_are_spotted() {
            for ip in ["192.168.0.1", "10.1.2.3", "172.16.5.5", "100.64.0.1", "100.127.255.254"] {
                assert!(is_shared_or_private(ip.parse().unwrap()), "{ip}");
            }
            for ip in ["86.12.200.7", "100.128.0.1", "8.8.8.8"] {
                assert!(!is_shared_or_private(ip.parse().unwrap()), "{ip}");
            }
        }

        #[test]
        fn bad_codes_are_rejected() {
            assert_eq!(decode_code("ABC"), None);
            assert_eq!(decode_code("UUUU-UUUU-UUUU-UUUU"), None);
            assert_eq!(decode_code("0000-0000-0000-0000"), None, "no address, port 0");
        }
    }
}

#[cfg(test)]
mod machine_tests {
    use super::*;

    #[test]
    fn shards_follow_clock_speed() {
        assert_eq!(power_shards_for(1.0), 0);
        assert_eq!(power_shards_for(1.0001), 1);
        assert_eq!(power_shards_for(1.5), 1);
        assert_eq!(power_shards_for(1.75), 2);
        assert_eq!(power_shards_for(2.5), 3);
    }

    #[test]
    fn whole_machines_keep_the_users_count() {
        let m = MachineSetup::new(1.0, 2.5, 0);
        assert_eq!((m.count, m.shards()), (1, 3), "one overclocked machine");
        let m = MachineSetup::new(2.5, 1.0, 1);
        assert_eq!(m.count, 3);
        assert!((m.clock - 2.5 / 3.0).abs() < 1e-12);
        assert_eq!((m.shards(), m.sloops()), (0, 3));
        assert_eq!(MachineSetup::new(0.0, 1.0, 2).count, 0);
    }

    #[test]
    fn somersloops_double_output_and_quadruple_power() {
        let b = Building { class: "Build_ManufacturerMk1_C".into(), name: "Manufacturer".into(), power_mw: 55.0, power_exponent: 1.321929, icon: String::new(), sloop_slots: 4, sloop_boost: 0.25, sloop_power_exponent: 2.0 };
        assert_eq!(b.amplification(0), 1.0);
        assert_eq!(b.amplification(2), 1.5);
        assert_eq!(b.amplification(9), 2.0, "capped at the slots");
        assert!((b.power_with(55.0, 1.0, 4) - 220.0).abs() < 1e-9);
        assert!((b.power_with(55.0, 1.0, 2) - 55.0 * 2.25).abs() < 1e-9);
    }
}
