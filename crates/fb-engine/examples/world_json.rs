//! Writes what a save has built (`fb_engine::world::read_world`) as JSON, for
//! checking the map against the save by eye.
//!
//!     cargo run -p fb-engine --example world_json -- <save.sav> <Satisfactory dir> > world.json

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (Some(save), Some(game_dir)) = (args.next(), args.next()) else {
        eprintln!("usage: world_json <save.sav> <Satisfactory dir>");
        std::process::exit(2);
    };
    let game = fb_engine::docs::load_game_data(std::path::Path::new(&game_dir))?;
    let bytes = std::fs::read(&save)?;
    let world = fb_engine::world::read_world(&bytes, &game).map_err(|e| e.to_string())?;
    println!("{}", serde_json::to_string(&world)?);
    Ok(())
}
