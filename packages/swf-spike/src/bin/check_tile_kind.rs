//! Classify each tile id by export name as "random"/"animated"/"static".

use std::path::PathBuf;
use anyhow::Result;
use swf_spike::swf_doc::{avm1_has_random_stop, OwnedOp, Symbol, SwfDoc};

fn classify(doc: &SwfDoc, export: &str) -> &'static str {
    let Some(Symbol::Sprite(top)) = doc.lookup_export(export) else {
        return "?";
    };
    if top.num_frames <= 1 {
        return "static";
    }
    for op in &top.ops {
        if let OwnedOp::DoAction(bc) = op
            && avm1_has_random_stop(bc)
        {
            return "random(top-DoAction)";
        }
        if let OwnedOp::Place(p) = op {
            for ca in &p.clip_actions {
                if avm1_has_random_stop(&ca.bytecode) {
                    return "random(top-clip)";
                }
            }
            if let Some(id) = p.character_id
                && let Some(Symbol::Sprite(child)) = doc.lookup_id(id)
            {
                for cop in &child.ops {
                    if let OwnedOp::DoAction(bc) = cop
                        && avm1_has_random_stop(bc)
                    {
                        return "random(child-DoAction)";
                    }
                    if let OwnedOp::Place(cp) = cop {
                        for ca in &cp.clip_actions {
                            if avm1_has_random_stop(&ca.bytecode) {
                                return "random(child-clip)";
                            }
                        }
                    }
                }
            }
        }
    }
    "animated"
}

fn main() -> Result<()> {
    let path = PathBuf::from(std::env::args().nth(1).unwrap());
    let doc = SwfDoc::load(&path)?;
    for arg in std::env::args().skip(2) {
        let kind = classify(&doc, &arg);
        let frames = if let Some(Symbol::Sprite(s)) = doc.lookup_export(&arg) {
            s.num_frames
        } else {
            0
        };
        println!("  id={} frames={} → {}", arg, frames, kind);
    }
    Ok(())
}
