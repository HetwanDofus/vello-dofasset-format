use std::path::PathBuf;
use anyhow::Result;
use swf_spike::swf_doc::{Symbol, SwfDoc};
fn main() -> Result<()> {
    let path = PathBuf::from(std::env::args().nth(1).unwrap());
    let doc = SwfDoc::load(&path)?;
    for (name, &id) in &doc.by_name {
        if let Some(Symbol::Sprite(sp)) = doc.lookup_id(id) {
            if sp.num_frames > 100 {
                println!("export={} (id={}) frames={}", name, id, sp.num_frames);
            }
        }
    }
    Ok(())
}
