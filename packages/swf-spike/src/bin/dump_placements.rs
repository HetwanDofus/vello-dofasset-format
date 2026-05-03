//! Print every PlaceObject across the SWF tree, listing blend_mode, filter
//! count, clip_actions, and clip_depth. Used to verify whether a spell SWF
//! actually uses BlendMode / filters before we tune the renderer for them.

use std::path::PathBuf;

use anyhow::{anyhow, Result};

use swf_spike::swf_doc::{OwnedOp, Symbol, SwfDoc};

fn main() -> Result<()> {
    let path = PathBuf::from(
        std::env::args().nth(1).ok_or_else(|| anyhow!("usage: dump-placements <swf>"))?,
    );
    let doc = SwfDoc::load(&path)?;

    let mut blend_count = 0usize;
    let mut filter_count = 0usize;
    let mut clip_action_count = 0usize;

    let verbose = std::env::args().any(|a| a == "--verbose" || a == "-v");
    let mut ids: Vec<u16> = doc.by_id.keys().copied().collect();
    ids.sort();
    for id in ids {
        if let Some(Symbol::Sprite(sp)) = doc.by_id.get(&id) {
            if verbose {
                println!("\n-- sprite {} ({} frames, {} ops) --", id, sp.num_frames, sp.ops.len());
            }
            let mut frame = 1u16;
            for op in &sp.ops {
                match op {
                    OwnedOp::Place(p) => {
                        let mut tags = Vec::new();
                        if let Some(bm) = p.blend_mode {
                            tags.push(format!("blend={:?}", bm));
                            blend_count += 1;
                        }
                        if !p.filters.is_empty() {
                            tags.push(format!("filters={}", p.filters.len()));
                            filter_count += 1;
                        }
                        if !p.clip_actions.is_empty() {
                            tags.push(format!("clip_actions={}", p.clip_actions.len()));
                            clip_action_count += 1;
                        }
                        if let Some(cd) = p.clip_depth {
                            tags.push(format!("clip_depth={}", cd));
                        }
                        if let Some(rt) = p.ratio {
                            tags.push(format!("ratio={}", rt));
                        }
                        if let Some(cx) = &p.color_transform {
                            // Only show if not identity.
                            if !cx.is_identity() {
                                tags.push(format!(
                                    "cx=mult({:.2},{:.2},{:.2},{:.2}) add({:.2},{:.2},{:.2},{:.2})",
                                    cx.mult_r, cx.mult_g, cx.mult_b, cx.mult_a,
                                    cx.add_r, cx.add_g, cx.add_b, cx.add_a
                                ));
                            }
                        }
                        if let Some(m) = p.matrix {
                            // Only show non-identity matrices.
                            let coeffs = m.as_coeffs();
                            let identity = (coeffs[0] - 1.0).abs() < 1e-6
                                && coeffs[1].abs() < 1e-6
                                && coeffs[2].abs() < 1e-6
                                && (coeffs[3] - 1.0).abs() < 1e-6
                                && coeffs[4].abs() < 1e-6
                                && coeffs[5].abs() < 1e-6;
                            if !identity {
                                tags.push(format!(
                                    "m=({:.1},{:.1},{:.1},{:.1};{:.1},{:.1})",
                                    coeffs[0], coeffs[1], coeffs[2], coeffs[3], coeffs[4], coeffs[5]
                                ));
                            }
                        }
                        if verbose {
                            let kind = if p.is_move { "Modify" } else { "Place" };
                            let tagstr = if tags.is_empty() { "".to_string() } else { format!(" [{}]", tags.join(",")) };
                            println!(
                                "  f{} {} depth={} char={:?}{}",
                                frame, kind, p.depth, p.character_id, tagstr
                            );
                        } else if !tags.is_empty() {
                            println!(
                                "sprite={} depth={} char={:?} {}",
                                id,
                                p.depth,
                                p.character_id,
                                tags.join(" ")
                            );
                        }
                    }
                    OwnedOp::Remove { depth } => {
                        if verbose {
                            println!("  f{} Remove depth={}", frame, depth);
                        }
                    }
                    OwnedOp::ShowFrame => {
                        frame += 1;
                    }
                    OwnedOp::DoAction(_) => {
                        if verbose {
                            println!("  f{} DoAction", frame);
                        }
                    }
                }
            }
        }
    }
    // Also walk the root timeline.
    for op in &doc.root.ops {
        if let OwnedOp::Place(p) = op {
            let mut tags = Vec::new();
            if let Some(bm) = p.blend_mode {
                tags.push(format!("blend={:?}", bm));
                blend_count += 1;
            }
            if !p.filters.is_empty() {
                tags.push(format!("filters={}", p.filters.len()));
                filter_count += 1;
            }
            if !p.clip_actions.is_empty() {
                tags.push(format!("clip_actions={}", p.clip_actions.len()));
                clip_action_count += 1;
            }
            if !tags.is_empty() {
                println!(
                    "ROOT depth={} char={:?} {}",
                    p.depth,
                    p.character_id,
                    tags.join(" ")
                );
            }
        }
    }
    println!();
    println!("totals: blend_mode={} filters={} clip_actions={}",
        blend_count, filter_count, clip_action_count);
    Ok(())
}
