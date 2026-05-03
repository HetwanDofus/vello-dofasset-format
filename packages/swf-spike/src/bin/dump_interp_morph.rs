//! Dump our build_morph_frame output for a given morph + ratio.

use std::path::PathBuf;
use anyhow::Result;
use swf_spike::swf_doc::{Symbol, SwfDoc};

fn main() -> Result<()> {
    let path = PathBuf::from(std::env::args().nth(1).unwrap());
    let target_id: u16 = std::env::args().nth(2).unwrap().parse().unwrap();
    let ratio: u16 = std::env::args().nth(3).unwrap().parse().unwrap();
    let doc = SwfDoc::load(&path)?;
    let sym = doc.by_id.get(&target_id).expect("symbol not found");
    if let Symbol::MorphShape(ms) = sym {
        let shape = swf_spike::morph::build_morph_frame(ms, ratio);
        println!("Interpolated shape at ratio={}:", ratio);
        println!("  bounds: {:?}", shape.shape_bounds);
        println!("  shape ({} records):", shape.shape.len());
        for (i, r) in shape.shape.iter().enumerate() {
            println!("    [{}] {:?}", i, r);
        }
        println!("  line_styles:");
        for ls in &shape.styles.line_styles {
            println!("    width={:?} fill={:?}", ls.width(), ls.fill_style());
        }
    } else {
        println!("not a morph shape");
        let _ = sym;
    }
    Ok(())
}
