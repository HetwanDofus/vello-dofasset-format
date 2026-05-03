//! Dump a morph shape's start/end shape records for debugging.

use std::path::PathBuf;
use anyhow::Result;
use swf_spike::swf_doc::{Symbol, SwfDoc};

fn main() -> Result<()> {
    let path = PathBuf::from(std::env::args().nth(1).unwrap());
    let target_id: u16 = std::env::args().nth(2).unwrap().parse().unwrap();
    let doc = SwfDoc::load(&path)?;
    let sym = doc.by_id.get(&target_id).expect("symbol not found");
    if let Symbol::MorphShape(ms) = sym {
        println!("morph id={} version={:?}", target_id, ms.version);
        println!("start.shape_bounds: {:?}", ms.start.shape_bounds);
        println!("end.shape_bounds:   {:?}", ms.end.shape_bounds);
        println!("start.fill_styles: {:?}", ms.start.fill_styles);
        println!("end.fill_styles:   {:?}", ms.end.fill_styles);
        println!("start.line_styles:");
        for ls in &ms.start.line_styles {
            println!("  width={:?} fill={:?}", ls.width(), ls.fill_style());
        }
        println!("end.line_styles:");
        for ls in &ms.end.line_styles {
            println!("  width={:?} fill={:?}", ls.width(), ls.fill_style());
        }
        println!("start.shape ({} records):", ms.start.shape.len());
        for (i, r) in ms.start.shape.iter().enumerate() {
            println!("  [{}] {:?}", i, r);
        }
        println!("end.shape ({} records):", ms.end.shape.len());
        for (i, r) in ms.end.shape.iter().enumerate() {
            println!("  [{}] {:?}", i, r);
        }
    } else {
        println!("not a morph shape");
        let _ = sym;
    }
    Ok(())
}
