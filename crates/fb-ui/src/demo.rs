//! Self-test only: a small generated world for the README's Live view
//! screenshots (`readme_live_*` views). Three live factories, on real
//! resource nodes near the start area, with example live figures. Nothing is
//! saved: the regions live in the in-memory settings, and nothing is written.

use crate::app::FactoryApp;
use fb_model::world::{Carrier, Circuit, HubBurner, Link, Machine, PowerLine, PowerSource, PowerUse, StoreLink, Structure, World};
use fb_model::{LiveFactoryDef, LiveMachine, LiveStructure, Purity};

/// The session the example regions belong to.
pub const SESSION: &str = "Example";

struct Builder {
    w: World,
    status: Vec<Option<LiveMachine>>,
    stores: Vec<Option<LiveStructure>>,
}

impl Builder {
    #[allow(clippy::too_many_arguments)]
    fn machine(&mut self, class: &str, recipe: Option<&str>, x: f32, y: f32, node: Option<&str>, live: (bool, f32), ins: &[(&str, f32)], outs: &[(&str, f32)], mw: f32) -> u32 {
        let id = self.w.machines.len() as u32;
        self.w.machines.push(Machine { id, class: class.into(), recipe: recipe.map(Into::into), clock: 1.0, boost: 1.0, x, y, z: 0.0, yaw: 0.0, node: node.map(Into::into) });
        let items = |l: &[(&str, f32)]| l.iter().map(|(i, n)| (i.to_string(), *n)).collect();
        self.status.push(Some(LiveMachine {
            class: class.into(),
            recipe: recipe.map(Into::into),
            x,
            y,
            producing: Some(live.0),
            productivity: Some(live.1),
            paused: Some(false),
            inputs: items(ins),
            outputs: items(outs),
            ..Default::default()
        }));
        self.w.machine_circuit.insert(id, 0);
        self.w.machine_power.insert(id, PowerUse { now: if live.0 { mw } else { 0.1 }, max: mw, makes: 0.0 });
        id
    }

    fn store(&mut self, x: f32, y: f32, items: &[(&str, f32)], slots: (u32, u32)) -> u32 {
        let k = self.w.structures.len() as u32;
        let class = "Build_StorageContainerMk1_C";
        self.w.structures.push(Structure { class: class.into(), x, y, z: 0.0, yaw: 0.0, circuit: None, power: None });
        self.stores.push(Some(LiveStructure { class: class.into(), x, y, items: items.iter().map(|(i, n)| (i.to_string(), *n)).collect(), slots: Some(slots), ..Default::default() }));
        k
    }

    fn pole(&mut self, x: f32, y: f32) -> [f32; 2] {
        self.w.structures.push(Structure { class: "Build_PowerPoleMk1_C".into(), x, y, z: 0.0, yaw: 0.0, circuit: Some(0), power: None });
        self.stores.push(None);
        [x, y]
    }

    fn belt(&mut self, a: [f32; 2], b: [f32; 2]) {
        self.w.carriers.push(Carrier { class: "Build_ConveyorBeltMk2_C".into(), fluid: false, points: vec![a, b], yaw: 0.0, z: [0.0, 0.0] });
    }

    fn link(&mut self, from: u32, to: u32, item: &str) {
        self.w.links.push(Link { from, to, item: Some(item.into()) });
        let (a, b) = (&self.w.machines[from as usize], &self.w.machines[to as usize]);
        let (a, b) = ([a.x, a.y], [b.x, b.y]);
        self.belt(a, b);
    }

    fn into_store(&mut self, machine: u32, store: u32, item: &str, into: bool) {
        self.w.store_links.push(StoreLink { machine, store, item: Some(item.into()), into });
        let m = &self.w.machines[machine as usize];
        let s = &self.w.structures[store as usize];
        let (a, b) = ([m.x, m.y], [s.x, s.y]);
        if into {
            self.belt(a, b);
        } else {
            self.belt(b, a);
        }
    }
}

/// A square region round `c`, `half` cm out.
fn square(c: [f32; 2], w: f32, h: f32) -> Vec<[f32; 2]> {
    vec![[c[0] - w, c[1] - h], [c[0] + w, c[1] - h], [c[0] + w, c[1] + h], [c[0] - w, c[1] + h]]
}

impl FactoryApp {
    /// Puts the example world on the Map tab, its live factories in the
    /// (in-memory) Live map settings and its live figures in place.
    /// Returns the middle of it (world cm).
    pub(crate) fn load_demo(&mut self) -> Option<[f32; 2]> {
        let info = self.map.info.clone()?;
        // The closest iron and copper nodes anywhere (not impure), 15 to 40 m
        // apart along x, so the three regions sit side by side.
        let of = |res: &str| -> Vec<fb_model::world::MapNode> {
            info.nodes.iter().filter(|n| n.resource.as_deref() == Some(res) && n.purity != Purity::Impure && n.kind == fb_model::world::SiteKind::Node).cloned().collect()
        };
        let (irons, coppers) = (of("Desc_OreIron_C"), of("Desc_OreCopper_C"));
        let (iron, copper) = irons
            .iter()
            .flat_map(|i| coppers.iter().map(move |c| (i, c)))
            .filter(|(i, c)| c.x - i.x > 15_000.0 && (c.y - i.y).abs() < 6_000.0)
            .min_by(|a, b| (a.1.x - a.0.x).total_cmp(&(b.1.x - b.0.x)))
            .map(|(i, c)| (i.clone(), c.clone()))?;
        let mut b = Builder { w: World::default(), status: Vec::new(), stores: Vec::new() };
        // Saved 2026-11-10 18:30 UTC (a fixed date, so the shots stay the same).
        b.w.save = fb_model::saves::SaveSummary { session: SESSION.into(), name: SESSION.into(), path: "example.sav".into(), saved_unix: 1_794_335_400, ..Default::default() };
        let (ix, iy) = (iron.x as f32, iron.y as f32);
        let ore = "Desc_OreIron_C";
        let ingot = "Desc_IronIngot_C";

        // Iron Works: a miner, three smelters, plates into storage, rods out.
        let miner = b.machine("Build_MinerMk1_C", None, ix, iy, Some(&iron.id), (true, 100.0), &[], &[(ore, 64.0)], 5.0);
        let smelt: Vec<u32> = (0..3)
            .map(|k| {
                let busy = k != 2;
                b.machine("Build_SmelterMk1_C", Some("Recipe_IngotIron_C"), ix + 3_500.0, iy + (k as f32 - 1.0) * 1_600.0, None, (busy, if busy { 100.0 } else { 0.0 }), &[(ore, if busy { 12.0 } else { 0.0 })], &[(ingot, if busy { 6.0 } else { 0.0 })], 4.0)
            })
            .collect();
        let plates: Vec<u32> = (0..2).map(|k| b.machine("Build_ConstructorMk1_C", Some("Recipe_IronPlate_C"), ix + 7_000.0, iy + (k as f32 - 0.5) * 1_800.0, None, (true, 96.5), &[(ingot, 9.0)], &[("Desc_IronPlate_C", 4.0)], 4.0)).collect();
        let rods = b.machine("Build_ConstructorMk1_C", Some("Recipe_IronRod_C"), ix + 7_000.0, iy + 2_600.0, None, (false, 41.0), &[(ingot, 2.0)], &[("Desc_IronRod_C", 15.0)], 4.0);
        let plate_store = b.store(ix + 10_000.0, iy, &[("Desc_IronPlate_C", 1_840.0)], (19, 24));
        for &s in &smelt {
            b.link(miner, s, ore);
        }
        b.link(smelt[0], plates[0], ingot);
        b.link(smelt[1], plates[1], ingot);
        b.link(smelt[2], rods, ingot);
        for &p in &plates {
            b.into_store(p, plate_store, "Desc_IronPlate_C", true);
        }

        // Parts: copper into wire and cable, and screws from Iron Works' rods.
        let (cx, cy) = (copper.x as f32, copper.y as f32);
        let cu = "Desc_OreCopper_C";
        let cu_ingot = "Desc_CopperIngot_C";
        let cminer = b.machine("Build_MinerMk1_C", None, cx, cy, Some(&copper.id), (true, 100.0), &[], &[(cu, 52.0)], 5.0);
        let csmelt: Vec<u32> = (0..2).map(|k| b.machine("Build_SmelterMk1_C", Some("Recipe_IngotCopper_C"), cx + 3_500.0, cy + (k as f32 - 0.5) * 1_800.0, None, (true, 100.0), &[(cu, 10.0)], &[(cu_ingot, 5.0)], 4.0)).collect();
        let wire: Vec<u32> = (0..2).map(|k| b.machine("Build_ConstructorMk1_C", Some("Recipe_Wire_C"), cx + 7_000.0, cy + (k as f32 - 0.5) * 1_800.0, None, (true, 100.0), &[(cu_ingot, 7.0)], &[("Desc_Wire_C", 18.0)], 4.0)).collect();
        let cable = b.machine("Build_ConstructorMk1_C", Some("Recipe_Cable_C"), cx + 10_000.0, cy - 900.0, None, (true, 88.0), &[("Desc_Wire_C", 26.0)], &[("Desc_Cable_C", 6.0)], 4.0);
        let screws = b.machine("Build_ConstructorMk1_C", Some("Recipe_Screw_C"), cx + 10_000.0, cy + 1_800.0, None, (true, 62.0), &[("Desc_IronRod_C", 3.0)], &[("Desc_IronScrew_C", 120.0)], 4.0);
        let parts_store = b.store(cx + 13_000.0, cy, &[("Desc_Cable_C", 420.0), ("Desc_Wire_C", 1_150.0), ("Desc_IronScrew_C", 2_400.0)], (11, 24));
        for &s in &csmelt {
            b.link(cminer, s, cu);
        }
        b.link(csmelt[0], wire[0], cu_ingot);
        b.link(csmelt[1], wire[1], cu_ingot);
        b.link(wire[0], cable, "Desc_Wire_C");
        b.into_store(wire[1], parts_store, "Desc_Wire_C", true);
        b.into_store(cable, parts_store, "Desc_Cable_C", true);
        b.into_store(screws, parts_store, "Desc_IronScrew_C", true);
        b.link(rods, screws, "Desc_IronRod_C");

        // Power: Biomass Burners fed from a container of Solid Biofuel.
        let (px, py) = (ix - 6_000.0, iy + 6_000.0);
        let burner_recipe = format!("{}Build_GeneratorBiomass_Automated_C__Desc_Biofuel_C", fb_model::GENERATOR_PREFIX);
        let fuel_store = b.store(px - 3_000.0, py, &[("Desc_Biofuel_C", 380.0)], (2, 24));
        let burners: Vec<u32> = (0..3)
            .map(|k| {
                let id = b.machine("Build_GeneratorBiomass_Automated_C", Some(&burner_recipe), px, py + (k as f32 - 1.0) * 1_400.0, None, (true, 100.0), &[("Desc_Biofuel_C", 42.0)], &[], 0.0);
                b.w.machine_power.insert(id, PowerUse { now: 0.0, max: 0.0, makes: 30.0 });
                id
            })
            .collect();
        for &g in &burners {
            b.into_store(g, fuel_store, "Desc_Biofuel_C", false);
        }

        // The HUB beside them, its burners on the same circuit.
        b.w.structures.push(Structure { class: "Build_TradingPost_C".into(), x: px - 9_000.0, y: py + 3_000.0, z: 0.0, yaw: 0.0, circuit: Some(1), power: None });
        b.stores.push(None);
        b.w.hub_burners = vec![
            HubBurner { x: px - 9_200.0, y: py + 2_000.0, circuit: Some(0), makes: 20.0, fuel: Some("Desc_Leaves_C".into()), fuel_amount: Some(31.0) },
            HubBurner { x: px - 8_800.0, y: py + 2_000.0, circuit: Some(0), makes: 13.4, fuel: Some("Desc_Wood_C".into()), fuel_amount: Some(12.0) },
        ];

        // Poles and lines joining them all.
        let poles = [b.pole(px + 1_500.0, py), b.pole(ix + 5_000.0, iy - 3_000.0), b.pole(cx + 5_000.0, cy - 3_000.0), b.pole(px - 7_000.0, py + 2_500.0)];
        for w in [(0, 1), (1, 2), (0, 3)] {
            b.w.power_lines.push(PowerLine { a: poles[w.0], b: poles[w.1], circuit: Some(0), sides: None });
        }
        let used: f32 = b.w.machine_power.values().map(|u| u.now).sum();
        let max: f32 = b.w.machine_power.values().map(|u| u.max).sum();
        b.w.circuits = vec![
            Circuit { consumption: used, max_consumption: max, capacity: 130.0, production: used, sources: vec![PowerSource { class: "Build_GeneratorBiomass_Automated_C".into(), count: 3, mw: 90.0 }], consumers: 17, ..Default::default() },
            Circuit::default(),
        ];

        // The live factories, their regions round each part.
        let iron_c = [ix + 5_000.0, iy + 500.0];
        let parts_c = [cx + 6_500.0, cy + 400.0];
        let power_c = [px - 1_500.0, py];
        let defs = vec![
            LiveFactoryDef { name: "Iron Works".into(), world: SESSION.into(), region: square(iron_c, 6_500.0, 4_600.0), color: Some([0xFA, 0x95, 0x49]), ..Default::default() },
            LiveFactoryDef { name: "Parts".into(), world: SESSION.into(), region: square(parts_c, 8_000.0, 4_000.0), color: Some([0x86, 0xC7, 0xF2]), ..Default::default() },
            LiveFactoryDef { name: "Power".into(), world: SESSION.into(), region: square(power_c, 3_200.0, 2_600.0), color: Some([0xA6, 0xE0, 0x86]), ..Default::default() },
        ];
        if let Some(s) = self.status.as_mut() {
            s.settings.live.enabled = true;
            s.settings.live.frm = false;
            s.settings.live.server_host.clear();
            s.settings.live.factories = defs;
            s.settings.live.layouts.clear();
        }
        let mid = [(ix + cx) / 2.0, (iy + cy) / 2.0 + 2_000.0];
        self.live.base = None;
        self.map.session = Some(SESSION.into());
        self.map.set_world(b.w);
        self.live.status = b.status;
        self.live.structure_status = b.stores;
        Some(mid)
    }
}
