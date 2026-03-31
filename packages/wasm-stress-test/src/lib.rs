use std::sync::Arc;

use dofasset_renderer::format::{self, DofAsset};
use dofasset_renderer::scene_builder;
use vello::kurbo::Affine;
use vello::peniko::Color;
use vello::{wgpu, AaConfig, Renderer, RendererOptions, Scene};
use wasm_bindgen::prelude::*;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::platform::web::{EventLoopExtWebSys, WindowAttributesExtWebSys};
use winit::window::{Window, WindowId};

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    fn log(s: &str);
}

macro_rules! console_log {
    ($($t:tt)*) => (log(&format!($($t)*)))
}

struct Character {
    asset_idx: usize,
    anim_idx: usize,
    frame_offset: usize,
    colors: [u32; 3],
    x: f64,
    y: f64,
}

fn simple_rng(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

struct GpuState {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: Renderer,
}

struct App {
    assets: Vec<DofAsset>,
    characters: Vec<Character>,
    resolution: f32,
    canvas_id: String,
    gpu: Option<GpuState>,
    window: Option<Arc<Window>>,
    surface: Option<wgpu::Surface<'static>>,
    surface_format: wgpu::TextureFormat,
    frame_count: u64,
    start_time: f64,
    last_fps_time: f64,
    last_fps_count: u64,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let canvas = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.get_element_by_id(&self.canvas_id))
            .and_then(|e| e.dyn_into::<web_sys::HtmlCanvasElement>().ok())
            .expect("Canvas element not found");

        let width = canvas.width();
        let height = canvas.height();

        let attrs = Window::default_attributes()
            .with_canvas(Some(canvas))
            .with_prevent_default(true);

        let window = Arc::new(event_loop.create_window(attrs).unwrap());

        let gpu = self.gpu.as_ref().unwrap();
        let surface = gpu.instance.create_surface(window.clone()).unwrap();

        let caps = surface.get_capabilities(&gpu.adapter);
        let surface_format = caps
            .formats
            .iter()
            .find(|f| **f == wgpu::TextureFormat::Rgba8Unorm)
            .copied()
            .unwrap_or(caps.formats[0]);

        surface.configure(
            &gpu.device,
            &wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format: surface_format,
                width: width.max(1),
                height: height.max(1),
                present_mode: wgpu::PresentMode::AutoNoVsync,
                alpha_mode: caps.alpha_modes[0],
                view_formats: vec![],
                desired_maximum_frame_latency: 2,
            },
        );

        self.surface_format = surface_format;
        self.window = Some(window);
        self.surface = Some(surface);

        console_log!("Rendering {} chars at {}x on {}x{}", self.characters.len(), self.resolution, width, height);
    }

    fn window_event(&mut self, _event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        if let WindowEvent::RedrawRequested = event {
            let window = self.window.as_ref().unwrap();
            let surface = self.surface.as_ref().unwrap();
            let gpu = self.gpu.as_mut().unwrap();

            let size = window.inner_size();
            if size.width == 0 || size.height == 0 {
                return;
            }

            let now = js_sys::Date::now();
            let frame_time = (now - self.start_time) / 1000.0;
            let scale = self.resolution as f64;

            let mut scene = Scene::new();
            for ch in &self.characters {
                let asset = &self.assets[ch.asset_idx];
                let anim = &asset.animations[ch.anim_idx];
                if anim.frame_ids.is_empty() {
                    continue;
                }
                let fps = anim.fps.max(1) as f64;
                let frame_idx = ((frame_time * fps) as usize + ch.frame_offset) % anim.frame_ids.len();
                let char_scene = scene_builder::build_frame_scene(
                    asset, &anim.name, frame_idx, None, self.resolution, &[],
                );
                scene.append(&char_scene, Some(Affine::translate((ch.x * scale, ch.y * scale))));
            }

            let render_tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("vello_target"),
                size: wgpu::Extent3d { width: size.width, height: size.height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let render_view = render_tex.create_view(&wgpu::TextureViewDescriptor::default());

            let params = vello::RenderParams {
                base_color: Color::from_rgba8(30, 30, 30, 255),
                width: size.width,
                height: size.height,
                antialiasing_method: AaConfig::Area,
            };

            if let Err(e) = gpu.renderer.render_to_texture(&gpu.device, &gpu.queue, &scene, &render_view, &params) {
                console_log!("Render error: {e}");
                return;
            }

            let surface_tex = match surface.get_current_texture() {
                Ok(t) => t,
                Err(e) => { console_log!("Surface error: {e}"); return; }
            };
            let surface_view = surface_tex.texture.create_view(&Default::default());
            blit(&gpu.device, &gpu.queue, &render_view, &surface_view, self.surface_format);
            surface_tex.present();

            self.frame_count += 1;
            let elapsed = (now - self.last_fps_time) / 1000.0;
            if elapsed >= 1.0 {
                let fps = (self.frame_count - self.last_fps_count) as f64 / elapsed;
                self.last_fps_count = self.frame_count;
                self.last_fps_time = now;
                if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
                    if let Some(el) = doc.get_element_by_id("stats") {
                        el.set_text_content(Some(&format!(
                            "{fps:.0} FPS | {} chars | frame {}", self.characters.len(), self.frame_count
                        )));
                    }
                }
            }

            window.request_redraw();
        }
    }
}

#[wasm_bindgen]
pub async fn run(asset_data_list: Vec<js_sys::Uint8Array>, char_count: u32, resolution: f32, canvas_id: String) {
    console_error_panic_hook::set_once();
    console_log!("Loading {} asset(s)...", asset_data_list.len());

    let mut assets = Vec::new();
    for data in &asset_data_list {
        let bytes = data.to_vec();
        let asset = format::load(&bytes);
        console_log!("  {} anims, {} frames", asset.animations.len(), asset.frames.len());
        assets.push(asset);
    }

    // Do async GPU init BEFORE the event loop
    console_log!("Initializing WebGPU...");
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::BROWSER_WEBGPU | wgpu::Backends::GL,
        ..Default::default()
    });

    console_log!("Requesting WebGPU adapter...");

    // wgpu's async request_adapter doesn't work well with wasm_bindgen_futures
    // when called from a sync context. Use a JS-level adapter request as fallback.
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            compatible_surface: None,
            ..Default::default()
        })
        .await
        .expect("No WebGPU adapter found. Check chrome://gpu for WebGPU status.");

    console_log!("Adapter: {:?}", adapter.get_info().name);

    let (device, queue) = adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: Some("wasm-stress-test"),
                required_features: wgpu::Features::empty(),
                required_limits: adapter.limits(),
                ..Default::default()
            },
            None,
        )
        .await
        .expect("No device");

    let renderer = Renderer::new(
        &device,
        RendererOptions {
            use_cpu: false,
            antialiasing_support: vello::AaSupport::area_only(),
            num_init_threads: std::num::NonZeroUsize::new(1),
            pipeline_cache: None,
        },
    )
    .expect("Renderer failed");

    console_log!("GPU ready.");

    // Generate characters
    let mut seed: u64 = 0xDEADBEEF_CAFEBABE;
    let cols = (char_count as f64).sqrt().ceil() as usize;
    let mut characters = Vec::with_capacity(char_count as usize);
    for i in 0..char_count as usize {
        let asset_idx = (simple_rng(&mut seed) as usize) % assets.len();
        let anim_count = assets[asset_idx].animations.len();
        let col = i % cols;
        let row = i / cols;
        characters.push(Character {
            asset_idx,
            anim_idx: (simple_rng(&mut seed) as usize) % anim_count,
            frame_offset: (simple_rng(&mut seed) as usize) % 1000,
            colors: [
                (simple_rng(&mut seed) as u32) & 0xFFFFFF,
                (simple_rng(&mut seed) as u32) & 0xFFFFFF,
                (simple_rng(&mut seed) as u32) & 0xFFFFFF,
            ],
            x: col as f64 * 70.0 + 40.0,
            y: row as f64 * 80.0 + 60.0,
        });
    }

    let now = js_sys::Date::now();
    let app = App {
        assets,
        characters,
        resolution,
        canvas_id,
        gpu: Some(GpuState { instance, adapter, device, queue, renderer }),
        window: None,
        surface: None,
        surface_format: wgpu::TextureFormat::Bgra8Unorm,
        frame_count: 0,
        start_time: now,
        last_fps_time: now,
        last_fps_count: 0,
    };

    let event_loop = EventLoop::new().unwrap();
    event_loop.spawn_app(app);
}

fn blit(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    src_view: &wgpu::TextureView,
    dst_view: &wgpu::TextureView,
    format: wgpu::TextureFormat,
) {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("blit"),
        source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(
            r#"
            @group(0) @binding(0) var src: texture_2d<f32>;
            @group(0) @binding(1) var samp: sampler;
            struct VOut { @builtin(position) pos: vec4f, @location(0) uv: vec2f };
            @vertex fn vs(@builtin(vertex_index) i: u32) -> VOut {
                let uv = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
                return VOut(vec4f(uv * 2.0 - 1.0, 0.0, 1.0), vec2f(uv.x, 1.0 - uv.y));
            }
            @fragment fn fs(v: VOut) -> @location(0) vec4f {
                return textureSample(src, samp, v.uv);
            }
            "#,
        )),
    });
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: None,
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0, visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2, multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1, visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        mag_filter: wgpu::FilterMode::Nearest, min_filter: wgpu::FilterMode::Nearest, ..Default::default()
    });
    let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None, layout: &bgl,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(src_view) },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) },
        ],
    });
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None, bind_group_layouts: &[&bgl], push_constant_ranges: &[],
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: None, layout: Some(&pl),
        vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs"), buffers: &[], compilation_options: Default::default() },
        fragment: Some(wgpu::FragmentState {
            module: &shader, entry_point: Some("fs"),
            targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, ..Default::default() },
        depth_stencil: None, multisample: Default::default(), multiview: None, cache: None,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: dst_view, resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
            })],
            ..Default::default()
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.draw(0..3, 0..1);
    }
    queue.submit(Some(encoder.finish()));
}
