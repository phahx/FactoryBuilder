//! Reads what a save has built (see `fb_model::world`): machines with their
//! recipes and clock speeds, belts, lifts and pipes as lines, and the item
//! flow between machines.
//!
//! Layout of a 1.x save (checked against save version 60, and against the
//! TypeScript parser github.com/etothepii4/satisfactory-file-parser):
//!
//! * The header (see `save.rs`), then zlib chunks. Inflated, they are the body.
//! * Body: i32 size, i32 padding (save version >= 37), FSaveObjectVersionData
//!   (>= 53), the validation grids (>= 38), i32 level count, then that many
//!   levels each preceded by its name, then the persistent level.
//! * Level: i32 TOC length, i32 0, the TOC, i32 data length, i32 0, the data.
//!   Sublevels then have their own version (>= 51), a collectables list and,
//!   from 53, optional FSaveObjectVersionData. The persistent level has the
//!   destroyed-actors map instead.
//! * TOC: one header per object: class path, root, instance name, flags
//!   (level version >= 49); entities add a transform, components their parent.
//! * Data: one payload per object, in TOC order, each with its own version
//!   and size, so an object that can't be understood is skipped by size.
//! * Properties: a list of tags ending in "None". From object version 53 with
//!   UE5 package version >= 1012 each tag carries its complete type as a tree
//!   (`FPropertyTagNode`); older saves use the legacy tag.
//!
//! The body of a late-game save is ~180 MB, so it is never inflated whole:
//! chunks are inflated as the reader walks forward, and only small sublevels
//! and the objects this module cares about are copied out.

use crate::save::{read_header, Chunks, SaveError};
use fb_model::world::{Carrier, Link, Machine, NodeOverride, Structure, World};
use fb_model::{GameData, Purity};
use std::collections::{BTreeSet, HashMap, HashSet};

/// Satisfactory 1.0's save version. Older saves use other layouts.
const MIN_SAVE_VERSION: i32 = 46;
/// `SaveCustomVersion::SerializeDataPackageVersionAndCustomVersions`.
const VERSION_DATA: i32 = 53;
/// `EUnrealEngineObjectUE5Version::PROPERTY_TAG_COMPLETE_TYPE_NAME`.
const COMPLETE_TAG_UE5: i32 = 1012;
const MAX_STRING: i32 = 64 * 1024;
const MAX_LEVELS: i32 = 1_000_000;
const MAX_OBJECTS: i32 = 20_000_000;
const MAX_LIST: i32 = 10_000_000;
const MAX_PROPS: usize = 100_000;
const MAX_TYPE_DEPTH: u32 = 8;
const MAX_TYPE_NODES: u32 = 64;
const MAX_SPLINE_POINTS: usize = 10_000;
/// Belt and pipe curves get at most this many extra points per segment.
const MAX_CURVE_POINTS: usize = 6;

#[derive(Debug, PartialEq)]
pub enum WorldError {
    NotASave,
    /// Saved before Satisfactory 1.0 (the save version).
    TooOld(i32),
    Damaged(&'static str),
}

impl std::fmt::Display for WorldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorldError::NotASave => write!(f, "That isn't a Satisfactory save file."),
            WorldError::TooOld(v) => write!(
                f,
                "That save is from before Satisfactory 1.0 (save version {v}), so its world can't be read. \
                 Load it in Satisfactory 1.0 or later and save it again, then open the new save."
            ),
            WorldError::Damaged(why) => write!(f, "That save file couldn't be read ({why})."),
        }
    }
}

impl From<SaveError> for WorldError {
    fn from(e: SaveError) -> Self {
        match e {
            SaveError::NotASave => WorldError::NotASave,
            SaveError::Damaged(why) => WorldError::Damaged(why),
            SaveError::NoProgress => WorldError::Damaged("no data"),
        }
    }
}

// ------------------------------------------------------------- byte readers

/// Malformed object data (the object is skipped, the save goes on).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Bad;
type R<T> = Result<T, Bad>;

/// An Unreal `FString` as stored: Latin-1, or UTF-16 when the length is
/// negative. Without the terminating NUL.
#[derive(Debug, Clone, Copy)]
enum FStr<'a> {
    Narrow(&'a [u8]),
    Wide(&'a [u8]),
}

impl FStr<'_> {
    fn is(&self, s: &str) -> bool {
        match self {
            FStr::Narrow(b) => *b == s.as_bytes(),
            FStr::Wide(b) => {
                b.len() == s.len() * 2 && b.as_chunks::<2>().0.iter().zip(s.bytes()).all(|(u, c)| u16::from_le_bytes([u[0], u[1]]) == c as u16)
            }
        }
    }
    fn string(&self) -> String {
        match self {
            FStr::Narrow(b) => b.iter().map(|&c| c as char).collect(),
            FStr::Wide(b) => {
                let units: Vec<u16> = b.as_chunks::<2>().0.iter().map(|u| u16::from_le_bytes([u[0], u[1]])).collect();
                String::from_utf16_lossy(&units)
            }
        }
    }
}

/// Byte count of an `FString` body from its length field.
fn fstr_bytes(n: i32) -> R<usize> {
    if !(-MAX_STRING..=MAX_STRING).contains(&n) {
        return Err(Bad);
    }
    Ok(if n >= 0 { n as usize } else { (-n) as usize * 2 })
}

/// The string in `raw` (the body after a length field of `n`).
fn fstr_from(n: i32, raw: &[u8]) -> R<FStr<'_>> {
    if n == 0 {
        return Ok(FStr::Narrow(&[]));
    }
    if n > 0 {
        let (body, nul) = raw.split_at(raw.len() - 1);
        if nul != [0] {
            return Err(Bad);
        }
        Ok(FStr::Narrow(body))
    } else {
        let (body, nul) = raw.split_at(raw.len() - 2);
        if nul != [0, 0] {
            return Err(Bad);
        }
        Ok(FStr::Wide(body))
    }
}

/// Bounds-checked little-endian reader over a slice.
#[derive(Clone, Copy)]
struct Cur<'a> {
    b: &'a [u8],
    o: usize,
}

impl<'a> Cur<'a> {
    fn new(b: &'a [u8]) -> Self {
        Cur { b, o: 0 }
    }
    fn left(&self) -> usize {
        self.b.len() - self.o
    }
    fn take(&mut self, n: usize) -> R<&'a [u8]> {
        let end = self.o.checked_add(n).ok_or(Bad)?;
        let s = self.b.get(self.o..end).ok_or(Bad)?;
        self.o = end;
        Ok(s)
    }
    fn arr<const N: usize>(&mut self) -> R<[u8; N]> {
        self.take(N)?.try_into().map_err(|_| Bad)
    }
    fn u8(&mut self) -> R<u8> {
        Ok(self.arr::<1>()?[0])
    }
    fn i32(&mut self) -> R<i32> {
        Ok(i32::from_le_bytes(self.arr()?))
    }
    fn f32(&mut self) -> R<f32> {
        Ok(f32::from_le_bytes(self.arr()?))
    }
    fn f64(&mut self) -> R<f64> {
        Ok(f64::from_le_bytes(self.arr()?))
    }
    fn fstr(&mut self) -> R<FStr<'a>> {
        let n = self.i32()?;
        let raw = self.take(fstr_bytes(n)?)?;
        fstr_from(n, raw)
    }
    /// An object reference (level name, path name): returns the path.
    fn objref(&mut self) -> R<FStr<'a>> {
        self.fstr()?;
        self.fstr()
    }
}

/// The body, inflated chunk by chunk as it is read. Only the bytes not yet
/// read (plus the current chunk) are kept.
struct Stream<'a> {
    chunks: Option<Chunks<'a>>,
    buf: Vec<u8>,
    pos: usize,
    /// Body offset of `buf[0]`.
    base: u64,
}

fn damaged(_: Bad) -> WorldError {
    WorldError::Damaged("bad data")
}

impl<'a> Stream<'a> {
    fn from_chunks(chunks: Chunks<'a>) -> Self {
        Stream { chunks: Some(chunks), buf: Vec::new(), pos: 0, base: 0 }
    }
    fn from_vec(buf: Vec<u8>) -> Self {
        Stream { chunks: None, buf, pos: 0, base: 0 }
    }
    /// Offset in the body.
    fn at(&self) -> u64 {
        self.base + self.pos as u64
    }
    fn fill(&mut self, n: usize) -> Result<(), WorldError> {
        while self.buf.len() - self.pos < n {
            if self.pos > 0 {
                self.buf.drain(..self.pos);
                self.base += self.pos as u64;
                self.pos = 0;
            }
            match self.chunks.as_mut().and_then(|c| c.next()) {
                Some(Ok(data)) => self.buf.extend_from_slice(&data),
                Some(Err(e)) => return Err(e.into()),
                None => return Err(WorldError::Damaged("the data ends early")),
            }
        }
        Ok(())
    }
    fn take(&mut self, n: usize) -> Result<&[u8], WorldError> {
        self.fill(n)?;
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn skip(&mut self, mut n: u64) -> Result<(), WorldError> {
        loop {
            let have = (self.buf.len() - self.pos) as u64;
            if have >= n {
                self.pos += n as usize;
                return Ok(());
            }
            n -= have;
            self.pos = self.buf.len();
            self.fill(1)?;
        }
    }
    fn skip_to(&mut self, at: u64) -> Result<(), WorldError> {
        match at.checked_sub(self.at()) {
            Some(n) => self.skip(n),
            None => Err(WorldError::Damaged("bad sizes")),
        }
    }
    fn i32(&mut self) -> Result<i32, WorldError> {
        let b = self.take(4)?;
        Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    /// A count that must be within `0..=max`.
    fn count(&mut self, max: i32) -> Result<usize, WorldError> {
        let n = self.i32()?;
        if !(0..=max).contains(&n) {
            return Err(WorldError::Damaged("bad count"));
        }
        Ok(n as usize)
    }
    fn string(&mut self) -> Result<String, WorldError> {
        let n = self.i32()?;
        let len = fstr_bytes(n).map_err(damaged)?;
        let raw = self.take(len)?;
        Ok(fstr_from(n, raw).map_err(damaged)?.string())
    }
    fn skip_string(&mut self) -> Result<(), WorldError> {
        let n = self.i32()?;
        let len = fstr_bytes(n).map_err(damaged)?;
        self.skip(len as u64)
    }
    /// `FSaveObjectVersionData`; returns its UE5 package version.
    fn version_data(&mut self) -> Result<i32, WorldError> {
        self.take(4)?; // its own version
        self.i32()?; // UE4 package version
        let ue5 = self.i32()?;
        self.take(4)?; // licensee version
        self.take(2 * 3 + 4)?; // engine major, minor, patch, changelist
        self.skip_string()?; // engine branch
        let n = self.count(MAX_LIST)?;
        self.skip(n as u64 * 20)?; // custom versions: guid + i32
        Ok(ue5)
    }
}

// --------------------------------------------------------------- properties

/// One property tag. `ty` is the type and, when present, its first child
/// and first grandchild: `ArrayProperty` / `StructProperty` /
/// `SplinePointData` or `ByteProperty` / `EResourcePurity`.
#[derive(Debug, Clone, Copy)]
struct Tag<'a> {
    name: FStr<'a>,
    ty: [Option<FStr<'a>>; 3],
    size: usize,
    /// BoolProperty's value, which lives in the tag.
    #[cfg_attr(not(test), allow(dead_code))]
    bool_val: bool,
}

impl Tag<'_> {
    fn ty_is(&self, level: usize, s: &str) -> bool {
        self.ty[level].is_some_and(|t| t.is(s))
    }
}

fn type_node<'a>(c: &mut Cur<'a>, depth: u32, slot: usize, ty: &mut [Option<FStr<'a>>; 3], nodes: &mut u32) -> R<()> {
    *nodes += 1;
    if depth > MAX_TYPE_DEPTH || *nodes > MAX_TYPE_NODES {
        return Err(Bad);
    }
    let name = c.fstr()?;
    let children = c.i32()?;
    if !(0..=16).contains(&children) {
        return Err(Bad);
    }
    if slot < 3 {
        ty[slot] = Some(name);
    }
    for k in 0..children {
        let next = if k == 0 && slot < 2 { slot + 1 } else { 3 };
        type_node(c, depth + 1, next, ty, nodes)?;
    }
    Ok(())
}

/// Reads a tag header; `None` at the end of the list. The value (`size`
/// bytes) follows.
fn read_tag<'a>(c: &mut Cur<'a>, complete: bool) -> R<Option<Tag<'a>>> {
    let name = c.fstr()?;
    if name.is("None") {
        return Ok(None);
    }
    let mut ty = [None; 3];
    let mut bool_val = false;
    let size;
    if complete {
        let mut nodes = 0;
        type_node(c, 0, 0, &mut ty, &mut nodes)?;
        size = c.i32()?;
        let flags = c.u8()?;
        if flags & 0x01 != 0 {
            c.i32()?; // array index
        }
        if flags & 0x02 != 0 {
            c.take(16)?; // property guid
        }
        if flags & 0x04 != 0 {
            // Property extensions: not seen in any save checked, so their
            // layout is unverified. Give up on this object rather than guess.
            return Err(Bad);
        }
        bool_val = flags & 0x10 != 0;
    } else {
        let t = c.fstr()?;
        ty[0] = Some(t);
        size = c.i32()?;
        c.i32()?; // array index
        if t.is("ArrayProperty") || t.is("SetProperty") || t.is("EnumProperty") {
            ty[1] = Some(c.fstr()?);
        } else if t.is("StructProperty") {
            ty[1] = Some(c.fstr()?);
            c.take(16)?; // struct guid
        } else if t.is("BoolProperty") {
            bool_val = c.u8()? != 0;
        } else if t.is("ByteProperty") {
            let e = c.fstr()?;
            if !e.is("None") {
                ty[1] = Some(e);
            }
        } else if t.is("MapProperty") {
            ty[1] = Some(c.fstr()?);
            c.fstr()?;
        }
        if c.u8()? != 0 {
            c.take(16)?; // property guid
        }
    }
    if size < 0 {
        return Err(Bad);
    }
    Ok(Some(Tag { name, ty, size: size as usize, bool_val }))
}

/// Walks a property list, handing each tag and its value to `f`.
fn props<'a>(c: &mut Cur<'a>, complete: bool, mut f: impl FnMut(&Tag<'a>, Cur<'a>) -> R<()>) -> R<()> {
    for _ in 0..MAX_PROPS {
        let Some(tag) = read_tag(c, complete)? else { return Ok(()) };
        let value = Cur::new(c.take(tag.size)?);
        f(&tag, value)?;
    }
    Err(Bad)
}

fn float_value(t: &Tag, mut v: Cur) -> R<f32> {
    if t.ty_is(0, "FloatProperty") {
        v.f32()
    } else if t.ty_is(0, "DoubleProperty") {
        Ok(v.f64()? as f32)
    } else {
        Err(Bad)
    }
}

fn object_value(t: &Tag, mut v: Cur) -> R<String> {
    if !t.ty_is(0, "ObjectProperty") {
        return Err(Bad);
    }
    Ok(v.objref()?.string())
}

/// An enum held as a name: ByteProperty with an enum type, or EnumProperty.
fn enum_value(t: &Tag, mut v: Cur) -> R<String> {
    let named = (t.ty_is(0, "ByteProperty") && t.ty[1].is_some()) || t.ty_is(0, "EnumProperty");
    if !named {
        return Err(Bad);
    }
    Ok(v.fstr()?.string())
}

/// `Vector` struct value: three doubles (save version >= 41) or floats.
fn vector(mut v: Cur) -> R<[f64; 3]> {
    match v.left() {
        24 => Ok([v.f64()?, v.f64()?, v.f64()?]),
        12 => Ok([v.f32()? as f64, v.f32()? as f64, v.f32()? as f64]),
        _ => Err(Bad),
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct SplinePoint {
    location: [f64; 3],
    arrive: [f64; 3],
    leave: [f64; 3],
}

/// `mSplineData`: ArrayProperty of StructProperty `SplinePointData`, each
/// element a property list with Location, ArriveTangent and LeaveTangent.
fn spline_value(t: &Tag, mut v: Cur, complete: bool) -> R<Vec<SplinePoint>> {
    if !t.ty_is(0, "ArrayProperty") || !t.ty_is(1, "StructProperty") {
        return Err(Bad);
    }
    let count = v.i32()?;
    let struct_ok = if complete {
        t.ty_is(2, "SplinePointData")
    } else {
        // Legacy arrays of structs repeat a full tag before the elements.
        let inner = read_tag(&mut v, false)?.ok_or(Bad)?;
        inner.ty_is(1, "SplinePointData")
    };
    if !struct_ok || count < 0 || count as usize > MAX_SPLINE_POINTS {
        return Err(Bad);
    }
    let mut out = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let mut p = SplinePoint::default();
        props(&mut v, complete, |t, val| {
            if t.name.is("Location") {
                p.location = vector(val)?;
            } else if t.name.is("ArriveTangent") {
                p.arrive = vector(val)?;
            } else if t.name.is("LeaveTangent") {
                p.leave = vector(val)?;
            }
            Ok(())
        })?;
        out.push(p);
    }
    Ok(out)
}

/// The Translation of a `Transform` struct (`mTopTransform` on lifts).
fn transform_translation(t: &Tag, mut v: Cur, complete: bool) -> R<[f64; 3]> {
    if !t.ty_is(0, "StructProperty") || !t.ty_is(1, "Transform") {
        return Err(Bad);
    }
    let mut translation = None;
    props(&mut v, complete, |t, val| {
        if t.name.is("Translation") {
            translation = Some(vector(val)?);
        }
        Ok(())
    })?;
    translation.ok_or(Bad)
}

/// An array of object references.
fn object_array(t: &Tag, mut v: Cur) -> R<Vec<String>> {
    if !t.ty_is(0, "ArrayProperty") || !t.ty_is(1, "ObjectProperty") {
        return Err(Bad);
    }
    let count = v.i32()?;
    if count < 0 || count as usize > v.left() / 8 {
        return Err(Bad);
    }
    (0..count).map(|_| Ok(v.objref()?.string())).collect()
}

/// `mWireInstances`: an array of structs, each with the wire's `Locations`
/// (a fixed array of two vectors, one tag each).
fn wire_ends(t: &Tag, mut v: Cur, complete: bool) -> R<Vec<[f64; 3]>> {
    if !t.ty_is(0, "ArrayProperty") || !t.ty_is(1, "StructProperty") {
        return Err(Bad);
    }
    let count = v.i32()?;
    if !complete {
        read_tag(&mut v, false)?.ok_or(Bad)?;
    }
    if !(0..=4).contains(&count) {
        return Err(Bad);
    }
    let mut out = Vec::new();
    for _ in 0..count {
        props(&mut v, complete, |t, val| {
            if t.name.is("Locations") {
                out.push(vector(val)?);
            }
            Ok(())
        })?;
    }
    Ok(out)
}

fn purity_from(name: &str) -> Option<Purity> {
    // "EResourcePurity::RP_Pure" or just "RP_Pure"; the game spells it Inpure.
    match name.rsplit("::").next().unwrap_or(name) {
        "RP_Inpure" | "RP_Impure" => Some(Purity::Impure),
        "RP_Normal" => Some(Purity::Normal),
        "RP_Pure" => Some(Purity::Pure),
        _ => None,
    }
}

/// What's after the last '.': the class of a class path, or an object's
/// own name in a path.
fn last_part(path: &str) -> &str {
    path.rsplit('.').next().unwrap_or(path)
}

// ------------------------------------------------------------------ objects

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CarrierKind {
    Belt,
    Lift,
    Pipe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Machine,
    Carrier(CarrierKind),
    /// Splitters, mergers, pipe junctions, pumps and valves: flow goes on.
    Pass,
    /// Storage, stations, the sink...: flow ends here.
    End,
    /// Resource nodes: only their overrides are read.
    Node,
    /// Power poles, switches, batteries; the HUB's own burners.
    Power,
    /// Power lines.
    Wire,
}

/// Classes whose objects matter, from the game data.
struct Classes<'g> {
    game: &'g GameData,
    extractors: HashSet<&'g str>,
}

/// Buildings that get a mark on the map and end a chain of belts or pipes.
const END_MARKS: &[&str] = &[
    "Build_Storage",
    "Build_CentralStorage",
    "Build_IndustrialTank",
    "Build_PipeStorageTank",
    "Build_TrainDockingStation",
    "Build_TrainStation",
    "Build_TruckStation",
    "Build_DroneStation",
    "Build_ResourceSink",
    "Build_SpaceElevator",
    "Build_FrackingSmasher",
    "Build_TradingPost",
];

/// Power buildings that get a mark (or their render) on the map.
const POWER_MARKS: &[&str] = &["Build_PowerPole", "Build_PowerTower", "Build_PowerSwitch", "Build_PriorityPowerSwitch", "Build_PowerStorage"];

/// Switches: their two sides are one circuit only while switched on.
const SWITCHES: &[&str] = &["Build_PowerSwitch", "Build_PriorityPowerSwitch"];

/// Power components: connections (joined by wires) and what a building
/// draws or makes.
const POWER_CLASSES: [&str; 2] = ["FGPowerConnectionComponent", "FGPowerInfoComponent"];

const CONNECTION_CLASSES: [(&str, bool); 3] =
    [("FGFactoryConnectionComponent", false), ("FGPipeConnectionComponent", true), ("FGPipeConnectionFactory", true)];

impl<'g> Classes<'g> {
    fn new(game: &'g GameData) -> Self {
        Classes { game, extractors: game.extraction.extractors.iter().map(|e| e.class.as_str()).collect() }
    }
    fn is_extractor(&self, class: &str) -> bool {
        self.extractors.contains(class)
    }
    /// Role and whether it gets a map mark.
    fn entity(&self, class: &str) -> Option<(Role, bool)> {
        // The HUB's burners are part of its model: on a circuit, not drawn.
        if class.starts_with("Build_GeneratorIntegratedBiomass") {
            return Some((Role::Power, false));
        }
        if class.starts_with("Build_PowerLine") {
            return Some((Role::Wire, false));
        }
        if POWER_MARKS.iter().any(|p| class.starts_with(p)) {
            return Some((Role::Power, true));
        }
        if self.game.buildings.contains_key(class) || self.is_extractor(class) || class.starts_with("Build_Generator") {
            return Some((Role::Machine, false));
        }
        if class.starts_with("Build_ConveyorBelt") {
            return Some((Role::Carrier(CarrierKind::Belt), false));
        }
        if class.starts_with("Build_ConveyorLift") {
            return Some((Role::Carrier(CarrierKind::Lift), false));
        }
        if class.starts_with("Build_Pipeline_") || class.starts_with("Build_PipelineMK2") {
            return Some((Role::Carrier(CarrierKind::Pipe), false));
        }
        if ["Build_ConveyorAttachment", "Build_PipelineJunction", "Build_PipelinePump", "Build_Valve"].iter().any(|p| class.starts_with(p)) {
            return Some((Role::Pass, true));
        }
        if END_MARKS.iter().any(|p| class.starts_with(p)) {
            return Some((Role::End, true));
        }
        if class.starts_with("BP_ResourceNode") || class.starts_with("BP_FrackingSatellite") || class.starts_with("BP_FrackingCore") {
            return Some((Role::Node, false));
        }
        None
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Xf {
    /// Quaternion x, y, z, w.
    rot: [f64; 4],
    pos: [f64; 3],
    scale: [f64; 3],
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

impl Xf {
    fn from_toc(f: &[f32; 10]) -> Self {
        let d = |i: usize| f[i] as f64;
        let mut rot = [d(0), d(1), d(2), d(3)];
        let len = rot.iter().map(|v| v * v).sum::<f64>().sqrt();
        if len.is_finite() && len > 1e-6 {
            rot.iter_mut().for_each(|v| *v /= len);
        } else {
            rot = [0.0, 0.0, 0.0, 1.0];
        }
        Xf { rot, pos: [d(4), d(5), d(6)], scale: [d(7), d(8), d(9)] }
    }
    fn rotate(&self, v: [f64; 3]) -> [f64; 3] {
        let [x, y, z, w] = self.rot;
        let u = [x, y, z];
        let t = cross(u, v).map(|c| c * 2.0);
        let ut = cross(u, t);
        [v[0] + w * t[0] + ut[0], v[1] + w * t[1] + ut[1], v[2] + w * t[2] + ut[2]]
    }
    fn vector(&self, v: [f64; 3]) -> [f64; 3] {
        self.rotate([v[0] * self.scale[0], v[1] * self.scale[1], v[2] * self.scale[2]])
    }
    fn point(&self, v: [f64; 3]) -> [f64; 3] {
        let r = self.vector(v);
        [r[0] + self.pos[0], r[1] + self.pos[1], r[2] + self.pos[2]]
    }
    /// Rotation about Z, degrees.
    fn yaw(&self) -> f64 {
        let [x, y, z, w] = self.rot;
        (2.0 * (w * z + x * y)).atan2(1.0 - 2.0 * (y * y + z * z)).to_degrees()
    }
}

/// What the TOC says about an object worth reading.
enum Head {
    Entity { role: Role, mark: bool, class: String, name: String, xf: Xf },
    Conn { fluid: bool, name: String, parent: String },
    Power { name: String, parent: String },
}

/// What the properties say.
#[derive(Debug, Default)]
struct Info {
    recipe: Option<String>,
    potential: Option<f32>,
    boost: Option<f32>,
    extractable: Option<String>,
    fuel: Option<String>,
    spline: Vec<SplinePoint>,
    top: Option<[f64; 3]>,
    resource_override: Option<String>,
    purity_override: Option<Purity>,
    connected: Option<String>,
    /// Power connections: the power lines on them, and hidden links (the
    /// third rail of train tracks).
    wires: Vec<String>,
    hidden: Vec<String>,
    /// Power info: MW drawn now, and MW a generator can make now.
    target: Option<f32>,
    dynamic_capacity: Option<f32>,
    base_production: Option<f32>,
    /// Power Storage charge (MWh); a switch's state.
    power_store: Option<f32>,
    switch_on: Option<bool>,
    /// A power line's two ends.
    wire_ends: Vec<[f64; 3]>,
}

struct Ent {
    role: Role,
    mark: bool,
    class: String,
    name: String,
    xf: Xf,
    info: Info,
}

struct Conn {
    fluid: bool,
    name: String,
    parent: String,
    connected: Option<String>,
}

struct PowerComp {
    name: String,
    parent: String,
    info: Info,
}

/// Counts for tests and diagnostics.
#[derive(Debug, Default, Clone)]
pub(crate) struct Stats {
    pub objects: usize,
    pub levels: usize,
    /// Entities whose class starts with `Build_`.
    pub buildables: usize,
    /// Objects read with complete / legacy property tags.
    pub complete_tags: usize,
    pub legacy_tags: usize,
}

#[derive(Default)]
struct Acc {
    ents: Vec<Ent>,
    conns: Vec<Conn>,
    power: Vec<PowerComp>,
    overrides: Vec<NodeOverride>,
    skipped: usize,
    stats: Stats,
}

/// Reads one object's payload.
fn read_object(payload: &[u8], entity: bool, complete: bool) -> R<Info> {
    let mut c = Cur::new(payload);
    if entity {
        c.objref()?; // parent
        let n = c.i32()?;
        if n < 0 || n as usize > c.left() / 8 {
            return Err(Bad);
        }
        for _ in 0..n {
            c.objref()?;
        }
    }
    if complete {
        c.u8()?; // serialization control
    }
    let mut info = Info::default();
    props(&mut c, complete, |t, v| {
        let n = &t.name;
        if n.is("mCurrentRecipe") {
            info.recipe = Some(last_part(&object_value(t, v)?).to_string()).filter(|r| !r.is_empty());
        } else if n.is("mCurrentPotential") {
            info.potential = Some(float_value(t, v)?);
        } else if n.is("mCurrentProductionBoost") {
            info.boost = Some(float_value(t, v)?);
        } else if n.is("mExtractableResource") {
            info.extractable = Some(last_part(&object_value(t, v)?).to_string()).filter(|r| !r.is_empty());
        } else if n.is("mCurrentFuelClass") {
            info.fuel = Some(last_part(&object_value(t, v)?).to_string()).filter(|r| !r.is_empty());
        } else if n.is("mSplineData") {
            info.spline = spline_value(t, v, complete)?;
        } else if n.is("mTopTransform") {
            info.top = Some(transform_translation(t, v, complete)?);
        } else if n.is("mResourceClassOverride") {
            info.resource_override = Some(last_part(&object_value(t, v)?).to_string()).filter(|r| !r.is_empty());
        } else if n.is("mPurityOverride") || n.is("mPurityOverwrite") {
            info.purity_override = purity_from(&enum_value(t, v)?);
        } else if n.is("mConnectedComponent") {
            info.connected = Some(object_value(t, v)?).filter(|r| !r.is_empty());
        } else if n.is("mWires") {
            info.wires = object_array(t, v)?;
        } else if n.is("mHiddenConnections") {
            info.hidden = object_array(t, v)?;
        } else if n.is("mTargetConsumption") {
            info.target = Some(float_value(t, v)?);
        } else if n.is("mDynamicProductionCapacity") {
            info.dynamic_capacity = Some(float_value(t, v)?);
        } else if n.is("mBaseProduction") {
            info.base_production = Some(float_value(t, v)?);
        } else if n.is("mPowerStore") {
            info.power_store = Some(float_value(t, v)?);
        } else if n.is("mIsSwitchOn") && t.ty_is(0, "BoolProperty") {
            info.switch_on = Some(t.bool_val);
        } else if n.is("mWireInstances") {
            info.wire_ends = wire_ends(t, v, complete)?;
        }
        Ok(())
    })?;
    Ok(info)
}

/// Reads a level's TOC (`lv` = the level's save version).
fn read_toc(s: &mut Stream, lv: i32, classes: &Classes, stats: &mut Stats) -> Result<Vec<Option<Box<Head>>>, WorldError> {
    let count = s.count(MAX_OBJECTS)?;
    let mut heads = Vec::with_capacity(count.min(1 << 20));
    for _ in 0..count {
        let kind = s.i32()?;
        let class_path = s.string()?;
        s.skip_string()?; // root object
        let name = s.string()?;
        if lv >= 49 {
            s.take(4)?; // object flags
        }
        let class = last_part(&class_path);
        let head = match kind {
            1 => {
                s.take(4)?; // needs transform
                let t = s.take(40)?;
                let mut f = [0f32; 10];
                for (i, v) in f.iter_mut().enumerate() {
                    *v = f32::from_le_bytes([t[i * 4], t[i * 4 + 1], t[i * 4 + 2], t[i * 4 + 3]]);
                }
                s.take(4)?; // placed in level
                if class.starts_with("Build_") {
                    stats.buildables += 1;
                }
                classes.entity(class).map(|(role, mark)| Box::new(Head::Entity { role, mark, class: class.to_string(), name, xf: Xf::from_toc(&f) }))
            }
            0 => {
                let parent = s.string()?;
                if POWER_CLASSES.contains(&class) {
                    Some(Box::new(Head::Power { name, parent }))
                } else {
                    CONNECTION_CLASSES.iter().find(|(c, _)| *c == class).map(|&(_, fluid)| Box::new(Head::Conn { fluid, name, parent }))
                }
            }
            _ => return Err(WorldError::Damaged("unknown object type")),
        };
        heads.push(head);
    }
    stats.objects += count;
    Ok(heads)
}

/// Reads a level's data blob, which ends at body offset `end`. If it turns
/// out damaged, the objects of interest not read yet count as skipped.
fn read_data(s: &mut Stream, end: u64, heads: Vec<Option<Box<Head>>>, lv: i32, ue5: i32, acc: &mut Acc) -> Result<(), WorldError> {
    let mut pending = heads.iter().filter(|h| h.is_some()).count();
    let r = read_data_inner(s, end, heads, lv, ue5, acc, &mut pending);
    if r.is_err() {
        acc.skipped += pending;
    }
    r
}

fn read_data_inner(
    s: &mut Stream,
    end: u64,
    heads: Vec<Option<Box<Head>>>,
    lv: i32,
    ue5: i32,
    acc: &mut Acc,
    pending: &mut usize,
) -> Result<(), WorldError> {
    let count = s.count(MAX_OBJECTS)?;
    if count != heads.len() {
        return Err(WorldError::Damaged("object counts differ"));
    }
    let mut payload = Vec::new();
    for head in heads {
        let (version, _migrate) = if lv >= 38 { (s.i32()?, s.i32()?) } else { (lv, 0) };
        let size = s.i32()?;
        if size < 0 || s.at() + size as u64 > end {
            return Err(WorldError::Damaged("bad object size"));
        }
        if head.is_some() {
            payload.clear();
            payload.extend_from_slice(s.take(size as usize)?);
        } else {
            s.skip(size as u64)?;
        }
        let mut object_ue5 = None;
        if version >= VERSION_DATA && s.i32()? == 1 {
            object_ue5 = Some(s.version_data()?);
        }
        let Some(head) = head else { continue };
        *pending -= 1;
        let complete = version >= VERSION_DATA && object_ue5.unwrap_or(ue5) >= COMPLETE_TAG_UE5;
        if complete {
            acc.stats.complete_tags += 1;
        } else {
            acc.stats.legacy_tags += 1;
        }
        let entity = matches!(*head, Head::Entity { .. });
        let Ok(info) = read_object(&payload, entity, complete) else {
            acc.skipped += 1;
            continue;
        };
        match *head {
            Head::Entity { role: Role::Node, name, .. } => {
                if info.resource_override.is_some() || info.purity_override.is_some() {
                    acc.overrides.push(NodeOverride { id: last_part(&name).to_string(), resource: info.resource_override, purity: info.purity_override });
                }
            }
            Head::Entity { role, mark, class, name, xf } => acc.ents.push(Ent { role, mark, class, name, xf, info }),
            Head::Conn { fluid, name, parent } => acc.conns.push(Conn { fluid, name, parent, connected: info.connected }),
            Head::Power { name, parent } => acc.power.push(PowerComp { name, parent, info }),
        }
    }
    if s.at() != end {
        return Err(WorldError::Damaged("level size"));
    }
    Ok(())
}

/// Reads one level. A sublevel is small and copied whole (its version comes
/// after its data); the persistent level is read as it streams by.
fn read_level(s: &mut Stream, persistent: bool, sv: i32, ue5: i32, classes: &Classes, acc: &mut Acc) -> Result<(), WorldError> {
    let toc_len = s.count(i32::MAX)? as u64;
    if sv >= 37 {
        s.i32()?;
    }
    if persistent {
        let toc_end = s.at() + toc_len;
        let heads = read_toc(s, sv, classes, &mut acc.stats);
        // The TOC may hold more after the headers (the destroyed actors again).
        let heads = match heads {
            Ok(h) if s.at() <= toc_end => Some(h),
            _ => None,
        };
        s.skip_to(toc_end)?;
        let data_len = s.count(i32::MAX)? as u64;
        if sv >= 37 {
            s.i32()?;
        }
        let data_end = s.at() + data_len;
        match heads {
            // Keep what was read before any damage.
            Some(heads) => {
                let _ = read_data(s, data_end, heads, sv, ue5, acc);
            }
            None => acc.skipped += 1,
        }
        s.skip_to(data_end)?;
        // Destroyed actors: level name, then references.
        for _ in 0..s.count(MAX_LIST)? {
            s.skip_string()?;
            for _ in 0..s.count(MAX_LIST)? {
                s.skip_string()?;
                s.skip_string()?;
            }
        }
    } else {
        let toc = s.take(toc_len as usize)?.to_vec();
        let data_len = s.count(i32::MAX)?;
        if sv >= 37 {
            s.i32()?;
        }
        let data = s.take(data_len)?.to_vec();
        let lv = if sv >= 51 { s.i32()? } else { sv };
        for _ in 0..s.count(MAX_LIST)? {
            s.skip_string()?;
            s.skip_string()?;
        }
        let mut level_ue5 = ue5;
        if sv >= VERSION_DATA && s.i32()? >= 1 {
            level_ue5 = s.version_data()?;
        }
        // A damaged sublevel is skipped; its framing above was fine.
        let mut t = Stream::from_vec(toc);
        match read_toc(&mut t, lv, classes, &mut acc.stats) {
            Ok(heads) => {
                let mut d = Stream::from_vec(data);
                let end = data_len as u64;
                let _ = read_data(&mut d, end, heads, lv, level_ue5, acc);
            }
            Err(_) => acc.skipped += 1,
        }
    }
    acc.stats.levels += 1;
    Ok(())
}

fn read_body(bytes: &[u8], classes: &Classes) -> Result<(fb_model::saves::SaveSummary, Acc), WorldError> {
    let (summary, header_end) = read_header(bytes)?;
    let sv = summary.save_version;
    if sv < MIN_SAVE_VERSION {
        return Err(WorldError::TooOld(sv));
    }
    let chunks = Chunks::after_header(bytes, header_end).ok_or(WorldError::Damaged("no data"))?;
    let mut s = Stream::from_chunks(chunks);
    s.i32()?; // body size
    if sv >= 37 {
        s.i32()?;
    }
    let mut ue5 = 0;
    if sv >= VERSION_DATA {
        ue5 = s.version_data()?;
    }
    if sv >= 38 {
        // Validation grids: name, cell size, hash, then (name, hash) pairs.
        for _ in 0..s.count(MAX_LIST)? {
            s.skip_string()?;
            s.take(8)?;
            for _ in 0..s.count(MAX_LIST)? {
                s.skip_string()?;
                s.take(4)?;
            }
        }
    }
    let levels = s.count(MAX_LEVELS)?;
    let mut acc = Acc::default();
    for i in 0..=levels {
        let persistent = i == levels;
        if !persistent {
            s.skip_string()?; // level name
        }
        read_level(&mut s, persistent, sv, ue5, classes, &mut acc)?;
    }
    Ok((summary, acc))
}

// -------------------------------------------------------------- the world

/// Points to draw for a belt or pipe: its spline in world X/Y, with a few
/// extra points along curved segments.
fn spline_points(pts: &[SplinePoint], xf: &Xf) -> Vec<[f32; 2]> {
    let world: Vec<(_, _, _)> = pts.iter().map(|p| (xf.point(p.location), xf.vector(p.arrive), xf.vector(p.leave))).collect();
    let mut out: Vec<[f64; 2]> = Vec::new();
    for (i, w) in world.iter().enumerate() {
        if i > 0 {
            let (p0, _, t0) = world[i - 1];
            let (p1, t1, _) = *w;
            let chord = [p1[0] - p0[0], p1[1] - p0[1]];
            let len = (chord[0] * chord[0] + chord[1] * chord[1]).sqrt();
            let angle = |t: [f64; 3]| {
                let tl = (t[0] * t[0] + t[1] * t[1]).sqrt();
                if tl < 1e-6 || len < 1e-6 {
                    0.0
                } else {
                    ((t[0] * chord[0] + t[1] * chord[1]) / (tl * len)).clamp(-1.0, 1.0).acos().to_degrees()
                }
            };
            let bend = angle(t0).max(angle(t1));
            // One extra point per 15 degrees of bend, for segments that bend.
            let extra = if len < 50.0 || bend < 3.0 { 0 } else { ((bend / 15.0).ceil() as usize).clamp(1, MAX_CURVE_POINTS) };
            for k in 1..=extra {
                let a = k as f64 / (extra + 1) as f64;
                let (a2, a3) = (a * a, a * a * a);
                let (h00, h10, h01, h11) = (2.0 * a3 - 3.0 * a2 + 1.0, a3 - 2.0 * a2 + a, -2.0 * a3 + 3.0 * a2, a3 - a2);
                let c = |j: usize| h00 * p0[j] + h10 * t0[j] + h01 * p1[j] + h11 * t1[j];
                out.push([c(0), c(1)]);
            }
        }
        out.push([w.0[0], w.0[1]]);
    }
    let mut thin: Vec<[f32; 2]> = Vec::with_capacity(out.len());
    for p in out {
        if !(p[0].is_finite() && p[1].is_finite()) {
            continue;
        }
        let q = [p[0] as f32, p[1] as f32];
        // Drop points within 10 cm of the previous one (vertical runs).
        if thin.last().is_some_and(|l| (l[0] - q[0]).hypot(l[1] - q[1]) < 10.0) {
            continue;
        }
        thin.push(q);
    }
    if thin.len() == 1 {
        thin.push(thin[0]);
    }
    thin
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dir {
    In,
    Out,
    Any,
}

fn finite(v: f64) -> Option<f32> {
    Some(v as f32).filter(|v| v.is_finite())
}

/// Reads the world from a save file's bytes.
pub fn read_world(bytes: &[u8], game: &GameData) -> Result<World, WorldError> {
    read_world_stats(bytes, game).map(|(w, _)| w)
}

pub(crate) fn read_world_stats(bytes: &[u8], game: &GameData) -> Result<(World, Stats), WorldError> {
    let classes = Classes::new(game);
    let (save, acc) = read_body(bytes, &classes)?;
    let Acc { ents, conns, power, overrides, skipped, stats } = acc;
    let resource_of: HashMap<&str, &str> = overrides.iter().filter_map(|o| Some((o.id.as_str(), o.resource.as_deref()?))).collect();

    let mut world = World { save, skipped, ..Default::default() };

    // Machines (ids in order), carriers and marks.
    let mut machine_of: Vec<Option<u32>> = vec![None; ents.len()];
    for (i, e) in ents.iter().enumerate() {
        let [x, y, z] = e.xf.pos;
        let (Some(x), Some(y), Some(z)) = (finite(x), finite(y), finite(z)) else { continue };
        if e.mark {
            world.structures.push(Structure { class: e.class.clone(), x, y, yaw: finite(e.xf.yaw()).unwrap_or(0.0), circuit: None });
        }
        match e.role {
            Role::Machine => {
                let fuel_recipe = e.info.fuel.as_ref().map(|f| crate::power::generator_class(&e.class, f)).filter(|g| game.recipes.contains_key(g));
                let node = e.info.extractable.clone().filter(|n| n.starts_with("BP_"));
                let ratio = |v: Option<f32>| v.filter(|v| v.is_finite() && *v >= 0.0).unwrap_or(1.0);
                machine_of[i] = Some(world.machines.len() as u32);
                world.machines.push(Machine {
                    id: world.machines.len() as u32,
                    class: e.class.clone(),
                    recipe: e.info.recipe.clone().or(fuel_recipe),
                    clock: ratio(e.info.potential),
                    boost: ratio(e.info.boost),
                    x,
                    y,
                    z,
                    yaw: finite(e.xf.yaw()).unwrap_or(0.0),
                    node,
                });
            }
            Role::Carrier(kind) => {
                let points = match kind {
                    CarrierKind::Lift => {
                        let top = e.xf.point(e.info.top.unwrap_or([0.0; 3]));
                        match (finite(top[0]), finite(top[1])) {
                            (Some(tx), Some(ty)) => vec![[x, y], [tx, ty]],
                            _ => vec![[x, y], [x, y]],
                        }
                    }
                    _ => spline_points(&e.info.spline, &e.xf),
                };
                if points.len() >= 2 {
                    world.carriers.push(Carrier { class: e.class.clone(), fluid: kind == CarrierKind::Pipe, points, yaw: finite(e.xf.yaw()).unwrap_or(0.0) });
                }
            }
            _ => {}
        }
    }
    world.node_overrides = overrides.clone();
    circuits(game, &ents, &machine_of, &power, &mut world);

    // Connections: owner entity, the component on the other side, direction.
    let ent_by_name: HashMap<&str, u32> = ents.iter().enumerate().map(|(i, e)| (e.name.as_str(), i as u32)).collect();
    let conn_by_name: HashMap<&str, u32> = conns.iter().enumerate().map(|(i, c)| (c.name.as_str(), i as u32)).collect();
    let owner: Vec<Option<u32>> = conns.iter().map(|c| ent_by_name.get(c.parent.as_str()).copied()).collect();
    let mut peer: Vec<Option<u32>> =
        conns.iter().map(|c| c.connected.as_deref().and_then(|p| conn_by_name.get(p).copied())).collect();
    for a in 0..conns.len() {
        if let Some(b) = peer[a] {
            if peer[b as usize].is_none() {
                peer[b as usize] = Some(a as u32);
            }
        }
    }
    let mut comps: Vec<Vec<u32>> = vec![Vec::new(); ents.len()];
    for (c, o) in owner.iter().enumerate() {
        if let Some(o) = o {
            comps[*o as usize].push(c as u32);
        }
    }

    let recipe_of = |m: u32| world.machines[m as usize].recipe.as_ref().and_then(|r| game.recipes.get(r));
    // A machine's single unnamed pipe connection: extractors and machines
    // whose recipe only makes fluids push out; everything else takes in.
    let unnamed_pipe_dir = |e: &Ent, m: Option<u32>| -> Dir {
        if classes.is_extractor(&e.class) {
            return Dir::Out;
        }
        let fluid = |a: &fb_model::Amount| game.item_form(&a.item).is_fluid();
        match m.and_then(recipe_of) {
            Some(r) if r.products.iter().any(fluid) && !r.ingredients.iter().any(fluid) => Dir::Out,
            _ => Dir::In,
        }
    };
    let dir: Vec<Dir> = conns
        .iter()
        .enumerate()
        .map(|(c, conn)| {
            let Some(o) = owner[c] else { return Dir::Any };
            let e = &ents[o as usize];
            let side = last_part(&conn.name);
            if matches!(e.role, Role::Carrier(CarrierKind::Belt | CarrierKind::Lift)) {
                return match side {
                    "ConveyorAny0" => Dir::In,
                    "ConveyorAny1" => Dir::Out,
                    _ => Dir::Any,
                };
            }
            let lower = side.to_ascii_lowercase();
            if lower.contains("input") {
                Dir::In
            } else if lower.contains("output") {
                Dir::Out
            } else if conn.fluid && e.role == Role::Machine {
                unnamed_pipe_dir(e, machine_of[o as usize])
            } else {
                Dir::Any
            }
        })
        .collect();

    // Follow each machine output downstream to the machines it feeds.
    let mut found: BTreeSet<(u32, u32, bool)> = BTreeSet::new();
    let mut stamp: Vec<u32> = vec![u32::MAX; conns.len()];
    let mut stack: Vec<u32> = Vec::new();
    let mut walk = 0u32;
    for (start, conn) in conns.iter().enumerate() {
        let Some(from_ent) = owner[start] else { continue };
        let Some(from) = machine_of[from_ent as usize] else { continue };
        if dir[start] != Dir::Out {
            continue;
        }
        let fluid = conn.fluid;
        stack.clear();
        stack.extend(peer[start]);
        while let Some(e) = stack.pop() {
            let e = e as usize;
            if stamp[e] == walk || conns[e].fluid != fluid {
                continue;
            }
            stamp[e] = walk;
            let Some(x) = owner[e] else { continue };
            let x = x as usize;
            match ents[x].role {
                Role::Machine => {
                    if let Some(to) = machine_of[x] {
                        if to != from && dir[e] != Dir::Out {
                            found.insert((from, to, fluid));
                        }
                    }
                }
                Role::Carrier(_) | Role::Pass => {
                    // Belts flow one way; pipes go anywhere they connect.
                    if !fluid && dir[e] == Dir::Out {
                        continue;
                    }
                    for &f in &comps[x] {
                        let f = f as usize;
                        if f != e && conns[f].fluid == fluid && (fluid || dir[f] != Dir::In) {
                            stack.extend(peer[f]);
                        }
                    }
                }
                _ => {}
            }
        }
        walk += 1;
    }

    // Generators holding no fuel store no fuel class: take the recipe from
    // the only fuel the building burns, or the only one fed to it.
    let mut feeders: HashMap<u32, Vec<u32>> = HashMap::new();
    for &(from, to, _) in &found {
        feeders.entry(to).or_default().push(from);
    }
    for i in 0..world.machines.len() {
        if world.machines[i].recipe.is_some() {
            continue;
        }
        let candidates = generator_recipes(game, &world.machines[i].class);
        let pick = match candidates.len() {
            0 => None,
            1 => Some(candidates[0]),
            _ => {
                let fed: HashSet<&str> =
                    feeders.get(&(i as u32)).into_iter().flatten().flat_map(|&f| produced(game, &classes, &resource_of, &world.machines[f as usize])).collect();
                // Ingredients every candidate shares (water) tell nothing.
                let telling = |a: &&fb_model::Amount| !candidates.iter().all(|r| r.ingredients.iter().any(|b| b.item == a.item));
                let fits: Vec<&&fb_model::Recipe> =
                    candidates.iter().filter(|r| r.ingredients.iter().filter(telling).any(|a| fed.contains(a.item.as_str()))).collect();
                (fits.len() == 1).then(|| *fits[0])
            }
        };
        world.machines[i].recipe = pick.map(|r| r.class.clone());
    }

    // What flows: the producer's products that the consumer takes in.
    let ent_of_machine: HashMap<u32, usize> = machine_of.iter().enumerate().filter_map(|(i, m)| Some(((*m)?, i))).collect();
    for (from, to, fluid) in found {
        let (pm, cm) = (&world.machines[from as usize], &world.machines[to as usize]);
        let takes = taken(game, cm, ents[ent_of_machine[&to]].info.fuel.as_deref());
        let mut items: Vec<&str> = produced(game, &classes, &resource_of, pm)
            .into_iter()
            .filter(|i| game.item_form(i).is_fluid() == fluid && takes.contains(i))
            .collect();
        items.sort_unstable();
        items.dedup();
        let item = if items.len() == 1 { Some(items[0].to_string()) } else { None };
        world.links.push(Link { from, to, item });
    }
    Ok((world, stats))
}

/// Joins power connections into circuits (through power lines, the third
/// rail, and the buildings they belong to) and totals each circuit.
fn circuits(game: &GameData, ents: &[Ent], machine_of: &[Option<u32>], power: &[PowerComp], world: &mut World) {
    fn id<'a>(s: &'a str, index: &mut HashMap<&'a str, usize>) -> usize {
        let n = index.len();
        *index.entry(s).or_insert(n)
    }
    let mut index: HashMap<&str, usize> = HashMap::new();
    let ent_by_name: HashMap<&str, usize> = ents.iter().enumerate().map(|(i, e)| (e.name.as_str(), i)).collect();
    let switch_open = |parent: &str| {
        ent_by_name.get(parent).is_some_and(|&e| SWITCHES.iter().any(|s| ents[e].class.starts_with(s)) && ents[e].info.switch_on != Some(true))
    };
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    for p in power {
        let c = id(&p.name, &mut index);
        if !switch_open(&p.parent) {
            let o = id(&p.parent, &mut index);
            pairs.push((c, o));
        }
        for w in p.info.wires.iter().chain(&p.info.hidden) {
            let o = id(w, &mut index);
            pairs.push((c, o));
        }
    }
    for e in ents.iter().filter(|e| e.role == Role::Wire) {
        id(&e.name, &mut index);
    }
    let mut uf = petgraph::unionfind::UnionFind::<usize>::new(index.len().max(1));
    for (a, b) in pairs {
        uf.union(a, b);
    }
    let root = |s: &str| index.get(s).map(|&i| uf.find(i));

    // Circuits that have something on them, in the order first met.
    let mut circuit_of_root: HashMap<usize, u32> = HashMap::new();
    let mut circuit = |r: usize, world: &mut World| -> u32 {
        *circuit_of_root.entry(r).or_insert_with(|| {
            world.circuits.push(fb_model::world::Circuit::default());
            (world.circuits.len() - 1) as u32
        })
    };
    let generator = |class: &str| class.starts_with("Build_Generator");
    let mut seen_owner: HashSet<&str> = HashSet::new();
    for p in power {
        let Some(r) = root(&p.name) else { continue };
        let c = circuit(r, world) as usize;
        let Some(&e) = ent_by_name.get(p.parent.as_str()) else {
            // Something the map doesn't draw (a drop pod, a radar tower...).
            let target = p.info.target.unwrap_or(0.0).max(0.0);
            if target > 0.0 && seen_owner.insert(p.parent.as_str()) {
                world.circuits[c].consumption += target;
                world.circuits[c].max_consumption += target;
                world.circuits[c].consumers += 1;
            }
            continue;
        };
        let ent = &ents[e];
        if let Some(m) = machine_of[e] {
            world.machine_circuit.insert(m, c as u32);
        }
        if ent.class.starts_with("Build_PowerStorage") && seen_owner.insert(p.parent.as_str()) {
            let cir = &mut world.circuits[c];
            cir.batteries += 1;
            cir.stored_mwh += ent.info.power_store.unwrap_or(0.0).max(0.0);
            cir.battery_mwh += game.power_store_mwh.get(&ent.class).copied().unwrap_or(0.0) as f32;
        }
        let made = p.info.dynamic_capacity.unwrap_or(0.0).max(0.0) + p.info.base_production.unwrap_or(0.0).max(0.0);
        if generator(&ent.class) {
            if made > 0.0 || p.info.target.is_none() {
                let cir = &mut world.circuits[c];
                match cir.sources.iter_mut().find(|s| s.class == ent.class) {
                    Some(s) if seen_owner.insert(p.parent.as_str()) => {
                        s.count += 1;
                        s.mw += made;
                    }
                    Some(s) => s.mw += made,
                    None => {
                        seen_owner.insert(p.parent.as_str());
                        cir.sources.push(fb_model::world::PowerSource { class: ent.class.clone(), count: 1, mw: made });
                    }
                }
                cir.capacity += made;
            }
            continue;
        }
        let Some(target) = p.info.target else { continue };
        if !seen_owner.insert(p.parent.as_str()) {
            continue;
        }
        let cir = &mut world.circuits[c];
        cir.consumption += target.max(0.0);
        cir.consumers += 1;
        // What it draws running at its clock speed.
        let full = match machine_of[e].map(|m| &world.machines[m as usize]) {
            Some(m) => machine_power(game, m),
            None => game.buildable_power.get(&ent.class).map(|p| *p as f32),
        };
        world.circuits[c].max_consumption += full.unwrap_or(target).max(target);
    }
    for c in &mut world.circuits {
        c.sources.sort_by(|a, b| b.mw.total_cmp(&a.mw).then(a.class.cmp(&b.class)));
    }

    // Poles and switches on the map, and the lines.
    let mut k = 0;
    for e in ents.iter() {
        let [x, y, z] = e.xf.pos;
        if finite(x).is_none() || finite(y).is_none() || finite(z).is_none() {
            continue;
        }
        if e.mark {
            if let Some(s) = world.structures.get_mut(k) {
                let comp = power.iter().find(|p| p.parent == e.name).map(|p| p.name.as_str());
                s.circuit = comp.and_then(root).and_then(|r| circuit_of_root.get(&r).copied());
            }
            k += 1;
        }
        if e.role == Role::Wire {
            if let [a, b, ..] = e.info.wire_ends[..] {
                if let (Some(ax), Some(ay), Some(bx), Some(by)) = (finite(a[0]), finite(a[1]), finite(b[0]), finite(b[1])) {
                    let circuit = root(&e.name).and_then(|r| circuit_of_root.get(&r).copied());
                    world.power_lines.push(fb_model::world::PowerLine { a: [ax, ay], b: [bx, by], circuit });
                }
            }
        }
    }
}

/// MW a machine draws running, at its clock speed and Somersloops.
fn machine_power(game: &GameData, m: &Machine) -> Option<f32> {
    if let Some(b) = game.buildings.get(&m.class) {
        let r = m.recipe.as_ref().and_then(|r| game.recipes.get(r));
        let base = r.map(|r| game.recipe_power_mw(r)).unwrap_or(b.power_mw);
        // The boost (1 + Somersloops x boost each) tells how many are in it.
        let sloops = if b.sloop_boost > 0.0 { ((m.boost as f64 - 1.0) / b.sloop_boost).round().clamp(0.0, b.sloop_slots as f64) as u32 } else { 0 };
        return Some(b.power_with(base, m.clock as f64, sloops) as f32);
    }
    let x = game.extraction.extractors.iter().find(|x| x.class == m.class)?;
    Some((x.power_mw * (m.clock as f64).powf(x.power_exponent)) as f32)
}

/// The generator recipes (building + fuel) for a building.
fn generator_recipes<'g>(game: &'g GameData, building: &str) -> Vec<&'g fb_model::Recipe> {
    game.recipes.values().filter(|r| r.is_generator() && r.building == building).collect()
}

/// What a machine can send out: its recipe's products, or for an
/// extractor its node's resource when the save overrides it, else every
/// resource it can extract.
fn produced<'g>(game: &'g GameData, classes: &Classes, resource_of: &HashMap<&'g str, &'g str>, m: &Machine) -> Vec<&'g str> {
    if classes.is_extractor(&m.class) {
        if let Some(r) = m.node.as_deref().and_then(|n| resource_of.get(n)) {
            return vec![*r];
        }
        let Some(x) = game.extraction.extractors.iter().find(|x| x.class == m.class) else { return Vec::new() };
        if !x.resources.is_empty() {
            return x.resources.iter().map(String::as_str).collect();
        }
        return game.items.values().filter(|i| i.raw && x.forms.contains(&i.form)).map(|i| i.class.as_str()).collect();
    }
    m.recipe.as_ref().and_then(|r| game.recipes.get(r)).map(|r| r.products.iter().map(|a| a.item.as_str()).collect()).unwrap_or_default()
}

/// What a machine takes in: its recipe's ingredients; for a generator with
/// no known recipe, what any of its recipes (with `fuel`, if known) take.
fn taken<'g>(game: &'g GameData, m: &Machine, fuel: Option<&'g str>) -> Vec<&'g str> {
    if let Some(r) = m.recipe.as_ref().and_then(|r| game.recipes.get(r)) {
        return r.ingredients.iter().map(|a| a.item.as_str()).collect();
    }
    let mut out: Vec<&str> = generator_recipes(game, &m.class)
        .into_iter()
        .filter(|r| fuel.is_none_or(|f| r.ingredients.iter().any(|a| a.item == f)))
        .flat_map(|r| r.ingredients.iter().map(|a| a.item.as_str()))
        .collect();
    out.extend(fuel);
    out
}

/// Fills in what extractors send down their links from the map's resource
/// nodes (`<icons>/map/map.json`): the save only stores the nodes that the
/// 1.2 game modes changed, so without the map a miner feeding a Foundry
/// (Iron Ore or Coal?) can't be told. Only links whose consumer takes the
/// node's resource get it.
pub fn apply_map(world: &mut World, map: &fb_model::world::MapInfo, game: &GameData) {
    let mut resource: HashMap<&str, &str> = map.nodes.iter().filter_map(|n| Some((n.id.as_str(), n.resource.as_deref()?))).collect();
    for o in &world.node_overrides {
        if let Some(r) = o.resource.as_deref() {
            resource.insert(o.id.as_str(), r);
        }
    }
    let extractors: HashSet<&str> = game.extraction.extractors.iter().map(|e| e.class.as_str()).collect();
    for link in world.links.iter_mut().filter(|l| l.item.is_none()) {
        let (Some(p), Some(c)) = (world.machines.get(link.from as usize), world.machines.get(link.to as usize)) else { continue };
        if !extractors.contains(p.class.as_str()) {
            continue;
        }
        let Some(res) = p.node.as_deref().and_then(|n| resource.get(n)) else { continue };
        if taken(game, c, None).contains(res) {
            link.item = Some(res.to_string());
        }
    }
}

#[cfg(test)]
mod tests;
