//! Reads what a Satisfactory save has unlocked, without loading the world.
//!
//! A `.sav` file is a header followed by zlib-compressed chunks. Each chunk
//! starts with the package tag 0x9E2A83C1; the 1.x layout adds a 0x22222222
//! marker and a compression byte (49-byte chunk header; older saves have 48).
//! The game state's `mPurchasedSchematics` property is an array of object
//! references (level name + path), one per schematic bought: milestones,
//! MAM research, alternates from hard drives and so on.
//!
//! Checked against Update 4 (header 8, save version 25) and 1.x saves
//! (header 14, save version 60). Chunks are inflated one at a time and only
//! a small window is kept, so even a late-game save (180 MB unpacked) needs
//! a few MB.

use fb_model::saves::SaveSummary;

const TAG: [u8; 4] = 0x9E2A_83C1u32.to_le_bytes();
const CHUNK_V2: u32 = 0x2222_2222;
/// `FString` "mPurchasedSchematics" with its length prefix (20 chars + NUL).
const KEY: &[u8] = b"\x15\x00\x00\x00mPurchasedSchematics\x00";
/// .NET ticks (100 ns since 0001-01-01) at the Unix epoch.
const EPOCH_TICKS: i64 = 621_355_968_000_000_000;
const MAX_CHUNK: usize = 64 * 1024 * 1024;
const MAX_STRING: i32 = 64 * 1024;
const MAX_SCHEMATICS: i32 = 100_000;
/// How far past the property name the array may run before giving up.
const MAX_ARRAY_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, PartialEq)]
pub enum SaveError {
    NotASave,
    Damaged(&'static str),
    NoProgress,
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaveError::NotASave => write!(f, "That isn't a Satisfactory save file."),
            SaveError::Damaged(why) => write!(f, "That save file couldn't be read ({why})."),
            SaveError::NoProgress => write!(f, "No unlocked recipes were found in that save."),
        }
    }
}

/// Little-endian reader that says when it ran off the end.
struct Reader<'a> {
    b: &'a [u8],
    o: usize,
}

impl<'a> Reader<'a> {
    fn i32(&mut self) -> Option<i32> {
        let v = self.b.get(self.o..self.o + 4)?;
        self.o += 4;
        Some(i32::from_le_bytes(v.try_into().ok()?))
    }
    fn i64(&mut self) -> Option<i64> {
        let v = self.b.get(self.o..self.o + 8)?;
        self.o += 8;
        Some(i64::from_le_bytes(v.try_into().ok()?))
    }
    /// Unreal `FString`: length (with the NUL) then Latin-1, or negative
    /// length then UTF-16. `Err(())` = malformed, `Ok(None)` = ran off the end.
    fn fstring(&mut self) -> Result<Option<String>, ()> {
        let Some(n) = self.i32() else { return Ok(None) };
        if n == 0 {
            return Ok(Some(String::new()));
        }
        if !(-MAX_STRING..=MAX_STRING).contains(&n) {
            return Err(());
        }
        if n > 0 {
            let n = n as usize;
            let Some(raw) = self.b.get(self.o..self.o + n) else { return Ok(None) };
            self.o += n;
            if raw[n - 1] != 0 {
                return Err(());
            }
            Ok(Some(raw[..n - 1].iter().map(|&c| c as char).collect()))
        } else {
            let n = (-n) as usize * 2;
            let Some(raw) = self.b.get(self.o..self.o + n) else { return Ok(None) };
            self.o += n;
            let units: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            Ok(Some(String::from_utf16_lossy(&units[..units.len() - 1])))
        }
    }
}

/// Reads the header. Returns it and where the header ends.
pub fn read_header(bytes: &[u8]) -> Result<(SaveSummary, usize), SaveError> {
    let mut r = Reader { b: bytes, o: 0 };
    let bad = SaveError::NotASave;
    let header_version = r.i32().ok_or(SaveError::NotASave)?;
    let save_version = r.i32().ok_or(SaveError::NotASave)?;
    let _build = r.i32().ok_or(SaveError::NotASave)?;
    if !(0..=1000).contains(&header_version) || !(0..=10_000).contains(&save_version) {
        return Err(bad);
    }
    let s = |r: &mut Reader| r.fstring().ok().flatten().ok_or(SaveError::NotASave);
    // Header 14 (1.1) added the save's own name.
    let name = if header_version >= 14 { s(&mut r)? } else { String::new() };
    let map = s(&mut r)?;
    let _options = s(&mut r)?;
    let session = s(&mut r)?;
    let play = r.i32().ok_or(SaveError::NotASave)?;
    let ticks = r.i64().ok_or(SaveError::NotASave)?;
    if map.is_empty() || play < 0 {
        return Err(SaveError::NotASave);
    }
    let summary = SaveSummary {
        session,
        name,
        saved_unix: ticks.saturating_sub(EPOCH_TICKS) / 10_000_000,
        play_seconds: play as i64,
        header_version,
        save_version,
        ..Default::default()
    };
    Ok((summary, r.o))
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from >= hay.len() {
        return None;
    }
    hay[from..].windows(needle.len()).position(|w| w == needle).map(|i| i + from)
}

/// The compressed chunks, inflated one by one.
pub(crate) struct Chunks<'a> {
    b: &'a [u8],
    o: usize,
}

impl<'a> Chunks<'a> {
    /// The chunks that follow the header, which ends at `header_end`.
    pub(crate) fn after_header(bytes: &'a [u8], header_end: usize) -> Option<Self> {
        find(bytes, &TAG, header_end).map(|o| Chunks { b: bytes, o })
    }

    pub(crate) fn next(&mut self) -> Option<Result<Vec<u8>, SaveError>> {
        if self.o >= self.b.len() {
            return None;
        }
        let h = &self.b[self.o..];
        if h.len() < 49 || h[..4] != TAG {
            return Some(Err(SaveError::Damaged("bad chunk")));
        }
        let ver = u32::from_le_bytes(h[4..8].try_into().unwrap());
        let (comp_at, header) = if ver == CHUNK_V2 { (17, 49) } else { (16, 48) };
        let comp = i64::from_le_bytes(h[comp_at..comp_at + 8].try_into().unwrap());
        let raw = i64::from_le_bytes(h[comp_at + 8..comp_at + 16].try_into().unwrap());
        if comp <= 0 || raw <= 0 || raw as usize > MAX_CHUNK || header + comp as usize > h.len() {
            return Some(Err(SaveError::Damaged("bad chunk size")));
        }
        let data = &h[header..header + comp as usize];
        self.o += header + comp as usize;
        Some(miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(data, raw as usize).map_err(|_| SaveError::Damaged("bad compressed data")))
    }
}

enum Parse {
    Done(Vec<String>),
    NeedMore,
    Bad,
}

/// The array after the property name at `at`: find the first element (an
/// object reference: level name, then a path starting with '/'), read the
/// count just before it, then exactly that many references.
fn parse_array(b: &[u8], at: usize) -> Parse {
    let start = at + KEY.len();
    for j in start..start + 256 {
        if j + 8 > b.len() {
            return Parse::NeedMore;
        }
        let mut r = Reader { b, o: j };
        let Ok(Some(_level)) = r.fstring() else { continue };
        let path_at = r.o;
        match r.fstring() {
            Ok(Some(p)) if p.starts_with("/Game/") || p.starts_with("/Script/") => {}
            Ok(None) if b.len() < path_at + 4 + 8 => return Parse::NeedMore,
            _ => continue,
        }
        let count = i32::from_le_bytes(b[j - 4..j].try_into().unwrap());
        if !(1..=MAX_SCHEMATICS).contains(&count) {
            continue;
        }
        let mut r = Reader { b, o: j };
        let mut out = Vec::with_capacity(count as usize);
        for _ in 0..count {
            match (r.fstring(), r.fstring()) {
                (Ok(Some(_)), Ok(Some(p))) if p.starts_with('/') => out.push(p),
                (Ok(None), _) | (_, Ok(None)) => return Parse::NeedMore,
                _ => return Parse::Bad,
            }
        }
        return Parse::Done(out);
    }
    Parse::Bad
}

/// Paths of every schematic bought in the save.
pub fn purchased_schematics(bytes: &[u8]) -> Result<Vec<String>, SaveError> {
    let (_, header_end) = read_header(bytes)?;
    let first = find(bytes, &TAG, header_end).ok_or(SaveError::Damaged("no data"))?;
    let mut chunks = Chunks { b: bytes, o: first };
    let mut window: Vec<u8> = Vec::new();
    let mut hit: Option<usize> = None;
    loop {
        let more = match chunks.next() {
            Some(c) => {
                window.extend_from_slice(&c?);
                true
            }
            None => false,
        };
        loop {
            if hit.is_none() {
                hit = find(&window, KEY, 0);
                if hit.is_none() {
                    // Keep just enough to catch a name split across chunks.
                    let keep = KEY.len() - 1;
                    if window.len() > keep {
                        window.drain(..window.len() - keep);
                    }
                    break;
                }
            }
            let at = hit.unwrap();
            match parse_array(&window, at) {
                Parse::Done(list) => return Ok(list),
                Parse::NeedMore if more && window.len() - at < MAX_ARRAY_BYTES => break,
                // Not the property we want (or cut short): look for the next one.
                _ => {
                    window.drain(..at + 1);
                    hit = None;
                }
            }
        }
        if !more {
            return Err(SaveError::NoProgress);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fstr(s: &str) -> Vec<u8> {
        let mut v = ((s.len() + 1) as i32).to_le_bytes().to_vec();
        v.extend_from_slice(s.as_bytes());
        v.push(0);
        v
    }

    /// A minimal 1.x-style save: header, then the body split over chunks so
    /// the property straddles a chunk boundary.
    fn fake_save(schematics: &[&str], chunk: usize) -> Vec<u8> {
        let mut f = Vec::new();
        f.extend(14i32.to_le_bytes());
        f.extend(60i32.to_le_bytes());
        f.extend(513_885i32.to_le_bytes());
        for s in ["My save", "Persistent_Level", "?startloc=Grass Fields", "My session"] {
            f.extend(fstr(s));
        }
        f.extend(3600i32.to_le_bytes());
        f.extend((EPOCH_TICKS + 1_700_000_000 * 10_000_000).to_le_bytes());
        f.extend([0u8; 40]); // the rest of the header: not read
        let mut body = vec![7u8; 1000];
        body.extend(KEY);
        body.extend(fstr("ArrayProperty"));
        body.extend(1i32.to_le_bytes());
        body.extend(fstr("ObjectProperty"));
        body.extend([0u8; 9]);
        body.extend((schematics.len() as i32).to_le_bytes());
        for s in schematics {
            body.extend(0i32.to_le_bytes());
            let file = s.rsplit('/').next().unwrap();
            body.extend(fstr(&format!("/Game/FactoryGame/Schematics/{s}.{file}_C")));
        }
        body.extend(vec![9u8; 500]);
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

    #[test]
    fn reads_the_header() {
        let (h, _) = read_header(&fake_save(&["Schematic_1-1"], 4096)).unwrap();
        assert_eq!((h.session.as_str(), h.name.as_str(), h.play_seconds, h.saved_unix), ("My session", "My save", 3600, 1_700_000_000));
        assert_eq!(read_header(b"hello world, not a save at all"), Err(SaveError::NotASave));
    }

    #[test]
    fn finds_schematics_even_across_chunk_boundaries() {
        let names = ["Schematic_1-1", "Alternate/Schematic_Alternate_Screw", "Schematic_2-3"];
        for chunk in [64, 100, 1037, 1 << 16] {
            let got = purchased_schematics(&fake_save(&names, chunk)).unwrap();
            let classes: Vec<&str> = got.iter().map(|p| crate::progress::schematic_class(p)).collect();
            assert_eq!(classes, ["Schematic_1-1_C", "Schematic_Alternate_Screw_C", "Schematic_2-3_C"], "chunk {chunk}");
        }
    }

    #[test]
    fn damaged_files_fail_cleanly() {
        let good = fake_save(&["Schematic_1-1"], 256);
        let (_, header_end) = read_header(&good).unwrap();
        let first_chunk = find(&good, &TAG, header_end).unwrap();
        // Cut off inside the first chunk, before the property.
        assert!(purchased_schematics(&good[..first_chunk + 120]).is_err());
        // Garbage in the first chunk's compressed data.
        let mut bad = good.clone();
        for b in &mut bad[first_chunk + 49..first_chunk + 69] {
            *b ^= 0x5A;
        }
        assert!(purchased_schematics(&bad).is_err());
        // No property at all.
        assert_eq!(purchased_schematics(&good[..first_chunk]).unwrap_err(), SaveError::Damaged("no data"));
    }

    /// Real saves, if present: set FB_TEST_SAVES to a folder of .sav files.
    #[test]
    fn real_saves() {
        let Ok(dir) = std::env::var("FB_TEST_SAVES") else { return };
        for e in std::fs::read_dir(dir).unwrap().filter_map(Result::ok) {
            if !e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("sav")) {
                continue;
            }
            let bytes = std::fs::read(e.path()).unwrap();
            let (h, _) = read_header(&bytes).unwrap();
            let list = purchased_schematics(&bytes).unwrap();
            let alts = list.iter().filter(|p| p.contains("Schematic_Alternate_")).count();
            eprintln!("{:?}: header {} v{} session {:?} play {}s: {} schematics, {} alternates", e.file_name(), h.header_version, h.save_version, h.session, h.play_seconds, list.len(), alts);
            assert!(list.iter().any(|p| p.ends_with("Schematic_StartingRecipes_C")));
        }
    }
}
