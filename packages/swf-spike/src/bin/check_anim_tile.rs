//! For each tile id used by map 35, dump frame count to find animated tiles.

use std::path::PathBuf;
use anyhow::Result;
use swf_spike::swf_doc::{Symbol, SwfDoc};

fn main() -> Result<()> {
    let path = PathBuf::from(std::env::args().nth(1).unwrap());
    let doc = SwfDoc::load(&path)?;
    let target_ids: Vec<u16> = std::env::args().skip(2).filter_map(|s| s.parse().ok()).collect();
    println!("checking {} tile ids in {}:", target_ids.len(), path.display());
    for id in target_ids {
        let n = doc.lookup_export(&id.to_string());
        if let Some(Symbol::Sprite(sp)) = n {
            if sp.num_frames > 1 {
                println!("  id={} sprite with {} frames", id, sp.num_frames);
            }
        }
    }
    Ok(())
}
