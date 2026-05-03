//! Render a single quadratic Bezier with morph 15's interpolated values
//! to see if Vello produces the expected curve.

use std::path::PathBuf;
use anyhow::Result;
use vello::kurbo::{Affine, BezPath, Point, Stroke};
use vello::peniko::{Color, Brush};
use vello::Scene;
use swf_spike::headless::Headless;

fn main() -> Result<()> {
    pollster::block_on(run())
}

async fn run() -> Result<()> {
    let out = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| "/tmp/test_quad.png".into()));
    let mut headless = Headless::new().await?;
    let mut scene = Scene::new();

    // Morph 15 at ratio 31208: pen=(-745,-685), ctrl=(565,-1069), end=(697,757)
    let mut path = BezPath::new();
    path.move_to(Point::new(-745.0, -685.0));
    path.quad_to(Point::new(565.0, -1069.0), Point::new(697.0, 757.0));

    let stroke = Stroke::new(140.0)
        .with_caps(vello::kurbo::Cap::Round)
        .with_join(vello::kurbo::Join::Round);
    let xform = Affine::scale(0.1).then_translate(vello::kurbo::Vec2::new(128.0, 128.0));
    scene.stroke(&stroke, xform, &Brush::Solid(Color::from_rgba8(0, 51, 0, 255)), None, &path);

    let pixels = headless.render_to_pixels(&scene, 256, 256, Color::WHITE)?;
    image::save_buffer(&out, &pixels, 256, 256, image::ColorType::Rgba8)?;
    eprintln!("wrote {}", out.display());
    Ok(())
}
