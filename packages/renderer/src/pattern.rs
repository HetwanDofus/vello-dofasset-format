use vello::peniko::{Brush, Extend, Image};
use vello::kurbo::Affine;

/// Create a repeating image brush for pattern fills.
pub fn create_pattern_brush(image: &Image, pattern_transform: Affine) -> (Brush, Affine) {
    let mut img = image.clone();
    img.x_extend = Extend::Repeat;
    img.y_extend = Extend::Repeat;
    (Brush::Image(img), pattern_transform)
}
