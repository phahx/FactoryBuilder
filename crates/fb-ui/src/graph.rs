//! egui_node_graph2 types for factory graphs.
//!
//! Ports are typed by item class, so a wire can only join matching items.
//! Machine nodes have one input per ingredient and one output per product,
//! plus "Machines" and "Clock Speed" numbers. Resource/Input nodes supply one
//! item; Output/Sink nodes take one.

use crate::assets::{Assets, UiIcon};
use crate::fmt::{fmt_num, fmt_rate_unit};
use crate::theme::{self, *};
use crate::widgets;
use egui::{vec2, Align, Color32, Layout, RichText, Ui};
use egui_node_graph2::*;
use fb_model::{GameData, Recipe};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::HashMap;
use std::rc::Rc;

pub const MACHINES: &str = "Machines";
pub const CLOCK: &str = "Clock Speed";
/// Somersloops per machine (only on machines that take them).
pub const SLOOPS: &str = "Somersloops";
pub const RATE: &str = "Rate";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemType(pub String);

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub enum PortValue {
    #[default]
    None,
    /// An item port: wires only.
    Item(String),
    /// A power port on the master graph (power_net.rs): wires only.
    Power,
    /// An editable number.
    Number {
        value: f64,
        min: f64,
        max: f64,
        suffix: String,
    },
}

impl PortValue {
    pub fn number(&self) -> f64 {
        match self {
            PortValue::Number { value, .. } => *value,
            _ => 0.0,
        }
    }
    pub fn set_number(&mut self, v: f64) {
        if let PortValue::Number {
            value, min, max, ..
        } = self
        {
            *value = v.clamp(*min, *max);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum NodeKind {
    Recipe {
        recipe: String,
    },
    /// A raw resource: the node(s) its extractors stand on, of `purity`.
    Resource {
        item: String,
        #[serde(default)]
        purity: fb_model::Purity,
    },
    /// Supplied from another factory.
    Input {
        item: String,
    },
    /// A product this factory delivers.
    Output {
        item: String,
    },
    /// AWESOME Sink / storage: takes whatever arrives.
    Sink {
        item: String,
    },
    /// A whole factory on the project's master graph: what it takes in and
    /// sends out per minute (item class, rate), copied from the factory.
    Factory {
        id: u32,
        #[serde(default)]
        ins: Vec<(String, f64)>,
        #[serde(default)]
        outs: Vec<(String, f64)>,
        /// MW its machines use and its generators make.
        #[serde(default)]
        uses_mw: f64,
        #[serde(default)]
        makes_mw: f64,
    },
    /// A power pole on the master graph (power_net.rs).
    Pole {
        id: u32,
    },
    /// Joins wires of one item (routers.rs); a Pipeline Junction for fluids.
    Merger {
        item: String,
    },
    /// Shares one wire of an item out over several (routers.rs).
    Splitter {
        item: String,
    },
    /// An extractor building on a resource (extractors.rs): Miner, Water
    /// Extractor, Oil Extractor or Resource Well Extractor.
    Extractor {
        item: String,
        class: String,
        #[serde(default)]
        purity: fb_model::Purity,
    },
    /// A Pipeline Pump on a pipe (routers.rs).
    Pump {
        item: String,
        class: String,
    },
}

impl NodeKind {
    pub fn template(&self) -> Template {
        match self.clone() {
            NodeKind::Recipe { recipe } => Template::Recipe(recipe),
            NodeKind::Resource { item, .. } => Template::Resource(item),
            NodeKind::Input { item } => Template::Input(item),
            NodeKind::Output { item } => Template::Output(item),
            NodeKind::Sink { item } => Template::Sink(item),
            NodeKind::Factory { id, .. } => Template::Factory(id),
            NodeKind::Pole { id } => Template::Pole(id),
            NodeKind::Merger { item } => Template::Merger(item),
            NodeKind::Splitter { item } => Template::Splitter(item),
            NodeKind::Extractor { item, class, purity } => Template::Extractor { item, class, purity },
            NodeKind::Pump { item, class } => Template::Pump { item, class },
        }
    }

    /// Item shown in the title bar.
    pub fn icon_item<'a>(&'a self, game: &'a GameData) -> Option<&'a str> {
        match self {
            NodeKind::Recipe { recipe } => game.recipes.get(recipe).and_then(|r| r.icon_item()),
            NodeKind::Resource { item, .. }
            | NodeKind::Input { item }
            | NodeKind::Output { item }
            | NodeKind::Sink { item } => Some(item),
            // A factory shows its first product.
            NodeKind::Factory { outs, .. } => outs.first().map(|(item, _)| item.as_str()),
            NodeKind::Pole { .. } | NodeKind::Merger { .. } | NodeKind::Splitter { .. } | NodeKind::Pump { .. } => None,
            // The building's icon.
            NodeKind::Extractor { class, .. } => crate::extractors::find(game, class).map(|e| e.icon.as_str()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeData {
    pub kind: NodeKind,
    /// Colour label shown as a strip across the top of the header.
    #[serde(default)]
    pub color: Option<[u8; 3]>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Template {
    Recipe(String),
    Resource(String),
    Input(String),
    Output(String),
    Sink(String),
    /// A factory block on the master graph.
    Factory(u32),
    /// A power pole on the master graph.
    Pole(u32),
    Merger(String),
    Splitter(String),
    Extractor {
        item: String,
        class: String,
        purity: fb_model::Purity,
    },
    Pump {
        item: String,
        class: String,
    },
}

#[derive(Debug, Clone)]
pub enum Resp {
    /// The user committed a number on a node.
    Edited,
    /// A resource node's purity was changed.
    Purity(NodeId, fb_model::Purity),
}
impl UserResponseTrait for Resp {}

#[derive(Debug, Clone, Copy, Default)]
pub struct PortStat {
    /// Actual flow per minute.
    pub have: f64,
    /// Wanted (inputs) or possible (outputs) per minute.
    pub want: f64,
    pub connected: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NodeStat {
    pub efficiency: f64,
    pub power_mw: f64,
}

/// Shared state the graph widgets read: game data, assets, live stats.
#[derive(Default, Serialize, Deserialize)]
pub struct UserState {
    #[serde(skip)]
    pub game: Rc<GameData>,
    #[serde(skip)]
    pub assets: Option<Assets>,
    #[serde(skip)]
    pub ports: HashMap<(NodeId, String), PortStat>,
    #[serde(skip)]
    pub nodes: HashMap<NodeId, NodeStat>,
    #[serde(skip)]
    pub zoom: f32,
    /// The project's factories (for the blocks on the master graph).
    #[serde(skip)]
    pub factories: HashMap<u32, crate::project::FactoryInfo>,
    /// The master graph is the one being drawn.
    #[serde(skip)]
    pub master: bool,
    /// Power wires drawn bright (from node, to node): the ones about what's
    /// selected or under the pointer (power_net::lit_wires).
    #[serde(skip)]
    pub power_lit: std::collections::HashSet<(NodeId, NodeId)>,
    /// Power pole tiers (pole id -> 1 to 3), from the master graph's plan.
    #[serde(skip)]
    pub pole_tiers: HashMap<u32, u8>,
    /// Resource nodes with extractors on them: what those extract, and what
    /// they could.
    #[serde(skip)]
    pub ground: HashMap<NodeId, f64>,
    #[serde(skip)]
    pub ground_cap: HashMap<NodeId, f64>,
    /// Resource nodes wired straight into machines (no extractor).
    #[serde(skip)]
    pub direct_resources: std::collections::HashSet<NodeId>,
    /// Output nodes: what arrives.
    #[serde(skip)]
    pub delivered: HashMap<NodeId, f64>,
    /// Input nodes no other factory (enough of) brings in: what arrives of
    /// what's wanted, when it's linked at all.
    #[serde(skip)]
    pub unsupplied: HashMap<NodeId, Option<(f64, f64)>>,
}

impl UserState {
    fn z(&self) -> f32 {
        self.zoom.max(0.2)
    }

    pub(crate) fn item_icon(&mut self, ui: &mut Ui, class: &str, size: f32) {
        let size = size * self.z();
        if let Some(a) = self.assets.as_mut() {
            a.item(ui, class, size);
        }
    }

    pub(crate) fn ui_icon(&mut self, ui: &mut Ui, icon: UiIcon, size: f32, tint: Color32) {
        let size = size * self.z();
        if let Some(a) = self.assets.as_mut() {
            a.ui(ui, icon, size, tint);
        }
    }

    pub fn item_color(&self, item: &str) -> Color32 {
        if let Some(it) = self.game.items.get(item) {
            if it.form.is_fluid() && it.color[3] > 0 {
                let [r, g, b, _] = it.color;
                return Color32::from_rgb(r, g, b);
            }
        }
        // Solids: a stable muted hue per item so wires can be told apart.
        let h = item
            .bytes()
            .fold(2166136261u32, |h, b| (h ^ b as u32).wrapping_mul(16777619));
        egui::ecolor::Hsva::new((h % 360) as f32 / 360.0, 0.45, 0.8, 1.0).into()
    }
}

pub type FactoryGraph = Graph<NodeData, ItemType, PortValue>;
pub type EditorState = GraphEditorState<NodeData, ItemType, PortValue, Template, UserState>;

impl DataTypeTrait<UserState> for ItemType {
    fn data_type_color(&self, user_state: &mut UserState) -> Color32 {
        if self.0.is_empty() {
            DIVIDER
        } else if crate::power_net::is_power(&self.0) {
            crate::power_net::WIRE
        } else {
            user_state.item_color(&self.0)
        }
    }
    fn name(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.0)
    }

    fn wire_color(&self, user_state: &mut UserState, from: NodeId, to: NodeId) -> Color32 {
        if crate::power_net::is_power(&self.0) {
            crate::power_net::wire_color(user_state.power_lit.contains(&(from, to)))
        } else {
            self.data_type_color(user_state)
        }
    }
}

fn number(value: f64, min: f64, max: f64, suffix: &str) -> PortValue {
    PortValue::Number {
        value,
        min,
        max,
        suffix: suffix.into(),
    }
}

/// Port names are the item's display name; recipes never list the same item
/// twice on one side, but fall back to the class name if one ever does.
fn port_name(game: &GameData, item: &str, taken: &[(String, impl Copy)]) -> String {
    let name = game.item_name(item).to_string();
    if taken.iter().any(|(n, _)| *n == name) {
        item.to_string()
    } else {
        name
    }
}

impl Template {
    pub fn kind(&self) -> NodeKind {
        match self.clone() {
            Template::Recipe(recipe) => NodeKind::Recipe { recipe },
            Template::Resource(item) => NodeKind::Resource { item, purity: fb_model::Purity::Normal },
            Template::Input(item) => NodeKind::Input { item },
            Template::Output(item) => NodeKind::Output { item },
            Template::Sink(item) => NodeKind::Sink { item },
            Template::Factory(id) => NodeKind::Factory { id, ins: Vec::new(), outs: Vec::new(), uses_mw: 0.0, makes_mw: 0.0 },
            Template::Pole(id) => NodeKind::Pole { id },
            Template::Merger(item) => NodeKind::Merger { item },
            Template::Splitter(item) => NodeKind::Splitter { item },
            Template::Extractor { item, class, purity } => NodeKind::Extractor { item, class, purity },
            Template::Pump { item, class } => NodeKind::Pump { item, class },
        }
    }
}

impl NodeTemplateTrait for Template {
    type NodeData = NodeData;
    type DataType = ItemType;
    type ValueType = PortValue;
    type UserState = UserState;
    type CategoryType = String;

    fn node_finder_label(&self, s: &mut UserState) -> Cow<'_, str> {
        Cow::Owned(self.node_graph_label(s))
    }

    fn node_finder_categories(&self, s: &mut UserState) -> Vec<String> {
        vec![match self {
            Template::Recipe(r) => s
                .game
                .recipes
                .get(r)
                .and_then(|r| s.game.buildings.get(&r.building))
                .map(|b| b.name.clone())
                .unwrap_or_else(|| "Machines".into()),
            Template::Resource(_) => "Resources".into(),
            Template::Input(_) => "Inputs".into(),
            Template::Output(_) => "Outputs".into(),
            Template::Sink(_) => "Sinks".into(),
            Template::Factory(_) => "Factories".into(),
            Template::Pole(_) => "Power".into(),
            Template::Merger(_) | Template::Splitter(_) | Template::Pump { .. } => "Logistics".into(),
            Template::Extractor { .. } => "Extractors".into(),
        }]
    }

    fn node_graph_label(&self, s: &mut UserState) -> String {
        match self {
            Template::Recipe(r) => s
                .game
                .recipes
                .get(r)
                .map(|r| r.name.clone())
                .unwrap_or_else(|| r.clone()),
            Template::Resource(i)
            | Template::Input(i)
            | Template::Output(i)
            | Template::Sink(i) => s.game.item_name(i).to_string(),
            Template::Factory(id) => s.factories.get(id).map(|f| f.name.clone()).unwrap_or_else(|| "Factory".into()),
            Template::Pole(_) => "Power pole".into(),
            Template::Merger(_) | Template::Splitter(_) | Template::Pump { .. } => crate::routers::name(&s.game, &self.kind()).into(),
            Template::Extractor { class, .. } => crate::extractors::find(&s.game, class).map(|e| e.name.clone()).unwrap_or_else(|| class.clone()),
        }
    }

    fn user_data(&self, s: &mut UserState) -> NodeData {
        let kind = match self {
            Template::Factory(id) => {
                let f = s.factories.get(id).cloned().unwrap_or_default();
                NodeKind::Factory { id: *id, ins: f.ins, outs: f.outs, uses_mw: f.power_mw, makes_mw: f.generation.unwrap_or(0.0) }
            }
            t => t.kind(),
        };
        NodeData { kind, color: None }
    }

    fn build_node(&self, graph: &mut FactoryGraph, s: &mut UserState, node_id: NodeId) {
        let game = s.game.clone();
        let constant = |graph: &mut FactoryGraph, name: &str, v: PortValue| {
            graph.add_input_param(
                node_id,
                name.into(),
                ItemType(String::new()),
                v,
                InputParamKind::ConstantOnly,
                true,
            );
        };
        let item_in = |graph: &mut FactoryGraph, item: &str| {
            add_item_input(&game, graph, node_id, item);
        };
        let item_out = |graph: &mut FactoryGraph, item: &str| {
            add_item_output(&game, graph, node_id, item);
        };
        let unit = |item: &str| {
            if game.item_form(item).is_fluid() {
                "m³/min"
            } else {
                "per min"
            }
        };
        match self {
            Template::Recipe(r) => {
                let Some(recipe) = game.recipes.get(r) else {
                    return;
                };
                constant(graph, MACHINES, number(1.0, 0.0, 10_000.0, ""));
                constant(graph, CLOCK, number(100.0, 1.0, 250.0, "%"));
                if let Some(slots) = game.buildings.get(&recipe.building).map(|b| b.sloop_slots).filter(|s| *s > 0) {
                    constant(graph, SLOOPS, number(0.0, 0.0, slots as f64, ""));
                }
                for a in &recipe.ingredients {
                    item_in(graph, &a.item);
                }
                for a in &recipe.products {
                    item_out(graph, &a.item);
                }
            }
            Template::Resource(i) | Template::Input(i) => {
                constant(graph, RATE, number(60.0, 0.0, 1e6, unit(i)));
                item_out(graph, i);
            }
            Template::Output(i) => {
                constant(graph, RATE, number(60.0, 0.0, 1e6, unit(i)));
                item_in(graph, i);
            }
            Template::Sink(i) => item_in(graph, i),
            Template::Factory(id) => {
                let f = s.factories.get(id).cloned().unwrap_or_default();
                for (item, _) in &f.ins {
                    item_in(graph, item);
                }
                for (item, _) in &f.outs {
                    item_out(graph, item);
                }
            }
            Template::Pole(_) => crate::power_net::add_pole_ports(graph, node_id),
            Template::Merger(i) => {
                item_in(graph, i);
                item_out(graph, i);
            }
            Template::Extractor { item, .. } => {
                constant(graph, MACHINES, number(1.0, 0.0, 10_000.0, ""));
                constant(graph, CLOCK, number(100.0, 1.0, 250.0, "%"));
                item_in(graph, item);
                item_out(graph, item);
            }
            Template::Pump { item, .. } => {
                item_in(graph, item);
                item_out(graph, item);
            }
            Template::Splitter(i) => {
                // One wire in.
                let name = game.item_name(i).to_string();
                graph.add_wide_input_param(node_id, name, ItemType(i.clone()), PortValue::Item(i.clone()), InputParamKind::ConnectionOnly, std::num::NonZeroU32::new(1), true);
                item_out(graph, i);
            }
        }
    }
}

/// An item input port (any number of wires).
pub fn add_item_input(game: &GameData, graph: &mut FactoryGraph, node: NodeId, item: &str) -> InputId {
    let name = port_name(game, item, &graph[node].inputs);
    graph.add_wide_input_param(node, name, ItemType(item.into()), PortValue::Item(item.into()), InputParamKind::ConnectionOnly, None, true)
}

/// An item output port.
pub fn add_item_output(game: &GameData, graph: &mut FactoryGraph, node: NodeId, item: &str) -> OutputId {
    let name = port_name(game, item, &graph[node].outputs);
    graph.add_output_param(node, name, ItemType(item.into()))
}

/// Everything that can be added from the right-click finder.
pub struct AllTemplates(pub Vec<Template>);

impl AllTemplates {
    pub fn from_game(game: &GameData) -> Self {
        let mut v: Vec<Template> = game.recipes.keys().cloned().map(Template::Recipe).collect();
        for (class, item) in &game.items {
            if item.raw {
                v.push(Template::Resource(class.clone()));
            }
            v.push(Template::Input(class.clone()));
            v.push(Template::Output(class.clone()));
            v.push(Template::Sink(class.clone()));
        }
        Self(v)
    }
}

impl NodeTemplateIter for &AllTemplates {
    type Item = Template;
    fn all_kinds(&self) -> Vec<Template> {
        self.0.clone()
    }
}

impl WidgetValueTrait for PortValue {
    type Response = Resp;
    type UserState = UserState;
    type NodeData = NodeData;

    fn value_widget(
        &mut self,
        name: &str,
        node_id: NodeId,
        ui: &mut Ui,
        s: &mut UserState,
        d: &NodeData,
    ) -> Vec<Resp> {
        // A resource gives what the extractors on it take, and an Output
        // takes what arrives: their rates are shown, not edited (a resource
        // wired straight into machines keeps its own rate).
        if let PortValue::Number { suffix, .. } = &*self {
            let shown = match &d.kind {
                NodeKind::Resource { .. } if name == RATE && !s.direct_resources.contains(&node_id) => Some((
                    "Extracted",
                    format!("{} of {}", fmt_num(s.ground.get(&node_id).copied().unwrap_or(0.0)), fmt_num(s.ground_cap.get(&node_id).copied().unwrap_or(0.0))),
                    "What's taken of what the extractors on it can extract: their building, number and clock speed, and this node's purity, decide that. The rest backs up.",
                )),
                NodeKind::Output { .. } if name == RATE => Some((
                    "Delivered",
                    fmt_num(s.delivered.get(&node_id).copied().unwrap_or(0.0)),
                    "What arrives here from the machines before it.",
                )),
                _ => None,
            };
            if let Some((label, v, tip)) = shown {
                let suffix = suffix.clone();
                ui.horizontal(|ui| {
                    ui.label(RichText::new(label).color(TEXT_DIM));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(suffix).color(TEXT_DIM));
                        ui.label(RichText::new(v).color(TEXT));
                    });
                })
                .response
                .on_hover_text(tip);
                return vec![];
            }
        }
        match self {
            PortValue::Number {
                value,
                min,
                max,
                suffix,
            } => {
                let z = s.z();
                let mut changed = false;
                ui.horizontal(|ui| {
                    ui.label(RichText::new(name).color(TEXT_DIM));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if !suffix.is_empty() {
                            ui.label(RichText::new(suffix.as_str()).color(TEXT_DIM));
                        }
                        let id = egui::Id::new(("node-number", node_id, name));
                        let mut v = *value;
                        if widgets::num_field(ui, id, &mut v, 72.0 * z) {
                            *value = v.clamp(*min, *max);
                            changed = true;
                        }
                    });
                });
                if changed {
                    vec![Resp::Edited]
                } else {
                    vec![]
                }
            }
            PortValue::Item(item) => {
                let item = item.clone();
                port_row(ui, s, node_id, name, &item, true);
                vec![]
            }
            PortValue::Power => {
                crate::power_net::port_row(ui, s, node_id, name, true);
                vec![]
            }
            PortValue::None => vec![],
        }
    }

    fn value_widget_connected(
        &mut self,
        name: &str,
        node_id: NodeId,
        ui: &mut Ui,
        s: &mut UserState,
        d: &NodeData,
    ) -> Vec<Resp> {
        self.value_widget(name, node_id, ui, s, d)
    }
}

/// Item port: icon + name, and below it "have / want per min" (inputs) or
/// "rate per min" (outputs), coloured by how well it's supplied.
fn port_row(ui: &mut Ui, s: &mut UserState, node_id: NodeId, name: &str, item: &str, input: bool) {
    let stat = s
        .ports
        .get(&(node_id, name.to_string()))
        .copied()
        .unwrap_or_default();
    let form = s.game.item_form(item);
    let layout = if input {
        Layout::left_to_right(Align::Center)
    } else {
        Layout::right_to_left(Align::Center)
    };
    // Sized allocation: `with_layout` would hand the row all remaining
    // height and centre it there, stretching the node.
    ui.allocate_ui_with_layout(vec2(ui.available_width(), 0.0), layout, |ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        s.item_icon(ui, item, 24.0);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let align = if input {
                Layout::top_down(Align::Min)
            } else {
                Layout::top_down(Align::Max)
            };
            ui.with_layout(align, |ui| {
                ui.label(RichText::new(name).color(TEXT));
                let (text, color) = if s.master && !stat.connected {
                    // Factory blocks: unlinked inputs come from outside the
                    // project; unlinked outputs are what the project makes.
                    let t = if input { format!("{} · from outside", fmt_rate_unit(stat.want, form)) } else { fmt_rate_unit(stat.want, form) };
                    (t, TEXT_DIM)
                } else if input {
                    let t = if stat.connected {
                        format!(
                            "{} / {}",
                            crate::fmt::fmt_rate(stat.have),
                            fmt_rate_unit(stat.want, form)
                        )
                    } else {
                        format!("{} · not connected", fmt_rate_unit(stat.want, form))
                    };
                    (t, supply_color(stat.have, stat.want))
                } else if stat.connected {
                    (
                        fmt_rate_unit(stat.have, form),
                        theme::supply_color(stat.have, stat.want),
                    )
                } else {
                    (
                        format!("{} · not connected", fmt_rate_unit(stat.want, form)),
                        if stat.want > 1e-9 { ACCENT } else { TEXT_DIM },
                    )
                };
                ui.label(RichText::new(text).small().color(color));
            });
        });
    });
}

impl NodeDataTrait for NodeData {
    /// Poles' and routers' wires meet at one bubble per side.
    fn shared_hooks(&self) -> bool {
        self.knot_size().is_some()
    }

    /// Poles and routers are small knots.
    fn knot_size(&self) -> Option<egui::Vec2> {
        match self.kind {
            NodeKind::Pole { .. } => Some(crate::power_net::POLE_SIZE),
            NodeKind::Merger { .. } | NodeKind::Splitter { .. } | NodeKind::Pump { .. } => Some(crate::routers::SIZE),
            _ => None,
        }
    }

    fn knot_ui(&self, ui: &mut Ui, rect: egui::Rect, node_id: NodeId, s: &mut UserState) {
        match self.kind {
            NodeKind::Pole { id } => crate::power_net::pole_ui(ui, rect, node_id, id, s),
            _ => crate::routers::router_ui(ui, rect, node_id, &self.kind, s),
        }
    }

    /// Like the buildings in game: a pole takes as many wires as its tier,
    /// mergers and splitters three belts, a pipeline junction four pipes.
    fn accepts_wire(&self, node_id: NodeId, port: AnyParameterId, graph: &FactoryGraph, s: &mut UserState) -> bool {
        let wires = match port {
            AnyParameterId::Input(i) => graph.connections(i).len(),
            AnyParameterId::Output(o) => graph.output_connections(o).len(),
        };
        match &self.kind {
            NodeKind::Pole { id } => crate::power_net::wire_count(graph, node_id) < crate::power_net::max_wires(s.pole_tiers.get(id).copied().unwrap_or(1)),
            // A whole factory's block takes any number of links.
            NodeKind::Factory { .. } => true,
            // A resource is the ground, not a belt: any number of extractors
            // can stand on it.
            NodeKind::Resource { .. } if matches!(port, AnyParameterId::Output(_)) => true,
            // A building's belt or pipe port takes one, as in game: more
            // goes through a merger or splitter.
            NodeKind::Recipe { .. } | NodeKind::Resource { .. } | NodeKind::Input { .. } | NodeKind::Output { .. } | NodeKind::Sink { .. } | NodeKind::Extractor { .. } => wires < 1,
            k => crate::routers::accepts(&s.game, k, graph, node_id, port),
        }
    }

    type Response = Resp;
    type UserState = UserState;
    type DataType = ItemType;
    type ValueType = PortValue;

    fn output_ui(
        &self,
        ui: &mut Ui,
        node_id: NodeId,
        graph: &FactoryGraph,
        s: &mut UserState,
        name: &str,
    ) -> Vec<NodeResponse<Resp, NodeData>> {
        if let Ok(out) = graph[node_id].get_output(name) {
            let item = graph[out].typ.0.clone();
            if crate::power_net::is_power(&item) {
                crate::power_net::port_row(ui, s, node_id, name, false);
            } else {
                port_row(ui, s, node_id, name, &item, false);
            }
        }
        vec![]
    }

    fn top_bar_ui(
        &self,
        ui: &mut Ui,
        node_id: NodeId,
        _graph: &FactoryGraph,
        s: &mut UserState,
    ) -> Vec<NodeResponse<Resp, NodeData>> {
        // The library lays this out right to left, before the title.
        {
            let caption = match &self.kind {
                NodeKind::Factory { .. } => {
                    let eff = s.nodes.get(&node_id).map(|n| n.efficiency).unwrap_or(1.0);
                    if eff < 0.995 {
                        ui.label(RichText::new(format!("{:.0}%", eff * 100.0)).small().color(supply_color(eff, 1.0)))
                            .on_hover_text("Running speed: how much of its planned rate this factory can run at with what the other factories send it.");
                    }
                    Some("Factory")
                }
                NodeKind::Recipe { .. } => {
                    let eff = s.nodes.get(&node_id).map(|n| n.efficiency).unwrap_or(1.0);
                    let color = supply_color(eff, 1.0);
                    ui.label(
                        RichText::new(format!("{:.0}%", eff * 100.0))
                            .small()
                            .color(color),
                    )
                    .on_hover_text(
                        "Running speed: how much of its full rate this machine group can run at.",
                    );
                    None
                }
                NodeKind::Resource { .. } => Some("Resource"),
                NodeKind::Input { .. } => Some("Input"),
                NodeKind::Output { .. } => Some("Output"),
                NodeKind::Sink { .. } => Some("Sink"),
                NodeKind::Pole { .. } | NodeKind::Merger { .. } | NodeKind::Splitter { .. } | NodeKind::Pump { .. } => None,
                NodeKind::Extractor { .. } => {
                    let eff = s.nodes.get(&node_id).map(|n| n.efficiency).unwrap_or(1.0);
                    ui.label(RichText::new(format!("{:.0}%", eff * 100.0)).small().color(TEXT_DIM))
                        .on_hover_text("How much of what it can extract is taken; the rest backs up, as in game.");
                    None
                }
            };
            if let Some(c) = caption {
                ui.label(RichText::new(c).small().color(TEXT_DIM));
            }
            let game = s.game.clone();
            if let Some(item) = self.kind.icon_item(&game) {
                ui.add_space(4.0);
                s.item_icon(ui, item, 20.0);
            }
        }
        vec![]
    }

    fn titlebar_color(
        &self,
        _ui: &Ui,
        _node_id: NodeId,
        _graph: &FactoryGraph,
        _s: &mut UserState,
    ) -> Option<Color32> {
        Some(HEADER)
    }

    /// A factory block that needs power and gets none (or not enough); an
    /// Input no other factory (enough of) brings in.
    fn warning(&self, node_id: NodeId, graph: &FactoryGraph, s: &mut UserState) -> Option<(Color32, String)> {
        match &self.kind {
            NodeKind::Input { item } => {
                let name = s.game.item_name(item).to_string();
                let unit = if s.game.item_form(item).is_fluid() { "m³/min" } else { "per min" };
                let text = match s.unsupplied.get(&node_id)? {
                    None => format!("Nothing brings this in: on the master graph, link {name} into this factory from another one."),
                    Some((got, want)) => format!("Not enough comes in: {} of {} {unit} arrives from other factories.", fmt_num(*got), fmt_num(*want)),
                };
                Some((DANGER, text))
            }
            NodeKind::Factory { .. } => {
                let p = s.ports.get(&(node_id, crate::power_net::PORT.to_string()))?;
                // Its Power input only (an output has have == want).
                if !(p.want > 1e-9 && p.have < p.want - 1e-6) {
                    return None;
                }
                let text = if p.connected {
                    format!("Short of power: gets {} of {} MW.", fmt_num(p.have), fmt_num(p.want))
                } else {
                    "Not getting power: its Power input isn't wired to a pole or a power plant.".to_string()
                };
                Some((DANGER, text))
            }
            NodeKind::Extractor { item, .. } if !crate::extractors::on_resource(graph, node_id) => {
                let name = s.game.item_name(item).to_string();
                Some((DANGER, format!("Not on a resource node: it extracts nothing until a {name} resource node is wired into it.")))
            }
            _ => None,
        }
    }

    fn titlebar_strip(
        &self,
        _node_id: NodeId,
        _graph: &FactoryGraph,
        _s: &mut UserState,
    ) -> Option<Color32> {
        self.color.map(|[r, g, b]| Color32::from_rgb(r, g, b))
    }

    fn separator(
        &self,
        ui: &mut Ui,
        _node_id: NodeId,
        param: AnyParameterId,
        graph: &FactoryGraph,
        _s: &mut UserState,
    ) {
        // A hairline between the numbers and the item ports.
        if let AnyParameterId::Input(i) = param {
            let is_number = matches!(graph[i].value, PortValue::Number { .. });
            let next_is_item = graph[graph[i].node]
                .inputs
                .iter()
                .skip_while(|(_, id)| *id != i)
                .nth(1)
                .is_some_and(|(_, id)| matches!(graph[*id].value, PortValue::Item(_)));
            if is_number && next_is_item {
                ui.add_space(2.0);
                let (r, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 1.0), egui::Sense::hover());
                ui.painter().rect_filled(r, 0.0, DIVIDER);
                ui.add_space(2.0);
            }
        }
    }

    fn bottom_ui(
        &self,
        ui: &mut Ui,
        node_id: NodeId,
        graph: &FactoryGraph,
        s: &mut UserState,
    ) -> Vec<NodeResponse<Resp, NodeData>> {
        let game = s.game.clone();
        match &self.kind {
            NodeKind::Recipe { recipe } => {
                let Some(r) = game.recipes.get(recipe) else {
                    ui.colored_label(DANGER, "This recipe is not in the game data.");
                    return vec![];
                };
                if r.is_generator() {
                    crate::power_ui::generator_footer(ui, s, graph, node_id, r);
                    return vec![];
                }
                let setup = machine_setup(&game, graph, node_id);
                let power = s.nodes.get(&node_id).map(|n| n.power_mw).unwrap_or(0.0);
                let (r_div, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 1.0), egui::Sense::hover());
                ui.painter().rect_filled(r_div, 0.0, DIVIDER);
                let extra = extras_hint(&setup);
                let name = game.buildings.get(&r.building).map(|b| b.name.clone()).unwrap_or_else(|| r.building.clone());
                let icon = game.buildings.get(&r.building).map(|b| b.icon.clone());
                // Power on the right first, so the building name and the
                // build line truncate instead of running under it.
                widgets::split_row_with(ui, if extra.is_some() { 48.0 } else { 36.0 }, s, |ui, s| {
                    ui.label(RichText::new(format!("{} MW", fmt_num(power))).small().color(TEXT_DIM));
                    s.ui_icon(ui, UiIcon::Power, 14.0, TEXT_DIM);
                }, |ui, s| {
                    if let Some(icon) = &icon {
                        s.item_icon(ui, icon, 32.0);
                    }
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        widgets::name_label(ui, RichText::new(&name).color(TEXT));
                        if let Some(line) = build_line(graph, node_id, &setup) {
                            widgets::name_label(ui, RichText::new(line).small().color(TEXT_DIM));
                        }
                        if let Some(extra) = &extra {
                            widgets::name_label(ui, RichText::new(extra).small().color(ACCENT));
                        }
                    });
                });
            }
            NodeKind::Extractor { item, class, .. } => crate::extractors::footer(ui, s, graph, node_id, item, class, crate::extractors::purity_of(graph, node_id)),
            NodeKind::Resource { item, purity } => return crate::extractors::resource_footer(ui, s, graph, node_id, item, *purity),
            NodeKind::Factory { id, .. } => {
                let Some(f) = s.factories.get(id).cloned() else { return vec![] };
                let (r_div, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), egui::Sense::hover());
                ui.painter().rect_filled(r_div, 0.0, DIVIDER);
                widgets::split_row_with(ui, 24.0, s, |ui, s| {
                    ui.label(RichText::new(format!("{} MW", fmt_num(f.power_mw))).small().color(TEXT_DIM));
                    s.ui_icon(ui, UiIcon::Power, 14.0, TEXT_DIM);
                }, |ui, _| {
                    let n = f.machines;
                    widgets::name_label(ui, RichText::new(format!("{n} {}", if n == 1 { "building" } else { "buildings" })).small().color(TEXT_DIM));
                });
            }
            NodeKind::Sink { item } => {
                let got = graph[node_id]
                    .inputs
                    .first()
                    .and_then(|(n, _)| s.ports.get(&(node_id, n.clone())))
                    .map(|p| p.have)
                    .unwrap_or(0.0);
                let pts = game.items.get(item).map(|i| i.sink_points).unwrap_or(0) as f64 * got;
                ui.label(
                    RichText::new(format!(
                        "{} points per min in the AWESOME Sink",
                        fmt_num(pts)
                    ))
                    .small()
                    .color(TEXT_DIM),
                );
            }
            _ => {}
        }
        vec![]
    }
}

/// What to build, when a node's Machines isn't a whole number (older
/// plans): "Build 4 at 81.25% clock speed".
pub fn build_line(graph: &FactoryGraph, node: NodeId, m: &fb_model::MachineSetup) -> Option<String> {
    let machines = input_number(graph, node, MACHINES);
    ((machines - m.count as f64).abs() > 1e-6).then(|| build_hint(m))
}

/// "Build 3 at 100% clock speed" / "Build 4 at 81.25% clock speed".
pub fn build_hint(m: &fb_model::MachineSetup) -> String {
    if m.count == 0 {
        return "Idle".into();
    }
    format!("Build {} at {}% clock speed", m.count, fmt_num(m.clock * 100.0))
}

/// "Needs 6 Power Shards and 2 Somersloops", when it needs any.
pub fn extras_hint(m: &fb_model::MachineSetup) -> Option<String> {
    let plural = |n: u32, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let parts: Vec<String> = [
        (m.shards() > 0).then(|| plural(m.shards(), "Power Shard", "Power Shards")),
        (m.sloops() > 0).then(|| plural(m.sloops(), "Somersloop", "Somersloops")),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!parts.is_empty()).then(|| format!("Needs {}", parts.join(" and ")))
}

/// Somersloops per machine on a recipe node (0 if it has none or can't take them).
pub fn sloops(game: &GameData, graph: &FactoryGraph, node: NodeId) -> u32 {
    let NodeKind::Recipe { recipe } = &graph[node].user_data.kind else { return 0 };
    let slots = game.recipes.get(recipe).and_then(|r| game.buildings.get(&r.building)).map(|b| b.sloop_slots).unwrap_or(0);
    (input_number(graph, node, SLOOPS).round().max(0.0) as u32).min(slots)
}

/// How a recipe node is built: whole machines, their clock speed, shards and Somersloops.
pub fn machine_setup(game: &GameData, graph: &FactoryGraph, node: NodeId) -> fb_model::MachineSetup {
    fb_model::MachineSetup::new(input_number(graph, node, MACHINES), input_number(graph, node, CLOCK) / 100.0, sloops(game, graph, node))
}

/// Recipe nodes saved before Somersloops existed get the field. Returns
/// true if anything changed.
pub fn add_missing_sloop_inputs(game: &GameData, graph: &mut FactoryGraph) -> bool {
    let mut changed = false;
    let ids: Vec<NodeId> = graph.iter_nodes().collect();
    for id in ids {
        let NodeKind::Recipe { recipe } = graph[id].user_data.kind.clone() else { continue };
        let Some(slots) = game.recipes.get(&recipe).and_then(|r| game.buildings.get(&r.building)).map(|b| b.sloop_slots).filter(|s| *s > 0) else {
            continue;
        };
        if graph[id].get_input(SLOOPS).is_err() {
            graph.add_input_param(id, SLOOPS.into(), ItemType(String::new()), number(0.0, 0.0, slots as f64, ""), InputParamKind::ConstantOnly, true);
            changed = true;
        }
    }
    changed
}

pub fn input_number(graph: &FactoryGraph, node: NodeId, name: &str) -> f64 {
    graph[node]
        .get_input(name)
        .map(|i| graph[i].value.number())
        .unwrap_or(0.0)
}

/// Items (or m³) per minute per input and output port at 100% speed.
pub fn node_rates(
    game: &GameData,
    graph: &FactoryGraph,
    node: NodeId,
) -> (Vec<(String, f64)>, Vec<(String, f64)>) {
    let n = &graph[node];
    let item_inputs = n
        .inputs
        .iter()
        .filter(|(_, id)| matches!(graph[*id].value, PortValue::Item(_)));
    let per = |r: &Recipe, item: &str, list: &[fb_model::Amount]| {
        list.iter()
            .filter(|a| a.item == item)
            .map(|a| r.per_min(a))
            .sum::<f64>()
    };
    match &n.user_data.kind {
        NodeKind::Recipe { recipe } => {
            let Some(r) = game.recipes.get(recipe) else {
                return (vec![], vec![]);
            };
            let k = input_number(graph, node, MACHINES) * input_number(graph, node, CLOCK) / 100.0;
            // Somersloops boost output only; ingredients stay the same.
            let boost = game.buildings.get(&r.building).map(|b| b.amplification(sloops(game, graph, node))).unwrap_or(1.0);
            let ins = item_inputs
                .map(|(name, id)| (name.clone(), per(r, &graph[*id].typ.0, &r.ingredients) * k))
                .collect();
            let outs = n
                .outputs
                .iter()
                .map(|(name, id)| (name.clone(), per(r, &graph[*id].typ.0, &r.products) * k * boost))
                .collect();
            (ins, outs)
        }
        NodeKind::Resource { .. } | NodeKind::Input { .. } => {
            // A deposit gives only what's taken from it: by its extractors
            // (worked out from them), or straight by machines (its rate).
            let direct = || n.outputs.iter().any(|(_, o)| graph.output_connections(*o).iter().any(|i| !crate::extractors::from_the_ground(graph, *o, *i)));
            let rate = if matches!(n.user_data.kind, NodeKind::Resource { .. }) && !direct() { 0.0 } else { input_number(graph, node, RATE) };
            (
                vec![],
                n.outputs
                    .iter()
                    .map(|(name, _)| (name.clone(), rate))
                    .collect(),
            )
        }
        // Takes whatever arrives, like a sink.
        NodeKind::Output { .. } => (item_inputs.map(|(name, _)| (name.clone(), 0.0)).collect(), vec![]),
        NodeKind::Sink { .. } => (
            item_inputs.map(|(name, _)| (name.clone(), 0.0)).collect(),
            vec![],
        ),
        NodeKind::Factory { ins, outs, .. } => {
            let rate = |list: &[(String, f64)], item: &str| list.iter().filter(|(i, _)| i == item).map(|(_, r)| *r).sum::<f64>();
            // Power ports come after the item ports and aren't items.
            (
                item_inputs.map(|(name, id)| (name.clone(), rate(ins, &graph[*id].typ.0))).collect(),
                n.outputs.iter().filter(|(_, id)| !crate::power_net::is_power(&graph[*id].typ.0)).map(|(name, id)| (name.clone(), rate(outs, &graph[*id].typ.0))).collect(),
            )
        }
        // Not in the simulation: their wires are joined up (routers.rs).
        NodeKind::Pole { .. } | NodeKind::Merger { .. } | NodeKind::Splitter { .. } | NodeKind::Pump { .. } => (vec![], vec![]),
        // What it takes from the deposit is what it puts out.
        // Nothing without a resource node to stand on.
        NodeKind::Extractor { class, .. } => {
            let purity = crate::extractors::purity_of(graph, node);
            let on = if crate::extractors::on_resource(graph, node) { 1.0 } else { 0.0 };
            let rate = on * crate::extractors::per_machine(game, class, purity) * input_number(graph, node, MACHINES) * input_number(graph, node, CLOCK) / 100.0;
            (item_inputs.map(|(name, _)| (name.clone(), rate)).collect(), n.outputs.iter().map(|(name, _)| (name.clone(), rate)).collect())
        }
    }
}
