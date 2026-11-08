//! How long reading a save's world takes and how much memory it needs.
//! Runs only when FB_TEST_SAVES names a folder with late.sav (and the game's
//! Docs are found); meant for release builds:
//! `cargo test --release -p fb-engine --test world_perf -- --nocapture`

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

/// Counts live heap bytes and the peak, for this test binary only.
struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            let now = LIVE.fetch_add(l.size(), Relaxed) + l.size();
            PEAK.fetch_max(now, Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) };
        LIVE.fetch_sub(l.size(), Relaxed);
    }
}

#[global_allocator]
static A: Counting = Counting;

#[test]
fn late_save_speed_and_memory() {
    let Ok(dir) = std::env::var("FB_TEST_SAVES") else { return };
    let Ok(bytes) = std::fs::read(std::path::Path::new(&dir).join("late.sav")) else { return };
    let game_dir = std::env::var("SATISFACTORY_DIR").unwrap_or_else(|_| r"C:\Program Files (x86)\Steam\steamapps\common\Satisfactory".into());
    let Ok(game) = fb_engine::docs::load_game_data(std::path::Path::new(&game_dir)) else { return };

    let mut best = std::time::Duration::MAX;
    let mut extra = 0;
    for _ in 0..3 {
        let before = LIVE.load(Relaxed);
        PEAK.store(before, Relaxed);
        let t = std::time::Instant::now();
        let world = fb_engine::world::read_world(&bytes, &game).unwrap();
        best = best.min(t.elapsed());
        extra = PEAK.load(Relaxed) - before;
        drop(world);
    }
    let world = fb_engine::world::read_world(&bytes, &game).unwrap();
    let json = serde_json::to_string(&world).unwrap();
    eprintln!(
        "late.sav ({:.1} MB file): best of 3 {:?}, peak heap during the read {:.1} MB above the file bytes, World JSON {:.0} KB",
        bytes.len() as f64 / 1e6,
        best,
        extra as f64 / 1e6,
        json.len() as f64 / 1024.0
    );
    let kb = |v: &dyn erased::Json| v.json_len() as f64 / 1024.0;
    eprintln!(
        "  JSON parts: machines {:.0} KB, carriers {:.0} KB ({} points), links {:.0} KB, structures {:.0} KB",
        kb(&world.machines),
        kb(&world.carriers),
        world.carriers.iter().map(|c| c.points.len()).sum::<usize>(),
        kb(&world.links),
        kb(&world.structures)
    );
    if !cfg!(debug_assertions) {
        assert!(best.as_secs_f64() < 3.0, "{best:?}");
    }
    assert!(extra < 200 * 1024 * 1024, "{extra} bytes");
}

mod erased {
    pub trait Json {
        fn json_len(&self) -> usize;
    }
    impl<T: serde::Serialize> Json for T {
        fn json_len(&self) -> usize {
            serde_json::to_string(self).map(|s| s.len()).unwrap_or(0)
        }
    }
}
