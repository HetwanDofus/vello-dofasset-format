use std::path::PathBuf;
use anyhow::Result;
use swf_spike::swf_doc::{Symbol, SwfDoc};
fn main() -> Result<()> {
    let path = PathBuf::from(std::env::args().nth(1).unwrap());
    let id: u16 = std::env::args().nth(2).unwrap().parse().unwrap();
    let doc = SwfDoc::load(&path)?;
    if let Some(sym) = doc.lookup_id(id) {
        match sym {
            Symbol::Sprite(sp) => println!("id={} sprite, {} frames, {} ops", id, sp.num_frames, sp.ops.len()),
            Symbol::Shape(_) => println!("id={} shape", id),
            Symbol::MorphShape(_) => println!("id={} morphshape", id),
            Symbol::Bitmap(_) => println!("id={} bitmap", id),
        }
    }
    Ok(())
}
