//! Render Dofus 1.29 map 7411 + walking/running player sprite 10 by reading
//! original SWFs directly into Vello — no SVG, no .dofasset.
//!
//! Usage:
//!   render-map --map output/map-7411.json --out render-map.png \
//!              [--cell N] [--anim walkR] [--frame 0] [--all-frames]

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use vello::kurbo::Affine;
use vello::peniko::Color;
use vello::Scene;

use swf_spike::headless::Headless;
use swf_spike::render::{render_export, render_symbol, DocPool};
use swf_spike::swf_doc::{Symbol, SwfDoc};

// ---------------------------------------------------------------------------
// Constants — port from PHP MapRenderer.
const DISPLAY_WIDTH: u32 = 742;
const DISPLAY_HEIGHT: u32 = 432;
const CELL_WIDTH: f64 = 53.0;
const CELL_HEIGHT: f64 = 27.0;
const CELL_HALF_WIDTH: f64 = 26.5;
const CELL_HALF_HEIGHT: f64 = 13.5;
const LEVEL_HEIGHT: f64 = 20.0;

// SWF coordinates are twips (1/20 px). Tile sprites are designed at "real px"
// once you convert. SWF→pixel = twips / 20. Vello sees twips, so we apply
// `Affine::scale(1/20)` everywhere.
const TWIP_TO_PX: f64 = 1.0 / 20.0;

#[derive(Debug, serde::Deserialize)]
struct MapJson {
    id: u32,
    width: u32,
    height: u32,
    background: u32,
    cells: Vec<CellJson>,
}

#[derive(Debug, serde::Deserialize)]
struct CellJson {
    id: u32,
    active: bool,
    ground: u32,
    layer1: u32,
    layer2: u32,
    #[serde(rename = "groundLevel")]
    ground_level: i32,
    #[serde(default)]
    #[allow(dead_code)]
    ground_slope: u32,
    #[serde(rename = "groundRot", default)]
    ground_rot: u32,
    #[serde(rename = "groundFlip", default)]
    ground_flip: bool,
    #[serde(rename = "layer1Rot", default)]
    layer1_rot: u32,
    #[serde(rename = "layer1Flip", default)]
    layer1_flip: bool,
    #[serde(rename = "layer2Flip", default)]
    layer2_flip: bool,
}

// ---------------------------------------------------------------------------
struct Args {
    map_path: PathBuf,
    out_path: PathBuf,
    player_cell: Option<u32>,
    player_anim: String,
    player_frame: u32,
    all_frames: bool,
    scale: f64,
}

fn parse_args() -> Result<Args> {
    let argv: Vec<String> = std::env::args().collect();
    let mut a = Args {
        map_path: PathBuf::from("output/map-7411.json"),
        out_path: PathBuf::from("output/render-map.png"),
        player_cell: None,
        player_anim: "walkR".to_string(),
        player_frame: 0,
        all_frames: false,
        scale: 1.0,
    };
    let mut i = 1;
    while i < argv.len() {
        match argv[i].as_str() {
            "--map" => {
                a.map_path = PathBuf::from(argv.get(i + 1).cloned().unwrap_or_default());
                i += 2;
            }
            "--out" => {
                a.out_path = PathBuf::from(argv.get(i + 1).cloned().unwrap_or_default());
                i += 2;
            }
            "--cell" => {
                a.player_cell = argv.get(i + 1).and_then(|s| s.parse().ok());
                i += 2;
            }
            "--anim" => {
                a.player_anim = argv.get(i + 1).cloned().unwrap_or_default();
                i += 2;
            }
            "--frame" => {
                a.player_frame = argv.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(0);
                i += 2;
            }
            "--all-frames" => {
                a.all_frames = true;
                i += 1;
            }
            "--scale" => {
                a.scale = argv
                    .get(i + 1)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(1.0);
                i += 2;
            }
            other => return Err(anyhow!("unknown argument: {other}")),
        }
    }
    Ok(a)
}

// ---------------------------------------------------------------------------
const CLIPS_BASE: &str =
    "/Users/grandnainconnu/Work/personal/dofus/dofus1.29/dofus-client-recode/dofuswebclient2/assets/sources/clips";

fn ground_swfs() -> Vec<PathBuf> {
    ["g1.swf", "g2.swf"]
        .iter()
        .map(|n| PathBuf::from(format!("{CLIPS_BASE}/gfx/{n}")))
        .collect()
}

fn object_swfs() -> Vec<PathBuf> {
    (1..=12)
        .map(|i| PathBuf::from(format!("{CLIPS_BASE}/gfx/o{i}.swf")))
        .collect()
}

fn player_swf() -> PathBuf {
    PathBuf::from(format!("{CLIPS_BASE}/sprites/10.swf"))
}

// ---------------------------------------------------------------------------
fn cell_position(cell_id: u32, map_width: u32, ground_level: i32) -> (f64, f64) {
    // Mirrors CellShape::fromCellId.
    let line_div = (map_width as i32 * 2 - 1).max(1);
    let line = (cell_id as i32) / line_div;
    let mut col = (cell_id as i32) % line_div;
    let sub_line = if col >= map_width as i32 {
        col -= map_width as i32;
        1
    } else {
        0
    };
    let x = f64::from(col) * CELL_WIDTH + f64::from(sub_line) * CELL_HALF_WIDTH;
    let y = f64::from(line) * CELL_HEIGHT + f64::from(sub_line) * CELL_HALF_HEIGHT
        - LEVEL_HEIGHT * f64::from(ground_level - 7);
    (x, y)
}

// ---------------------------------------------------------------------------
fn load_doc(path: &Path) -> Result<SwfDoc> {
    SwfDoc::load(path).with_context(|| format!("loading {}", path.display()))
}

// Build a flat name→(doc_idx, char_id) export map across a list of docs so
// numeric tile IDs (which can sit in any of g1/g2 or o1..o12) resolve in O(1).
struct Pool {
    docs: Vec<SwfDoc>,
    /// numeric export → doc index that owns it
    by_id: HashMap<u32, usize>,
}

impl Pool {
    fn new(paths: &[PathBuf]) -> Result<Self> {
        let mut docs = Vec::with_capacity(paths.len());
        for p in paths {
            docs.push(load_doc(p)?);
        }
        let mut by_id: HashMap<u32, usize> = HashMap::new();
        // Earlier files win — matches the PHP SwfSpriteRepository priority.
        for (i, d) in docs.iter().enumerate() {
            for (name, _) in &d.by_name {
                if let Ok(n) = name.parse::<u32>() {
                    by_id.entry(n).or_insert(i);
                }
            }
        }
        Ok(Pool { docs, by_id })
    }

    fn lookup(&self, id: u32) -> Option<(&SwfDoc, &Symbol)> {
        let doc_idx = *self.by_id.get(&id)?;
        let doc = &self.docs[doc_idx];
        let name = id.to_string();
        let sym = doc.lookup_export(&name)?;
        Some((doc, sym))
    }
}

// ---------------------------------------------------------------------------
/// Build a Vello scene for one whole map frame. `player_anim_frame` is the
/// player's walk/run frame index, propagated to the player sprite tree only.
fn build_map_scene(
    map: &MapJson,
    grounds: &Pool,
    objects: &Pool,
    player_doc: &SwfDoc,
    player_anim: &str,
    player_cell: u32,
    player_frame: u16,
    output_scale: f64,
) -> Result<Scene> {
    let mut scene = Scene::new();

    // The whole tile coordinate space is in twips inside the SWF; we want
    // pixels in the output image. Apply the scale at every placement.
    let twip_scale = Affine::scale(TWIP_TO_PX * output_scale);

    // 1. Background sprite — covers the full display.
    if map.background != 0 {
        if let Some((doc, sym)) = grounds.lookup(map.background) {
            // Background sprites have their natural origin somewhere in their
            // bounding box. The PHP renderer uses its `offsetX/offsetY` as the
            // top-left target. For the spike, plant it at (0,0) in pixel space.
            render_symbol(doc, sym, &mut scene, twip_scale, 0);
        }
    }

    // 2. Per-cell tiles.
    for cell in &map.cells {
        if !cell.active && cell.ground == 0 && cell.layer1 == 0 && cell.layer2 == 0 {
            continue;
        }
        let (cx, cy) = cell_position(cell.id, map.width, cell.ground_level);
        let center = (
            (cx + CELL_HALF_WIDTH) * output_scale,
            (cy + CELL_HALF_HEIGHT) * output_scale,
        );

        // Ground.
        if cell.ground != 0 {
            place_tile(
                &mut scene,
                grounds,
                cell.ground,
                center,
                cell.ground_rot,
                cell.ground_flip,
                output_scale,
            );
        }
        // Layer1 (objects below the player).
        if cell.layer1 != 0 {
            place_tile(
                &mut scene,
                objects,
                cell.layer1,
                center,
                cell.layer1_rot,
                cell.layer1_flip,
                output_scale,
            );
        }
    }

    // 3. Player sprite at the requested cell.
    if let Some(cell) = map.cells.iter().find(|c| c.id == player_cell) {
        let (cx, cy) = cell_position(cell.id, map.width, cell.ground_level);
        let center = (
            (cx + CELL_HALF_WIDTH) * output_scale,
            (cy + CELL_HALF_HEIGHT) * output_scale,
        );
        let pool = DocPool::new(vec![player_doc]);
        let xform =
            Affine::scale(TWIP_TO_PX * output_scale).then_translate(center.into());
        render_export(&pool, player_anim, &mut scene, xform, player_frame)?;
    }

    // 4. Layer 2 second pass (over the player).
    for cell in &map.cells {
        if cell.layer2 == 0 {
            continue;
        }
        let (cx, cy) = cell_position(cell.id, map.width, cell.ground_level);
        let center = (
            (cx + CELL_HALF_WIDTH) * output_scale,
            (cy + CELL_HALF_HEIGHT) * output_scale,
        );
        place_tile(
            &mut scene,
            objects,
            cell.layer2,
            center,
            0,
            cell.layer2_flip,
            output_scale,
        );
    }

    Ok(scene)
}

fn place_tile(
    scene: &mut Scene,
    pool: &Pool,
    id: u32,
    center: (f64, f64),
    rot: u32,
    flip: bool,
    output_scale: f64,
) {
    let Some((doc, sym)) = pool.lookup(id) else {
        return;
    };
    let rot_rad = std::f64::consts::FRAC_PI_2 * f64::from(rot);
    let mut xform = Affine::scale(TWIP_TO_PX * output_scale);
    xform = xform.then_rotate(rot_rad);
    if flip {
        xform = xform.then_scale_non_uniform(-1.0, 1.0);
    }
    xform = xform.then_translate(vello::kurbo::Vec2::new(center.0, center.1));
    render_symbol(doc, sym, scene, xform, 0);
}

// ---------------------------------------------------------------------------
fn save_png(out: &Path, pixels: &[u8], w: u32, h: u32) -> Result<()> {
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent).ok();
    }
    image::save_buffer(out, pixels, w, h, image::ColorType::Rgba8)?;
    Ok(())
}

fn main() -> Result<()> {
    pollster::block_on(run())
}

async fn run() -> Result<()> {
    let args = parse_args()?;
    eprintln!("loading map {}", args.map_path.display());
    let map_text = fs::read_to_string(&args.map_path)?;
    let map: MapJson = serde_json::from_str(&map_text)?;
    eprintln!(
        "map {} ({}x{}, bg={}, {} cells)",
        map.id,
        map.width,
        map.height,
        map.background,
        map.cells.len()
    );

    eprintln!("loading SWFs (g1/g2 + o1..o12 + sprites/10)…");
    let grounds = Pool::new(&ground_swfs())?;
    let objects = Pool::new(&object_swfs())?;
    let player = load_doc(&player_swf())?;
    eprintln!(
        "  ground tiles: {} ids; object tiles: {} ids; player exports: {}",
        grounds.by_id.len(),
        objects.by_id.len(),
        player.by_name.len()
    );

    let player_cell = args
        .player_cell
        .or_else(|| map.cells.iter().find(|c| c.active).map(|c| c.id))
        .unwrap_or(0);
    eprintln!("player @ cell {}, anim {}", player_cell, args.player_anim);

    let mut headless = Headless::new().await?;
    let canvas_w = (f64::from(DISPLAY_WIDTH) * args.scale) as u32;
    let canvas_h = (f64::from(DISPLAY_HEIGHT) * args.scale) as u32;

    if args.all_frames {
        let frames = anim_frame_count(&player, &args.player_anim).max(1);
        eprintln!("rendering {} frames…", frames);
        let cols = (frames as f64).sqrt().ceil() as u32;
        let rows = (u32::from(frames) + cols - 1) / cols;
        let mut grid = vec![0u8; (cols * canvas_w * rows * canvas_h * 4) as usize];
        for f in 0..frames {
            let scene = build_map_scene(
                &map,
                &grounds,
                &objects,
                &player,
                &args.player_anim,
                player_cell,
                f,
                args.scale,
            )?;
            let pixels =
                headless.render_to_pixels(&scene, canvas_w, canvas_h, Color::TRANSPARENT)?;
            let col = u32::from(f) % cols;
            let row = u32::from(f) / cols;
            blit(
                &mut grid,
                cols * canvas_w,
                rows * canvas_h,
                col * canvas_w,
                row * canvas_h,
                &pixels,
                canvas_w,
                canvas_h,
            );
        }
        save_png(&args.out_path, &grid, cols * canvas_w, rows * canvas_h)?;
        eprintln!("wrote {}", args.out_path.display());
    } else {
        let scene = build_map_scene(
            &map,
            &grounds,
            &objects,
            &player,
            &args.player_anim,
            player_cell,
            args.player_frame as u16,
            args.scale,
        )?;
        let pixels =
            headless.render_to_pixels(&scene, canvas_w, canvas_h, Color::TRANSPARENT)?;
        save_png(&args.out_path, &pixels, canvas_w, canvas_h)?;
        eprintln!("wrote {}", args.out_path.display());
    }
    Ok(())
}

/// `walkR` and friends are 1-frame wrappers that place a child sprite holding
/// the actual animation. Recurse a few levels and return the largest
/// `num_frames` we find — that's the playable animation length.
fn anim_frame_count(doc: &SwfDoc, name: &str) -> u16 {
    let Some(Symbol::Sprite(top)) = doc.lookup_export(name) else {
        return 1;
    };
    let mut best = top.num_frames;
    fn walk(doc: &SwfDoc, sprite: &swf_spike::swf_doc::OwnedSprite, depth: u32, best: &mut u16) {
        if depth == 0 {
            return;
        }
        for op in &sprite.ops {
            if let swf_spike::swf_doc::OwnedOp::Place(p) = op {
                if let Some(id) = p.character_id {
                    if let Some(Symbol::Sprite(child)) = doc.lookup_id(id) {
                        if child.num_frames > *best {
                            *best = child.num_frames;
                        }
                        walk(doc, child, depth - 1, best);
                    }
                }
            }
        }
    }
    walk(doc, top, 4, &mut best);
    best
}

fn blit(
    dst: &mut [u8],
    dst_w: u32,
    _dst_h: u32,
    dx: u32,
    dy: u32,
    src: &[u8],
    sw: u32,
    sh: u32,
) {
    for y in 0..sh {
        let src_off = (y * sw * 4) as usize;
        let dst_off = (((dy + y) * dst_w + dx) * 4) as usize;
        let len = (sw * 4) as usize;
        dst[dst_off..dst_off + len].copy_from_slice(&src[src_off..src_off + len]);
    }
}
