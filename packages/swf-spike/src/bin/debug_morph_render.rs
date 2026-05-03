//! Render morph 1370 directly at a few ratios to confirm the morph
//! interpolation produces non-empty geometry. If this works, then the
//! sprite-level path is the bug.

use anyhow::Result;
use std::path::PathBuf;
use vello::kurbo::{Affine, Vec2};
use vello::peniko::Color;
use vello::Scene;

use swf_spike::headless::Headless;
use swf_spike::render::render_symbol;
use swf_spike::swf_doc::{Symbol, SwfDoc};

fn main() -> Result<()> {
    pollster::block_on(run())
}

async fn run() -> Result<()> {
    let argv: Vec<String> = std::env::args().collect();
    let bundle = PathBuf::from(&argv[1]);
    let id: u16 = argv[2].parse().unwrap();
    let out = PathBuf::from(&argv[3]);

    let doc = SwfDoc::load(&bundle)?;
    let Some(sym) = doc.lookup_id(id) else {
        eprintln!("id missing");
        return Ok(());
    };
    let Symbol::MorphShape(ms) = sym else {
        eprintln!("not a morph");
        return Ok(());
    };
    eprintln!(
        "morph {}: start_bounds=({:.1},{:.1})-({:.1},{:.1}) end_bounds=({:.1},{:.1})-({:.1},{:.1})",
        id,
        f64::from(ms.start.shape_bounds.x_min.get()),
        f64::from(ms.start.shape_bounds.y_min.get()),
        f64::from(ms.start.shape_bounds.x_max.get()),
        f64::from(ms.start.shape_bounds.y_max.get()),
        f64::from(ms.end.shape_bounds.x_min.get()),
        f64::from(ms.end.shape_bounds.y_min.get()),
        f64::from(ms.end.shape_bounds.x_max.get()),
        f64::from(ms.end.shape_bounds.y_max.get()),
    );

    // Render at ratio 0, 16384, 32768, 49152, 65535 by manually building
    // the morph frame at each ratio and rendering it as a Shape.
    use swf_spike::morph::build_morph_frame;
    let ratios: [u16; 5] = [0, 16384, 32768, 49152, 65535];
    let union_b = swf_spike::morph::morph_bounds_union(ms);
    let scale = 4.0_f64;
    let twip_scale = scale / 20.0;
    let pad = 4.0;
    let px_min_x = (f64::from(union_b.x_min.get()) * twip_scale).floor() - pad;
    let px_min_y = (f64::from(union_b.y_min.get()) * twip_scale).floor() - pad;
    let px_max_x = (f64::from(union_b.x_max.get()) * twip_scale).ceil() + pad;
    let px_max_y = (f64::from(union_b.y_max.get()) * twip_scale).ceil() + pad;
    let cw = ((px_max_x - px_min_x) as u32).max(1);
    let ch = ((px_max_y - px_min_y) as u32).max(1);
    eprintln!("each cell {}x{}", cw, ch);

    let mut headless = Headless::new().await?;
    let cols = ratios.len() as u32;
    let mut sheet = vec![0u8; (cw * cols * ch * 4) as usize];
    for (i, &r) in ratios.iter().enumerate() {
        let interp = build_morph_frame(ms, r);
        let xform = Affine::scale(twip_scale)
            .then_translate(Vec2::new(-px_min_x, -px_min_y));
        let mut scene = Scene::new();
        let pseudo_shape_sym = Symbol::Shape(interp);
        render_symbol(&doc, &pseudo_shape_sym, &mut scene, xform, 0);
        let pixels = headless.render_to_pixels(&scene, cw, ch, Color::BLACK)?;
        for y in 0..ch {
            let dst = (y * cw * cols + (i as u32) * cw) * 4;
            let src = y * cw * 4;
            sheet[dst as usize..(dst + cw * 4) as usize]
                .copy_from_slice(&pixels[src as usize..(src + cw * 4) as usize]);
        }
        eprintln!("rendered ratio {}", r);
    }
    image::save_buffer(&out, &sheet, cw * cols, ch, image::ColorType::Rgba8)?;
    eprintln!("wrote {}", out.display());
    Ok(())
}
