//! Render a spell SWF on top of a player SWF, frame-by-frame, into a PNG
//! sequence. Used to preview spell VFX as they'd composite over a caster
//! in-game (so we can side-by-side compare with a real client recording).
//!
//! Usage:
//!     render-spell-with-player <spell.swf> <player.swf> <out_dir> \
//!         [--scale N] [--player-export anim1R] [--bg COLOR] [--frames N]
//!
//! Layout: each output PNG is one composite frame. The player draws first
//! (statelessly), then the spell on top via the stateful AVM1 renderer.
//! Pipe the resulting `out_dir/0000.png … N.png` through ffmpeg to get a
//! video.

use std::fs;
use std::path::PathBuf;

use anyhow::{anyhow, Result};
use vello::kurbo::{Affine, Rect, Vec2};
use vello::peniko::Color;
use vello::Scene;

use swf_spike::headless::Headless;
use swf_spike::render::{render_symbol_xformed, symbol_bounds, WgpuCtx};
use swf_spike::render_avm1::AvmRenderer;
use swf_spike::swf_doc::{OwnedColorTransform, OwnedOp, OwnedSprite, Symbol, SwfDoc};
use swf_spike::wgpu_filters::FilterPipelines;

fn main() -> Result<()> {
    pollster::block_on(run())
}

async fn run() -> Result<()> {
    let argv: Vec<String> = std::env::args().collect();
    if argv.len() < 4 {
        eprintln!(
            "usage: render-spell-with-player <spell.swf> <player.swf> <out_dir> \
             [--scale N] [--player-export anim1R] [--bg COLOR] [--frames N]"
        );
        std::process::exit(2);
    }
    let spell_path = PathBuf::from(&argv[1]);
    let player_path = PathBuf::from(&argv[2]);
    let out_dir = PathBuf::from(&argv[3]);
    let scale: f64 = parse_flag(&argv, "--scale").unwrap_or(2.0);
    let player_export =
        parse_flag_str(&argv, "--player-export").unwrap_or_else(|| "anim1R".to_string());
    let bg = match parse_flag_str(&argv, "--bg").as_deref() {
        Some("white") => Color::WHITE,
        Some("black") => Color::BLACK,
        Some("gray") | Some("grey") => Color::from_rgba8(128, 128, 128, 255),
        // Sampled from the user-provided in-game reference screenshot of
        // spell 802 cast — average of several grass pixels (172,185,60).
        // Spell shape 1's low-alpha colours blend into this almost
        // invisibly, matching the in-game look.
        Some("grass") | Some("game") => Color::from_rgba8(172, 185, 60, 255),
        _ => Color::TRANSPARENT,
    };
    let frames_override: Option<u16> = parse_flag(&argv, "--frames");

    let spell_doc = SwfDoc::load(&spell_path)?;
    let player_doc = SwfDoc::load(&player_path)?;

    // Spell: prefer the sprite the root timeline actually places at depth 1
    // (e.g. spell 909 root places sprite 22, but the SWF also contains a
    // larger sibling sprite 21 with bounds extending 3000+ twips offscreen
    // — picking "longest" there inflates the canvas to ~900×1200 px and
    // shrinks the visible content to a corner). Fall back to "longest
    // sprite" only if the root has no such Place op.
    let (spell_sprite_id, spell_total) = {
        let mut from_root: Option<(u16, u16)> = None;
        for op in &spell_doc.root.ops {
            if let swf_spike::swf_doc::OwnedOp::Place(p) = op
                && let Some(cid) = p.character_id
                && let Some(Symbol::Sprite(s)) = spell_doc.by_id.get(&cid)
            {
                from_root = Some((cid, s.num_frames));
                break;
            }
        }
        if let Some(v) = from_root {
            v
        } else {
            let mut best: Option<(u16, u16)> = None;
            for (id, sym) in &spell_doc.by_id {
                if let Symbol::Sprite(s) = sym {
                    match best {
                        None => best = Some((*id, s.num_frames)),
                        Some((_, n)) if s.num_frames > n => {
                            best = Some((*id, s.num_frames))
                        }
                        _ => {}
                    }
                }
            }
            best.ok_or_else(|| anyhow!("no DefineSprite in spell SWF"))?
        }
    };

    let player_sym = player_doc
        .lookup_export(&player_export)
        .ok_or_else(|| anyhow!("no export `{}` in player SWF", player_export))?;
    let player_total = match player_sym {
        Symbol::Sprite(s) => longest_nested_frames(&player_doc, s, 4),
        _ => 1,
    };

    let total_frames = frames_override.unwrap_or_else(|| spell_total.max(player_total));
    eprintln!(
        "spell sprite={} ({} frames), player export `{}` ({} frames), output {} frames",
        spell_sprite_id, spell_total, player_export, player_total, total_frames
    );

    // Per-frame canvas: union over the entire animation of BOTH anims so the
    // composite never clips.
    let twip_scale = scale / 20.0;
    let pad = 8.0;
    let mut union: Option<Rect> = None;
    let spell_sym_main = spell_doc.by_id.get(&spell_sprite_id).unwrap();
    for f in 0..total_frames.max(1) {
        let r1 = symbol_bounds(
            &spell_doc,
            spell_sym_main,
            f.min(spell_total.saturating_sub(1)),
        );
        let r2 = symbol_bounds(
            &player_doc,
            player_sym,
            f.min(player_total.saturating_sub(1)),
        );
        if r1.width() > 0.0 && r1.height() > 0.0 {
            union = Some(match union {
                None => r1,
                Some(prev) => prev.union(r1),
            });
        }
        if r2.width() > 0.0 && r2.height() > 0.0 {
            union = Some(match union {
                None => r2,
                Some(prev) => prev.union(r2),
            });
        }
    }
    let r = union.unwrap_or(Rect::new(0.0, 0.0, 1.0, 1.0));
    let px_min_x = (r.x0 * twip_scale).floor() - pad;
    let px_min_y = (r.y0 * twip_scale).floor() - pad;
    let px_max_x = (r.x1 * twip_scale).ceil() + pad;
    let px_max_y = (r.y1 * twip_scale).ceil() + pad;
    let cell_w = ((px_max_x - px_min_x) as u32).max(1);
    let cell_h = ((px_max_y - px_min_y) as u32).max(1);
    eprintln!("frame: {}×{} px (scale={})", cell_w, cell_h, scale);

    fs::create_dir_all(&out_dir)?;

    let mut headless = Headless::new().await?;
    let filter_pipelines = FilterPipelines::new(&headless.device);
    let mut renderer = AvmRenderer::new(spell_doc.root.num_frames);
    // Inject host game-state vars that scripts read via `_parent.cellFrom`,
    // `_parent.cellTo`, `_parent.level`, etc. In Dofus the host pushes
    // these before the spell SWF starts ticking. We hardcode "3 cells in
    // front" — caster at origin, target ~3 cells away in screen space (a
    // Dofus tile is ~86×43 px so 3 cells ≈ 258 px in iso x).
    renderer.set_host_cell("cellFrom", 0.0, 0.0);
    renderer.set_host_cell("cellTo", 258.0, 0.0);
    renderer.set_host_var("level", swf_spike::avm1::Value::Number(1.0));
    renderer.set_host_var("angle", swf_spike::avm1::Value::Number(0.0));
    renderer.set_host_var("distance", swf_spike::avm1::Value::Number(258.0));
    renderer.set_host_var("i", swf_spike::avm1::Value::Number(0.0));
    renderer.set_host_var("t", swf_spike::avm1::Value::Number(0.0));
    let spell_root_sym = Symbol::Sprite(spell_doc.root.clone());

    // World transform: shift twip-space origin to (pad, pad) device pixels
    // so both the player and spell render with the same viewport.
    let xform_base = Affine::scale(twip_scale).then_translate(Vec2::new(
        -r.x0 * twip_scale - pad,
        -r.y0 * twip_scale - pad,
    ));

    for frame_idx in 0..total_frames {
        // Tick the spell AVM1 timeline so per-frame rotation/alpha/etc.
        // mutations actually drive the rendered state.
        renderer.tick(&spell_doc, &spell_root_sym);

        let mut player_scene = Scene::new();
        let mut scene = Scene::new();
        // Caster underneath. The cast animation plays once and then holds
        // the final pose — don't loop it (in-game the character returns to
        // idle after the cast resolves, but this preview shows the cast
        // window only).
        let player_frame = if player_total == 0 {
            0
        } else {
            frame_idx.min(player_total.saturating_sub(1))
        };
        if std::env::var("NO_PLAYER").is_err() {
            render_symbol_xformed(
                &player_doc,
                player_sym,
                &mut player_scene,
                xform_base,
                player_frame,
                OwnedColorTransform::IDENTITY,
            );
        }
        // Spell composites on top via the stateful AVM1 renderer.
        {
            let mut ctx = WgpuCtx {
                device: &headless.device,
                queue: &headless.queue,
                renderer: &mut headless.renderer,
                filter_pipelines: &filter_pipelines,
                output_scale: scale,
            };
            renderer.render(
                &spell_doc,
                &spell_root_sym,
                &mut scene,
                xform_base,
                Some(&mut ctx),
            );
        }

        // Render player and spell to SEPARATE textures, then composite in
        // pixel space. Vello's tile-based encoder shares fine-shading state
        // between commands within a single Scene; with the player's many
        // hundreds of paths plus the spell's morph stroke in one Scene,
        // certain tiles overflow and the morph's mid-curve stroke gets
        // partially dropped — visible as a phantom notch/M-shape on the
        // morph arch. Compositing two pre-rasterized textures sidesteps
        // the encoder-state coupling entirely.
        let player_pixels = headless.render_to_pixels(&player_scene, cell_w, cell_h, bg)?;
        let spell_pixels = headless.render_to_pixels(
            &scene,
            cell_w,
            cell_h,
            vello::peniko::Color::TRANSPARENT,
        )?;
        let mut pixels = player_pixels;
        // SrcOver composite: spell on top of player.
        for i in 0..(cell_w * cell_h) as usize {
            let off = i * 4;
            let sa = spell_pixels[off + 3] as u32;
            let inv = 255 - sa;
            for c in 0..3 {
                pixels[off + c] = ((spell_pixels[off + c] as u32 * sa
                    + pixels[off + c] as u32 * inv)
                    / 255) as u8;
            }
            pixels[off + 3] =
                (sa + pixels[off + 3] as u32 * inv / 255).min(255) as u8;
        }
        let out_path = out_dir.join(format!("{:04}.png", frame_idx));
        image::save_buffer(&out_path, &pixels, cell_w, cell_h, image::ColorType::Rgba8)?;

        renderer.advance(&spell_doc, &spell_root_sym);
    }
    eprintln!("wrote {} frames to {}", total_frames, out_dir.display());
    eprintln!(
        "next: ffmpeg -framerate 24 -i {}/%04d.png -c:v libx264 -pix_fmt yuv420p -vf \"pad=ceil(iw/2)*2:ceil(ih/2)*2\" out.mp4",
        out_dir.display()
    );
    Ok(())
}

fn longest_nested_frames(doc: &SwfDoc, sp: &OwnedSprite, depth: u32) -> u16 {
    let mut best = sp.num_frames;
    fn recurse(doc: &SwfDoc, sp: &OwnedSprite, depth: u32, best: &mut u16) {
        if depth == 0 {
            return;
        }
        for op in &sp.ops {
            if let OwnedOp::Place(p) = op {
                if let Some(id) = p.character_id {
                    if let Some(Symbol::Sprite(child)) = doc.lookup_id(id) {
                        if child.num_frames > *best {
                            *best = child.num_frames;
                        }
                        recurse(doc, child, depth - 1, best);
                    }
                }
            }
        }
    }
    recurse(doc, sp, depth, &mut best);
    best
}

fn parse_flag<T: std::str::FromStr>(argv: &[String], name: &str) -> Option<T> {
    argv.iter()
        .position(|a| a == name)
        .and_then(|i| argv.get(i + 1))
        .and_then(|s| s.parse().ok())
}

fn parse_flag_str(argv: &[String], name: &str) -> Option<String> {
    argv.iter()
        .position(|a| a == name)
        .and_then(|i| argv.get(i + 1))
        .cloned()
}
