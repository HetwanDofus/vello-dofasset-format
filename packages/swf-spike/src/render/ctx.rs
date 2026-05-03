//! `RenderCtx` — single value carrying every parameter the renderer
//! needs as it walks a SWF symbol tree. Replaces the 10-arg parameter
//! chain that grew across `render_symbol_xformed_ctx_ratio`,
//! `render_sprite_ctx`, etc.
//!
//! Phase 1 status: struct + builder methods are defined here, but the
//! existing rendering functions still take individual parameters. The
//! adoption switch happens in Phase 2 — every internal `render_*`
//! function will then take `&mut RenderCtx<'_>` instead.
//!
//! Lifetime model: one `'a` covers the doc + scene + wgpu borrows.
//! `child()` and `with_*()` return short-lived ctxs that reborrow the
//! mutable scene/wgpu refs, so a child render can't outlive its
//! parent.

use vello::kurbo::Affine;

use crate::recolor::PlayerColors;
use crate::swf_doc::{OwnedColorTransform, OwnedPlace, SwfDoc};

use super::cache::RenderCache;
use super::WgpuCtx;

/// Everything the renderer reads while walking a symbol tree.
///
/// Construct with `RenderCtx::new(doc, scene)` for stateless
/// rendering (tile path) or `RenderCtx::new(...).with_wgpu(...)` for
/// the filter-capable path.
pub struct RenderCtx<'a> {
    /// The SWF document being rendered.
    pub doc: &'a SwfDoc,
    /// Vello scene we're emitting commands into. Mutably borrowed.
    pub scene: &'a mut vello::Scene,
    /// Optional wgpu context. Required for filter rendering
    /// (drop-shadow / glow / blur compute passes); `None` for
    /// stateless tile rendering.
    pub wgpu: Option<WgpuCtx<'a>>,

    /// World-space transform from twip coordinates to scene units.
    /// Composes with each placement's matrix as we recurse.
    pub transform: Affine,
    /// Current sprite frame (0-based). Multi-frame sprite children
    /// inherit this from the parent; single-frame children always
    /// see frame 0.
    pub frame: u16,
    /// Morph interpolation ratio of the *current* placement
    /// (0..=65535). Only meaningful when the current symbol is a
    /// `MorphShape`; sprites/shapes ignore it.
    pub ratio: u16,

    /// Multiplicative + additive color transform from PlaceObject's
    /// CXFORM. Composes with the parent's at each placement.
    pub color_xform: OwnedColorTransform,
    /// HSL-recolor target for the current zone (the player's chosen
    /// colour for whichever zone covers this subtree). `None` means
    /// no recolor — fills emit as authored.
    pub recolor_target: Option<u32>,
    /// Player's three zone colours. Used to *resolve* a child's
    /// `applyColor` zone into a `recolor_target`. Stays the same as
    /// we descend; the per-subtree change is `recolor_target`, not
    /// this.
    pub player_colors: PlayerColors,

    /// Memoization for shape flattening, morph interpolation, and
    /// frame-state walks. Shared across the whole render call (and
    /// across multiple calls if the caller reuses the cache).
    pub cache: &'a RenderCache,
}

impl<'a> RenderCtx<'a> {
    /// Stateless renderer (no filters). Used by tile rendering and
    /// the AVM1-free spell preview path.
    pub fn new(
        doc: &'a SwfDoc,
        scene: &'a mut vello::Scene,
        cache: &'a RenderCache,
    ) -> Self {
        Self {
            doc,
            scene,
            wgpu: None,
            transform: Affine::IDENTITY,
            frame: 0,
            ratio: 0,
            color_xform: OwnedColorTransform::IDENTITY,
            recolor_target: None,
            player_colors: PlayerColors::default(),
            cache,
        }
    }

    /// Attach a wgpu context so filtered placements can render to
    /// intermediate textures and apply compute passes. Without this,
    /// filtered placements fall back to the cheap halo approximation.
    pub fn with_wgpu(mut self, wgpu: WgpuCtx<'a>) -> Self {
        self.wgpu = Some(wgpu);
        self
    }

    /// Set the world transform. Typically used at the entry point to
    /// position the symbol within the output texture.
    pub fn with_transform(mut self, transform: Affine) -> Self {
        self.transform = transform;
        self
    }

    /// Set the starting frame. Tile rendering passes the desired
    /// frame here; nested children pick it up via `child()`.
    pub fn with_frame(mut self, frame: u16) -> Self {
        self.frame = frame;
        self
    }

    /// Set the player's three zone colours. After this, descending
    /// into a sprite tagged with an `applyColor` zone will resolve
    /// to the right `recolor_target` automatically.
    pub fn with_player_colors(mut self, pc: PlayerColors) -> Self {
        self.player_colors = pc;
        self
    }

    /// Seed the color transform. Used by `render_symbol_xformed`
    /// callers (e.g. spell with player background) that want a
    /// non-identity starting CXFORM at the entry point.
    pub fn with_color_xform(mut self, cx: OwnedColorTransform) -> Self {
        self.color_xform = cx;
        self
    }

    /// Compute the child ctx for a placement at the given
    /// `child_id`. Composes transform + color_xform, propagates
    /// frame to multi-frame sprite children, picks the placement's
    /// ratio, and resolves the child's zone (if any) into a fresh
    /// `recolor_target` (else inherits the parent's).
    ///
    /// Returns a *reborrow*: the new ctx mutably borrows the same
    /// scene/wgpu, so the parent ctx is suspended until the child
    /// drops. This is the same pattern Vello uses for `Scene`
    /// recursion and prevents accidentally rendering twice into the
    /// same scene from two ctxs.
    #[must_use]
    pub fn child<'b>(&'b mut self, place: &OwnedPlace, child_id: u16) -> RenderCtx<'b>
    where
        'a: 'b,
    {
        let transform = self.transform * place.matrix.unwrap_or(Affine::IDENTITY);
        let color_xform = self
            .color_xform
            .compose(place.color_transform.unwrap_or(OwnedColorTransform::IDENTITY));

        // Zone resolution: if this child sprite has its own
        // applyColor zone, that overrides the inherited recolor
        // target for this subtree.
        let recolor_target = self
            .doc
            .sprite_color_zones
            .get(&child_id)
            .and_then(|z| self.player_colors.lookup(*z))
            .or(self.recolor_target);

        // Frame propagation: multi-frame sprite children step in
        // sync with the parent; single-frame children (shapes,
        // bitmaps, morphs, 1-frame wrapper sprites) always see 0.
        let frame = match self.doc.lookup_id(child_id) {
            Some(crate::swf_doc::Symbol::Sprite(c)) if c.num_frames > 1 => self.frame,
            _ => 0,
        };

        let ratio = place.ratio.unwrap_or(0);

        RenderCtx {
            doc: self.doc,
            scene: self.scene,
            wgpu: self.wgpu.as_mut().map(|w| WgpuCtx {
                device: w.device,
                queue: w.queue,
                renderer: w.renderer,
                filter_pipelines: w.filter_pipelines,
                output_scale: w.output_scale,
            }),
            transform,
            frame,
            ratio,
            color_xform,
            recolor_target,
            player_colors: self.player_colors,
            cache: self.cache,
        }
    }
}
