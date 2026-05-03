//! For a given exported tile, walk each child's frame-1 ops and report
//! which children have a Stop() — those are the ones tripping the
//! "slope" classifier.

use std::path::PathBuf;
use anyhow::Result;
use swf_spike::swf_doc::{avm1_classify_frame1, OwnedOp, Symbol, SwfDoc, TileScriptKind};

fn main() -> Result<()> {
    let path = PathBuf::from(std::env::args().nth(1).unwrap());
    let exp = std::env::args().nth(2).unwrap();
    let doc = SwfDoc::load(&path)?;
    let Some(Symbol::Sprite(top)) = doc.lookup_export(&exp) else {
        eprintln!("not a sprite");
        return Ok(());
    };
    println!("export={} top frames={} ops={}", exp, top.num_frames, top.ops.len());
    for op in &top.ops {
        if matches!(op, OwnedOp::ShowFrame) { break; }
        match op {
            OwnedOp::DoAction(bc) => {
                println!("  TOP frame-1 DoAction ({} bytes): {:?}", bc.len(), avm1_classify_frame1(bc));
            }
            OwnedOp::Place(p) => {
                if let Some(id) = p.character_id
                    && let Some(Symbol::Sprite(child)) = doc.lookup_id(id)
                {
                    let mut child_kind: Option<TileScriptKind> = None;
                    for cop in &child.ops {
                        if matches!(cop, OwnedOp::ShowFrame) { break; }
                        match cop {
                            OwnedOp::DoAction(bc) => {
                                if let Some(k) = avm1_classify_frame1(bc) {
                                    child_kind = Some(k);
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                    if let Some(k) = child_kind {
                        println!(
                            "  child id={} depth={} frames={} → {:?}",
                            id, p.depth, child.num_frames, k
                        );
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}
