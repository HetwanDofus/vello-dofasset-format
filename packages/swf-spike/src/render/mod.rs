//! Walk a SwfDoc, recurse through DefineSprites, and emit Vello fills/strokes.
//!
//! Twip → pixel scaling is intentionally NOT done here — we paint in twip
//! coordinates and let the caller's `parent_transform` do the scale. That
//! matches how the existing `dofasset_renderer` handles its scenes.

pub mod cache;
pub mod ctx;
pub mod snapshot;
pub use cache::RenderCache;
pub use ctx::RenderCtx;
pub use snapshot::{render_snapshot, PlacedSnapshot, ResolvedPlace};

use anyhow::Result;
use std::collections::BTreeMap;
use std::sync::Arc;

use vello::kurbo::{Affine, BezPath, PathEl, Point, Rect, Stroke, Vec2};
use vello::peniko::{
    color::AlphaColor, BlendMode, Blob, Brush, Color, Fill, ImageAlphaType, ImageBrush, ImageData,
    ImageFormat,
};
use vello::wgpu;
use vello::{AaConfig, Renderer, Scene};

use crate::shape::{flatten_shape, DrawCmd, DrawKind};
use crate::swf_doc::{
    OwnedColorTransform, OwnedFilter, OwnedOp, OwnedPlace, OwnedSprite, SwfDoc, Symbol,
};
use crate::wgpu_filters::FilterPipelines;

/// Per-frame wgpu context shared with the renderer. Required by the
/// filtered-placement path which renders sub-scenes to intermediate textures
/// and applies compute-shader filter passes.
pub struct WgpuCtx<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub renderer: &'a mut Renderer,
    pub filter_pipelines: &'a FilterPipelines,
    /// Output pixels per shape twip (= renderScale / 20.0). Used to size
    /// intermediate filter textures consistent with the main render.
    pub output_scale: f64,
}

/// Multiple SwfDocs probed in order; first hit wins.
pub struct DocPool<'a> {
    pub docs: Vec<&'a SwfDoc>,
}

impl<'a> DocPool<'a> {
    pub fn new(docs: Vec<&'a SwfDoc>) -> Self {
        Self { docs }
    }

    pub fn lookup_export(&self, name: &str) -> Option<(&'a SwfDoc, &'a Symbol)> {
        for d in &self.docs {
            if let Some(s) = d.lookup_export(name) {
                return Some((*d, s));
            }
        }
        None
    }

    /// Lookup helper that takes the raw character_id within the doc that
    /// originally owned the sprite. Cross-doc references aren't a thing in
    /// these SWFs.
    pub fn lookup_id_in(doc: &'a SwfDoc, id: u16) -> Option<&'a Symbol> {
        doc.lookup_id(id)
    }
}

pub fn render_export(
    pool: &DocPool<'_>,
    name: &str,
    scene: &mut Scene,
    transform: Affine,
    frame: u16,
) -> Result<()> {
    let (doc, sym) = pool
        .lookup_export(name)
        .ok_or_else(|| anyhow::anyhow!("export `{}` not found in any loaded SWF", name))?;
    render_symbol(doc, sym, scene, transform, frame);
    Ok(())
}

// ---------------------------------------------------------------
// Public entry points. All of these now build a `RenderCtx` and
// dispatch to the single internal workhorse `render_symbol_into`.
// Their parameter lists are kept byte-stable so vello-wasm and the
// debug bins compile unchanged.
// ---------------------------------------------------------------

pub fn render_symbol(
    doc: &SwfDoc,
    sym: &Symbol,
    scene: &mut Scene,
    transform: Affine,
    frame: u16,
) {
    let cache = RenderCache::new();
    let mut ctx = RenderCtx::new(doc, scene, &cache)
        .with_transform(transform)
        .with_frame(frame);
    render_symbol_into(&mut ctx, sym);
}

pub fn render_symbol_with_ctx(
    wgpu: &mut WgpuCtx,
    doc: &SwfDoc,
    sym: &Symbol,
    scene: &mut Scene,
    transform: Affine,
    frame: u16,
) {
    let inner = reborrow_wgpu(wgpu);
    let cache = RenderCache::new();
    let mut ctx = RenderCtx::new(doc, scene, &cache)
        .with_wgpu(inner)
        .with_transform(transform)
        .with_frame(frame);
    render_symbol_into(&mut ctx, sym);
}

/// Same as `render_symbol_with_ctx` but applies HSL-preserve-lightness
/// recolor to every fill inside a sprite tagged with an applyColor
/// zone (see `SwfDoc.sprite_color_zones`). Used by the player-tinting
/// path. Tile/spell renders should keep using `render_symbol_with_ctx`
/// since they have no zones.
pub fn render_symbol_with_ctx_tinted(
    wgpu: &mut WgpuCtx,
    doc: &SwfDoc,
    sym: &Symbol,
    scene: &mut Scene,
    transform: Affine,
    frame: u16,
    player_colors: crate::recolor::PlayerColors,
) {
    let inner = reborrow_wgpu(wgpu);
    let cache = RenderCache::new();
    let mut ctx = RenderCtx::new(doc, scene, &cache)
        .with_wgpu(inner)
        .with_transform(transform)
        .with_frame(frame)
        .with_player_colors(player_colors);
    render_symbol_into(&mut ctx, sym);
}

pub fn render_symbol_xformed(
    doc: &SwfDoc,
    sym: &Symbol,
    scene: &mut Scene,
    transform: Affine,
    frame: u16,
    color_xform: OwnedColorTransform,
) {
    let cache = RenderCache::new();
    let mut ctx = RenderCtx::new(doc, scene, &cache)
        .with_transform(transform)
        .with_frame(frame)
        .with_color_xform(color_xform);
    render_symbol_into(&mut ctx, sym);
}

pub fn render_shape(
    scene: &mut Scene,
    doc: &SwfDoc,
    shape: &swf::Shape,
    transform: Affine,
    color_xform: OwnedColorTransform,
) {
    let cache = RenderCache::new();
    let mut ctx = RenderCtx::new(doc, scene, &cache)
        .with_transform(transform)
        .with_color_xform(color_xform);
    render_shape_into(&mut ctx, shape);
}

/// Reborrow a mutable WgpuCtx reference into a fresh owned WgpuCtx.
/// Used by the public entries so they can hand a value-typed
/// WgpuCtx to RenderCtx::with_wgpu.
fn reborrow_wgpu<'a>(w: &'a mut WgpuCtx<'_>) -> WgpuCtx<'a> {
    WgpuCtx {
        device: w.device,
        queue: w.queue,
        renderer: &mut *w.renderer,
        filter_pipelines: w.filter_pipelines,
        output_scale: w.output_scale,
    }
}

// ---------------------------------------------------------------
// Internal workhorses — all take `&mut RenderCtx<'_>`.
// ---------------------------------------------------------------

/// Dispatch a Symbol to the right rendering function. Reads
/// `ctx.ratio` for MorphShape interpolation and `ctx.frame` for
/// sprite timeline position. Morph interpolation hits the cache
/// (bucketed by ratio high byte).
fn render_symbol_into(ctx: &mut RenderCtx<'_>, sym: &Symbol) {
    match sym {
        Symbol::Shape(shape) => render_shape_into(ctx, shape),
        Symbol::Sprite(sprite) => render_sprite_into(ctx, sprite),
        Symbol::Bitmap { .. } => {}
        Symbol::MorphShape(ms) => {
            let interp = ctx.cache.morph_frame(ms, ctx.ratio);
            render_shape_into(ctx, &interp);
        }
    }
}

/// Flatten a shape into draw commands (cached), optionally
/// HSL-recolor each brush to the active zone target, then emit
/// through `emit_cmd`. Cache hit avoids re-walking shape records;
/// the no-recolor path emits the cached `DrawCmd` directly with no
/// per-fill clone.
fn render_shape_into(ctx: &mut RenderCtx<'_>, shape: &swf::Shape) {
    let recolor_target = ctx.recolor_target;
    let transform = ctx.transform;
    let color_xform = ctx.color_xform;
    let cmds = ctx.cache.shape_cmds(shape, ctx.doc);
    for cmd in cmds.iter() {
        if let Some(target) = recolor_target {
            // Recolor needs a mutable copy. BezPath + Brush are
            // internally Arc-shared so the clone is cheap.
            let mut cmd = cmd.clone();
            recolor_cmd_in_place(&mut cmd, target);
            emit_cmd(ctx.scene, &cmd, transform, color_xform);
        } else {
            emit_cmd(ctx.scene, cmd, transform, color_xform);
        }
    }
}

fn recolor_cmd_in_place(cmd: &mut DrawCmd, target: u32) {
    use crate::recolor::recolor_to_zone;
    let recolor_brush = |b: &mut Brush| match b {
        Brush::Solid(c) => *c = recolor_to_zone(*c, target),
        Brush::Gradient(g) => {
            for stop in g.stops.iter_mut() {
                let alpha = stop.color.to_alpha_color::<vello::peniko::color::Srgb>();
                let recolored = recolor_to_zone(alpha, target);
                stop.color = recolored.into();
            }
        }
        Brush::Image(_) => {} // bitmap fills aren't zone-tinted
    };
    match &mut cmd.kind {
        DrawKind::Fill { brush, .. } => recolor_brush(brush),
        DrawKind::Stroke { brush, .. } => recolor_brush(brush),
    }
}

fn emit_cmd(scene: &mut Scene, cmd: &DrawCmd, transform: Affine, cx: OwnedColorTransform) {
    match &cmd.kind {
        DrawKind::Fill {
            brush,
            brush_transform,
            rule,
        } => {
            let tinted = if cx.is_identity() {
                brush.clone()
            } else {
                tint_brush(brush, cx)
            };
            scene.fill(*rule, transform, &tinted, *brush_transform, &cmd.path);
        }
        DrawKind::Stroke {
            brush,
            width,
            cap,
            join,
            miter_limit,
            non_scaling,
        } => {
            let tinted = if cx.is_identity() {
                brush.clone()
            } else {
                tint_brush(brush, cx)
            };
            // dofasset NonScaling port (scene_builder.rs:357-366): for SWF
            // widths < 1 px, take `max(natural_width, one_device_px)` in
            // path-local units. After Vello multiplies by world_scale this
            // yields `max(value_px × resolution, 1)` device pixels — exactly
            // the floor that gives us 1 device pixel for hairlines instead of
            // `1 logical pixel × resolution`. Normal widths pass through and
            // scale naturally with the world transform (Fixed mode).
            let final_width = if *non_scaling {
                let s = transform.determinant().abs().sqrt().max(f64::EPSILON);
                let one_device_px = 1.0 / s;
                (*width).max(one_device_px)
            } else {
                *width
            };
            let stroke = Stroke::new(final_width)
                .with_caps(*cap)
                .with_join(*join)
                .with_miter_limit(*miter_limit);
            scene.stroke(&stroke, transform, &tinted, None, &cmd.path);
        }
    }
}

/// Apply a SWF ColorTransform to a Vello brush. Solid + gradient brushes are
/// straightforward channel multiplications; bitmap brushes get a fresh tinted
/// pixel buffer so per-channel scale + add lands in the texture itself.
fn tint_brush(brush: &Brush, cx: OwnedColorTransform) -> Brush {
    match brush {
        Brush::Solid(c) => Brush::Solid(tint_color(*c, cx)),
        Brush::Gradient(g) => {
            let mut tinted = g.clone();
            for stop in tinted.stops.iter_mut() {
                stop.color = tint_dyn_color(&stop.color, cx).into();
            }
            Brush::Gradient(tinted)
        }
        Brush::Image(ib) => {
            let src = &ib.image;
            let mut bytes = src.data.as_ref().to_vec();
            tint_rgba_in_place(&mut bytes, cx);
            let new_data = ImageData {
                data: Blob::new(Arc::new(bytes)),
                format: src.format,
                alpha_type: src.alpha_type,
                width: src.width,
                height: src.height,
            };
            let mut tinted = vello::peniko::ImageBrush::from(new_data);
            tinted.sampler = ib.sampler;
            Brush::Image(tinted)
        }
    }
}

fn tint_color(c: Color, cx: OwnedColorTransform) -> Color {
    let arr = c.to_rgba8().to_u8_array();
    let r = mult_add_u8(arr[0], cx.mult_r, cx.add_r);
    let g = mult_add_u8(arr[1], cx.mult_g, cx.add_g);
    let b = mult_add_u8(arr[2], cx.mult_b, cx.add_b);
    let a = mult_add_u8(arr[3], cx.mult_a, cx.add_a);
    AlphaColor::from_rgba8(r, g, b, a)
}

fn tint_dyn_color(c: &vello::peniko::color::DynamicColor, cx: OwnedColorTransform) -> Color {
    // peniko stores stop colors as DynamicColor; round-trip via rgba8 keeps it
    // simple at the cost of a clamp.
    let alpha = c.to_alpha_color::<vello::peniko::color::Srgb>();
    tint_color(alpha, cx)
}

fn tint_rgba_in_place(rgba: &mut [u8], cx: OwnedColorTransform) {
    for chunk in rgba.chunks_exact_mut(4) {
        chunk[0] = mult_add_u8(chunk[0], cx.mult_r, cx.add_r);
        chunk[1] = mult_add_u8(chunk[1], cx.mult_g, cx.add_g);
        chunk[2] = mult_add_u8(chunk[2], cx.mult_b, cx.add_b);
        chunk[3] = mult_add_u8(chunk[3], cx.mult_a, cx.add_a);
    }
}

fn mult_add_u8(c: u8, mult: f32, add: f32) -> u8 {
    let v = (f32::from(c) / 255.0) * mult + add;
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}


/// Resolve a single timeline frame: walk ops, applying Place/Remove,
/// producing a snapshot of `depth → placement state` at that frame.
pub fn build_frame_state(sprite: &OwnedSprite, frame: u16) -> BTreeMap<u16, OwnedPlace> {
    let mut state: BTreeMap<u16, OwnedPlace> = BTreeMap::new();
    let mut cur_frame: u16 = 0;
    let target = frame.min(sprite.num_frames.saturating_sub(1));

    for op in &sprite.ops {
        if cur_frame > target {
            break;
        }
        match op {
            OwnedOp::Place(p) => {
                if p.is_move {
                    if let Some(prev) = state.get_mut(&p.depth) {
                        if p.character_id.is_some() {
                            prev.character_id = p.character_id;
                        }
                        if p.matrix.is_some() {
                            prev.matrix = p.matrix;
                        }
                        if p.color_transform.is_some() {
                            prev.color_transform = p.color_transform;
                        }
                        if p.ratio.is_some() {
                            prev.ratio = p.ratio;
                        }
                        if p.clip_depth.is_some() {
                            prev.clip_depth = p.clip_depth;
                        }
                        if p.name.is_some() {
                            prev.name = p.name.clone();
                        }
                    } else {
                        // Modify on empty depth — treat as fresh place.
                        state.insert(p.depth, p.clone());
                    }
                } else {
                    state.insert(p.depth, p.clone());
                }
            }
            OwnedOp::Remove { depth } => {
                state.remove(depth);
            }
            OwnedOp::ShowFrame => {
                cur_frame += 1;
            }
            // Stateless renderer ignores frame-level AVM1 — the AS-aware path
            // (`render_avm1`) is the one that consumes these. Skipping here
            // keeps the cheap "no AS" preview rendering working unchanged.
            OwnedOp::DoAction(_) => {}
        }
    }
    state
}

/// Twip-space bounds for the symbol at the requested frame. Recurses into
/// nested DefineSprites, applying placement matrices. Used by the WASM bridge
/// to size the output texture.
pub fn symbol_bounds(doc: &SwfDoc, sym: &Symbol, frame: u16) -> Rect {
    match sym {
        Symbol::Shape(shape) => Rect::new(
            f64::from(shape.shape_bounds.x_min.get()),
            f64::from(shape.shape_bounds.y_min.get()),
            f64::from(shape.shape_bounds.x_max.get()),
            f64::from(shape.shape_bounds.y_max.get()),
        ),
        Symbol::Sprite(sprite) => sprite_bounds(doc, sprite, frame, Affine::IDENTITY),
        Symbol::Bitmap(enc) => bitmap_dims(enc)
            .map(|(w, h)| Rect::new(0.0, 0.0, f64::from(w) * 20.0, f64::from(h) * 20.0))
            .unwrap_or(Rect::ZERO),
        Symbol::MorphShape(ms) => {
            // Use the union of start+end bounds — covers the worst-case
            // extent across all morph ratios so layout doesn't recompute.
            let r = crate::morph::morph_bounds_union(ms);
            Rect::new(
                f64::from(r.x_min.get()),
                f64::from(r.y_min.get()),
                f64::from(r.x_max.get()),
                f64::from(r.y_max.get()),
            )
        }
    }
}

/// Cheap dimension lookup for an encoded bitmap. Lossless tags carry w/h
/// explicitly; JPEGs would require decoding the header, so for the spike we
/// return None and let the caller treat the bitmap bounds as zero — bitmaps
/// are virtually never placed directly as sprite children, only via fill
/// styles that compute their own geometry from the parent shape's path.
fn bitmap_dims(enc: &crate::swf_doc::EncodedBitmap) -> Option<(u32, u32)> {
    use crate::swf_doc::EncodedBitmap;
    match enc {
        EncodedBitmap::Lossless { width, height, .. } => {
            Some((u32::from(*width), u32::from(*height)))
        }
        _ => None,
    }
}

fn sprite_bounds(doc: &SwfDoc, sprite: &OwnedSprite, frame: u16, parent: Affine) -> Rect {
    let state = build_frame_state(sprite, frame);
    let mut acc: Option<Rect> = None;
    for (_depth, p) in &state {
        let id = match p.character_id {
            Some(id) => id,
            None => continue,
        };
        let child = match doc.lookup_id(id) {
            Some(c) => c,
            None => continue,
        };
        let xform = parent * p.matrix.unwrap_or(Affine::IDENTITY);
        let local = match child {
            Symbol::Shape(s) => Rect::new(
                f64::from(s.shape_bounds.x_min.get()),
                f64::from(s.shape_bounds.y_min.get()),
                f64::from(s.shape_bounds.x_max.get()),
                f64::from(s.shape_bounds.y_max.get()),
            ),
            Symbol::Sprite(child_sprite) => {
                let child_frame = if child_sprite.num_frames > 1 { frame } else { 0 };
                sprite_bounds(doc, child_sprite, child_frame, Affine::IDENTITY)
            }
            Symbol::MorphShape(ms) => {
                let r = crate::morph::morph_bounds_union(ms);
                Rect::new(
                    f64::from(r.x_min.get()),
                    f64::from(r.y_min.get()),
                    f64::from(r.x_max.get()),
                    f64::from(r.y_max.get()),
                )
            }
            Symbol::Bitmap(enc) => bitmap_dims(enc)
                .map(|(w, h)| Rect::new(0.0, 0.0, f64::from(w) * 20.0, f64::from(h) * 20.0))
                .unwrap_or(Rect::ZERO),
        };
        let world = transform_rect(xform, local);
        acc = Some(match acc {
            Some(a) => a.union(world),
            None => world,
        });
    }
    acc.unwrap_or(Rect::ZERO)
}

fn transform_rect(m: Affine, r: Rect) -> Rect {
    let pts = [
        m * Point::new(r.x0, r.y0),
        m * Point::new(r.x1, r.y0),
        m * Point::new(r.x0, r.y1),
        m * Point::new(r.x1, r.y1),
    ];
    let mut min_x = pts[0].x;
    let mut min_y = pts[0].y;
    let mut max_x = pts[0].x;
    let mut max_y = pts[0].y;
    for p in &pts[1..] {
        if p.x < min_x {
            min_x = p.x;
        }
        if p.x > max_x {
            max_x = p.x;
        }
        if p.y < min_y {
            min_y = p.y;
        }
        if p.y > max_y {
            max_y = p.y;
        }
    }
    Rect::new(min_x, min_y, max_x, max_y)
}

/// Walk a sprite's timeline at `ctx.frame`, emit clip-mask layers,
/// recurse into placements via `ctx.child(place, id)`. The single
/// abstraction `child()` composes transform + color_xform, propagates
/// frame, picks ratio, and resolves the child's applyColor zone — so
/// the body of this fn is purely the timeline structure (clip stack,
/// filter handoff, ordinary recursion).
fn render_sprite_into(ctx: &mut RenderCtx<'_>, sprite: &OwnedSprite) {
    let state = build_frame_state(sprite, ctx.frame);

    // SWF clip-mask stack: each Place with clip_depth=N defines a
    // mask that applies to placements at depths in (this_place_depth,
    // N]. We push a Vello layer with the mask geometry and pop when
    // the current depth exceeds the mask's clip_depth.
    let mut clip_stack: Vec<u16> = Vec::new();

    for (depth, p) in &state {
        // Pop any clips whose range has ended.
        while clip_stack
            .last()
            .map(|&end| *depth > end)
            .unwrap_or(false)
        {
            clip_stack.pop();
            ctx.scene.pop_layer();
        }

        let id = match p.character_id {
            Some(id) => id,
            None => continue,
        };
        let child_sym = match ctx.doc.lookup_id(id) {
            Some(s) => s,
            None => continue,
        };

        // Clip-mask: don't draw the mask itself; push a Vello clip
        // layer using its geometry.
        if let Some(clip_depth) = p.clip_depth {
            if clip_depth > 0 && clip_depth > *depth {
                let local_xform = p.matrix.unwrap_or(Affine::IDENTITY);
                let mask_frame = match child_sym {
                    Symbol::Sprite(c) if c.num_frames > 1 => ctx.frame,
                    _ => 0,
                };
                let mut mask_path = BezPath::new();
                collect_mask_path(
                    ctx.doc,
                    child_sym,
                    local_xform,
                    mask_frame,
                    p.ratio.unwrap_or(0),
                    &mut mask_path,
                );
                if mask_path.elements().len() >= 4 {
                    ctx.scene.push_layer(
                        Fill::NonZero,
                        BlendMode::default(),
                        1.0,
                        ctx.transform,
                        &mask_path,
                    );
                    clip_stack.push(clip_depth);
                }
                continue;
            }
        }

        // Filtered placement: hand off to the legacy filter helpers
        // (still take loose params; ctxified in Phase 4 alongside
        // render_avm1). Pre-compute the values they want from ctx.
        if !p.filters.is_empty() {
            let child_xform = ctx.transform * p.matrix.unwrap_or(Affine::IDENTITY);
            let placement_cx = p
                .color_transform
                .unwrap_or(OwnedColorTransform::IDENTITY);
            let child_cx = ctx.color_xform.compose(placement_cx);
            let child_frame = match child_sym {
                Symbol::Sprite(c) if c.num_frames > 1 => ctx.frame,
                _ => 0,
            };

            // Real filtered render via wgpu compute, if available.
            // `ctx.wgpu` and `ctx.scene` are disjoint fields of
            // `*ctx`; the borrow checker accepts simultaneous
            // mutable access.
            let mut handled = false;
            if let Some(wgpu) = ctx.wgpu.as_mut() {
                handled = render_filtered(
                    wgpu,
                    ctx.doc,
                    child_sym,
                    ctx.scene,
                    child_xform,
                    child_frame,
                    child_cx,
                    &p.filters,
                );
            }
            if handled {
                continue;
            }

            // Fallback halo + body render (no wgpu, or filter set
            // wasn't supported by the GPU path).
            apply_filters_pre(
                &p.filters,
                ctx.doc,
                child_sym,
                ctx.scene,
                child_xform,
                child_frame,
                child_cx,
            );
            let body_cx = compose_color_matrix(child_cx, &p.filters);
            render_symbol_xformed(
                ctx.doc,
                child_sym,
                ctx.scene,
                child_xform,
                child_frame,
                body_cx,
            );
            continue;
        }

        // Ordinary recursion: ctx.child() handles transform / color
        // xform / frame propagation / ratio / zone resolution.
        let mut child = ctx.child(p, id);
        render_symbol_into(&mut child, child_sym);
    }

    // Pop any remaining clips at sprite end.
    while !clip_stack.is_empty() {
        clip_stack.pop();
        ctx.scene.pop_layer();
    }
}

/// Recursively collect the fill geometry of a mask character into one BezPath
/// (in world coordinates). Used as the clip shape for Vello's push_layer.
///
/// `ratio` is the morph ratio of the parent placement of `sym`; only
/// matters when `sym` (or a recursed child) is a `MorphShape`. Sprites
/// recurse with each placement's own ratio so a clip-mask whose mask
/// is itself a sprite-of-morphs interpolates correctly per child.
pub fn collect_mask_path(
    doc: &SwfDoc,
    sym: &Symbol,
    transform: Affine,
    frame: u16,
    ratio: u16,
    out: &mut BezPath,
) {
    match sym {
        Symbol::Shape(shape) => {
            for cmd in flatten_shape(shape, doc) {
                if matches!(cmd.kind, DrawKind::Fill { .. }) {
                    append_path_transformed(out, &cmd.path, transform);
                }
            }
        }
        Symbol::Sprite(sprite) => {
            let state = build_frame_state(sprite, frame);
            for (_, p) in &state {
                let Some(id) = p.character_id else { continue };
                let Some(child) = doc.lookup_id(id) else { continue };
                let child_xform = transform * p.matrix.unwrap_or(Affine::IDENTITY);
                let child_frame = match child {
                    Symbol::Sprite(c) if c.num_frames > 1 => frame,
                    _ => 0,
                };
                collect_mask_path(
                    doc,
                    child,
                    child_xform,
                    child_frame,
                    p.ratio.unwrap_or(0),
                    out,
                );
            }
        }
        Symbol::Bitmap(_) => {}
        Symbol::MorphShape(ms) => {
            let interp = crate::morph::build_morph_frame(ms, ratio);
            for cmd in flatten_shape(&interp, doc) {
                if matches!(cmd.kind, DrawKind::Fill { .. }) {
                    append_path_transformed(out, &cmd.path, transform);
                }
            }
        }
    }
}

/// Pre-render any filter-derived passes (DropShadow, Glow, Blur) BEFORE the
/// body. Vello has no off-screen-blur shader for arbitrary shapes; we
/// approximate by drawing the symbol multiple times at small offsets, tinted
/// in the filter's color. Cheap, sub-pixel-accurate enough for tile/sprite
/// halos at the spike's render scale.
#[allow(clippy::too_many_arguments)]
fn apply_filters_pre(
    filters: &[OwnedFilter],
    doc: &SwfDoc,
    sym: &Symbol,
    scene: &mut Scene,
    transform: Affine,
    frame: u16,
    parent_cx: OwnedColorTransform,
) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WARN_BEVEL: AtomicBool = AtomicBool::new(true);
    static WARN_GRAD_GLOW: AtomicBool = AtomicBool::new(true);
    static WARN_GRAD_BEVEL: AtomicBool = AtomicBool::new(true);
    static WARN_CONV: AtomicBool = AtomicBool::new(true);

    for f in filters {
        match f {
            OwnedFilter::DropShadow {
                color_rgba,
                blur_x,
                blur_y,
                angle,
                distance,
                strength,
                inner,
                ..
            } => {
                if *inner {
                    // Inner shadows would require knock-out clipping —
                    // skipped for now.
                    continue;
                }
                // SWF distance/blur are in PIXELS. Path coords are in twips.
                let dx_twips = f64::from(*distance) * angle.cos() as f64 * 20.0;
                let dy_twips = f64::from(*distance) * angle.sin() as f64 * 20.0;
                let halo_cx =
                    color_replace_xform(*color_rgba, parent_cx, *strength);
                // Multi-sample halo to approximate gaussian blur.
                for (ox, oy, weight) in halo_offsets(*blur_x, *blur_y) {
                    let weighted = scale_alpha(halo_cx, weight);
                    let xf = transform
                        .then_translate(vello::kurbo::Vec2::new(
                            dx_twips + ox as f64 * 20.0,
                            dy_twips + oy as f64 * 20.0,
                        ));
                    render_symbol_xformed(doc, sym, scene, xf, frame, weighted);
                }
            }
            OwnedFilter::Glow {
                color_rgba,
                blur_x,
                blur_y,
                strength,
                inner,
                ..
            } => {
                if *inner {
                    continue;
                }
                let halo_cx =
                    color_replace_xform(*color_rgba, parent_cx, *strength);
                for (ox, oy, weight) in halo_offsets(*blur_x, *blur_y) {
                    let weighted = scale_alpha(halo_cx, weight);
                    let xf = transform.then_translate(vello::kurbo::Vec2::new(
                        ox as f64 * 20.0,
                        oy as f64 * 20.0,
                    ));
                    render_symbol_xformed(doc, sym, scene, xf, frame, weighted);
                }
            }
            OwnedFilter::Blur { blur_x, blur_y, passes } => {
                let intensity = (*passes as f32).max(1.0).min(3.0);
                let bx = *blur_x * intensity;
                let by = *blur_y * intensity;
                for (ox, oy, weight) in halo_offsets(bx, by) {
                    let weighted = scale_alpha(parent_cx, weight);
                    let xf = transform.then_translate(vello::kurbo::Vec2::new(
                        ox as f64 * 20.0,
                        oy as f64 * 20.0,
                    ));
                    render_symbol_xformed(doc, sym, scene, xf, frame, weighted);
                }
            }
            OwnedFilter::ColorMatrix { .. } => {
                // Folded into the ColorTransform applied to the body — see
                // compose_color_matrix below.
            }
            OwnedFilter::Unsupported(name) => {
                let warn = match *name {
                    "Bevel" => &WARN_BEVEL,
                    "GradientGlow" => &WARN_GRAD_GLOW,
                    "GradientBevel" => &WARN_GRAD_BEVEL,
                    "Convolution" => &WARN_CONV,
                    _ => continue,
                };
                if warn.swap(false, Ordering::Relaxed) {
                    #[cfg(target_arch = "wasm32")]
                    {
                        let msg = format!("swf-spike: unsupported SWF filter: {name}");
                        web_sys::console::warn_1(
                            &js_sys::JsString::from(msg.as_str()).into(),
                        );
                    }
                    #[cfg(not(target_arch = "wasm32"))]
                    eprintln!("swf-spike: unsupported SWF filter: {name}");
                }
            }
        }
    }
}

/// 9-tap halo (centered + 8 cardinals/diagonals) with gaussian-ish weights.
/// Returns (offset_x_px, offset_y_px, weight) tuples that sum to 1.
fn halo_offsets(blur_x: f32, blur_y: f32) -> Vec<(f32, f32, f32)> {
    let bx = blur_x.max(0.0).min(8.0);
    let by = blur_y.max(0.0).min(8.0);
    if bx < 0.5 && by < 0.5 {
        return vec![(0.0, 0.0, 1.0)];
    }
    let dx = (bx * 0.5).max(0.5);
    let dy = (by * 0.5).max(0.5);
    // Gaussian-ish: center heavy, corners light.
    let c = 0.30;
    let s = 0.10;
    let d = 0.05;
    vec![
        (0.0, 0.0, c),
        (dx, 0.0, s),
        (-dx, 0.0, s),
        (0.0, dy, s),
        (0.0, -dy, s),
        (dx, dy, d),
        (-dx, dy, d),
        (dx, -dy, d),
        (-dx, -dy, d),
    ]
}

/// Build a ColorTransform that **replaces** the source color with the filter
/// color, scaled by alpha and strength. Used for shadow/glow halos.
fn color_replace_xform(
    rgba: [u8; 4],
    parent: OwnedColorTransform,
    strength: f32,
) -> OwnedColorTransform {
    let s = strength.max(0.0).min(8.0);
    let af = (f32::from(rgba[3]) / 255.0) * s;
    // Compose: first parent (modifies the source body's color), then
    // override-via-add to the filter color. Simplification: zero out RGB
    // mults so RGB is fully replaced by add, ignore parent's mult/add for
    // RGB. Keep parent's alpha mult so the halo respects the body's alpha
    // tint (e.g. fade-in).
    OwnedColorTransform {
        mult_r: 0.0,
        mult_g: 0.0,
        mult_b: 0.0,
        mult_a: parent.mult_a * af,
        add_r: f32::from(rgba[0]) / 255.0,
        add_g: f32::from(rgba[1]) / 255.0,
        add_b: f32::from(rgba[2]) / 255.0,
        add_a: parent.add_a,
    }
}

fn scale_alpha(mut cx: OwnedColorTransform, weight: f32) -> OwnedColorTransform {
    cx.mult_a *= weight;
    cx
}

/// Fold a ColorMatrix filter (4x5) into a ColorTransform when the matrix has
/// no cross-channel mixing — the common Dofus case. Otherwise, applies just
/// the diagonal+offset terms (cross-channel mixing is dropped). This keeps
/// the body's render path a single transform.
fn compose_color_matrix(cx: OwnedColorTransform, filters: &[OwnedFilter]) -> OwnedColorTransform {
    let mut out = cx;
    for f in filters {
        if let OwnedFilter::ColorMatrix { matrix } = f {
            // matrix layout: row-major [r-row, g-row, b-row, a-row], each
            // 5 floats: [m_r, m_g, m_b, m_a, m_offset (in 0..255)].
            let cm = OwnedColorTransform {
                mult_r: matrix[0],
                mult_g: matrix[6],
                mult_b: matrix[12],
                mult_a: matrix[18],
                add_r: matrix[4] / 255.0,
                add_g: matrix[9] / 255.0,
                add_b: matrix[14] / 255.0,
                add_a: matrix[19] / 255.0,
            };
            out = out.compose(cm);
        }
    }
    out
}

/// Real filtered render: walk down to one placement, render it to an
/// intermediate texture, apply Gaussian blur / color-matrix / convolve passes
/// from `crate::wgpu_filters`, read back the pixels, and composite as an
/// Image brush in the main `scene`.
///
/// Returns true on success. Returns false when bounds collapse to zero, the
/// renderer fails, or the texture readback fails — caller should fall back to
/// the multi-sample halo approximation in those cases.
#[allow(clippy::too_many_arguments)]
pub fn render_filtered(
    ctx: &mut WgpuCtx,
    doc: &SwfDoc,
    sym: &Symbol,
    scene: &mut Scene,
    transform: Affine,
    frame: u16,
    color_xform: OwnedColorTransform,
    filters: &[OwnedFilter],
) -> bool {
    // 1. Twip-space bounds of the symbol, then expand for filter blur.
    let local_bounds = symbol_bounds(doc, sym, frame);
    if local_bounds.width() <= 0.0 || local_bounds.height() <= 0.0 {
        return false;
    }
    let world_bounds = transform_rect(transform, local_bounds);
    // SWF blur_x/y are pixels. Distance is also pixels. Pad world bounds
    // (twips) by max blur extent + max distance, scaled to twips (× 20).
    let mut pad_px = 4.0_f32; // baseline AA pad
    for f in filters {
        match f {
            OwnedFilter::DropShadow {
                blur_x,
                blur_y,
                distance,
                ..
            } => {
                pad_px = pad_px.max(blur_x.max(*blur_y) * 3.0 + distance.abs());
            }
            OwnedFilter::Glow { blur_x, blur_y, .. } | OwnedFilter::Blur { blur_x, blur_y, .. } => {
                pad_px = pad_px.max(blur_x.max(*blur_y) * 3.0);
            }
            _ => {}
        }
    }
    let pad_twips = f64::from(pad_px) * 20.0;
    let bb_x = world_bounds.x0 - pad_twips;
    let bb_y = world_bounds.y0 - pad_twips;
    let bb_x1 = world_bounds.x1 + pad_twips;
    let bb_y1 = world_bounds.y1 + pad_twips;

    // 2. Convert to output pixel size.
    let scale = ctx.output_scale;
    let bb_w = (((bb_x1 - bb_x) * scale).ceil() as u32).max(1);
    let bb_h = (((bb_y1 - bb_y) * scale).ceil() as u32).max(1);
    if bb_w == 0 || bb_h == 0 {
        return false;
    }

    // 3. Build a sub-scene where the symbol's twip coords map directly to
    //    pixels of the intermediate texture (origin at bb_x, bb_y).
    let sub_world = Affine::scale(scale)
        .then_translate(Vec2::new(-bb_x * scale, -bb_y * scale))
        * transform;
    let mut sub_scene = vello::Scene::new();
    render_symbol_xformed(
        doc,
        sym,
        &mut sub_scene,
        sub_world,
        frame,
        color_xform,
    );

    // 4. Render sub-scene to a fresh storage texture.
    let tex = create_filter_texture(ctx.device, bb_w, bb_h);
    let view = tex.create_view(&Default::default());
    let params = vello::RenderParams {
        base_color: Color::TRANSPARENT,
        width: bb_w,
        height: bb_h,
        antialiasing_method: AaConfig::Area,
    };
    if ctx
        .renderer
        .render_to_texture(ctx.device, ctx.queue, &sub_scene, &view, &params)
        .is_err()
    {
        return false;
    }

    // 5. Apply each filter as a compute pass. DropShadow / Glow are
    //    "additive halos" — we apply the blur to a recolored copy of the
    //    rendered content, then composite that BELOW the unfiltered body.
    let mut halo_textures: Vec<(wgpu::Texture, [u8; 4], f32, f32)> = Vec::new();
    let mut body_tex = tex;
    for filter in filters {
        match filter {
            OwnedFilter::DropShadow {
                color_rgba,
                blur_x,
                blur_y,
                angle,
                distance,
                strength,
                inner: _,
                ..
            } => {
                let halo = recolor(
                    ctx,
                    &body_tex,
                    bb_w,
                    bb_h,
                    *color_rgba,
                    *strength,
                );
                let halo = ctx.filter_pipelines.apply_gaussian_blur(
                    ctx.device,
                    ctx.queue,
                    &halo,
                    bb_w,
                    bb_h,
                    *blur_x * 0.5,
                    *blur_y * 0.5,
                );
                let off_x = angle.cos() * *distance;
                let off_y = angle.sin() * *distance;
                halo_textures.push((halo, *color_rgba, off_x, off_y));
            }
            OwnedFilter::Glow {
                color_rgba,
                blur_x,
                blur_y,
                strength,
                ..
            } => {
                let halo = recolor(
                    ctx,
                    &body_tex,
                    bb_w,
                    bb_h,
                    *color_rgba,
                    *strength,
                );
                let halo = ctx.filter_pipelines.apply_gaussian_blur(
                    ctx.device,
                    ctx.queue,
                    &halo,
                    bb_w,
                    bb_h,
                    *blur_x * 0.5,
                    *blur_y * 0.5,
                );
                halo_textures.push((halo, *color_rgba, 0.0, 0.0));
            }
            OwnedFilter::Blur {
                blur_x,
                blur_y,
                passes,
            } => {
                let mut tex = body_tex;
                for _ in 0..(*passes).max(1) {
                    tex = ctx.filter_pipelines.apply_gaussian_blur(
                        ctx.device,
                        ctx.queue,
                        &tex,
                        bb_w,
                        bb_h,
                        *blur_x * 0.5,
                        *blur_y * 0.5,
                    );
                }
                body_tex = tex;
            }
            OwnedFilter::ColorMatrix { matrix } => {
                body_tex = ctx.filter_pipelines.apply_color_matrix(
                    ctx.device,
                    ctx.queue,
                    &body_tex,
                    bb_w,
                    bb_h,
                    matrix,
                );
            }
            OwnedFilter::Unsupported(_) => {}
        }
    }

    // 6. Composite halos BELOW body, then body on top.
    for (halo_tex, _color, off_x, off_y) in halo_textures {
        let pixels = readback_filter_texture(ctx.device, ctx.queue, &halo_tex, bb_w, bb_h);
        composite_image(
            scene,
            pixels,
            bb_w,
            bb_h,
            bb_x + f64::from(off_x) * 20.0,
            bb_y + f64::from(off_y) * 20.0,
            scale,
        );
    }
    let body_pixels = readback_filter_texture(ctx.device, ctx.queue, &body_tex, bb_w, bb_h);
    composite_image(scene, body_pixels, bb_w, bb_h, bb_x, bb_y, scale);
    true
}

/// Apply a "color-replace" pass: zero RGB, then add filter color * (alpha *
/// strength). Result texture has the same alpha shape as input but the
/// filter's RGB. Used to colorize halos before blurring.
fn recolor(
    ctx: &mut WgpuCtx,
    input: &wgpu::Texture,
    w: u32,
    h: u32,
    rgba: [u8; 4],
    strength: f32,
) -> wgpu::Texture {
    // Build a 4×5 color matrix that does: out = (0, 0, 0, 1) * src + (R, G, B, 0)
    // Effectively: out.rgb = filter.rgb (when src.a > 0), out.a = src.a * strength
    let r = f32::from(rgba[0]) / 255.0;
    let g = f32::from(rgba[1]) / 255.0;
    let b = f32::from(rgba[2]) / 255.0;
    let s = strength.clamp(0.0, 8.0);
    // Encode SVG row-major 4×5: each row is [m_r, m_g, m_b, m_a, m_offset (0..1)]
    let m = [
        // r' = r * src.a + 0 (so out is opaque-tinted where src.a > 0)
        // Actually we want r' = filter.r * src.a (premultiplied feel).
        0.0, 0.0, 0.0, r, 0.0, // r' = r * a
        0.0, 0.0, 0.0, g, 0.0, // g' = g * a
        0.0, 0.0, 0.0, b, 0.0, // b' = b * a
        0.0, 0.0, 0.0, s, 0.0, // a' = src.a * strength
    ];
    ctx.filter_pipelines
        .apply_color_matrix(ctx.device, ctx.queue, input, w, h, &m)
}

/// Composite an RGBA pixel buffer into the main scene via Image brush.
fn composite_image(
    scene: &mut Scene,
    pixels: Vec<u8>,
    w: u32,
    h: u32,
    bb_x_twips: f64,
    bb_y_twips: f64,
    scale: f64,
) {
    let data = ImageData {
        data: Blob::new(Arc::new(pixels)),
        format: ImageFormat::Rgba8,
        // Vello's render_to_texture writes premultiplied RGBA. Marking it as
        // straight would have Vello premultiply again on sample, halving
        // visible alpha and darkening colors.
        alpha_type: ImageAlphaType::AlphaPremultiplied,
        width: w,
        height: h,
    };
    let brush = ImageBrush::from(data);
    // brush_transform: bitmap_pixel → main-scene twip coords. The bitmap
    // pixel (0, 0) corresponds to twip (bb_x, bb_y); each pixel covers 1/scale
    // twips.
    let inv_scale = 1.0 / scale;
    let brush_transform = Affine::translate((bb_x_twips, bb_y_twips))
        * Affine::scale(inv_scale);
    let path = Rect::new(
        bb_x_twips,
        bb_y_twips,
        bb_x_twips + f64::from(w) * inv_scale,
        bb_y_twips + f64::from(h) * inv_scale,
    );
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        &Brush::Image(brush),
        Some(brush_transform),
        &path,
    );
}

fn create_filter_texture(device: &wgpu::Device, w: u32, h: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("swf_spike_filter_input"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn readback_filter_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    tex: &wgpu::Texture,
    w: u32,
    h: u32,
) -> Vec<u8> {
    let bpr = (w * 4 + 255) & !255;
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("swf_spike_readback"),
        size: u64::from(bpr * h),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bpr),
                rows_per_image: None,
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(enc.finish()));

    let slice = buf.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        tx.send(r).unwrap();
    });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .ok();
    rx.recv().unwrap().ok();

    let data = slice.get_mapped_range();
    let mut out = vec![0u8; (w * h * 4) as usize];
    for row in 0..h {
        let src_start = (row * bpr) as usize;
        let dst_start = (row * w * 4) as usize;
        let row_bytes = (w * 4) as usize;
        out[dst_start..dst_start + row_bytes]
            .copy_from_slice(&data[src_start..src_start + row_bytes]);
    }
    drop(data);
    buf.unmap();
    out
}

fn append_path_transformed(out: &mut BezPath, path: &BezPath, m: Affine) {
    for el in path.elements() {
        let mapped = match *el {
            PathEl::MoveTo(p) => PathEl::MoveTo(m * p),
            PathEl::LineTo(p) => PathEl::LineTo(m * p),
            PathEl::QuadTo(c, p) => PathEl::QuadTo(m * c, m * p),
            PathEl::CurveTo(a, b, c) => PathEl::CurveTo(m * a, m * b, m * c),
            PathEl::ClosePath => PathEl::ClosePath,
        };
        out.push(mapped);
    }
}
