//! Print the union bbox (across all frames) of the longest sprite in a SWF.
//! Used to compute the right `--offset-x/y` and `--stage-width/height` for
//! Ruffle's exporter so spell content lands inside the viewport.

use std::path::PathBuf;

use anyhow::{anyhow, Result};
use vello::kurbo::Rect;

use swf_spike::render::symbol_bounds;
use swf_spike::swf_doc::{Symbol, SwfDoc};

fn main() -> Result<()> {
    let path = PathBuf::from(
        std::env::args().nth(1).ok_or_else(|| anyhow!("usage: print-bounds <swf>"))?,
    );
    let doc = SwfDoc::load(&path)?;

    let (sym, frames) = doc
        .by_id
        .values()
        .filter_map(|s| if let Symbol::Sprite(sp) = s { Some((s, sp.num_frames)) } else { None })
        .max_by_key(|(_, n)| *n)
        .ok_or_else(|| anyhow!("no DefineSprite"))?;

    let mut union: Option<Rect> = None;
    for f in 0..frames.max(1) {
        let r = symbol_bounds(&doc, sym, f);
        if r.width() <= 0.0 || r.height() <= 0.0 {
            continue;
        }
        union = Some(match union {
            None => r,
            Some(prev) => prev.union(r),
        });
    }

    let r = union.ok_or_else(|| anyhow!("all frames degenerate"))?;
    let twip_to_px = 1.0 / 20.0;
    println!("frames: {}", frames);
    println!("twip bbox: x=[{:.1} .. {:.1}] y=[{:.1} .. {:.1}]", r.x0, r.x1, r.y0, r.y1);
    println!("px bbox:   x=[{:.1} .. {:.1}] y=[{:.1} .. {:.1}]",
        r.x0 * twip_to_px, r.x1 * twip_to_px,
        r.y0 * twip_to_px, r.y1 * twip_to_px);
    println!("size:      {:.1} × {:.1} px",
        r.width() * twip_to_px, r.height() * twip_to_px);
    println!();
    println!("ruffle exporter args (centered in stage):");
    let stage_w = (r.width() * twip_to_px).ceil() as i64;
    let stage_h = (r.height() * twip_to_px).ceil() as i64;
    let offset_x = -(r.x0 * twip_to_px);
    let offset_y = -(r.y0 * twip_to_px);
    println!("  --stage-width {} --stage-height {}", stage_w, stage_h);
    println!("  --width {} --height {}", stage_w, stage_h);
    println!("  --offset-x {:.2} --offset-y {:.2}", offset_x, offset_y);
    Ok(())
}
