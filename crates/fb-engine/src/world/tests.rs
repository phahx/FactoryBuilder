use super::*;
use fb_model::{Amount, Building, Extractor, ExtractorKind, Form, Item, Recipe};

// ------------------------------------------------------------ byte builders

fn fstr(s: &str) -> Vec<u8> {
    if s.is_empty() {
        return 0i32.to_le_bytes().to_vec();
    }
    let mut v = ((s.len() + 1) as i32).to_le_bytes().to_vec();
    v.extend_from_slice(s.as_bytes());
    v.push(0);
    v
}

fn wide(s: &str) -> Vec<u8> {
    let units: Vec<u16> = s.encode_utf16().chain([0]).collect();
    let mut v = (-(units.len() as i32)).to_le_bytes().to_vec();
    for u in units {
        v.extend(u.to_le_bytes());
    }
    v
}

fn objref(level: &str, path: &str) -> Vec<u8> {
    [fstr(level), fstr(path)].concat()
}

fn doubles(v: &[f64]) -> Vec<u8> {
    v.iter().flat_map(|d| d.to_le_bytes()).collect()
}

/// A type tree node for complete tags.
struct Ty(&'static str, Vec<Ty>);

fn ty_bytes(t: &Ty) -> Vec<u8> {
    let mut v = fstr(t.0);
    v.extend((t.1.len() as i32).to_le_bytes());
    for c in &t.1 {
        v.extend(ty_bytes(c));
    }
    v
}

fn struct_ty(name: &'static str, package: &'static str) -> Ty {
    Ty("StructProperty", vec![Ty(name, vec![Ty(package, vec![])])])
}

/// A complete (UE 5.4+) tag with its value.
fn ctag(name: &str, ty: Ty, flags: u8, value: &[u8]) -> Vec<u8> {
    let mut v = fstr(name);
    v.extend(ty_bytes(&ty));
    v.extend((value.len() as i32).to_le_bytes());
    v.push(flags);
    if flags & 1 != 0 {
        v.extend(0i32.to_le_bytes());
    }
    if flags & 2 != 0 {
        v.extend([0xAB; 16]);
    }
    v.extend_from_slice(value);
    v
}

/// A legacy tag: `extra` is what the type adds after the array index.
fn ltag(name: &str, ty: &str, extra: &[u8], value: &[u8]) -> Vec<u8> {
    let mut v = fstr(name);
    v.extend(fstr(ty));
    v.extend((value.len() as i32).to_le_bytes());
    v.extend(0i32.to_le_bytes());
    v.extend_from_slice(extra);
    v.push(0); // no property guid
    v.extend_from_slice(value);
    v
}

fn none() -> Vec<u8> {
    fstr("None")
}

fn vector_tag(name: &str, p: [f64; 3], complete: bool) -> Vec<u8> {
    if complete {
        ctag(name, struct_ty("Vector", "/Script/CoreUObject"), 0x08, &doubles(&p))
    } else {
        ltag(name, "StructProperty", &[fstr("Vector"), vec![0; 16]].concat(), &doubles(&p))
    }
}

fn spline_tag(points: &[([f64; 3], [f64; 3])], complete: bool) -> Vec<u8> {
    let mut elems = Vec::new();
    for (loc, tan) in points {
        elems.extend(vector_tag("Location", *loc, complete));
        elems.extend(vector_tag("ArriveTangent", *tan, complete));
        elems.extend(vector_tag("LeaveTangent", *tan, complete));
        elems.extend(none());
    }
    let mut value = (points.len() as i32).to_le_bytes().to_vec();
    if complete {
        value.extend(elems);
        let ty = Ty("ArrayProperty", vec![struct_ty("SplinePointData", "/Script/Engine")]);
        ctag("mSplineData", ty, 0, &value)
    } else {
        let mut inner = fstr("mSplineData");
        inner.extend(fstr("StructProperty"));
        inner.extend((elems.len() as i32).to_le_bytes());
        inner.extend(0i32.to_le_bytes());
        inner.extend(fstr("SplinePointData"));
        inner.extend([0u8; 16]);
        inner.push(0);
        value.extend(inner);
        value.extend(elems);
        ltag("mSplineData", "ArrayProperty", &fstr("StructProperty"), &value)
    }
}

fn float_tag(name: &str, v: f32, complete: bool) -> Vec<u8> {
    if complete {
        ctag(name, Ty("FloatProperty", vec![]), 0, &v.to_le_bytes())
    } else {
        ltag(name, "FloatProperty", &[], &v.to_le_bytes())
    }
}

fn object_tag(name: &str, level: &str, path: &str, complete: bool) -> Vec<u8> {
    if complete {
        ctag(name, Ty("ObjectProperty", vec![]), 0, &objref(level, path))
    } else {
        ltag(name, "ObjectProperty", &[], &objref(level, path))
    }
}

fn purity_tag(value: &str, complete: bool) -> Vec<u8> {
    if complete {
        let ty = Ty("ByteProperty", vec![Ty("EResourcePurity", vec![Ty("/Script/FactoryGame", vec![])])]);
        ctag("mPurityOverride", ty, 0, &fstr(value))
    } else {
        ltag("mPurityOverride", "ByteProperty", &fstr("EResourcePurity"), &fstr(value))
    }
}

fn top_transform_tag(translation: [f64; 3], complete: bool) -> Vec<u8> {
    let mut value = Vec::new();
    if complete {
        value.extend(ctag("Rotation", struct_ty("Quat", "/Script/CoreUObject"), 0x08, &doubles(&[0.0, 0.0, 0.0, 1.0])));
    } else {
        value.extend(ltag("Rotation", "StructProperty", &[fstr("Quat"), vec![0; 16]].concat(), &doubles(&[0.0, 0.0, 0.0, 1.0])));
    }
    value.extend(vector_tag("Translation", translation, complete));
    value.extend(none());
    if complete {
        ctag("mTopTransform", struct_ty("Transform", "/Script/CoreUObject"), 0, &value)
    } else {
        ltag("mTopTransform", "StructProperty", &[fstr("Transform"), vec![0; 16]].concat(), &value)
    }
}

// --------------------------------------------------------- tag reader tests

fn read_all(bytes: &[u8], complete: bool) -> R<Info> {
    read_object(bytes, false, complete)
}

fn component_payload(tags: &[Vec<u8>], complete: bool) -> Vec<u8> {
    let mut v = if complete { vec![0u8] } else { Vec::new() };
    for t in tags {
        v.extend(t);
    }
    v.extend(none());
    v.extend(0i32.to_le_bytes());
    v
}

/// `Xf::yaw` is Unreal's yaw (FQuat::Rotator): positive turns local +X towards
/// world +Y. A building's local point (x, y) lands at world position +
/// (x cos(yaw) - y sin(yaw), x sin(yaw) + y cos(yaw)), which is how the map
/// places its top-down sprites. Checked against late.sav: 1185 of 1238
/// Constructor ports (and 288 of 294 Foundry ports, which are not mirror
/// symmetric) meet a belt end of the right direction with this formula; with
/// -yaw, yaw+90 or a mirrored Y almost none do.
#[test]
fn yaw_is_unreal_yaw() {
    for deg in [0.0f64, 30.0, 90.0, -120.0, 179.0] {
        let h = deg.to_radians() / 2.0;
        let xf = Xf::from_toc(&[0.0, 0.0, h.sin() as f32, h.cos() as f32, 10.0, 20.0, 0.0, 1.0, 1.0, 1.0]);
        assert!((xf.yaw() - deg).abs() < 1e-3, "{deg}: {}", xf.yaw());
        // The quaternion's own rotation agrees with the 2D formula above.
        let (local_x, local_y) = (300.0, -150.0);
        let p = xf.point([local_x, local_y, 0.0]);
        let a = xf.yaw().to_radians();
        let want = [10.0 + local_x * a.cos() - local_y * a.sin(), 20.0 + local_x * a.sin() + local_y * a.cos()];
        assert!((p[0] - want[0]).abs() < 1e-2 && (p[1] - want[1]).abs() < 1e-2, "{deg}: {p:?} vs {want:?}");
    }
    // +90 turns local +X onto world +Y.
    let h = 45f64.to_radians();
    let xf = Xf::from_toc(&[0.0, 0.0, h.sin() as f32, h.cos() as f32, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0]);
    let p = xf.vector([1.0, 0.0, 0.0]);
    assert!(p[0].abs() < 1e-6 && (p[1] - 1.0).abs() < 1e-6, "{p:?}");
}

#[test]
fn complete_tags_are_read() {
    let tags = vec![
        ctag("mSomethingElse", Ty("MapProperty", vec![Ty("IntProperty", vec![]), Ty("StrProperty", vec![])]), 0x01, &[1, 2, 3, 4, 5]),
        ctag("mProductivityMonitorEnabled", Ty("BoolProperty", vec![]), 0x10, &[]),
        float_tag("mCurrentPotential", 2.5, true),
        float_tag("mCurrentProductionBoost", 2.0, true),
        ctag("mCurrentRecipe", Ty("ObjectProperty", vec![]), 0x02, &objref("", "/Game/FactoryGame/Recipes/Constructor/Recipe_Wire.Recipe_Wire_C")),
        object_tag("mExtractableResource", "Persistent_Level", "Persistent_Level:PersistentLevel.BP_ResourceNode40", true),
        purity_tag("RP_Inpure", true),
        spline_tag(&[([0.0; 3], [0.0, -50.0, 0.0]), ([0.0, -100.0, 0.0], [0.0, -50.0, 0.0])], true),
    ];
    let info = read_all(&component_payload(&tags, true), true).unwrap();
    assert_eq!(info.potential, Some(2.5));
    assert_eq!(info.boost, Some(2.0));
    assert_eq!(info.recipe.as_deref(), Some("Recipe_Wire_C"));
    assert_eq!(info.extractable.as_deref(), Some("BP_ResourceNode40"));
    assert_eq!(info.purity_override, Some(Purity::Impure));
    assert_eq!(info.spline.len(), 2);
    assert_eq!(info.spline[1].location, [0.0, -100.0, 0.0]);
    assert_eq!(info.spline[0].leave, [0.0, -50.0, 0.0]);

    // The tag header on its own, including the bool held in the flags.
    let bytes = ctag("mIsProducing", Ty("BoolProperty", vec![]), 0x10, &[]);
    let tag = read_tag(&mut Cur::new(&bytes), true).unwrap().unwrap();
    assert!(tag.bool_val && tag.size == 0 && tag.ty_is(0, "BoolProperty"));
}

#[test]
fn legacy_tags_are_read() {
    let tags = vec![
        ltag("mProductivityMonitorEnabled", "BoolProperty", &[1], &[]),
        ltag("mColorSlot", "ByteProperty", &fstr("None"), &[11]),
        ltag("mSomeMap", "MapProperty", &[fstr("IntProperty"), fstr("IntProperty")].concat(), &[0; 8]),
        float_tag("mCurrentPotential", 1.5, false),
        object_tag("mCurrentRecipe", "", "/Game/FactoryGame/Recipes/Smelter/Recipe_IngotIron.Recipe_IngotIron_C", false),
        purity_tag("RP_Pure", false),
        spline_tag(&[([1.0, 2.0, 3.0], [10.0, 0.0, 0.0]), ([101.0, 2.0, 3.0], [10.0, 0.0, 0.0])], false),
        top_transform_tag([0.0, 0.0, 800.0], false),
    ];
    let info = read_all(&component_payload(&tags, false), false).unwrap();
    assert_eq!(info.potential, Some(1.5));
    assert_eq!(info.recipe.as_deref(), Some("Recipe_IngotIron_C"));
    assert_eq!(info.purity_override, Some(Purity::Pure));
    assert_eq!(info.spline.len(), 2);
    assert_eq!(info.spline[0].location, [1.0, 2.0, 3.0]);
    assert_eq!(info.spline[1].arrive, [10.0, 0.0, 0.0]);
    assert_eq!(info.top, Some([0.0, 0.0, 800.0]));

    let bytes = ltag("mIsProducing", "BoolProperty", &[1], &[]);
    let tag = read_tag(&mut Cur::new(&bytes), false).unwrap().unwrap();
    assert!(tag.bool_val && tag.size == 0);
}

#[test]
fn utf16_names_compare() {
    let mut bytes = wide("mCurrentPotential");
    bytes.extend(fstr("FloatProperty"));
    bytes.extend(4i32.to_le_bytes());
    bytes.extend(0i32.to_le_bytes());
    bytes.push(0);
    bytes.extend(3.0f32.to_le_bytes());
    bytes.extend(none());
    assert_eq!(read_all(&bytes, false).unwrap().potential, Some(3.0));
}

#[test]
fn malformed_tags_fail_cleanly() {
    let good = component_payload(&[float_tag("mCurrentPotential", 2.5, true), spline_tag(&[([0.0; 3], [1.0, 0.0, 0.0]); 3], true)], true);
    assert!(read_all(&good, true).is_ok());
    // Cut short anywhere: an error, never a panic.
    for n in 0..good.len() - 4 {
        assert!(read_all(&good[..n], true).is_err(), "cut at {n}");
    }
    // Every single byte corrupted: no panic (it may still parse).
    for i in 0..good.len() {
        for x in [0x00, 0x7F, 0x80, 0xFF] {
            let mut b = good.clone();
            b[i] = x;
            let _ = read_all(&b, true);
            let _ = read_all(&b, false);
        }
    }
    // Absurd lengths and counts.
    let huge_string = [i32::MAX.to_le_bytes(), [0; 4]].concat();
    assert!(read_all(&huge_string, false).is_err());
    let min_string = [i32::MIN.to_le_bytes(), [0; 4]].concat();
    assert!(read_all(&min_string, false).is_err());
    let negative_size = [fstr("mCurrentPotential"), fstr("FloatProperty"), (-4i32).to_le_bytes().to_vec(), vec![0; 9]].concat();
    assert!(read_all(&negative_size, false).is_err());
    // A type tree nested deeper than any real one.
    let mut deep = Ty("FloatProperty", vec![]);
    for _ in 0..20 {
        deep = Ty("ArrayProperty", vec![deep]);
    }
    let bytes = component_payload(&[ctag("mX", deep, 0, &[0; 4])], true);
    assert!(read_all(&bytes, true).is_err());
    // A spline claiming more points than there are bytes.
    let mut spline = (1_000_000i32).to_le_bytes().to_vec();
    spline.extend([0u8; 16]);
    let ty = Ty("ArrayProperty", vec![struct_ty("SplinePointData", "/Script/Engine")]);
    assert!(read_all(&component_payload(&[ctag("mSplineData", ty, 0, &spline)], true), true).is_err());
    // A missing NUL terminator.
    let mut no_nul = fstr("mCurrentPotential");
    *no_nul.last_mut().unwrap() = b'x';
    assert!(read_all(&no_nul, false).is_err());
    // Property extensions (flag 0x04): unverified layout, so the object is skipped.
    let ext = component_payload(&[ctag("mCurrentPotential", Ty("FloatProperty", vec![]), 0x04, &[0; 4])], true);
    assert!(read_all(&ext, true).is_err());
}

// ---------------------------------------------------------- synthetic saves

const TAG: [u8; 4] = 0x9E2A_83C1u32.to_le_bytes();
const CHUNK_V2: u32 = 0x2222_2222;
const EPOCH_TICKS: i64 = 621_355_968_000_000_000;
const PL: &str = "Persistent_Level:PersistentLevel.";

struct Obj {
    entity: bool,
    class: String,
    name: String,
    parent: String,
    pos: [f32; 3],
    yaw: f32,
    tags: Vec<Vec<u8>>,
}

fn entity(class: &str, n: u32, pos: [f32; 3], tags: Vec<Vec<u8>>) -> Obj {
    Obj { entity: true, class: format!("/Game/FactoryGame/X/{class}.{class}"), name: format!("{PL}{class}_{n}"), parent: String::new(), pos, yaw: 90.0, tags }
}

fn conn(c: bool, class: &str, owner: &str, side: &str, to: Option<&str>) -> Obj {
    let tags = to.map(|t| vec![object_tag("mConnectedComponent", "Persistent_Level", &format!("{PL}{t}"), c)]).unwrap_or_default();
    Obj { entity: false, class: format!("/Script/FactoryGame.{class}"), name: format!("{PL}{owner}.{side}"), parent: format!("{PL}{owner}"), pos: [0.0; 3], yaw: 0.0, tags }
}

fn belt_conn(c: bool, owner: &str, side: &str, to: Option<&str>) -> Obj {
    conn(c, "FGFactoryConnectionComponent", owner, side, to)
}

fn pipe_conn(c: bool, owner: &str, side: &str, to: Option<&str>) -> Obj {
    conn(c, if side == "FGPipeConnectionFactory" || side.starts_with("PipeInput") { "FGPipeConnectionFactory" } else { "FGPipeConnectionComponent" }, owner, side, to)
}

fn version_data(ue5: i32) -> Vec<u8> {
    let mut v = 1u32.to_le_bytes().to_vec();
    v.extend(522i32.to_le_bytes());
    v.extend(ue5.to_le_bytes());
    v.extend(0i32.to_le_bytes());
    for x in [5u16, 3, 2] {
        v.extend(x.to_le_bytes());
    }
    v.extend(0u32.to_le_bytes());
    v.extend(fstr("++UE5+Release-5.3"));
    v.extend(1i32.to_le_bytes());
    v.extend([7u8; 16]);
    v.extend(3i32.to_le_bytes());
    v
}

/// A save body with one empty sublevel and a persistent level holding
/// `objs`. Complete tags need object version >= 53 and UE5 >= 1012.
fn body(objs: &[Obj], complete: bool) -> Vec<u8> {
    let object_version: i32 = if complete { 60 } else { 52 };
    let level = |objs: &[Obj], persistent: bool| -> Vec<u8> {
        let mut toc = (objs.len() as i32).to_le_bytes().to_vec();
        let mut data = (objs.len() as i32).to_le_bytes().to_vec();
        for o in objs {
            toc.extend((o.entity as i32).to_le_bytes());
            toc.extend(fstr(&o.class));
            toc.extend(fstr("Persistent_Level"));
            toc.extend(fstr(&o.name));
            toc.extend(8u32.to_le_bytes());
            if o.entity {
                toc.extend(1i32.to_le_bytes());
                let h = (o.yaw as f64).to_radians() / 2.0;
                let floats = [0.0, 0.0, h.sin() as f32, h.cos() as f32, o.pos[0], o.pos[1], o.pos[2], 1.0, 1.0, 1.0];
                for f in floats {
                    toc.extend(f.to_le_bytes());
                }
                toc.extend(0i32.to_le_bytes());
            } else {
                toc.extend(fstr(&o.parent));
            }
            let mut payload = Vec::new();
            if o.entity {
                payload.extend(objref("", ""));
                payload.extend(0i32.to_le_bytes());
            }
            payload.extend(component_payload(&o.tags, complete));
            data.extend(object_version.to_le_bytes());
            data.extend(0i32.to_le_bytes());
            data.extend((payload.len() as i32).to_le_bytes());
            data.extend(payload);
            if object_version >= 53 {
                data.extend(0i32.to_le_bytes());
            }
        }
        let mut v = (toc.len() as i32).to_le_bytes().to_vec();
        v.extend(0i32.to_le_bytes());
        v.extend(toc);
        v.extend((data.len() as i32).to_le_bytes());
        v.extend(0i32.to_le_bytes());
        v.extend(data);
        if persistent {
            v.extend(1i32.to_le_bytes()); // destroyed actors: one level, one actor
            v.extend(fstr("Persistent_Level"));
            v.extend(1i32.to_le_bytes());
            v.extend(objref("Persistent_Level", "Persistent_Level:PersistentLevel.BP_Rock1"));
        } else {
            v.extend(60i32.to_le_bytes()); // level version
            v.extend(0i32.to_le_bytes()); // collectables
            v.extend(1i32.to_le_bytes());
            v.extend(version_data(1012));
        }
        v
    };
    let mut b = vec![0u8; 4];
    b.extend(0i32.to_le_bytes());
    b.extend(version_data(if complete { 1017 } else { 1009 }));
    b.extend(1i32.to_le_bytes()); // one validation grid
    b.extend(fstr("MainGrid"));
    b.extend(12800i32.to_le_bytes());
    b.extend(5u32.to_le_bytes());
    b.extend(1u32.to_le_bytes());
    b.extend(fstr("Cell_1"));
    b.extend(9u32.to_le_bytes());
    b.extend(1i32.to_le_bytes()); // one sublevel, then the persistent level
    b.extend(fstr("Sublevel_A"));
    b.extend(level(&[], false));
    b.extend(level(objs, true));
    let total = (b.len() - 8) as i32;
    b[..4].copy_from_slice(&total.to_le_bytes());
    b
}

fn wrap(body: &[u8], save_version: i32, chunk: usize) -> Vec<u8> {
    let mut f = Vec::new();
    f.extend(14i32.to_le_bytes());
    f.extend(save_version.to_le_bytes());
    f.extend(513_885i32.to_le_bytes());
    for s in ["My save", "Persistent_Level", "?startloc=Grass Fields", "My session"] {
        f.extend(fstr(s));
    }
    f.extend(3600i32.to_le_bytes());
    f.extend((EPOCH_TICKS + 1_700_000_000 * 10_000_000).to_le_bytes());
    f.extend([0u8; 40]);
    for part in body.chunks(chunk) {
        let packed = miniz_oxide::deflate::compress_to_vec_zlib(part, 6);
        f.extend(TAG);
        f.extend(CHUNK_V2.to_le_bytes());
        f.extend(131_072i64.to_le_bytes());
        f.push(3);
        for _ in 0..2 {
            f.extend((packed.len() as i64).to_le_bytes());
            f.extend((part.len() as i64).to_le_bytes());
        }
        f.extend(packed);
    }
    f
}

fn amount(item: &str, n: f64) -> Amount {
    Amount { item: item.into(), amount: n }
}

fn tiny_game() -> GameData {
    let mut g = GameData::default();
    for (class, form, raw) in [
        ("Desc_OreIron_C", Form::Solid, true),
        ("Desc_Coal_C", Form::Solid, true),
        ("Desc_IronIngot_C", Form::Solid, false),
        ("Desc_Water_C", Form::Liquid, true),
    ] {
        g.items.insert(class.into(), Item { class: class.into(), name: class.into(), description: String::new(), form, sink_points: 0, raw, color: [0; 4] });
    }
    let recipe = |class: &str, building: &str, ingredients: Vec<Amount>, products: Vec<Amount>| Recipe {
        class: class.into(),
        name: class.into(),
        alternate: false,
        duration_s: 2.0,
        ingredients,
        products,
        building: building.into(),
        variable_power_mw: None,
        generates_mw: 0.0,
    };
    for r in [
        recipe("Recipe_IngotIron_C", "Build_SmelterMk1_C", vec![amount("Desc_OreIron_C", 1.0)], vec![amount("Desc_IronIngot_C", 1.0)]),
        recipe("Recipe_IngotSteel_C", "Build_FoundryMk1_C", vec![amount("Desc_OreIron_C", 3.0), amount("Desc_Coal_C", 3.0)], vec![amount("Desc_IronIngot_C", 1.0)]),
        recipe("Recipe_WetIngot_C", "Build_OilRefinery_C", vec![amount("Desc_Water_C", 1.0), amount("Desc_OreIron_C", 1.0)], vec![amount("Desc_IronIngot_C", 2.0)]),
    ] {
        g.recipes.insert(r.class.clone(), r);
    }
    for b in ["Build_SmelterMk1_C", "Build_OilRefinery_C", "Build_FoundryMk1_C"] {
        g.buildings.insert(b.into(), Building { class: b.into(), name: b.into(), power_mw: 4.0, power_exponent: 1.3, icon: String::new(), sloop_slots: 1, sloop_boost: 1.0, sloop_power_exponent: 2.0 });
    }
    let ex = |class: &str, kind, forms: Vec<Form>, resources: Vec<&str>| Extractor {
        class: class.into(),
        name: class.into(),
        kind,
        cycle_s: 1.0,
        per_cycle: 1.0,
        forms,
        resources: resources.into_iter().map(String::from).collect(),
        power_mw: 5.0,
        power_exponent: 1.3,
        icon: String::new(),
    };
    g.extraction.extractors = vec![
        ex("Build_MinerMk1_C", ExtractorKind::Node, vec![Form::Solid], vec![]),
        ex("Build_WaterPump_C", ExtractorKind::Water, vec![Form::Liquid], vec!["Desc_Water_C"]),
    ];
    g
}

/// Miner -> belt -> splitter -> belt -> smelter -> belt -> refinery, the
/// splitter's other output into storage, a water pump piped through a
/// junction into the refinery, a lift, and an overridden node.
fn factory(complete: bool) -> Vec<Obj> {
    let c = complete;
    let straight = |len: f64| spline_tag(&[([0.0; 3], [len, 0.0, 0.0]), ([len, 0.0, 0.0], [len, 0.0, 0.0])], c);
    let curve = spline_tag(&[([0.0; 3], [1000.0, 0.0, 0.0]), ([1000.0, 1000.0, 0.0], [0.0, 1000.0, 0.0])], c);
    let res = "Persistent_Level:PersistentLevel.BP_ResourceNode1";
    let mut objs = vec![
        entity("Build_MinerMk1_C", 1, [0.0, 0.0, 100.0], vec![object_tag("mExtractableResource", "Persistent_Level", res, c)]),
        belt_conn(c, "Build_MinerMk1_C_1", "Output0", Some("Build_ConveyorBeltMk1_C_1.ConveyorAny0")),
        entity("Build_ConveyorBeltMk1_C", 1, [100.0, 0.0, 100.0], vec![curve]),
        belt_conn(c, "Build_ConveyorBeltMk1_C_1", "ConveyorAny0", None),
        belt_conn(c, "Build_ConveyorBeltMk1_C_1", "ConveyorAny1", Some("Build_ConveyorAttachmentSplitter_C_1.Input1")),
        entity("Build_ConveyorAttachmentSplitter_C", 1, [1100.0, 1000.0, 100.0], vec![]),
        belt_conn(c, "Build_ConveyorAttachmentSplitter_C_1", "Input1", Some("Build_ConveyorBeltMk1_C_1.ConveyorAny1")),
        belt_conn(c, "Build_ConveyorAttachmentSplitter_C_1", "Output1", Some("Build_ConveyorBeltMk1_C_2.ConveyorAny0")),
        belt_conn(c, "Build_ConveyorAttachmentSplitter_C_1", "Output2", Some("Build_ConveyorBeltMk1_C_3.ConveyorAny0")),
        entity("Build_ConveyorBeltMk1_C", 2, [1100.0, 1000.0, 100.0], vec![straight(500.0)]),
        belt_conn(c, "Build_ConveyorBeltMk1_C_2", "ConveyorAny0", Some("Build_ConveyorAttachmentSplitter_C_1.Output1")),
        belt_conn(c, "Build_ConveyorBeltMk1_C_2", "ConveyorAny1", Some("Build_SmelterMk1_C_1.Input0")),
        entity("Build_ConveyorBeltMk1_C", 3, [1100.0, 1000.0, 100.0], vec![straight(300.0)]),
        belt_conn(c, "Build_ConveyorBeltMk1_C_3", "ConveyorAny0", Some("Build_ConveyorAttachmentSplitter_C_1.Output2")),
        belt_conn(c, "Build_ConveyorBeltMk1_C_3", "ConveyorAny1", Some("Build_StorageContainerMk1_C_1.Input0")),
        entity("Build_StorageContainerMk1_C", 1, [1400.0, 1000.0, 100.0], vec![]),
        belt_conn(c, "Build_StorageContainerMk1_C_1", "Input0", Some("Build_ConveyorBeltMk1_C_3.ConveyorAny1")),
        belt_conn(c, "Build_StorageContainerMk1_C_1", "Output0", Some("Build_ConveyorBeltMk1_C_5.ConveyorAny0")),
        // Storage ends the chain even though a belt leaves it for the smelter.
        entity("Build_ConveyorBeltMk1_C", 5, [1400.0, 1000.0, 100.0], vec![straight(100.0)]),
        belt_conn(c, "Build_ConveyorBeltMk1_C_5", "ConveyorAny0", Some("Build_StorageContainerMk1_C_1.Output0")),
        belt_conn(c, "Build_ConveyorBeltMk1_C_5", "ConveyorAny1", None),
        entity(
            "Build_SmelterMk1_C",
            1,
            [1600.0, 1000.0, 100.0],
            vec![
                object_tag("mCurrentRecipe", "", "/Game/FactoryGame/Recipes/Smelter/Recipe_IngotIron.Recipe_IngotIron_C", c),
                float_tag("mCurrentPotential", 2.5, c),
                float_tag("mCurrentProductionBoost", 2.0, c),
            ],
        ),
        // Only the belt side stores this connection.
        belt_conn(c, "Build_SmelterMk1_C_1", "Input0", None),
        belt_conn(c, "Build_SmelterMk1_C_1", "Output0", Some("Build_ConveyorBeltMk1_C_4.ConveyorAny0")),
        entity("Build_ConveyorBeltMk1_C", 4, [1700.0, 1000.0, 100.0], vec![straight(200.0)]),
        belt_conn(c, "Build_ConveyorBeltMk1_C_4", "ConveyorAny0", Some("Build_SmelterMk1_C_1.Output0")),
        belt_conn(c, "Build_ConveyorBeltMk1_C_4", "ConveyorAny1", Some("Build_OilRefinery_C_1.Input0")),
        entity("Build_OilRefinery_C", 1, [1900.0, 1000.0, 100.0], vec![object_tag("mCurrentRecipe", "", "/Game/X/Recipe_WetIngot.Recipe_WetIngot_C", c)]),
        belt_conn(c, "Build_OilRefinery_C_1", "Input0", Some("Build_ConveyorBeltMk1_C_4.ConveyorAny1")),
        pipe_conn(c, "Build_OilRefinery_C_1", "PipeInputFactory", Some("Build_PipelineMK2_C_2.PipelineConnection0")),
        entity("Build_WaterPump_C", 1, [3000.0, 1000.0, 0.0], vec![object_tag("mExtractableResource", "Persistent_Level", "Persistent_Level:PersistentLevel.FGWaterVolume1", c)]),
        pipe_conn(c, "Build_WaterPump_C_1", "FGPipeConnectionFactory", Some("Build_PipelineMK2_C_1.PipelineConnection0")),
        entity("Build_PipelineMK2_C", 1, [3000.0, 1000.0, 0.0], vec![straight(400.0)]),
        pipe_conn(c, "Build_PipelineMK2_C_1", "PipelineConnection0", Some("Build_WaterPump_C_1.FGPipeConnectionFactory")),
        pipe_conn(c, "Build_PipelineMK2_C_1", "PipelineConnection1", Some("Build_PipelineJunction_Cross_C_1.Connection0")),
        entity("Build_PipelineJunction_Cross_C", 1, [3400.0, 1000.0, 0.0], vec![]),
        pipe_conn(c, "Build_PipelineJunction_Cross_C_1", "Connection0", Some("Build_PipelineMK2_C_1.PipelineConnection1")),
        pipe_conn(c, "Build_PipelineJunction_Cross_C_1", "Connection1", Some("Build_PipelineMK2_C_2.PipelineConnection1")),
        // Drawn from the refinery towards the junction: pipes have no direction.
        entity("Build_PipelineMK2_C", 2, [1900.0, 1000.0, 0.0], vec![straight(1500.0)]),
        pipe_conn(c, "Build_PipelineMK2_C_2", "PipelineConnection0", Some("Build_OilRefinery_C_1.PipeInputFactory")),
        pipe_conn(c, "Build_PipelineMK2_C_2", "PipelineConnection1", Some("Build_PipelineJunction_Cross_C_1.Connection1")),
        entity("Build_ConveyorLiftMk1_C", 1, [500.0, 500.0, 0.0], vec![top_transform_tag([0.0, 0.0, 800.0], c)]),
        Obj {
            name: format!("{PL}BP_ResourceNode1"),
            ..entity("BP_ResourceNode", 1, [0.0, 0.0, 0.0], vec![object_tag("mResourceClassOverride", "", "/Game/X/Desc_OreIron.Desc_OreIron_C", c), purity_tag("RP_Pure", c)])
        },
        entity("BP_Tree", 1, [0.0, 0.0, 0.0], vec![float_tag("mWhatever", 1.0, c)]),
    ];
    // A connection whose property data is broken: skipped, not fatal.
    let mut broken = belt_conn(c, "Build_ConveyorBeltMk1_C_5", "ConveyorAny9", Some("Nowhere.Input0"));
    let t = &mut broken.tags[0];
    let n = t.len();
    t.truncate(n - 3);
    objs.push(broken);
    objs
}

fn check_factory(w: &World) {
    let m = |class: &str| w.machines.iter().find(|m| m.class == class).unwrap_or_else(|| panic!("{class}"));
    assert_eq!(w.machines.len(), 4);
    let (miner, smelter, refinery, pump) = (m("Build_MinerMk1_C"), m("Build_SmelterMk1_C"), m("Build_OilRefinery_C"), m("Build_WaterPump_C"));
    assert_eq!(miner.node.as_deref(), Some("BP_ResourceNode1"));
    assert_eq!(pump.node, None, "water volumes aren't resource nodes");
    assert_eq!(smelter.recipe.as_deref(), Some("Recipe_IngotIron_C"));
    assert_eq!((smelter.clock, smelter.boost), (2.5, 2.0));
    assert_eq!((miner.clock, miner.boost), (1.0, 1.0));
    assert!((smelter.yaw - 90.0).abs() < 1e-3);
    assert_eq!((smelter.x, smelter.y, smelter.z), (1600.0, 1000.0, 100.0));
    for (i, mm) in w.machines.iter().enumerate() {
        assert_eq!(mm.id as usize, i);
    }

    let mut links: Vec<(u32, u32, Option<&str>)> = w.links.iter().map(|l| (l.from, l.to, l.item.as_deref())).collect();
    links.sort();
    let mut want = vec![
        (miner.id, smelter.id, Some("Desc_OreIron_C")),
        (smelter.id, refinery.id, None), // the refinery doesn't take ingots
        (pump.id, refinery.id, Some("Desc_Water_C")),
    ];
    want.sort();
    assert_eq!(links, want);

    assert_eq!(w.carriers.len(), 8, "5 belts, 2 pipes, 1 lift");
    assert_eq!(w.carriers.iter().filter(|c| c.fluid).count(), 2);
    let lift = w.carriers.iter().find(|c| c.class.contains("Lift")).unwrap();
    assert_eq!(lift.points, vec![[500.0, 500.0], [500.0, 500.0]]);
    // The curved belt (rotated 90 degrees) gets extra points along its bend.
    let belt1 = &w.carriers[0];
    assert_eq!(belt1.class, "Build_ConveyorBeltMk1_C");
    assert!(belt1.points.len() > 2, "{:?}", belt1.points);
    let (first, last) = (belt1.points[0], *belt1.points.last().unwrap());
    assert!((first[0] - 100.0).abs() < 1e-3 && first[1].abs() < 1e-3, "{first:?}");
    assert!((last[0] - (100.0 - 1000.0)).abs() < 1e-2 && (last[1] - 1000.0).abs() < 1e-2, "{last:?}");

    let mut marks: Vec<&str> = w.structures.iter().map(|s| s.class.as_str()).collect();
    marks.sort();
    assert_eq!(marks, ["Build_ConveyorAttachmentSplitter_C", "Build_PipelineJunction_Cross_C", "Build_StorageContainerMk1_C"]);
    // Marks and carriers carry the actor's facing too (sprites are turned by it).
    assert!(w.structures.iter().all(|s| (s.yaw - 90.0).abs() < 1e-3), "{:?}", w.structures);
    assert!((lift.yaw - 90.0).abs() < 1e-3);
    assert_eq!(w.node_overrides, vec![NodeOverride { id: "BP_ResourceNode1".into(), resource: Some("Desc_OreIron_C".into()), purity: Some(Purity::Pure) }]);
    assert_eq!(w.skipped, 1);
    assert_eq!((w.save.session.as_str(), w.save.save_version), ("My session", 60));
}

#[test]
fn reads_a_synthetic_factory_with_complete_tags() {
    let game = tiny_game();
    let (w, stats) = read_world_stats(&wrap(&body(&factory(true), true), 60, 4096), &game).unwrap();
    check_factory(&w);
    assert_eq!(stats.objects, factory(true).len());
    assert_eq!((stats.levels, stats.legacy_tags), (2, 0));
}

#[test]
fn reads_a_synthetic_factory_with_legacy_tags() {
    let game = tiny_game();
    for chunk in [97, 1 << 17] {
        let (w, stats) = read_world_stats(&wrap(&body(&factory(false), false), 60, chunk), &game).unwrap();
        check_factory(&w);
        assert_eq!(stats.complete_tags, 0);
    }
}

#[test]
fn without_the_override_the_miner_item_comes_from_the_consumer() {
    let game = tiny_game();
    let objs: Vec<Obj> = factory(true).into_iter().filter(|o| !o.name.ends_with("BP_ResourceNode1")).collect();
    let w = read_world(&wrap(&body(&objs, true), 60, 4096), &game).unwrap();
    let smelter = w.machines.iter().find(|m| m.class == "Build_SmelterMk1_C").unwrap().id;
    let l = w.links.iter().find(|l| l.to == smelter).unwrap();
    assert_eq!(l.item.as_deref(), Some("Desc_OreIron_C"), "the only raw solid the smelter takes");
}

#[test]
fn the_map_tells_what_miners_send() {
    use fb_model::world::{MapInfo, MapNode, SiteKind};
    let game = tiny_game();
    let machine = |id: u32, class: &str, recipe: Option<&str>, node: Option<&str>| Machine {
        id,
        class: class.into(),
        recipe: recipe.map(String::from),
        clock: 1.0,
        boost: 1.0,
        x: 0.0,
        y: 0.0,
        z: 0.0,
        yaw: 0.0,
        node: node.map(String::from),
    };
    let mut w = World {
        machines: vec![
            machine(0, "Build_MinerMk1_C", None, Some("BP_ResourceNode7")),
            machine(1, "Build_FoundryMk1_C", Some("Recipe_IngotSteel_C"), None),
            machine(2, "Build_MinerMk1_C", None, Some("BP_ResourceNode8")),
            machine(3, "Build_SmelterMk1_C", Some("Recipe_IngotIron_C"), None),
        ],
        links: vec![
            Link { from: 0, to: 1, item: None },
            Link { from: 2, to: 1, item: None },
            Link { from: 2, to: 3, item: None },
        ],
        node_overrides: vec![NodeOverride { id: "BP_ResourceNode8".into(), resource: Some("Desc_Coal_C".into()), purity: None }],
        ..Default::default()
    };
    let node = |id: &str, res: &str| MapNode { id: id.into(), kind: SiteKind::Node, resource: Some(res.into()), purity: Purity::Normal, x: 0.0, y: 0.0, z: 0.0 };
    let map = MapInfo { nodes: vec![node("BP_ResourceNode7", "Desc_OreIron_C"), node("BP_ResourceNode8", "Desc_OreIron_C")], ..Default::default() };
    apply_map(&mut w, &map, &game);
    let items: Vec<Option<&str>> = w.links.iter().map(|l| l.item.as_deref()).collect();
    // The save's override wins over the map; the smelter doesn't take coal.
    assert_eq!(items, [Some("Desc_OreIron_C"), Some("Desc_Coal_C"), None]);
}

#[test]
fn old_and_broken_saves_fail_cleanly() {
    let game = tiny_game();
    let good = body(&factory(true), true);
    assert_eq!(read_world(&wrap(&good, 25, 4096), &game).unwrap_err(), WorldError::TooOld(25));
    assert!(WorldError::TooOld(25).to_string().contains("Satisfactory 1.0 or later"));
    assert_eq!(read_world(b"not a save", &game).unwrap_err(), WorldError::NotASave);
    let file = wrap(&good, 60, 4096);
    for n in (0..file.len()).step_by(37) {
        assert!(read_world(&file[..n], &game).is_err(), "cut at {n}");
    }
    // Corrupt bytes all over the body: errors or partial worlds, no panics.
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut rand = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for _ in 0..400 {
        let mut b = good.clone();
        for _ in 0..1 + rand() % 4 {
            let i = (rand() % b.len() as u64) as usize;
            b[i] = rand() as u8;
        }
        if let Ok(w) = read_world(&wrap(&b, 60, 4096), &game) {
            assert!(w.machines.len() <= 4);
        }
    }
}

// ----------------------------------------------------------------- real saves

fn real_game() -> Option<GameData> {
    let dir = std::env::var("SATISFACTORY_DIR").unwrap_or_else(|_| r"C:\Program Files (x86)\Steam\steamapps\common\Satisfactory".into());
    crate::docs::load_game_data(std::path::Path::new(&dir)).ok()
}

/// Real saves, if present: FB_TEST_SAVES is a folder holding early.sav,
/// late.sav and old-u4.sav (each optional). Needs the game's Docs too.
#[test]
fn real_saves() {
    let Ok(dir) = std::env::var("FB_TEST_SAVES") else { return };
    let Some(game) = real_game() else {
        eprintln!("FB_TEST_SAVES set but the game's Docs weren't found (SATISFACTORY_DIR); skipping");
        return;
    };
    let dir = std::path::Path::new(&dir);
    if let Ok(bytes) = std::fs::read(dir.join("old-u4.sav")) {
        assert_eq!(read_world(&bytes, &game).unwrap_err(), WorldError::TooOld(25));
    }
    for (file, objects, levels) in [("early.sav", 5_931, 1_045), ("late.sav", 147_553, 3_982)] {
        let Ok(bytes) = std::fs::read(dir.join(file)) else { continue };
        let t = std::time::Instant::now();
        let (w, stats) = read_world_stats(&bytes, &game).unwrap();
        let took = t.elapsed();
        let json = serde_json::to_string(&w).unwrap();
        eprintln!(
            "{file}: {:?}, {} machines, {} carriers ({} points), {} links ({} with item), {} structures, {} overrides, {} skipped; JSON {} KB; {stats:?}",
            took,
            w.machines.len(),
            w.carriers.len(),
            w.carriers.iter().map(|c| c.points.len()).sum::<usize>(),
            w.links.len(),
            w.links.iter().filter(|l| l.item.is_some()).count(),
            w.structures.len(),
            w.node_overrides.len(),
            w.skipped,
            json.len() / 1024
        );
        assert_eq!((stats.objects, stats.levels), (objects, levels), "{file}");
        assert_eq!(w.skipped, 0, "{file}");
        // Every map point inside the map (with a little slack).
        let inside = |p: &[f32; 2]| (-330_000.0..=430_000.0).contains(&p[0]) && (-380_000.0..=380_000.0).contains(&p[1]);
        let outside: Vec<_> = w.carriers.iter().filter(|c| !c.points.iter().all(inside)).map(|c| (&c.class, c.points.clone())).take(5).collect();
        assert!(outside.is_empty(), "{file}: {outside:?}");
        assert!(w.machines.iter().all(|m| inside(&[m.x, m.y])));

        // Belt and pipe ends meet other ends, or the buildings they feed: a
        // wrong spline transform would scatter them.
        let mut grid: HashMap<(i32, i32), Vec<[f32; 2]>> = HashMap::new();
        let cell = |p: [f32; 2]| ((p[0] / 2500.0).floor() as i32, (p[1] / 2500.0).floor() as i32);
        let ends: Vec<[f32; 2]> = w.carriers.iter().flat_map(|c| [c.points[0], *c.points.last().unwrap()]).collect();
        let sites: Vec<[f32; 2]> = w.machines.iter().map(|m| [m.x, m.y]).chain(w.structures.iter().map(|s| [s.x, s.y])).collect();
        for &p in ends.iter().chain(&sites) {
            grid.entry(cell(p)).or_default().push(p);
        }
        let near = |p: [f32; 2], r: f32| {
            let (cx, cy) = cell(p);
            let mut hits = 0;
            for dx in -1..=1 {
                for dy in -1..=1 {
                    hits += grid.get(&(cx + dx, cy + dy)).map_or(0, |v| v.iter().filter(|q| (q[0] - p[0]).hypot(q[1] - p[1]) <= r).count());
                }
            }
            hits
        };
        // Itself counts once; another end within 1 m, or a building within 25 m.
        let met = ends.iter().filter(|&&p| near(p, 100.0) >= 2 || sites.iter().any(|q| (q[0] - p[0]).hypot(q[1] - p[1]) <= 2500.0)).count();
        let lasts: Vec<[f32; 2]> = w.carriers.iter().filter(|c| !c.class.contains("Lift")).map(|c| *c.points.last().unwrap()).collect();
        let joined = lasts.iter().filter(|&&p| near(p, 100.0) >= 2).count();
        eprintln!("{file}: {met} of {} carrier ends meet something; {joined} of {} belt/pipe last points are within 1 m of another end", ends.len(), lasts.len());
        assert!(met * 100 >= ends.len() * 95, "{file}: {met} of {}", ends.len());

        // Production machines (with a crafting recipe that takes something)
        // mostly have an input whose item is known.
        let fed: HashSet<u32> = w.links.iter().filter(|l| l.item.is_some()).map(|l| l.to).collect();
        let crafting: Vec<&Machine> = w
            .machines
            .iter()
            .filter(|m| m.recipe.as_ref().and_then(|r| game.recipes.get(r)).is_some_and(|r| !r.is_generator() && !r.ingredients.is_empty()))
            .collect();
        let with_input = crafting.iter().filter(|m| fed.contains(&m.id)).count();
        eprintln!("{file}: {with_input} of {} crafting machines have a known input", crafting.len());
        assert!(with_input * 10 >= crafting.len() * 8, "{file}: {with_input} of {}", crafting.len());
        assert!(json.len() < 8 * 1024 * 1024, "{file}: JSON {} bytes", json.len());

        if file == "late.sav" {
            assert_eq!(stats.buildables, 38_419);
            let count = |class: &str| w.machines.iter().filter(|m| m.class == class && m.recipe.is_some()).count();
            for (class, n) in [
                ("Build_ConstructorMk1_C", 599),
                ("Build_AssemblerMk1_C", 229),
                ("Build_SmelterMk1_C", 275),
                ("Build_FoundryMk1_C", 98),
                ("Build_OilRefinery_C", 160),
                ("Build_ManufacturerMk1_C", 38),
                ("Build_Blender_C", 25),
                ("Build_Converter_C", 12),
                ("Build_HadronCollider_C", 13),
                ("Build_QuantumEncoder_C", 5),
                ("Build_Packager_C", 2),
            ] {
                assert_eq!(count(class), n, "{class}");
            }
            let manufacturers = w.machines.iter().filter(|m| m.class == "Build_ManufacturerMk1_C");
            assert!(manufacturers.clone().any(|m| m.clock == 2.5));
            assert_eq!(manufacturers.filter(|m| m.clock != 1.0).count(), 16, "Manufacturers with mCurrentPotential != 1");
            assert!(w.machines.iter().any(|m| m.boost == 2.0));
            let extractors = w.machines.iter().filter(|m| m.class.starts_with("Build_Miner") || m.class == "Build_OilPump_C").count();
            let on_nodes = w.machines.iter().filter(|m| m.node.as_deref().is_some_and(|n| n.starts_with("BP_ResourceNode"))).count();
            assert!(on_nodes >= extractors, "{on_nodes} on nodes, {extractors} miners and oil pumps");
        }
        if file == "early.sav" {
            assert!(!w.node_overrides.is_empty());
        }
    }
}

