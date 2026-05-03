use std::path::PathBuf;
use anyhow::Result;
use swf_spike::swf_doc::{OwnedOp, SwfDoc};

fn main() -> Result<()> {
    let path = PathBuf::from(std::env::args().nth(1).unwrap());
    let doc = SwfDoc::load(&path)?;
    println!("root num_frames={}, ops={}", doc.root.num_frames, doc.root.ops.len());
    let mut frame = 1u16;
    for op in &doc.root.ops {
        match op {
            OwnedOp::Place(p) => println!("  f{} Place depth={} char={:?}", frame, p.depth, p.character_id),
            OwnedOp::Remove { depth } => println!("  f{} Remove depth={}", frame, depth),
            OwnedOp::ShowFrame => { frame += 1; }
            OwnedOp::DoAction(_) => println!("  f{} DoAction", frame),
        }
    }
    Ok(())
}
