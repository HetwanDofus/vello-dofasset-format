//! Walk every sprite in a character SWF and dump any DoAction whose
//! AS2 looks like `GAC.applyColor(this, N)` so we can confirm the
//! exact bytecode pattern before writing the zone detector.

use anyhow::Result;
use std::path::PathBuf;
use swf::avm1::read::Reader;
use swf::avm1::types::Action;
use swf_spike::swf_doc::{OwnedOp, OwnedSprite, Symbol, SwfDoc};

fn dump_action_seq(bc: &[u8]) -> Vec<String> {
    let mut r = Reader::new(bc, 6);
    let mut out = Vec::new();
    while let Ok(a) = r.read_action() {
        let s = match &a {
            Action::Push(p) => format!("Push({:?})", p.values),
            Action::CallMethod => "CallMethod".into(),
            Action::CallFunction => "CallFunction".into(),
            Action::GetVariable => "GetVariable".into(),
            Action::SetVariable => "SetVariable".into(),
            Action::GetMember => "GetMember".into(),
            Action::SetMember => "SetMember".into(),
            Action::End => break,
            _ => format!("{:?}", a),
        };
        out.push(s);
    }
    out
}

fn scan(doc: &SwfDoc, sp: &OwnedSprite, char_id: u16, depth: u32) {
    if depth > 6 { return; }
    for op in &sp.ops {
        if let OwnedOp::DoAction(bc) = op {
            let seq = dump_action_seq(bc);
            for w in seq.windows(2) {
                if w[1].contains("applyColor") || w[0].contains("applyColor") {
                    println!("char_id={} (depth {}):", char_id, depth);
                    for (i, line) in seq.iter().enumerate() {
                        println!("  [{}] {}", i, line);
                    }
                    return;
                }
            }
        }
        if let OwnedOp::Place(p) = op {
            for ca in &p.clip_actions {
                let seq = dump_action_seq(&ca.bytecode);
                if seq.iter().any(|s| s.contains("applyColor")) {
                    println!("char_id={} clip_action (depth {}):", char_id, depth);
                    for (i, line) in seq.iter().enumerate() {
                        println!("  [{}] {}", i, line);
                    }
                }
            }
            if let Some(id) = p.character_id
                && let Some(Symbol::Sprite(child)) = doc.lookup_id(id)
            {
                scan(doc, child, id, depth + 1);
            }
        }
    }
}

fn main() -> Result<()> {
    let path = PathBuf::from(std::env::args().nth(1).unwrap());
    let doc = SwfDoc::load(&path)?;
    // Walk every sprite character — exhaustive but small SWFs.
    let mut ids: Vec<u16> = doc.by_id.keys().copied().collect();
    ids.sort();
    for id in ids {
        if let Some(Symbol::Sprite(sp)) = doc.lookup_id(id) {
            scan(&doc, sp, id, 0);
        }
    }
    Ok(())
}
