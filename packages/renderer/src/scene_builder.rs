use std::collections::HashMap;
use vello::kurbo::{Affine, Stroke};
use vello::peniko::{self, Color, Fill, Gradient};
use vello::Scene;

use crate::format::{DofAsset, DrawCommand, FillRule, StrokeWidthMode};
use crate::pattern::create_pattern_brush;

/// A pre-parsed accessory SVG scene ready for compositing.
pub struct AccessoryScene {
    pub slot_id: u8,
    pub scene: Scene,
    /// The accessory SVG's origin offset (from its atlas.json)
    pub offset_x: f64,
    pub offset_y: f64,
}

/// Build a Vello Scene for a specific animation frame.
/// `accessories` contains pre-rendered accessory scenes keyed by slot_id.
pub fn build_frame_scene(
    asset: &DofAsset,
    animation_name: &str,
    frame_index: usize,
    player_colors: Option<&[u32; 3]>,
    resolution: f32,
    accessories: &[AccessoryScene],
) -> Scene {
    let mut scene = Scene::new();

    let anim_idx = match asset.animation_map.get(animation_name) {
        Some(&idx) => idx,
        None => {
            eprintln!("Animation not found: {animation_name}");
            return scene;
        }
    };

    let anim = &asset.animations[anim_idx];
    let actual_frame_idx = frame_index % anim.frame_ids.len();
    let global_frame_id = anim.frame_ids[actual_frame_idx] as usize;

    if global_frame_id >= asset.frames.len() {
        eprintln!("Frame index out of range: {global_frame_id}");
        return scene;
    }

    let frame = &asset.frames[global_frame_id];

    // Build color replacement map if player colors provided
    let color_replacements = player_colors.map(|colors| {
        crate::color::build_color_replacements(&asset.color_zones, colors)
    });

    // Separate clip translate from scale to match svg-renderer's transform composition.
    // The svg-renderer applies scale at the scene level via scene.append(..., Affine::scale(zoom)),
    // so we do the same to avoid float precision differences from baking scale into each transform.
    let scale = resolution as f64;
    let clip_offset = Affine::translate((
        -(frame.clip_rect[0] as f64),
        -(frame.clip_rect[1] as f64),
    ));

    // Get the frame's SVG offset transform (for accessory positioning)
    let frame_offset = asset.transforms.get(frame.frame_transform_id as usize)
        .copied()
        .unwrap_or(Affine::IDENTITY);

    // Build a map of accessory data by slot_id
    let acc_by_slot: HashMap<u8, &AccessoryScene> = accessories.iter()
        .map(|a| (a.slot_id, a))
        .collect();

    // Render everything into a sub-scene at native SVG coordinates
    let mut sub = Scene::new();

    // Render base frame below if present (baseZOrder == 0 means "below")
    let has_base_below = anim.base_frame_id != u32::MAX && anim.base_z_order == 0;
    let has_base_above = anim.base_frame_id != u32::MAX && anim.base_z_order == 1;

    if has_base_below {
        if let Some(base_frame) = asset.frames.get(anim.base_frame_id as usize) {
            let base_clip_offset = Affine::translate((
                -(base_frame.clip_rect[0] as f64),
                -(base_frame.clip_rect[1] as f64),
            ));
            render_frame_parts(&mut sub, asset, base_frame, base_clip_offset, &color_replacements, resolution);
        }
    }

    // Render body parts interleaved with accessories at correct depth
    render_frame_parts_with_accessories(
        &mut sub, asset, frame, clip_offset, frame_offset,
        &color_replacements, resolution, &acc_by_slot,
    );

    // Render base frame above if present
    if has_base_above {
        if let Some(base_frame) = asset.frames.get(anim.base_frame_id as usize) {
            let base_clip_offset = Affine::translate((
                -(base_frame.clip_rect[0] as f64),
                -(base_frame.clip_rect[1] as f64),
            ));
            render_frame_parts(&mut sub, asset, base_frame, base_clip_offset, &color_replacements, resolution);
        }
    }

    // Apply scale at scene level (matches svg-renderer approach)
    scene.append(&sub, Some(Affine::scale(scale)));

    scene
}

/// Render just the body parts of a frame (no accessories). Used for base frames.
fn render_frame_parts(
    scene: &mut Scene,
    asset: &DofAsset,
    frame: &crate::format::Frame,
    clip_offset: Affine,
    color_replacements: &Option<HashMap<u32, Color>>,
    resolution: f32,
) {
    for part_inst in &frame.parts {
        let body_part = match asset.body_parts.get(part_inst.body_part_id as usize) {
            Some(bp) => bp,
            None => continue,
        };
        let part_transform = match asset.transforms.get(part_inst.transform_id as usize) {
            Some(&t) => clip_offset * t,
            None => clip_offset,
        };
        for &cmd_id in &body_part.draw_command_ids {
            if let Some(cmd) = asset.draw_commands.get(cmd_id as usize) {
                render_draw_command(scene, asset, cmd, part_transform, color_replacements, resolution);
            }
        }
    }
}

/// Render body parts interleaved with accessories at correct depth.
fn render_frame_parts_with_accessories(
    scene: &mut Scene,
    asset: &DofAsset,
    frame: &crate::format::Frame,
    clip_offset: Affine,
    frame_offset: Affine,
    color_replacements: &Option<HashMap<u32, Color>>,
    resolution: f32,
    acc_by_slot: &HashMap<u8, &AccessoryScene>,
) {
    for (part_idx, part_inst) in frame.parts.iter().enumerate() {
        let body_part = match asset.body_parts.get(part_inst.body_part_id as usize) {
            Some(bp) => bp,
            None => continue,
        };
        let part_transform = match asset.transforms.get(part_inst.transform_id as usize) {
            Some(&t) => clip_offset * t,
            None => clip_offset,
        };
        for &cmd_id in &body_part.draw_command_ids {
            if let Some(cmd) = asset.draw_commands.get(cmd_id as usize) {
                render_draw_command(scene, asset, cmd, part_transform, color_replacements, resolution);
            }
        }
        let after_idx = part_idx + 1;
        for acc_slot in &frame.accessory_slots {
            if acc_slot.depth_index as usize == after_idx {
                render_accessory(scene, asset, acc_slot, acc_by_slot, clip_offset, frame_offset);
            }
        }
    }
    let part_count = frame.parts.len();
    for acc_slot in &frame.accessory_slots {
        if acc_slot.depth_index as usize >= part_count {
            render_accessory(scene, asset, acc_slot, acc_by_slot, clip_offset, frame_offset);
        }
    }
}

fn resolve_color(
    color: Color,
    zone_id: u8,
    replacements: &Option<HashMap<u32, Color>>,
) -> Color {
    if zone_id == 0 {
        return color;
    }
    if let Some(map) = replacements {
        let rgba = color.to_rgba8();
        let key = (rgba.r as u32) << 16 | (rgba.g as u32) << 8 | rgba.b as u32;
        if let Some(&replaced) = map.get(&key) {
            return replaced;
        }
    }
    color
}

fn render_draw_command(
    scene: &mut Scene,
    asset: &DofAsset,
    cmd: &DrawCommand,
    part_transform: Affine,
    color_replacements: &Option<HashMap<u32, Color>>,
    resolution: f32,
) {
    match cmd {
        DrawCommand::Fill { path_id, fill_rule, color, zone_id, transform } => {
            if let Some(path) = asset.paths.get(*path_id as usize) {
                let final_transform = part_transform * *transform;
                let resolved_color = resolve_color(*color, *zone_id, color_replacements);
                let fill = match fill_rule {
                    FillRule::NonZero => Fill::NonZero,
                    FillRule::EvenOdd => Fill::EvenOdd,
                };
                scene.fill(fill, final_transform, resolved_color, None, path);
            }
        }
        DrawCommand::Stroke { path_id, color, zone_id, width, opacity, line_cap, line_join, transform, .. } => {
            if let Some(path) = asset.paths.get(*path_id as usize) {
                let final_transform = part_transform * *transform;
                let resolved_color = resolve_color(*color, *zone_id, color_replacements);
                // Opacity is already baked into the color alpha by the compiler.
                let final_color = resolved_color;

                // All strokes use non-scaling-stroke semantics: divide by resolution.
                // Use f32 division to match usvg's f32 stroke-width precision.
                let stroke_width = (*width / resolution) as f64;

                let stroke = Stroke::new(stroke_width)
                    .with_caps(*line_cap)
                    .with_join(*line_join);

                scene.stroke(&stroke, final_transform, final_color, None, path);
            }
        }
        DrawCommand::PatternFill { path_id, image_id, pattern_transform, transform, fill_rule, .. } => {
            if let Some(path) = asset.paths.get(*path_id as usize) {
                if let Some(image) = asset.images.get(*image_id as usize) {
                    let final_transform = part_transform * *transform;
                    let (brush, brush_transform) = create_pattern_brush(image, *pattern_transform);
                    let fill = match fill_rule {
                        FillRule::NonZero => Fill::NonZero,
                        FillRule::EvenOdd => Fill::EvenOdd,
                    };
                    scene.fill(fill, final_transform, &brush, Some(brush_transform), path);
                }
            }
        }
        DrawCommand::GradientFill { path_id, fill_rule, gradient_type, cx, cy, fx, fy, r, gradient_transform, stops, transform } => {
            if let Some(path) = asset.paths.get(*path_id as usize) {
                let final_transform = part_transform * *transform;
                let fill = match fill_rule {
                    FillRule::NonZero => Fill::NonZero,
                    FillRule::EvenOdd => Fill::EvenOdd,
                };

                let grad_stops: Vec<peniko::ColorStop> = stops.clone();

                let brush = if *gradient_type == 0 {
                    // Two-point radial: center (cx,cy) with start radius 0,
                    // focal point (fx,fy) with end radius r — matches usvg's representation
                    Gradient::new_two_point_radial(
                        vello::kurbo::Point::new(*cx as f64, *cy as f64), 0_f32,
                        vello::kurbo::Point::new(*fx as f64, *fy as f64), *r,
                    ).with_stops(grad_stops.as_slice())
                } else {
                    Gradient::new_linear(
                        (*cx as f64, *cy as f64),
                        (*r as f64, 0.0),
                    ).with_stops(grad_stops.as_slice())
                };

                // gradientTransform maps gradient coordinates to path-local space.
                // Vello's brush_transform maps brush coords to the same space as the path.
                // Since we apply final_transform to the path, the brush should also go
                // through gradient_transform to land in path-local space.
                scene.fill(fill, final_transform, &brush, Some(*gradient_transform), path);
            }
        }
    }
}

fn render_accessory(
    scene: &mut Scene,
    asset: &DofAsset,
    acc_slot: &crate::format::AccessorySlot,
    acc_by_slot: &HashMap<u8, &AccessoryScene>,
    clip_offset: Affine,
    frame_offset: Affine,
) {
    let acc = match acc_by_slot.get(&acc_slot.slot_id) {
        Some(a) => *a,
        None => return,
    };

    // raw_matrix = [a, b, c, d, mtx, mty] from data-matrix
    let raw = match asset.transforms.get(acc_slot.transform_id as usize) {
        Some(&t) => t,
        None => return,
    };

    // Replicate the vite composeAccessory logic exactly:
    // Extract components
    let coeffs = raw.as_coeffs();
    let a = coeffs[0];
    let b = coeffs[1];
    let c = coeffs[2];
    let d = coeffs[3];
    let mtx = coeffs[4];
    let mty = coeffs[5];
    let offset_x = acc.offset_x;
    let offset_y = acc.offset_y;

    // Position = mtx + offsetX, mty + offsetY
    let pos_x = mtx + offset_x;
    let pos_y = mty + offset_y;

    // Check if there's rotation/scale (non-identity a,b,c,d)
    let has_scale = (a - 1.0).abs() > 1e-6 || b.abs() > 1e-6
        || c.abs() > 1e-6 || (d - 1.0).abs() > 1e-6;

    // Build the accessory local transform matching vite's compose:
    // <svg x="posX" y="posY">
    //   <g translate(pivotX, pivotY)>
    //     <g matrix(a,b,c,d,0,0)>
    //       <g translate(-pivotX, -pivotY)>
    //         [content]
    //       </g></g></g></svg>
    // pivot = (-offsetX, -offsetY)
    let acc_local = if has_scale {
        let pivot_x = -offset_x;
        let pivot_y = -offset_y;
        let scale_rotate = Affine::new([a, b, c, d, 0.0, 0.0]);
        Affine::translate((pos_x, pos_y))
            * Affine::translate((pivot_x, pivot_y))
            * scale_rotate
            * Affine::translate((-pivot_x, -pivot_y))
    } else {
        Affine::translate((pos_x, pos_y))
    };

    // Final: clip_offset * frame_offset * acc_local * [accessory content]
    let final_transform = clip_offset * frame_offset * acc_local;
    scene.append(&acc.scene, Some(final_transform));
}

/// Build scenes for all frames of an animation (useful for testing).
pub fn build_all_frames(
    asset: &DofAsset,
    animation_name: &str,
    player_colors: Option<&[u32; 3]>,
    resolution: f32,
    accessories: &[AccessoryScene],
) -> Vec<Scene> {
    let anim_idx = match asset.animation_map.get(animation_name) {
        Some(&idx) => idx,
        None => return Vec::new(),
    };

    let anim = &asset.animations[anim_idx];
    let frame_count = anim.frame_ids.len();

    (0..frame_count)
        .map(|i| build_frame_scene(asset, animation_name, i, player_colors, resolution, accessories))
        .collect()
}

/// Load an accessory SVG file and parse it into a vello Scene.
pub fn load_accessory_svg(svg_content: &str) -> Scene {
    vello_svg::render(svg_content).expect("Failed to parse accessory SVG")
}
