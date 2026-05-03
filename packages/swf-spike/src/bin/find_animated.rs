//! Find sprites with multi-frame and NO frame-1 stop/random — true animated tiles.
use std::path::PathBuf;
use anyhow::Result;
use swf::avm1::read::Reader;
use swf::avm1::types::Action;
use swf_spike::swf_doc::{OwnedOp, Symbol, SwfDoc};

fn has_frame1_stop_or_random(bc: &[u8]) -> Option<&'static str> {
    let mut r = Reader::new(bc, 6);
    let mut saw_random = false;
    let mut saw_total_frames = false;
    while let Ok(a) = r.read_action() {
        match a {
            Action::Stop => return Some("stop"),
            Action::RandomNumber => saw_random = true,
            Action::GetMember if saw_total_frames => return Some("random_method_call"),
            Action::Push(p) => {
                for v in &p.values {
                    if let swf::avm1::types::Value::ConstantPool(_) = v {}
                    if let swf::avm1::types::Value::Str(s) = v {
                        if s.to_string_lossy(swf::UTF_8) == "_totalframes" {
                            saw_total_frames = true;
                        }
                    }
                }
            }
            Action::GotoFrame2(g) if !g.set_playing && saw_random => return Some("random_gotoframe2"),
            Action::CallMethod if saw_random => return Some("random_method_call"),
            Action::End => break,
            _ => {}
        }
    }
    None
}

fn main() -> Result<()> {
    let path = PathBuf::from(std::env::args().nth(1).unwrap());
    let doc = SwfDoc::load(&path)?;
    for (name, &id) in &doc.by_name {
        if let Some(Symbol::Sprite(sp)) = doc.lookup_id(id) {
            if sp.num_frames <= 1 { continue; }
            // Check frame-1 ops
            let mut has_script = false;
            for op in &sp.ops {
                if let OwnedOp::ShowFrame = op { break; }
                match op {
                    OwnedOp::DoAction(bc) => {
                        if let Some(kind) = has_frame1_stop_or_random(bc) {
                            has_script = true;
                            let _ = kind;
                            break;
                        }
                        has_script = true;
                    }
                    OwnedOp::Place(p) => {
                        for ca in &p.clip_actions {
                            if has_frame1_stop_or_random(&ca.bytecode).is_some() {
                                has_script = true;
                            }
                        }
                    }
                    _ => {}
                }
            }
            if !has_script {
                println!("ANIMATED export={} id={} frames={}", name, id, sp.num_frames);
            }
        }
    }
    Ok(())
}
