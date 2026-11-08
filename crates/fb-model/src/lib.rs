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
}

impl Recipe {
    /// Items (or m³) per minute, per machine at 100% clock.
    pub fn per_min(&self, a: &Amount) -> f64 {
        a.amount * 60.0 / self.duration_s
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
}

impl Building {
    /// Power draw of one machine running at `clock` (1.0 = 100%).
    pub fn power_at(&self, base_mw: f64, clock: f64) -> f64 {
        base_mw * clock.max(0.0).powf(self.power_exponent)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GameData {
    pub items: BTreeMap<ClassName, Item>,
    pub recipes: BTreeMap<ClassName, Recipe>,
    pub buildings: BTreeMap<ClassName, Building>,
    /// Default extraction limits per minute (editable in the planner).
    pub resource_limits: BTreeMap<ClassName, f64>,
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
    pub enabled_recipes: Vec<ClassName>,
    pub resource_limits: BTreeMap<ClassName, f64>,
    pub objective: Objective,
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
    pub power_mw: f64,
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
