//! Lay out a SWF spell animation and the player's cast pose side-by-side as a
//! single PNG spritesheet — sanity check that SWF→Vello renders multi-frame
//! animations correctly across both spell-VFX and character-sprite content.
//!
//! Usage:
//!     render-cast-sheet <spell.swf> <player.swf> <out.png> \
//!         [--scale N] [--player-export anim1R] [--cols N]
//!
//! Layout: row 0 = spell anim1 frames, row 1 = player cast frames.
//! Each row is sized to the UNION of its per-frame bounds so frames align.

use std::fs;
use std::path::PathBuf;

use anyhow::{anyhow, Result};
use vello::kurbo::{Affine, Rect, Vec2};
use vello::peniko::Color;
use vello::Scene;

use swf_spike::headless::Headless;
use swf_spike::render::{render_symbol, symbol_bounds};
use swf_spike::swf_doc::{OwnedOp, OwnedSprite, Symbol, SwfDoc};

struct AnimSpec<'a> {
    label: &'static str,
    doc: &'a SwfDoc,
    sym: &'a Symbol,
    frame_count: u16,
}

fn main() -> Result<()> {
    pollster::block_on(run())
}

async fn run() -> Result<()> {
    let argv: Vec<String> = std::env::args().collect();
    if argv.len() < 4 {
        eprintln!(
            "usage: render-cast-sheet <spell.swf> <player.swf> <out.png> \
             [--scale N] [--player-export anim1R] [--cols N]"
        );
        std::process::exit(2);
    }
    let spell_path = PathBuf::from(&argv[1]);
    let player_path = PathBuf::from(&argv[2]);
    let out_path = PathBuf::from(&argv[3]);
    let scale: f64 = parse_flag(&argv, "--scale").unwrap_or(1.0);
    let player_export = parse_flag_str(&argv, "--player-export")
        .unwrap_or_else(|| "anim1R".to_string());
    let cols_override: Option<usize> = parse_flag(&argv, "--cols");

    let spell = SwfDoc::load(&spell_path)?;
    let player = SwfDoc::load(&player_path)?;

    // Spell SWFs typically expose no ExportAssets — pick the DefineSprite with
    // the most frames as the main animation (matches Arakne's convention of
    // labelling the longest sprite `anim1`).
    let (spell_sym, spell_frames) = pick_longest_sprite(&spell)
        .ok_or_else(|| anyhow!("{} has no DefineSprite tags", spell_path.display()))?;
    eprintln!(
        "spell main sprite: {} frames",
        spell_frames
    );

    // Player cast pose: Dofus convention is `anim1<DIR>` where DIR ∈ L/R/F/B/S.
    let player_sym = player
        .lookup_export(&player_export)
        .ok_or_else(|| anyhow!("no export `{}` in {}", player_export, player_path.display()))?;
    // Dofus character sprites (`anim1R`, `walkR`, etc.) are *wrapper* sprites:
    // a single-frame timeline that PlaceObjects an inner DefineSprite holding
    // the actual N-frame animation. Reading `num_frames` directly returns 1.
    // Recurse the same way `vello-wasm::swf_anim_frame_count` does (lib.rs:569)
    // and use the longest nested sprite as the effective frame count.
    let player_frames = match player_sym {
        Symbol::Sprite(s) => longest_nested_frames(&player, s, 4),
        _ => 1,
    };
    eprintln!(
        "player export `{}`: {} frames",
        player_export, player_frames
    );

    let specs = vec![
        AnimSpec { label: "spell", doc: &spell, sym: spell_sym, frame_count: spell_frames },
        AnimSpec { label: "player", doc: &player, sym: player_sym, frame_count: player_frames },
    ];

    // Per-row union bounds + cell size in device pixels.
    let twip_scale = scale / 20.0;
    let pad = 2.0;
    struct RowLayout {
        cell_w: u32,
        cell_h: u32,
        union_min_twip_x: f64,
        union_min_twip_y: f64,
        frame_count: u16,
    }
    let mut rows: Vec<RowLayout> = Vec::with_capacity(specs.len());
    for spec in &specs {
        let mut union: Option<Rect> = None;
        for f in 0..spec.frame_count.max(1) {
            let r = symbol_bounds(spec.doc, spec.sym, f);
            if r.width() <= 0.0 || r.height() <= 0.0 {
                continue;
            }
            union = Some(match union {
                None => r,
                Some(prev) => prev.union(r),
            });
        }
        let r = union.unwrap_or(Rect::new(0.0, 0.0, 1.0, 1.0));
        let px_min_x = (r.x0 * twip_scale).floor() - pad;
        let px_min_y = (r.y0 * twip_scale).floor() - pad;
        let px_max_x = (r.x1 * twip_scale).ceil() + pad;
        let px_max_y = (r.y1 * twip_scale).ceil() + pad;
        let cell_w = ((px_max_x - px_min_x) as u32).max(1);
        let cell_h = ((px_max_y - px_min_y) as u32).max(1);
        rows.push(RowLayout {
            cell_w,
            cell_h,
            union_min_twip_x: r.x0 - pad / twip_scale,
            union_min_twip_y: r.y0 - pad / twip_scale,
            frame_count: spec.frame_count.max(1),
        });
        eprintln!(
            "row `{}`: {} frames at {}×{} px",
            spec.label, spec.frame_count, cell_w, cell_h
        );
    }

    // Lay out frames in a grid: cap row width, wrap if needed.
    // Default cols = the longer animation's frame count, capped to a sensible
    // number so the PNG isn't 60 000 px wide for a 200-frame anim.
    let cols = cols_override
        .unwrap_or_else(|| {
            let max_frames = rows.iter().map(|r| usize::from(r.frame_count)).max().unwrap_or(1);
            max_frames.min(16)
        })
        .max(1);
    let row_strips: Vec<(Vec<(usize, usize)>, u32, u32)> = rows
        .iter()
        .map(|row| {
            let n = usize::from(row.frame_count);
            let strip_rows = (n + cols - 1) / cols;
            // (col, sub-row) per frame index
            let placements: Vec<(usize, usize)> = (0..n)
                .map(|i| (i % cols, i / cols))
                .collect();
            (placements, row.cell_w, row.cell_h * strip_rows as u32)
        })
        .collect();

    let sheet_w: u32 = row_strips
        .iter()
        .map(|(_, w, _)| *w * cols as u32)
        .max()
        .unwrap_or(1);
    let sheet_h: u32 = row_strips.iter().map(|(_, _, h)| *h).sum();
    eprintln!("sheet: {}×{} px ({} cols)", sheet_w, sheet_h, cols);

    let mut headless = Headless::new().await?;

    let mut sheet = vec![0u8; (sheet_w * sheet_h * 4) as usize];

    let mut row_y_offset: u32 = 0;
    for (i, spec) in specs.iter().enumerate() {
        let row = &rows[i];
        let (placements, _, strip_h) = &row_strips[i];

        for (frame_idx, (col, sub_row)) in placements.iter().enumerate() {
            let mut scene = Scene::new();
            // Translate the frame's content into the cell's pixel space.
            let xform = Affine::scale(twip_scale)
                .then_translate(Vec2::new(
                    -row.union_min_twip_x * twip_scale,
                    -row.union_min_twip_y * twip_scale,
                ));
            render_symbol(spec.doc, spec.sym, &mut scene, xform, frame_idx as u16);

            let pixels = headless.render_to_pixels(
                &scene,
                row.cell_w,
                row.cell_h,
                Color::TRANSPARENT,
            )?;

            // Blit into the sheet at (col * cell_w, row_y_offset + sub_row * cell_h).
            let dst_x = (*col as u32) * row.cell_w;
            let dst_y = row_y_offset + (*sub_row as u32) * row.cell_h;
            blit(
                &mut sheet,
                sheet_w,
                sheet_h,
                &pixels,
                row.cell_w,
                row.cell_h,
                dst_x,
                dst_y,
            );
        }

        row_y_offset += strip_h;
    }

    fs::create_dir_all(out_path.parent().unwrap_or(std::path::Path::new("."))).ok();
    image::save_buffer(&out_path, &sheet, sheet_w, sheet_h, image::ColorType::Rgba8)?;
    eprintln!("wrote {}", out_path.display());
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

fn pick_longest_sprite(doc: &SwfDoc) -> Option<(&Symbol, u16)> {
    let mut best: Option<(&Symbol, u16)> = None;
    for sym in doc.by_id.values() {
        if let Symbol::Sprite(s) = sym {
            match best {
                None => best = Some((sym, s.num_frames)),
                Some((_, n)) if s.num_frames > n => best = Some((sym, s.num_frames)),
                _ => {}
            }
        }
    }
    best
}

fn blit(
    dst: &mut [u8],
    dst_w: u32,
    _dst_h: u32,
    src: &[u8],
    src_w: u32,
    src_h: u32,
    x: u32,
    y: u32,
) {
    for row in 0..src_h {
        let src_off = (row * src_w * 4) as usize;
        let dst_off = (((y + row) * dst_w + x) * 4) as usize;
        let bytes = (src_w * 4) as usize;
        dst[dst_off..dst_off + bytes].copy_from_slice(&src[src_off..src_off + bytes]);
    }
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
