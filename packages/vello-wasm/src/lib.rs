use std::collections::HashMap;

use dofasset_renderer::format::{self, DofAsset};
use dofasset_renderer::scene_builder::{self, AccessoryScene};
use vello::peniko::Color;
use vello::{wgpu, AaConfig, Renderer, RendererOptions};
use wasm_bindgen::prelude::*;


#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    fn log(s: &str);
}

macro_rules! console_log {
    ($($t:tt)*) => (log(&format!($($t)*)))
}

/// Extract the raw `GPUDevice` JsValue from a `wgpu::Device` via our patched wgpu.
fn extract_gpu_device(device: &wgpu::Device) -> JsValue {
    device.inner.as_webgpu().inner.clone().into()
}

/// Extract the raw `GPUAdapter` JsValue from a `wgpu::Adapter`.
fn extract_gpu_adapter(adapter: &wgpu::Adapter) -> JsValue {
    adapter.inner.as_webgpu().inner.clone().into()
}

/// Extract the raw `GPUTexture` JsValue from a `wgpu::Texture`.
fn extract_gpu_texture(texture: &wgpu::Texture) -> JsValue {
    texture.inner.as_webgpu().inner.clone().into()
}

/// A frame queued for batch rendering.
struct QueuedFrame {
    scene: vello::Scene,
    width: u32,
    height: u32,
    dst_x: u32,
    dst_y: u32,
}

/// Cached uniform bounds for an animation (union across all frames, anchor-aligned).
#[derive(Clone, Copy)]
struct UniformBounds {
    min_x: f64,
    min_y: f64,
    width: f64,
    height: f64,
    /// Frame 0's offset_x/offset_y (reference anchor position)
    trim_x: f64,
    trim_y: f64,
}

#[wasm_bindgen]
pub struct VelloRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: Renderer,
    assets: HashMap<u32, DofAsset>,
    textures: HashMap<u64, wgpu::Texture>,
    next_texture_id: u64,
    queued_frames: Vec<QueuedFrame>,
    anim_bounds: HashMap<String, UniformBounds>,
    /// Raw JS GPUDevice/GPUAdapter — stored because wgpu 28 makes backend fields private.
    raw_gpu_device: JsValue,
    raw_gpu_adapter: JsValue,
}

/// Resolve which animation name to use for an accessory.
///
/// Exact mapping from original Dofus 1.29 SWF bytecode (verified sprites 10 & 100).
/// anim_type: 0=static, 1=walk, 2=run
///
/// Cape (slot 2, has L/R/RR/RL):          Shield (slot 4, has L/R):
///   dir  static  walk  run                 dir  static  walk  run
///    R     R      R    RR                   R     R      R     R
///    L     L      L    RL                   L     L      L     L
///    F     R      R    RR                   F     R      L     L
///    B     L      L    RL                   B     R      L     R
///    S     R      L     L                   S     R      R     R
///
/// Hat (slot 1, has R/L/F/B/S): uses dir_suffix directly.
/// Weapon (slot 0, has WR/WL/WF/WB/WS for walk/run, R/L/F/B/S for static): uses dir_suffix.
/// Pet (slot 3, has L/R/WL/WR/etc): tries W-prefix for walk/run, else dir_suffix.
fn resolve_accessory_anim(
    acc_asset: &DofAsset,
    animation: &str,
    dir_suffix: &str,
    anim_type: u8,
    slot_id: u8,
) -> Option<String> {
    // 1. Exact animation name match (e.g., "walkR" in the accessory)
    if acc_asset.animation_map.contains_key(animation) {
        return Some(animation.to_string());
    }

    // 2. W-prefix for walk/run (pet accessories have WR, WL, WS, WF, WB)
    if anim_type > 0 {
        let w_dir = format!("W{}", dir_suffix);
        if acc_asset.animation_map.contains_key(w_dir.as_str()) {
            return Some(w_dir);
        }
    }

    // 3. Slot-specific direction lookup from the Dofus truth table
    let candidates: &[&str] = match slot_id {
        // Cape
        2 => match (anim_type, dir_suffix) {
            (0, "R") | (0, "F") | (0, "S") => &["R", "L"],
            (0, "B") | (0, "L")            => &["L", "R"],
            (1, "R") | (1, "F")            => &["R", "L"],
            (1, "B") | (1, "L") | (1, "S") => &["L", "R"],
            (2, "R") | (2, "F")            => &["RR", "R", "L"],
            (2, "B") | (2, "L")            => &["RL", "L", "R"],
            (2, _)                          => &["L", "R"],
            _                               => &["R", "L"],
        },
        // Shield
        4 => match (anim_type, dir_suffix) {
            (0, "L")                        => &["L", "R"],
            (0, _)                          => &["R", "L"],
            (1, "F") | (1, "B") | (1, "L") => &["L", "R"],
            (1, _)                          => &["R", "L"],
            (2, "F") | (2, "L")            => &["L", "R"],
            (2, _)                          => &["R", "L"],
            _                               => &["R", "L"],
        },
        // Hat, weapon, pet, default: try dir_suffix, then common fallbacks
        _ => match dir_suffix {
            "R" => &["R", "S", "F"],
            "L" => &["L", "S", "F"],
            "F" => &["F", "S", "R"],
            "B" => &["B", "S", "L"],
            "S" => &["S", "R", "F"],
            _   => &["R", "L", "S"],
        },
    };

    for &c in candidates {
        if acc_asset.animation_map.contains_key(c) {
            return Some(c.to_string());
        }
    }

    None
}

/// Build accessory scenes from .dofasset data.
/// `acc_info` is a flat array of pairs: [asset_id, slot_id, asset_id, slot_id, ...]
/// Each accessory renders using the same animation name and frame index as the character.
/// Falls back to direction suffix (e.g., "R") if full animation name (e.g., "walkR") not found.
fn build_accessory_scenes(
    assets: &HashMap<u32, DofAsset>,
    acc_info: &Option<Vec<u32>>,
    animation: &str,
    frame_index: usize,
    _resolution: f32,
) -> Vec<AccessoryScene> {
    let Some(info) = acc_info else { return Vec::new() };
    if info.len() < 2 { return Vec::new(); }

    // Extract direction suffix for fallback (e.g., "walkR" → "R")
    let dir_suffix = if animation.len() >= 2 {
        &animation[animation.len()-1..]
    } else {
        "S"
    };

    // Classify animation type: static, walk, or run
    let anim_type = if animation.starts_with("run") { 2u8 }
        else if animation.starts_with("static") { 0 }
        else { 1 }; // walk and everything else

    let mut scenes = Vec::new();
    for pair in info.chunks(2) {
        let acc_asset_id = pair[0];
        let slot_id = pair[1] as u8;

        let Some(acc_asset) = assets.get(&acc_asset_id) else { continue };

        // Resolve the accessory animation name.
        // Priority: exact match ��� W-prefix (pet walk) → Dofus direction table ��� generic fallback
        let resolved_anim = resolve_accessory_anim(
            acc_asset, animation, dir_suffix, anim_type, slot_id,
        );
        let Some(resolved_anim) = resolved_anim else { continue };

        let &anim_idx = acc_asset.animation_map.get(resolved_anim.as_str()).unwrap();
        let anim = &acc_asset.animations[anim_idx];
        if anim.frame_ids.is_empty() { continue; }

        let actual_frame = frame_index % anim.frame_ids.len();
        let scene = scene_builder::build_accessory_scene_unscaled(
            acc_asset, &resolved_anim, actual_frame,
        );

        // Use net offset (where Flash 0,0 actually lands in the rendered scene)
        // instead of SWF metadata offset. Same fix as character positioning —
        // eliminates ~0.5px discrepancy that displaces accessories.
        let global_fid = anim.frame_ids[actual_frame] as usize;
        let (offset_x, offset_y, acc_w, acc_h) = if let Some(acc_frame) = acc_asset.frames.get(global_fid) {
            let net = scene_builder::compute_net_offset(acc_frame, &acc_asset.transforms);
            (
                -net.0,  // negate: acc_local ADDS this to slot position
                -net.1,
                acc_frame.clip_rect[2] as f64,
                acc_frame.clip_rect[3] as f64,
            )
        } else {
            (anim.offset_x as f64, anim.offset_y as f64, 0.0, 0.0)
        };

        scenes.push(AccessoryScene {
            slot_id,
            scene,
            offset_x,
            offset_y,
            width: acc_w,
            height: acc_h,
        });
    }
    scenes
}

#[wasm_bindgen]
impl VelloRenderer {
    /// Initialize the renderer. Returns a JS object `{ renderer, adapter, device }`
    /// so JS can pass `{ adapter, device }` to Pixi.js.
    #[wasm_bindgen(js_name = "init")]
    pub async fn init() -> Result<JsValue, JsValue> {
        console_error_panic_hook::set_once();
        console_log!("VelloRenderer: initializing WebGPU...");

        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::BROWSER_WEBGPU | wgpu::Backends::GL,
            ..Default::default()
        });

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                ..Default::default()
            })
            .await
            .map_err(|_| JsValue::from_str("No WebGPU adapter found"))?;

        let adapter_limits = adapter.limits();
        console_log!("VelloRenderer: adapter = {:?}", adapter.get_info().name);
        console_log!(
            "VelloRenderer: adapter limits: maxSampledTextures={}, maxSamplers={}",
            adapter_limits.max_sampled_textures_per_shader_stage,
            adapter_limits.max_samplers_per_shader_stage,
        );

        // Start from defaults (Chrome auto-raises most limits) and only
        // override the specific limits we need for large atlas textures.
        let mut required_limits = wgpu::Limits::default();
        required_limits.max_texture_dimension_2d = adapter_limits.max_texture_dimension_2d;
        required_limits.max_buffer_size = adapter_limits.max_buffer_size;
        required_limits.max_storage_buffer_binding_size = adapter_limits.max_storage_buffer_binding_size;
        console_log!("VelloRenderer: maxTexDim2D={} maxBufSize={}",
            required_limits.max_texture_dimension_2d, required_limits.max_buffer_size);

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("vello-pixi-shared"),
                    required_features: wgpu::Features::empty(),
                    required_limits,
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| JsValue::from_str(&format!("Device request failed: {e}")))?;

        let renderer = Renderer::new(
            &device,
            RendererOptions {
                use_cpu: false,
                antialiasing_support: vello::AaSupport::area_only(),
                num_init_threads: std::num::NonZeroUsize::new(1),
                pipeline_cache: None,
            },
        )
        .map_err(|e| JsValue::from_str(&format!("Vello renderer init failed: {e}")))?;

        console_log!("VelloRenderer: ready");

        // Extract raw GPU handles from wgpu's wrapper types for Pixi.js sharing.
        // wgpu 28 makes backend fields private, but the struct layout is known:
        //   Device { inner: DispatchDevice }
        // Extract raw GPU handles for Pixi.js (via patched wgpu with pub inner fields)
        let gpu_adapter = extract_gpu_adapter(&adapter);
        let gpu_device = extract_gpu_device(&device);

        let vello = VelloRenderer {
            device,
            queue,
            renderer,
            assets: HashMap::new(),
            textures: HashMap::new(),
            next_texture_id: 0,
            queued_frames: Vec::new(),
            anim_bounds: HashMap::new(),
            raw_gpu_device: gpu_device.clone(),
            raw_gpu_adapter: gpu_adapter.clone(),
        };

        // Return { renderer, adapter, device, maxTextureSize }
        let result = js_sys::Object::new();
        js_sys::Reflect::set(&result, &"renderer".into(), &vello.into())?;
        js_sys::Reflect::set(&result, &"adapter".into(), &gpu_adapter)?;
        js_sys::Reflect::set(&result, &"device".into(), &gpu_device)?;
        js_sys::Reflect::set(&result, &"maxTextureSize".into(), &JsValue::from(adapter_limits.max_texture_dimension_2d))?;

        Ok(result.into())
    }

    /// Load a .dofasset binary. Returns true on success, false if data is invalid.
    /// Never panics — invalid data is handled gracefully to avoid poisoning
    /// the wasm-bindgen RefCell (which would break ALL subsequent calls).
    #[wasm_bindgen(js_name = "loadAsset")]
    pub fn load_asset(&mut self, id: u32, data: &[u8]) -> bool {
        // Validate magic bytes before attempting full parse to avoid panics
        if data.len() < 4 || &data[0..4] != b"DASF" {
            return false;
        }
        let asset = format::load(data);
        console_log!(
            "VelloRenderer: loaded asset {} ({} anims, {} frames)",
            id,
            asset.animations.len(),
            asset.frames.len()
        );
        self.assets.insert(id, asset);
        true
    }

    /// Free a loaded asset.
    #[wasm_bindgen(js_name = "freeAsset")]
    pub fn free_asset(&mut self, id: u32) {
        self.assets.remove(&id);
    }

    // Accessories are loaded as regular .dofasset assets via loadAsset().
    // When rendering, pass accessory info as flat array: [asset_id, slot_id, asset_id, slot_id, ...]
    // Each accessory renders using the same animation name and frame index as the character.

    /// Get animation info. Returns { fps, frameCount, offsetX, offsetY, trimX, trimY, ... } or null.
    /// `offsetX/Y` is derived from the first frame's net offset (where Flash (0,0) actually
    /// lands in the rendered texture). This avoids sub-pixel discrepancies between the SWF
    /// metadata and the actual SVG content positioning that cause jitter between animations.
    #[wasm_bindgen(js_name = "getAnimationInfo")]
    pub fn get_animation_info(&self, asset_id: u32, animation: &str) -> JsValue {
        let Some(asset) = self.assets.get(&asset_id) else {
            return JsValue::NULL;
        };
        let Some(&anim_idx) = asset.animation_map.get(animation) else {
            return JsValue::NULL;
        };
        let anim = &asset.animations[anim_idx];

        // Derive offset from the first frame's net offset (clip_offset * offset_transform).
        // This is where Flash (0,0) — the registration point — actually lands in the
        // rendered texture. Using this instead of anim.offset_x/y eliminates the ~0.5px
        // discrepancy between SWF metadata and SVG content that causes visible shifts
        // when switching between animations.
        let (offset_x, offset_y, trim_x, trim_y, frame_w, frame_h) = if !anim.frame_ids.is_empty() {
            let fid = anim.frame_ids[0] as usize;
            if let Some(frame) = asset.frames.get(fid) {
                let net = scene_builder::compute_net_offset(frame, &asset.transforms);
                (
                    -(net.0 as f32),     // negate: net is position of reg point, offset is frame-to-regpoint
                    -(net.1 as f32),
                    frame.offset_x,
                    frame.offset_y,
                    frame.clip_rect[2],
                    frame.clip_rect[3],
                )
            } else {
                (anim.offset_x, anim.offset_y, 0.0, 0.0, 0.0, 0.0)
            }
        } else {
            (anim.offset_x, anim.offset_y, 0.0, 0.0, 0.0, 0.0)
        };

        let obj = js_sys::Object::new();
        let _ = js_sys::Reflect::set(&obj, &"fps".into(), &JsValue::from(anim.fps));
        let _ = js_sys::Reflect::set(&obj, &"frameCount".into(), &JsValue::from(anim.frame_ids.len() as u32));
        let _ = js_sys::Reflect::set(&obj, &"offsetX".into(), &JsValue::from(offset_x));
        let _ = js_sys::Reflect::set(&obj, &"offsetY".into(), &JsValue::from(offset_y));
        let _ = js_sys::Reflect::set(&obj, &"trimX".into(), &JsValue::from(trim_x));
        let _ = js_sys::Reflect::set(&obj, &"trimY".into(), &JsValue::from(trim_y));
        let _ = js_sys::Reflect::set(&obj, &"frameWidth".into(), &JsValue::from(frame_w));
        let _ = js_sys::Reflect::set(&obj, &"frameHeight".into(), &JsValue::from(frame_h));
        let _ = js_sys::Reflect::set(&obj, &"hasBaseFrame".into(), &JsValue::from(anim.base_frame_id != u32::MAX));
        let _ = js_sys::Reflect::set(&obj, &"baseZOrder".into(), &JsValue::from(anim.base_z_order));
        obj.into()
    }

    /// Get per-animation uniform canvas size + anchor.
    /// Returns { width, height, anchorX, anchorY } or null.
    /// All frames render at this size — eliminates jitter from per-frame size variation.
    /// The game engine draws the canvas at (screenX - anchorX, screenY - anchorY).
    #[wasm_bindgen(js_name = "getAnimationMeta")]
    pub fn get_animation_meta(
        &self,
        asset_id: u32,
        animation: &str,
        resolution: f32,
        acc_info: Option<Vec<u32>>,
    ) -> JsValue {
        let Some(asset) = self.assets.get(&asset_id) else {
            return JsValue::NULL;
        };

        let acc_scenes = build_accessory_scenes(
            &self.assets, &acc_info, animation, 0, resolution,
        );
        let acc_refs: Vec<&AccessoryScene> = acc_scenes.iter().collect();

        let meta = scene_builder::compute_animation_render_meta(
            asset, animation, resolution, &acc_refs,
        );

        let obj = js_sys::Object::new();
        let _ = js_sys::Reflect::set(&obj, &"width".into(), &JsValue::from(meta.canvas_width));
        let _ = js_sys::Reflect::set(&obj, &"height".into(), &JsValue::from(meta.canvas_height));
        let _ = js_sys::Reflect::set(&obj, &"anchorX".into(), &JsValue::from(meta.anchor_x));
        let _ = js_sys::Reflect::set(&obj, &"anchorY".into(), &JsValue::from(meta.anchor_y));
        obj.into()
    }

    /// Compute pixel dimensions for a frame at a given resolution.
    /// Returns the **uniform** canvas size for the entire animation (not per-frame).
    /// This ensures all frames of the same animation render at the same size,
    /// eliminating jitter from per-frame size variation.
    /// Returns [width, height].
    #[wasm_bindgen(js_name = "getFrameSize")]
    pub fn get_frame_size(
        &self,
        asset_id: u32,
        animation: &str,
        _frame_index: u32,
        resolution: f32,
    ) -> Vec<u32> {
        let Some(asset) = self.assets.get(&asset_id) else {
            return vec![0, 0];
        };

        let meta = scene_builder::compute_animation_render_meta(
            asset, animation, resolution, &[],
        );
        vec![meta.canvas_width.max(1), meta.canvas_height.max(1)]
    }

    /// Render a frame and return the raw `GPUTexture` as a JsValue.
    /// Render a frame with optional player colors [color1, color2, color3] as 0xRRGGBB.
    /// Pass null/undefined for no color replacement.
    /// Optional acc_info is a flat array of pairs: [asset_id, slot_id, asset_id, slot_id, ...]
    /// Each accessory must be loaded via loadAsset() first. They render using the same
    /// animation name and frame index as the character.
    #[wasm_bindgen(js_name = "renderFrame")]
    pub fn render_frame(
        &mut self,
        asset_id: u32,
        animation: &str,
        frame_index: u32,
        resolution: f32,
        colors: Option<Vec<u32>>,
        acc_info: Option<Vec<u32>>,
    ) -> JsValue {
        let Some(asset) = self.assets.get(&asset_id) else {
            return JsValue::NULL;
        };

        // Convert optional colors array to the format build_frame_scene expects
        let player_colors: Option<[u32; 3]> = colors.and_then(|c| {
            if c.len() >= 3 {
                Some([c[0], c[1], c[2]])
            } else {
                None
            }
        });

        // Build accessory scenes from .dofasset data
        let acc_scenes = build_accessory_scenes(
            &self.assets, &acc_info, animation, frame_index as usize, resolution,
        );
        let acc_refs: Vec<&AccessoryScene> = acc_scenes.iter().collect();

        // Compute canvas size including accessories
        let meta = scene_builder::compute_animation_render_meta(
            asset, animation, resolution, &acc_refs,
        );
        let w = meta.canvas_width.max(1);
        let h = meta.canvas_height.max(1);

        let scene = scene_builder::build_frame_scene(
            asset,
            animation,
            frame_index as usize,
            player_colors.as_ref(),
            resolution,
            &acc_refs,
            (0.0, 0.0),
        );

        // Render directly to the output texture.
        // STORAGE_BINDING: Vello compute writes
        // COPY_SRC: batchCopy into atlas
        // TEXTURE_BINDING: Pixi.js may sample it directly (e.g., non-atlas path)
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vello_frame"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[wgpu::TextureFormat::Rgba8UnormSrgb],
        });

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        let params = vello::RenderParams {
            base_color: Color::TRANSPARENT,
            width: w,
            height: h,
            antialiasing_method: AaConfig::Area,
        };

        if let Err(e) =
            self.renderer
                .render_to_texture(&self.device, &self.queue, &scene, &view, &params)
        {
            console_log!("VelloRenderer: render error: {e}");
            return JsValue::NULL;
        }

        // Extract GPUTexture — this is a temp texture for batchCopy into the atlas
        let gpu_texture = extract_gpu_texture(&texture);

        // Keep the wgpu::Texture alive so the GPUTexture isn't garbage collected
        let tex_id = self.next_texture_id;
        self.next_texture_id += 1;
        self.textures.insert(tex_id, texture);

        // Return { texture: GPUTexture, textureId: number, width: number, height: number }
        let result = js_sys::Object::new();
        let _ = js_sys::Reflect::set(&result, &"texture".into(), &gpu_texture);
        let _ = js_sys::Reflect::set(&result, &"textureId".into(), &JsValue::from(tex_id as f64));
        let _ = js_sys::Reflect::set(&result, &"width".into(), &JsValue::from(w));
        let _ = js_sys::Reflect::set(&result, &"height".into(), &JsValue::from(h));

        result.into()
    }

    /// Create an atlas texture for packing multiple character frames.
    /// Returns { texture: GPUTexture, textureId: number, width, height }.
    /// All characters' frames are copied into this single texture via renderFrameToAtlas.
    #[wasm_bindgen(js_name = "createAtlas")]
    pub fn create_atlas(&mut self, width: u32, height: u32) -> JsValue {
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vello_atlas"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[wgpu::TextureFormat::Rgba8UnormSrgb],
        });

        let gpu_texture = extract_gpu_texture(&texture);
        let tex_id = self.next_texture_id;
        self.next_texture_id += 1;
        self.textures.insert(tex_id, texture);

        let result = js_sys::Object::new();
        let _ = js_sys::Reflect::set(&result, &"texture".into(), &gpu_texture);
        let _ = js_sys::Reflect::set(&result, &"textureId".into(), &JsValue::from(tex_id as f64));
        let _ = js_sys::Reflect::set(&result, &"width".into(), &JsValue::from(width));
        let _ = js_sys::Reflect::set(&result, &"height".into(), &JsValue::from(height));
        result.into()
    }

    /// Batch-copy multiple textures into a destination texture. ONE queue.submit for all copies.
    /// `copies` is a flat array: [src_id, dst_x, dst_y, src_id, dst_x, dst_y, ...]
    /// All copies go to the same destination texture (the atlas).
    #[wasm_bindgen(js_name = "batchCopy")]
    pub fn batch_copy(
        &mut self,
        dst_texture_id: f64,
        copies: &[f64],
    ) -> bool {
        let dst_id = dst_texture_id as u64;
        if copies.len() < 3 { return false; }

        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("batch_copy"),
        });

        for chunk in copies.chunks(3) {
            let src_id = chunk[0] as u64;
            let dst_x = chunk[1] as u32;
            let dst_y = chunk[2] as u32;

            let Some(src) = self.textures.get(&src_id) else { continue };
            let src_w = src.width();
            let src_h = src.height();
            let Some(dst) = self.textures.get(&dst_id) else { return false };

            encoder.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: src, mip_level: 0,
                    origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: dst, mip_level: 0,
                    origin: wgpu::Origin3d { x: dst_x, y: dst_y, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d { width: src_w, height: src_h, depth_or_array_layers: 1 },
            );
        }

        self.queue.submit(Some(encoder.finish()));
        true
    }

    /// Render a frame and copy it into a slot of an atlas texture.
    /// Returns { width, height } of the rendered frame, or null on failure.
    /// dst_x/dst_y are pixel coordinates in the atlas.
    #[wasm_bindgen(js_name = "renderFrameToAtlas")]
    pub fn render_frame_to_atlas(
        &mut self,
        atlas_texture_id: f64,
        dst_x: u32,
        dst_y: u32,
        asset_id: u32,
        animation: &str,
        frame_index: u32,
        resolution: f32,
        colors: Option<Vec<u32>>,
        acc_info: Option<Vec<u32>>,
        slot_w: Option<u32>,
        slot_h: Option<u32>,
    ) -> JsValue {
        let atlas_id = atlas_texture_id as u64;
        let Some(atlas) = self.textures.get(&atlas_id) else {
            return JsValue::NULL;
        };
        // Clone the reference so we don't hold a borrow on self.textures
        let atlas_size = wgpu::Extent3d {
            width: atlas.width(),
            height: atlas.height(),
            depth_or_array_layers: 1,
        };
        // We need a way to reference the atlas texture for the copy.
        // Since copy_texture_to_texture takes &Texture, we need to work with the borrow.
        // Build the scene first (needs &self.assets), then do the GPU work.

        let Some(asset) = self.assets.get(&asset_id) else {
            return JsValue::NULL;
        };

        let player_colors: Option<[u32; 3]> = colors.and_then(|c| {
            if c.len() >= 3 { Some([c[0], c[1], c[2]]) } else { None }
        });

        let acc_scenes = build_accessory_scenes(
            &self.assets, &acc_info, animation, frame_index as usize, resolution,
        );
        let acc_refs: Vec<&AccessoryScene> = acc_scenes.iter().collect();

        let meta = scene_builder::compute_animation_render_meta(
            asset, animation, resolution, &acc_refs,
        );
        let w = meta.canvas_width.max(1);
        let h = meta.canvas_height.max(1);

        let scene = scene_builder::build_frame_scene(
            asset, animation, frame_index as usize,
            player_colors.as_ref(), resolution, &acc_refs, (0.0, 0.0),
        );

        // Render to a temporary texture sized to the FULL SLOT (not just the frame).
        // The scene content occupies (w, h) and the rest is transparent.
        // This ensures the entire slot area is overwritten when copied to the atlas,
        // preventing stale content from a previous (larger) frame showing through.
        let render_w = slot_w.unwrap_or(w).max(w);
        let render_h = slot_h.unwrap_or(h).max(h);
        let render_target = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vello_atlas_rt"),
            size: wgpu::Extent3d { width: render_w, height: render_h, depth_or_array_layers: 1 },
            mip_level_count: 1, sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let rt_view = render_target.create_view(&wgpu::TextureViewDescriptor::default());
        let params = vello::RenderParams {
            base_color: Color::TRANSPARENT,
            width: render_w, height: render_h,
            antialiasing_method: AaConfig::Area,
        };

        if self.renderer.render_to_texture(&self.device, &self.queue, &scene, &rt_view, &params).is_err() {
            return JsValue::NULL;
        }

        // Copy the full slot area (not just the frame) to clear any stale content
        let copy_w = render_w.min(atlas_size.width.saturating_sub(dst_x));
        let copy_h = render_h.min(atlas_size.height.saturating_sub(dst_y));
        if copy_w == 0 || copy_h == 0 {
            return JsValue::NULL;
        }

        // Copy rendered frame into atlas slot
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("atlas_copy"),
        });

        // Re-borrow atlas texture for the copy
        let atlas_tex = self.textures.get(&atlas_id).unwrap();
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &render_target, mip_level: 0,
                origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: atlas_tex, mip_level: 0,
                origin: wgpu::Origin3d { x: dst_x, y: dst_y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d { width: copy_w, height: copy_h, depth_or_array_layers: 1 },
        );
        self.queue.submit(Some(encoder.finish()));

        let result = js_sys::Object::new();
        let _ = js_sys::Reflect::set(&result, &"width".into(), &JsValue::from(w));
        let _ = js_sys::Reflect::set(&result, &"height".into(), &JsValue::from(h));
        result.into()
    }

    /// Queue a frame for batch rendering. Builds the Vello scene (CPU work) and stores it.
    /// Returns { width, height } of the frame, or null on failure.
    /// Call flushFrames() to render all queued frames in ONE GPU dispatch + copy.
    #[wasm_bindgen(js_name = "queueFrame")]
    pub fn queue_frame(
        &mut self,
        asset_id: u32,
        animation: &str,
        frame_index: u32,
        resolution: f32,
        colors: Option<Vec<u32>>,
        acc_info: Option<Vec<u32>>,
        dst_x: u32,
        dst_y: u32,
    ) -> JsValue {
        let Some(asset) = self.assets.get(&asset_id) else {
            return JsValue::NULL;
        };

        let player_colors: Option<[u32; 3]> = colors.and_then(|c| {
            if c.len() >= 3 { Some([c[0], c[1], c[2]]) } else { None }
        });

        let acc_scenes = build_accessory_scenes(
            &self.assets, &acc_info, animation, frame_index as usize, resolution,
        );
        let acc_refs: Vec<&AccessoryScene> = acc_scenes.iter().collect();

        // Use character-only canvas size (no accessories) for consistent dimensions.
        // Different accessory combinations produce different canvas sizes, causing
        // characters to shift position within variably-sized textures → visible jitter.
        // Accessories extending beyond the character bounds are clipped by flushFrames.
        let meta = scene_builder::compute_animation_render_meta(
            asset, animation, resolution, &[],
        );
        let w = meta.canvas_width.max(1);
        let h = meta.canvas_height.max(1);

        let scene = scene_builder::build_frame_scene(
            asset, animation, frame_index as usize,
            player_colors.as_ref(), resolution, &acc_refs, (0.0, 0.0),
        );

        self.queued_frames.push(QueuedFrame {
            scene, width: w, height: h, dst_x, dst_y,
        });

        let result = js_sys::Object::new();
        let _ = js_sys::Reflect::set(&result, &"width".into(), &JsValue::from(w));
        let _ = js_sys::Reflect::set(&result, &"height".into(), &JsValue::from(h));
        result.into()
    }

    /// Render all queued frames in a composite grid, then batch-copy to atlas.
    /// 1 Vello render + 1 batch copy = 2 GPU submits total.
    #[wasm_bindgen(js_name = "flushFrames")]
    pub fn flush_frames(&mut self, atlas_texture_id: f64) {
        if self.queued_frames.is_empty() {
            return;
        }

        let atlas_id = atlas_texture_id as u64;
        const MAX_TEX_DIM: u32 = 8192;
        // Cell size = atlas slot size (256 at 2x). Using the slot size ensures
        // each frame's copy overwrites the ENTIRE slot, clearing stale content
        // from previous occupants. The transparent background of the Vello render
        // fills uncovered areas within each cell.
        const SLOT_SIZE: u32 = 256;

        let max_dim = self.queued_frames.iter()
            .map(|f| f.width.max(f.height))
            .max()
            .unwrap_or(SLOT_SIZE);
        let cell = max_dim.max(SLOT_SIZE).next_power_of_two().min(2048);
        let cols = (MAX_TEX_DIM / cell).max(1);
        let max_rows = MAX_TEX_DIM / cell;
        let batch_size = (cols * max_rows) as usize;

        struct Region { src_x: u32, src_y: u32, dst_x: u32, dst_y: u32 }

        let mut offset = 0;
        while offset < self.queued_frames.len() {
            let end = (offset + batch_size).min(self.queued_frames.len());
            let batch = &self.queued_frames[offset..end];
            let batch_len = batch.len() as u32;

            let mut composite = vello::Scene::new();
            let mut regions: Vec<Region> = Vec::new();

            for (i, frame) in batch.iter().enumerate() {
                let col = (i as u32) % cols;
                let row = (i as u32) / cols;
                let cx = col * cell;
                let cy = row * cell;

                // Clip each frame to its cell so content can't bleed into neighbors
                let clip_rect = vello::kurbo::Rect::new(
                    cx as f64, cy as f64, (cx + cell) as f64, (cy + cell) as f64,
                );
                composite.push_layer(
                    vello::peniko::Fill::default(), vello::peniko::BlendMode::default(), 1.0,
                    vello::kurbo::Affine::IDENTITY, &clip_rect,
                );
                composite.append(
                    &frame.scene,
                    Some(vello::kurbo::Affine::translate((cx as f64, cy as f64))),
                );
                composite.pop_layer();

                regions.push(Region { src_x: cx, src_y: cy, dst_x: frame.dst_x, dst_y: frame.dst_y });
            }

            let num_rows = (batch_len + cols - 1) / cols;
            let used_cols = batch_len.min(cols);
            let total_width = used_cols * cell;
            let total_height = num_rows * cell;

            if total_width == 0 || total_height == 0 {
                offset = end;
                continue;
            }

            let render_target = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("batch_render"),
                size: wgpu::Extent3d { width: total_width, height: total_height, depth_or_array_layers: 1 },
                mip_level_count: 1, sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let rt_view = render_target.create_view(&wgpu::TextureViewDescriptor::default());
            let params = vello::RenderParams {
                base_color: Color::TRANSPARENT,
                width: total_width, height: total_height,
                antialiasing_method: AaConfig::Area,
            };

            if self.renderer.render_to_texture(&self.device, &self.queue, &composite, &rt_view, &params).is_err() {
                offset = end;
                continue;
            }

            if let Some(atlas_tex) = self.textures.get(&atlas_id) {
                let atlas_w = atlas_tex.width();
                let atlas_h = atlas_tex.height();
                let mut encoder = self.device.create_command_encoder(
                    &wgpu::CommandEncoderDescriptor { label: Some("batch_copy") },
                );

                // Copy FULL CELL (not just frame size) to clear stale content
                for r in &regions {
                    let cw = cell.min(atlas_w.saturating_sub(r.dst_x));
                    let ch = cell.min(atlas_h.saturating_sub(r.dst_y));
                    if cw == 0 || ch == 0 { continue; }

                    encoder.copy_texture_to_texture(
                        wgpu::TexelCopyTextureInfo {
                            texture: &render_target, mip_level: 0,
                            origin: wgpu::Origin3d { x: r.src_x, y: r.src_y, z: 0 },
                            aspect: wgpu::TextureAspect::All,
                        },
                        wgpu::TexelCopyTextureInfo {
                            texture: atlas_tex, mip_level: 0,
                            origin: wgpu::Origin3d { x: r.dst_x, y: r.dst_y, z: 0 },
                            aspect: wgpu::TextureAspect::All,
                        },
                        wgpu::Extent3d { width: cw, height: ch, depth_or_array_layers: 1 },
                    );
                }

                self.queue.submit(Some(encoder.finish()));
            }

            offset = end;
        }

        self.queued_frames.clear();
    }

    /// Render ALL frames of an animation into a single horizontal strip texture.
    /// Returns { texture, textureId, width, height, frameWidth, frameHeight, frameCount }.
    /// JS extracts sub-rectangles: frame i is at (i * frameWidth, 0, frameWidth, frameHeight).
    /// One GPU dispatch for all frames — much faster than rendering frames individually.
    /// Supports accessories: pass acc_info as flat [asset_id, slot_id, ...] pairs.
    #[wasm_bindgen(js_name = "renderAnimationStrip")]
    pub fn render_animation_strip(
        &mut self,
        asset_id: u32,
        animation: &str,
        resolution: f32,
        colors: Option<Vec<u32>>,
        acc_info: Option<Vec<u32>>,
    ) -> JsValue {
        let Some(asset) = self.assets.get(&asset_id) else {
            return JsValue::NULL;
        };
        let Some(&anim_idx) = asset.animation_map.get(animation) else {
            return JsValue::NULL;
        };
        let anim = &asset.animations[anim_idx];
        let frame_count = anim.frame_ids.len();
        if frame_count == 0 {
            return JsValue::NULL;
        }

        let player_colors: Option<[u32; 3]> = colors.and_then(|c| {
            if c.len() >= 3 { Some([c[0], c[1], c[2]]) } else { None }
        });

        // Compute canvas size INCLUDING accessories across ALL frames.
        // Accessories may extend into negative coordinates (hats above character),
        // so track min bounds to shift content into the positive canvas area.
        let mut global_min_x = 0.0_f64;
        let mut global_min_y = 0.0_f64;
        let mut global_max_x = 0.0_f64;
        let mut global_max_y = 0.0_f64;
        for i in 0..frame_count {
            let acc_scenes_i = build_accessory_scenes(
                &self.assets, &acc_info, animation, i, resolution,
            );
            let acc_refs_i: Vec<&AccessoryScene> = acc_scenes_i.iter().collect();
            let (bmin_x, bmin_y, bw, bh) = scene_builder::compute_frame_bounds(
                asset, animation, i, resolution, &acc_refs_i,
            );
            global_min_x = global_min_x.min(bmin_x);
            global_min_y = global_min_y.min(bmin_y);
            global_max_x = global_max_x.max(bmin_x + bw);
            global_max_y = global_max_y.max(bmin_y + bh);
        }

        // bounds_offset shifts content so negative accessories are visible
        let bounds_offset = (-global_min_x, -global_min_y);
        let max_w = ((global_max_x - global_min_x).ceil() as u32).max(1);
        let max_h = ((global_max_y - global_min_y).ceil() as u32).max(1);

        // Compute the anchor (registration point) within the tight frame.
        let first_fid = anim.frame_ids[0] as usize;
        let has_base = anim.base_frame_id != u32::MAX;
        let (anchor_x, anchor_y) = if has_base {
            if let Some(base_frame) = asset.frames.get(anim.base_frame_id as usize) {
                let net = scene_builder::compute_net_offset(base_frame, &asset.transforms);
                (net.0 * resolution as f64 + bounds_offset.0,
                 net.1 * resolution as f64 + bounds_offset.1)
            } else {
                (bounds_offset.0, bounds_offset.1)
            }
        } else if let Some(first_frame) = asset.frames.get(first_fid) {
            let net = scene_builder::compute_net_offset(first_frame, &asset.transforms);
            (net.0 * resolution as f64 + bounds_offset.0,
             net.1 * resolution as f64 + bounds_offset.1)
        } else {
            (bounds_offset.0, bounds_offset.1)
        };

        // Arrange frames in a 2D grid to stay within GPU texture limits (16384×16384).
        // E.g., 42 frames at 400px wide → 40 cols × 2 rows = 16000×800 instead of 16800×400.
        let max_cols = (16384 / max_w).max(1) as usize;
        let grid_cols = frame_count.min(max_cols);
        let grid_rows = (frame_count + grid_cols - 1) / grid_cols;
        let strip_w = max_w * grid_cols as u32;
        let strip_h = max_h * grid_rows as u32;

        if strip_w > 16384 || strip_h > 16384 {
            return JsValue::NULL;
        }

        // Build ONE composite Vello scene with all frames in a grid.
        // Single GPU dispatch for the entire animation.
        let mut composite = vello::Scene::new();
        for i in 0..frame_count {
            let col = i % grid_cols;
            let row = i / grid_cols;

            let acc_scenes = build_accessory_scenes(
                &self.assets, &acc_info, animation, i, resolution,
            );
            let acc_refs: Vec<&AccessoryScene> = acc_scenes.iter().collect();

            let frame_scene = scene_builder::build_frame_scene(
                asset, animation, i, player_colors.as_ref(), resolution, &acc_refs, bounds_offset,
            );

            let cx = (col as u32 * max_w) as f64;
            let cy = (row as u32 * max_h) as f64;
            let clip = vello::kurbo::Rect::new(cx, cy, cx + max_w as f64, cy + max_h as f64);
            composite.push_layer(vello::peniko::Fill::default(), vello::peniko::BlendMode::default(), 1.0, vello::kurbo::Affine::IDENTITY, &clip);
            composite.append(&frame_scene, Some(vello::kurbo::Affine::translate((cx, cy))));
            composite.pop_layer();
        }

        // Render entire strip in ONE Vello dispatch directly to STORAGE texture,
        // then copy to the final TEXTURE_BINDING texture.
        let render_target = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vello_strip_rt"),
            size: wgpu::Extent3d { width: strip_w, height: strip_h, depth_or_array_layers: 1 },
            mip_level_count: 1, sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let rt_view = render_target.create_view(&wgpu::TextureViewDescriptor::default());
        let params = vello::RenderParams {
            base_color: Color::TRANSPARENT,
            width: strip_w, height: strip_h,
            antialiasing_method: AaConfig::Area,
        };

        if self.renderer.render_to_texture(&self.device, &self.queue, &composite, &rt_view, &params).is_err() {
            return JsValue::NULL;
        }

        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vello_strip"),
            size: wgpu::Extent3d { width: strip_w, height: strip_h, depth_or_array_layers: 1 },
            mip_level_count: 1, sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });

        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("strip_copy"),
        });
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &render_target, mip_level: 0,
                origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &texture, mip_level: 0,
                origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d { width: strip_w, height: strip_h, depth_or_array_layers: 1 },
        );
        self.queue.submit(Some(encoder.finish()));
        // tmp_textures dropped here — safe, submit already consumed the command buffer

        let gpu_texture = extract_gpu_texture(&texture);
        let tex_id = self.next_texture_id;
        self.next_texture_id += 1;
        self.textures.insert(tex_id, texture);

        let result = js_sys::Object::new();
        let _ = js_sys::Reflect::set(&result, &"texture".into(), &gpu_texture);
        let _ = js_sys::Reflect::set(&result, &"textureId".into(), &JsValue::from(tex_id as f64));
        let _ = js_sys::Reflect::set(&result, &"width".into(), &JsValue::from(strip_w));
        let _ = js_sys::Reflect::set(&result, &"height".into(), &JsValue::from(strip_h));
        let _ = js_sys::Reflect::set(&result, &"frameWidth".into(), &JsValue::from(max_w));
        let _ = js_sys::Reflect::set(&result, &"frameHeight".into(), &JsValue::from(max_h));
        let _ = js_sys::Reflect::set(&result, &"frameCount".into(), &JsValue::from(frame_count as u32));
        let _ = js_sys::Reflect::set(&result, &"gridCols".into(), &JsValue::from(grid_cols as u32));
        // bounds_offset: how much the content was shifted to fit negative accessory positions
        let _ = js_sys::Reflect::set(&result, &"boundsOffsetX".into(), &JsValue::from(bounds_offset.0));
        let _ = js_sys::Reflect::set(&result, &"boundsOffsetY".into(), &JsValue::from(bounds_offset.1));
        // anchor: registration point within the tight frame (in pixels at render resolution)
        let _ = js_sys::Reflect::set(&result, &"anchorX".into(), &JsValue::from(anchor_x));
        let _ = js_sys::Reflect::set(&result, &"anchorY".into(), &JsValue::from(anchor_y));
        result.into()
    }

    /// Render a single zone mask frame. Returns { texture, textureId, width, height }.
    /// Optional acc_info: flat array of pairs [asset_id, slot_id, ...] for accessory occluders.
    #[wasm_bindgen(js_name = "renderZoneMaskFrame")]
    pub fn render_zone_mask_frame(
        &mut self,
        asset_id: u32,
        animation: &str,
        frame_index: u32,
        resolution: f32,
        acc_info: Option<Vec<u32>>,
    ) -> JsValue {
        let Some(asset) = self.assets.get(&asset_id) else { return JsValue::NULL };

        let acc_scenes = build_accessory_scenes(
            &self.assets, &acc_info, animation, frame_index as usize, resolution,
        );
        let acc_refs: Vec<&AccessoryScene> = acc_scenes.iter().collect();
        let frame_scene = scene_builder::build_zone_mask_scene(asset, animation, frame_index as usize, resolution, &acc_refs);
        let dims = self.get_frame_size(asset_id, animation, frame_index, resolution);
        let w = dims[0].max(1);
        let h = dims[1].max(1);

        // Same render-then-copy pattern as renderFrame to isolate Vello's
        // render target from Pixi.js's sampling texture.
        let render_target = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vello_zm_rt"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1, sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let rt_view = render_target.create_view(&wgpu::TextureViewDescriptor::default());
        let params = vello::RenderParams {
            base_color: Color::TRANSPARENT,
            width: w, height: h,
            antialiasing_method: AaConfig::Area,
        };

        if let Err(e) = self.renderer.render_to_texture(&self.device, &self.queue, &frame_scene, &rt_view, &params) {
            console_log!("VelloRenderer: zone mask frame render error: {e}");
            return JsValue::NULL;
        }

        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vello_zm_out"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1, sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("zm_copy"),
        });
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &render_target, mip_level: 0,
                origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &texture, mip_level: 0,
                origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.queue.submit(Some(encoder.finish()));

        let gpu_texture = extract_gpu_texture(&texture);
        let tex_id = self.next_texture_id;
        self.next_texture_id += 1;
        self.textures.insert(tex_id, texture);

        let result = js_sys::Object::new();
        let _ = js_sys::Reflect::set(&result, &"texture".into(), &gpu_texture);
        let _ = js_sys::Reflect::set(&result, &"textureId".into(), &JsValue::from(tex_id as f64));
        let _ = js_sys::Reflect::set(&result, &"width".into(), &JsValue::from(w));
        let _ = js_sys::Reflect::set(&result, &"height".into(), &JsValue::from(h));
        result.into()
    }

    /// Render a zone mask strip: same layout as renderAnimationStrip but with
    /// zone marker colors (R=zone1, G=zone2, B=zone3) and opaque black for non-zone.
    /// Used alongside the base strip for GPU color replacement without item bleed.
    /// Optional acc_info: flat array of pairs [asset_id, slot_id, ...] for accessory occluders.
    #[wasm_bindgen(js_name = "renderZoneMaskStrip")]
    pub fn render_zone_mask_strip(
        &mut self,
        asset_id: u32,
        animation: &str,
        resolution: f32,
        acc_info: Option<Vec<u32>>,
    ) -> JsValue {
        let Some(asset) = self.assets.get(&asset_id) else { return JsValue::NULL };
        let Some(&anim_idx) = asset.animation_map.get(animation) else { return JsValue::NULL };
        let anim = &asset.animations[anim_idx];
        let frame_count = anim.frame_ids.len();
        if frame_count == 0 { return JsValue::NULL; }

        let mut max_w: u32 = 1;
        let mut max_h: u32 = 1;
        for i in 0..frame_count {
            let dims = self.get_frame_size(asset_id, animation, i as u32, resolution);
            max_w = max_w.max(dims[0]);
            max_h = max_h.max(dims[1]);
        }

        let strip_w = max_w * frame_count as u32;
        let strip_h = max_h;
        if strip_w > 8192 { return JsValue::NULL; }

        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vello_zone_mask"),
            size: wgpu::Extent3d { width: strip_w, height: strip_h, depth_or_array_layers: 1 },
            mip_level_count: 1, sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });

        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("zone_mask_copy"),
        });
        let mut tmp_textures: Vec<wgpu::Texture> = Vec::with_capacity(frame_count);

        for i in 0..frame_count {
            let acc_scenes = build_accessory_scenes(&self.assets, &acc_info, animation, i, resolution);
            let acc_refs: Vec<&AccessoryScene> = acc_scenes.iter().collect();
            let frame_scene = scene_builder::build_zone_mask_scene(asset, animation, i, resolution, &acc_refs);
            let dims = self.get_frame_size(asset_id, animation, i as u32, resolution);
            let fw = dims[0].max(1);
            let fh = dims[1].max(1);

            let tmp = self.device.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d { width: fw, height: fh, depth_or_array_layers: 1 },
                mip_level_count: 1, sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let tmp_view = tmp.create_view(&wgpu::TextureViewDescriptor::default());
            let params = vello::RenderParams {
                base_color: Color::TRANSPARENT,
                width: fw, height: fh,
                antialiasing_method: AaConfig::Area,
            };

            if self.renderer.render_to_texture(&self.device, &self.queue, &frame_scene, &tmp_view, &params).is_err() {
                continue;
            }

            let copy_w = fw.min(max_w);
            let copy_h = fh.min(max_h);
            encoder.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &tmp, mip_level: 0,
                    origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &texture, mip_level: 0,
                    origin: wgpu::Origin3d { x: i as u32 * max_w, y: 0, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d { width: copy_w, height: copy_h, depth_or_array_layers: 1 },
            );
            tmp_textures.push(tmp);
        }

        self.queue.submit(Some(encoder.finish()));
        drop(tmp_textures);

        let gpu_texture = extract_gpu_texture(&texture);
        let tex_id = self.next_texture_id;
        self.next_texture_id += 1;
        self.textures.insert(tex_id, texture);

        let result = js_sys::Object::new();
        let _ = js_sys::Reflect::set(&result, &"texture".into(), &gpu_texture);
        let _ = js_sys::Reflect::set(&result, &"textureId".into(), &JsValue::from(tex_id as f64));
        let _ = js_sys::Reflect::set(&result, &"width".into(), &JsValue::from(strip_w));
        let _ = js_sys::Reflect::set(&result, &"height".into(), &JsValue::from(strip_h));
        let _ = js_sys::Reflect::set(&result, &"frameWidth".into(), &JsValue::from(max_w));
        let _ = js_sys::Reflect::set(&result, &"frameHeight".into(), &JsValue::from(max_h));
        result.into()
    }

    /// Free a rendered texture by its ID (returned from renderFrame).
    /// The wgpu::Texture is removed from our HashMap and dropped, releasing
    /// the reference to the underlying GPUTexture. We do NOT call tex.destroy()
    /// because Pixi.js may still have pending GPU commands referencing it.
    /// The browser's GC will collect the GPUTexture once all references are gone.
    #[wasm_bindgen(js_name = "freeTexture")]
    pub fn free_texture(&mut self, texture_id: f64) {
        let id = texture_id as u64;
        self.textures.remove(&id);
        // Dropped — releases our reference, does NOT call GPUTexture.destroy()
    }

    /// Free all cached textures.
    #[wasm_bindgen(js_name = "freeAllTextures")]
    pub fn free_all_textures(&mut self) {
        self.textures.clear();
    }


    /// List animation names for an asset.
    #[wasm_bindgen(js_name = "getAnimationNames")]
    pub fn get_animation_names(&self, asset_id: u32) -> Vec<String> {
        let Some(asset) = self.assets.get(&asset_id) else {
            return Vec::new();
        };
        asset.animations.iter().map(|a| a.name.clone()).collect()
    }
}
