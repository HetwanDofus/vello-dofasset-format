use std::path::PathBuf;
use anyhow::Result;
use swf_spike::swf_doc::{Symbol, SwfDoc};

fn main() -> Result<()> {
    let path = PathBuf::from(std::env::args().nth(1).unwrap());
    let doc = SwfDoc::load(&path)?;
    let mut ids: Vec<u16> = doc.by_id.keys().copied().collect();
    ids.sort();
    for id in ids {
        let s = doc.by_id.get(&id).unwrap();
        let kind = match s {
            Symbol::Shape(_) => "Shape",
            Symbol::Sprite(sp) => return_sprite(sp.num_frames),
            Symbol::MorphShape(_) => "MorphShape",
            Symbol::Bitmap(_) => "Bitmap",
        };
        println!("id={} kind={}", id, kind);
    }
    Ok(())
}
fn return_sprite(n: u16) -> &'static str {
    Box::leak(format!("Sprite({} frames)", n).into_boxed_str())
}
