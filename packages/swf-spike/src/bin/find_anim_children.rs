use std::path::PathBuf;
use anyhow::Result;
use swf_spike::swf_doc::{OwnedOp, Symbol, SwfDoc};

fn check_sprite(doc: &SwfDoc, sp: &swf_spike::swf_doc::OwnedSprite, depth: u32) -> Vec<(u16, u16)> {
    let mut results = Vec::new();
    if depth == 0 { return results; }
    for op in &sp.ops {
        if let OwnedOp::Place(p) = op
            && let Some(id) = p.character_id
            && let Some(Symbol::Sprite(child)) = doc.lookup_id(id)
        {
            if child.num_frames > 1 {
                results.push((id, child.num_frames));
            }
            results.extend(check_sprite(doc, child, depth - 1));
        }
    }
    results
}

fn main() -> Result<()> {
    let path = PathBuf::from(std::env::args().nth(1).unwrap());
    let doc = SwfDoc::load(&path)?;
    for arg in std::env::args().skip(2) {
        if let Some(Symbol::Sprite(top)) = doc.lookup_export(&arg) {
            let children = check_sprite(&doc, top, 6);
            if !children.is_empty() {
                println!("export={} top_frames={} children: {:?}", arg, top.num_frames, children);
            }
        }
    }
    Ok(())
}
