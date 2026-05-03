//! Walk a SWF and print every captured AVM1 chunk: per-frame DoActions and
//! per-placement onClipEvent handlers. Used to verify `swf_doc` actually
//! captured the bytecode, and to figure out which opcodes my AVM1
//! interpreter needs to handle.

use std::path::PathBuf;

use anyhow::{anyhow, Result};
use swf::avm1::read::Reader as AvmReader;
use swf::avm1::types::Action;

use swf_spike::swf_doc::{clip_event, OwnedOp, Symbol, SwfDoc};

fn main() -> Result<()> {
    let path = PathBuf::from(
        std::env::args().nth(1).ok_or_else(|| anyhow!("usage: dump-avm1 <swf>"))?,
    );
    let doc = SwfDoc::load(&path)?;

    println!("== root timeline ==");
    dump_ops(&doc.root.ops, 0);

    let mut ids: Vec<u16> = doc.by_id.keys().copied().collect();
    ids.sort();
    for id in ids {
        if let Some(Symbol::Sprite(sp)) = doc.by_id.get(&id) {
            // Only dump sprites that contain AS — the rest are just geometry.
            let has_as = sp.ops.iter().any(|o| match o {
                OwnedOp::DoAction(_) => true,
                OwnedOp::Place(p) => !p.clip_actions.is_empty(),
                _ => false,
            });
            if !has_as {
                continue;
            }
            println!("\n== sprite id={} ({} frames) ==", id, sp.num_frames);
            dump_ops(&sp.ops, 0);
        }
    }
    Ok(())
}

fn dump_ops(ops: &[OwnedOp], indent: usize) {
    let pad = "  ".repeat(indent);
    let mut frame = 1u16;
    for op in ops {
        match op {
            OwnedOp::ShowFrame => {
                frame += 1;
            }
            OwnedOp::Place(p) => {
                if !p.clip_actions.is_empty() {
                    println!(
                        "{pad}frame {frame}: PlaceObject depth={} char={:?} clip_actions={}",
                        p.depth,
                        p.character_id,
                        p.clip_actions.len()
                    );
                    for ca in &p.clip_actions {
                        let ev = events_to_str(ca.events);
                        println!("{pad}  on({ev}):");
                        dump_actions(&ca.bytecode, indent + 2);
                    }
                }
            }
            OwnedOp::DoAction(bc) => {
                println!("{pad}frame {frame}: DoAction ({} bytes):", bc.len());
                dump_actions(bc, indent + 1);
            }
            OwnedOp::Remove { .. } => {}
        }
    }
}

fn events_to_str(mask: u32) -> String {
    let mut parts = Vec::new();
    if mask & clip_event::LOAD != 0 {
        parts.push("load");
    }
    if mask & clip_event::ENTER_FRAME != 0 {
        parts.push("enterFrame");
    }
    if mask & clip_event::UNLOAD != 0 {
        parts.push("unload");
    }
    parts.join("+")
}

fn dump_actions(bytecode: &[u8], indent: usize) {
    let pad = "  ".repeat(indent);
    let mut reader = AvmReader::new(bytecode, 1);
    loop {
        match reader.read_action() {
            Ok(Action::End) => break,
            Ok(action) => println!("{pad}{action:?}"),
            Err(e) => {
                println!("{pad}<read error: {e:?}>");
                break;
            }
        }
    }
}
