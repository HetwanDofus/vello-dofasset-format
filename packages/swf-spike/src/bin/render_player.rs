//! Tiny harness to render *only* the player sprite from sprites/10.swf so we
//! can debug the SWF→Vello path without map composition in the way.

use anyhow::{anyhow, Result};
use std::path::PathBuf;
use vello::kurbo::{Affine, Vec2};
use vello::peniko::Color;
use vello::Scene;

use swf_spike::headless::Headless;
use swf_spike::render::{render_export, DocPool};
use swf_spike::swf_doc::SwfDoc;

const W: u32 = 600;
const H: u32 = 800;

fn main() -> Result<()> {
    pollster::block_on(run())
}

async fn run() -> Result<()> {
    let argv: Vec<String> = std::env::args().collect();
    let anim = argv.get(1).cloned().unwrap_or_else(|| "staticR".into());
    let frame: u16 = argv.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);

    let player_path = PathBuf::from(
        "/Users/grandnainconnu/Work/personal/dofus/dofus1.29/dofus-client-recode/dofuswebclient2/assets/sources/clips/sprites/10.swf",
    );
    let doc = SwfDoc::load(&player_path)?;
    eprintln!(
        "loaded player swf — stage {:?}, exports {}",
        doc.stage_size,
        doc.by_name.len()
    );
    if !doc.by_name.contains_key(&anim) {
        return Err(anyhow!("no export named {}", anim));
    }

    let mut scene = Scene::new();
    let pool = DocPool::new(vec![&doc]);
    // Twips → pixels, then drop player at (W/2, H * 0.8) so feet sit lower-center.
    let xform = Affine::scale(1.0 / 20.0)
        .then_translate(Vec2::new(f64::from(W) / 2.0, f64::from(H) * 0.8));
    render_export(&pool, &anim, &mut scene, xform, frame)?;

    let mut headless = Headless::new().await?;
    let pixels = headless.render_to_pixels(&scene, W, H, Color::from_rgba8(40, 40, 60, 255))?;
    image::save_buffer(
        "output/render-player.png",
        &pixels,
        W,
        H,
        image::ColorType::Rgba8,
    )?;
    eprintln!("wrote output/render-player.png");
    Ok(())
}
