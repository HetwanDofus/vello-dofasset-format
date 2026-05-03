//! Print every fill style + alpha in every shape so we can see whether the
//! SWF's authored colors really have alpha < 255 (which would explain why
//! the SVG export shows them as pale/translucent and ours doesn't).

use std::path::PathBuf;
use anyhow::{anyhow, Result};
use swf_spike::swf_doc::{Symbol, SwfDoc};

fn main() -> Result<()> {
    let path = PathBuf::from(
        std::env::args().nth(1).ok_or_else(|| anyhow!("usage: dump-fills <swf>"))?,
    );
    let doc = SwfDoc::load(&path)?;

    let mut ids: Vec<u16> = doc.by_id.keys().copied().collect();
    ids.sort();
    for id in ids {
        if let Some(Symbol::Shape(shape)) = doc.by_id.get(&id) {
            println!("\n== shape {} ==", id);
            for (i, fs) in shape.styles.fill_styles.iter().enumerate() {
                match fs {
                    swf::FillStyle::Color(c) => {
                        println!(
                            "  fill[{}] Color rgba=#{:02x}{:02x}{:02x}{:02x} (alpha={}/255 = {:.3})",
                            i, c.r, c.g, c.b, c.a, c.a, c.a as f32 / 255.0
                        );
                    }
                    swf::FillStyle::LinearGradient(g) | swf::FillStyle::RadialGradient(g) => {
                        let kind = if matches!(fs, swf::FillStyle::LinearGradient(_)) { "Linear" } else { "Radial" };
                        println!("  fill[{}] {}Gradient stops:", i, kind);
                        for (si, s) in g.records.iter().enumerate() {
                            println!(
                                "    stop[{}] ratio={} color=#{:02x}{:02x}{:02x}{:02x} (alpha={}/255)",
                                si, s.ratio, s.color.r, s.color.g, s.color.b, s.color.a, s.color.a
                            );
                        }
                    }
                    swf::FillStyle::FocalGradient { gradient: g, focal_point } => {
                        println!("  fill[{}] FocalGradient focal={} stops:", i, focal_point.to_f32());
                        for (si, s) in g.records.iter().enumerate() {
                            println!("    stop[{}] color=#{:02x}{:02x}{:02x}{:02x}", si, s.color.r, s.color.g, s.color.b, s.color.a);
                        }
                    }
                    swf::FillStyle::Bitmap { id, .. } => {
                        println!("  fill[{}] Bitmap id={}", i, id);
                    }
                }
            }
        }
    }
    Ok(())
}
