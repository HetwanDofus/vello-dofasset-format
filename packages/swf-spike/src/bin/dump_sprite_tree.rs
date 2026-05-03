//! Walk a sprite by id and print its op tree per frame, including
//! placement char-id types (sprite/shape/morph), so we can see what
//! the smoke effect is made of and at which frame range.

use anyhow::Result;
use std::path::PathBuf;
use swf_spike::swf_doc::{OwnedOp, OwnedSprite, Symbol, SwfDoc};

fn kind_of(sym: &Symbol) -> &'static str {
    match sym {
        Symbol::Sprite(_) => "sprite",
        Symbol::Shape(_) => "shape",
        Symbol::MorphShape(_) => "morph",
        Symbol::Bitmap(_) => "bitmap",
    }
}

fn dump(doc: &SwfDoc, sp: &OwnedSprite, indent: usize, max_depth: usize) {
    let pad = "  ".repeat(indent);
    let mut frame = 1u16;
    for op in &sp.ops {
        match op {
            OwnedOp::Place(p) => {
                let (k, child_frames) = match p.character_id.and_then(|id| doc.lookup_id(id)) {
                    Some(s) => (
                        kind_of(s),
                        if let Symbol::Sprite(c) = s { c.num_frames } else { 0 },
                    ),
                    None => ("?", 0),
                };
                println!(
                    "{}f{:>3} d={:>3} char={:?} kind={} child_frames={} ratio={:?} matrix={} clip_actions={}",
                    pad,
                    frame,
                    p.depth,
                    p.character_id,
                    k,
                    child_frames,
                    p.ratio,
                    p.matrix.is_some(),
                    p.clip_actions.len(),
                );
                if max_depth > 0 {
                    if let Some(id) = p.character_id
                        && let Some(Symbol::Sprite(child)) = doc.lookup_id(id)
                    {
                        dump(doc, child, indent + 1, max_depth - 1);
                    }
                }
            }
            OwnedOp::Remove { depth } => {
                println!("{}f{:>3} REMOVE d={}", pad, frame, depth);
            }
            OwnedOp::ShowFrame => {
                frame += 1;
            }
            OwnedOp::DoAction(bc) => {
                println!("{}f{:>3} DoAction({} bytes)", pad, frame, bc.len());
            }
        }
    }
}

fn main() -> Result<()> {
    let path = PathBuf::from(std::env::args().nth(1).unwrap());
    let id: u16 = std::env::args().nth(2).unwrap().parse().unwrap();
    let max_depth: usize = std::env::args()
        .nth(3)
        .and_then(|s| s.parse().ok())
        .unwrap_or(2);
    let doc = SwfDoc::load(&path)?;
    let Some(Symbol::Sprite(sp)) = doc.lookup_id(id) else {
        eprintln!("id {} not a sprite", id);
        return Ok(());
    };
    println!("id={} frames={} ops={}", id, sp.num_frames, sp.ops.len());
    dump(&doc, sp, 0, max_depth);
    Ok(())
}
