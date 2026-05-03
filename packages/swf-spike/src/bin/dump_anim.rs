//! Dump the structure of one exported sprite — depths, child characters, frames.

use anyhow::Result;
use std::path::PathBuf;
use swf_spike::swf_doc::{OwnedOp, OwnedSprite, SwfDoc, Symbol};

fn main() -> Result<()> {
    let argv: Vec<String> = std::env::args().collect();
    let path: PathBuf = argv
        .get(1)
        .cloned()
        .unwrap_or_else(|| {
            "/Users/grandnainconnu/Work/personal/dofus/dofus1.29/dofus-client-recode/dofuswebclient2/assets/sources/clips/sprites/10.swf".to_string()
        })
        .into();
    let name = argv.get(2).cloned().unwrap_or_else(|| "staticR".into());
    let depth = argv.get(3).and_then(|s| s.parse().ok()).unwrap_or(2u32);

    let doc = SwfDoc::load(&path)?;

    fn walk(doc: &SwfDoc, sym: &Symbol, depth: u32, indent: u32) {
        for _ in 0..indent {
            print!("  ");
        }
        match sym {
            Symbol::Shape(s) => {
                println!(
                    "Shape id≈? bounds [{},{} → {},{}] records={} fills={} lines={}",
                    s.shape_bounds.x_min.get(),
                    s.shape_bounds.y_min.get(),
                    s.shape_bounds.x_max.get(),
                    s.shape_bounds.y_max.get(),
                    s.shape.len(),
                    s.styles.fill_styles.len(),
                    s.styles.line_styles.len(),
                );
            }
            Symbol::Sprite(sp) => {
                println!("Sprite num_frames={} ops={}", sp.num_frames, sp.ops.len());
                if depth == 0 {
                    return;
                }
                describe_sprite(doc, sp, indent + 1, depth);
            }
            Symbol::Bitmap { width, height, .. } => {
                println!("Bitmap {}x{}", width, height);
            }
        }
    }

    fn describe_sprite(doc: &SwfDoc, sp: &OwnedSprite, indent: u32, depth: u32) {
        let mut frame = 0u16;
        for op in &sp.ops {
            match op {
                OwnedOp::Place(p) => {
                    for _ in 0..indent {
                        print!("  ");
                    }
                    println!(
                        "frame {} place depth={} char={:?} move={} matrix={:?}",
                        frame, p.depth, p.character_id, p.is_move, p.matrix
                    );
                    if let Some(id) = p.character_id {
                        if let Some(child) = doc.lookup_id(id) {
                            for _ in 0..(indent + 1) {
                                print!("  ");
                            }
                            print!("→ id={} ", id);
                            walk(doc, child, depth - 1, indent + 1);
                        }
                    }
                }
                OwnedOp::Remove { depth: d } => {
                    for _ in 0..indent {
                        print!("  ");
                    }
                    println!("frame {} remove depth={}", frame, d);
                }
                OwnedOp::ShowFrame => frame += 1,
            }
        }
    }

    let sym = doc
        .lookup_export(&name)
        .ok_or_else(|| anyhow::anyhow!("no export {}", name))?;
    print!("{}: ", name);
    walk(&doc, sym, depth, 0);
    Ok(())
}
