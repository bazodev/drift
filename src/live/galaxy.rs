//! The GPU side of the wallpaper: builds 176 400 stars of the galaxy, and while a clip plays 129 600
//! grains of its picture, in a compute pass, draws them into a high dynamic range image and gives
//! them real glow before they reach the screen.

use super::{
    palette::Palette,
    story::{Pose, WORD_FLOATS},
    words::{STARS_PER_WORD, WORD_SLOTS},
};

pub const COVER_SIZE: u32 = 512;
/// One picture of a song's clip, as the page hands it over: a small 16:9 JPEG, unpacked to RGB here.
pub const CLIP_WIDTH: u32 = 384;
pub const CLIP_HEIGHT: u32 = 216;
/// Bytes in one row of a clip picture as RGBA.
const CLIP_ROW: usize = (CLIP_WIDTH * 4) as usize;
const GRID: u32 = 420;
const GALAXY_STARS: u32 = GRID * GRID;
/// The clip's picture has stars of its own, this many across and down, kept after the galaxy's.
const SCREEN_W: u32 = 480;
const SCREEN_H: u32 = 270;
const GRAINS: u32 = SCREEN_W * SCREEN_H;
const STAR_COUNT: u32 = GALAXY_STARS + GRAINS;
const _: () = assert!((WORD_SLOTS * STARS_PER_WORD) as u32 <= GALAXY_STARS);
/// Seconds over which one picture of the clip blends into the next: about the time between two of them.
const CLIP_BLEND: f32 = 0.035;
/// Dark scenes are lifted until the brightest tenth of the picture reaches this much light, by at most
/// this much. How quickly the lift follows the scenes, and how quickly it settles after a cut.
const EXPOSURE_TARGET: f32 = 0.45;
const EXPOSURE_MAX: f32 = 4.;
const EXPOSURE_SPEED: f32 = 1.4;
const EXPOSURE_CUT_SPEED: f32 = 7.;
/// How much of the picture must change between two frames to count as a cut, how soon another cut
/// may burst after one, and how quickly a grain stirred by movement calms down.
const CUT_CHANGE: f32 = 0.16;
const CUT_GAP: f32 = 1.5;
/// When this much of the whole picture changes at once (a pan, a flash), movement makes no sparks.
const CALM_CHANGE: (f32, f32) = (0.04, 0.1);
const HEAT_COOL: f32 = 3.5;
/// While a clip plays the camera sways a little, so the picture's depth shows. On the chorus it swings.
const SWAY_REST: f32 = 0.035;
const SWAY_CHORUS: f32 = 0.2;

/// How much of the picture changed between two frames, from their brightness alone. A cut scores high.
fn picture_change(before: &[u8], after: &[u8]) -> f32 {
    if before.is_empty() || before.len() != after.len() {
        return 0.;
    }
    let total: u64 = before.iter().zip(after).map(|(old, new)| u64::from(old.abs_diff(*new))).sum();
    total as f32 / (before.len() as f32 * 255.)
}

/// The brightness of every other pixel in every other row of a clip picture (RGBA): enough to tell cuts
/// and dark scenes, for a quarter of the work.
fn brightness_of(rgba: &[u8]) -> Vec<u8> {
    rgba.as_chunks::<CLIP_ROW>()
        .0
        .iter()
        .step_by(2)
        .flat_map(|row| row.as_chunks::<4>().0.iter().step_by(2))
        .map(|&[r, g, b, _]| ((u32::from(r) * 77 + u32::from(g) * 150 + u32::from(b) * 29) >> 8) as u8)
        .collect()
}

/// How much a scene (RGBA) must be lifted for its brightest tenth to reach the target. Bright scenes are
/// left alone. A pixel counts by its strongest channel, so a deep red or blue is not mistaken for darkness.
fn exposure_for(rgba: &[u8]) -> f32 {
    let mut counts = [0usize; 256];
    let mut total = 0;
    for row in rgba.as_chunks::<CLIP_ROW>().0.iter().step_by(2) {
        for &[r, g, b, _] in row.as_chunks::<4>().0.iter().step_by(2) {
            counts[usize::from(r.max(g).max(b))] += 1;
            total += 1;
        }
    }
    if total == 0 {
        return 1.;
    }
    let wanted = (total / 10).max(1);
    let mut seen = 0;
    let level = (0..256).rev().find(|&value| {
        seen += counts[value];
        seen >= wanted
    });
    let Some(level) = level else { return 1. };
    // The brightness is sRGB-encoded; the picture's light is linear.
    let light = to_linear(level as f32 / 255.);
    (EXPOSURE_TARGET / light.max(0.02)).clamp(1., EXPOSURE_MAX)
}

/// One picture of the clip, unpacked and measured on its own thread, so the render thread only uploads it.
pub struct ClipPicture {
    rgba: Vec<u8>,
    brightness: Vec<u8>,
    exposure: f32,
}

impl ClipPicture {
    /// Unpacks a picture as the page sends it: a JPEG of the clip's size. Anything else is not taken.
    pub fn from_jpeg(jpeg: &[u8]) -> Option<Self> {
        let picture = image::load_from_memory_with_format(jpeg, image::ImageFormat::Jpeg).ok()?;
        if picture.width() != CLIP_WIDTH || picture.height() != CLIP_HEIGHT {
            return None;
        }
        Some(Self::from_rgba(picture.into_rgba8().into_raw()))
    }

    /// A picture given as RGB of the clip's size, as tests make them.
    #[cfg(test)]
    pub fn from_rgb(rgb: &[u8]) -> Option<Self> {
        if rgb.len() != (3 * CLIP_WIDTH * CLIP_HEIGHT) as usize {
            return None;
        }
        Some(Self::from_rgba(rgb.as_chunks::<3>().0.iter().flat_map(|&[r, g, b]| [r, g, b, 255]).collect()))
    }

    fn from_rgba(rgba: Vec<u8>) -> Self {
        let brightness = brightness_of(&rgba);
        let exposure = exposure_for(&rgba);
        Self { rgba, brightness, exposure }
    }

    /// The colors of the scene, from a share of its pixels.
    pub fn palette(&self) -> Palette {
        super::palette::from_pixels(self.rgba.as_chunks::<4>().0.iter().step_by(5).map(|&[r, g, b, _]| [r, g, b]))
    }
}

/// Stars to build and draw this frame: the clip's own grains only while the clip is on stage.
fn star_count(pose: &Pose) -> u32 {
    if pose.clip > 0. { STAR_COUNT } else { GALAXY_STARS }
}
const STAR_BYTES: u64 = 32;
/// Three seconds of the voice, which the scene spreads outward as ripples.
const WAVE: usize = 180;
const WAVE_RATE: f32 = 60.;
const PARAM_FLOATS: usize = 48;
const WORKGROUP: u32 = 128;
const BLOOM_LEVELS: usize = 5;
const HDR: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

const COMMON: &str = include_str!("common.wgsl");
const BUILD: &str = include_str!("build.wgsl");
const STARS: &str = include_str!("stars.wgsl");
const SKY: &str = include_str!("sky.wgsl");
const FILTER: &str = include_str!("filter.wgsl");
const POST: &str = include_str!("post.wgsl");

/// Takes light away: what is drawn darkens what is already there, never below black. The scene's alpha,
/// one everywhere else, keeps how much was taken, so the glow added at the very end is cut out as well.
const DARKEN: wgpu::BlendState = wgpu::BlendState {
    color: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::Zero,
        dst_factor: wgpu::BlendFactor::OneMinusSrc,
        operation: wgpu::BlendOperation::Add,
    },
    alpha: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::Zero,
        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
        operation: wgpu::BlendOperation::Add,
    },
};

const ADD: wgpu::BlendState = wgpu::BlendState {
    color: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::One,
        operation: wgpu::BlendOperation::Add,
    },
    alpha: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::One,
        operation: wgpu::BlendOperation::Add,
    },
};

#[derive(Clone)]
pub struct Cover {
    pub rgba: Vec<u8>,
    pub palette: Palette,
}

/// What differs between screens.
#[derive(Clone, Copy, Debug)]
pub struct Screen {
    pub look: (f32, f32),
    pub headroom: f32,
}

fn approach(current: f32, target: f32, speed: f32, dt: f32) -> f32 {
    current + (target - current) * (1. - (-speed * dt).exp())
}

fn as_bytes<T: Copy>(values: &[T]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(values.as_ptr().cast(), std::mem::size_of_val(values)) }
}

fn to_linear(value: f32) -> f32 {
    if value <= 0.04045 { value / 12.92 } else { ((value + 0.055) / 1.055).powf(2.4) }
}

fn linear_rgb(color: gpui::Rgba) -> [f32; 3] {
    [to_linear(color.r), to_linear(color.g), to_linear(color.b)]
}

fn uniform_entry(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn storage_entry(binding: u32, visibility: wgpu::ShaderStages, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn texture_entry(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn sampler_entry(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

fn storage_buffer(device: &wgpu::Device, label: &'static str, size: u64, writable_from_cpu: bool) -> wgpu::Buffer {
    let extra = if writable_from_cpu { wgpu::BufferUsages::COPY_DST } else { wgpu::BufferUsages::empty() };
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage: wgpu::BufferUsages::STORAGE | extra,
        mapped_at_creation: false,
    })
}

fn uniform_buffer(device: &wgpu::Device, label: &'static str) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: (PARAM_FLOATS * 4) as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn cover_texture(device: &wgpu::Device) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("cover"),
        size: wgpu::Extent3d { width: COVER_SIZE, height: COVER_SIZE, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn clip_texture(device: &wgpu::Device) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("clip"),
        size: wgpu::Extent3d { width: CLIP_WIDTH, height: CLIP_HEIGHT, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

fn light_buffer(device: &wgpu::Device, width: u32, height: u32) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("light"),
            size: wgpu::Extent3d { width: width.max(1), height: height.max(1), depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: HDR,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
        .create_view(&Default::default())
}

struct Draw<'a> {
    label: &'static str,
    layout: &'a wgpu::BindGroupLayout,
    module: &'a wgpu::ShaderModule,
    vertex: &'static str,
    fragment: &'static str,
    format: wgpu::TextureFormat,
    blend: Option<wgpu::BlendState>,
    topology: wgpu::PrimitiveTopology,
}

fn draw_pipeline(device: &wgpu::Device, spec: Draw<'_>) -> wgpu::RenderPipeline {
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(spec.label),
        bind_group_layouts: &[Some(spec.layout)],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(spec.label),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: spec.module,
            entry_point: Some(spec.vertex),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState { topology: spec.topology, ..Default::default() },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: spec.module,
            entry_point: Some(spec.fragment),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: spec.format,
                blend: spec.blend,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

fn render_pass<'a>(
    encoder: &'a mut wgpu::CommandEncoder,
    label: &'static str,
    target: &wgpu::TextureView,
    load: wgpu::LoadOp<wgpu::Color>,
) -> wgpu::RenderPass<'a> {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(label),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: target,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations { load, store: wgpu::StoreOp::Store },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    })
}

pub struct Galaxy {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    build: wgpu::ComputePipeline,
    /// Settles how much each word is cut out of the picture, before the stars are placed.
    settle: wgpu::ComputePipeline,
    build_groups: [wgpu::BindGroup; 2],
    build_params: wgpu::Buffer,
    sky: wgpu::RenderPipeline,
    stars: wgpu::RenderPipeline,
    /// Draws the cut-out letters of words over a bright picture, taking light away.
    cut: wgpu::RenderPipeline,
    bright: wgpu::RenderPipeline,
    down: wgpu::RenderPipeline,
    up: wgpu::RenderPipeline,
    composite: wgpu::RenderPipeline,
    card: wgpu::RenderPipeline,
    sky_layout: wgpu::BindGroupLayout,
    star_layout: wgpu::BindGroupLayout,
    filter_layout: wgpu::BindGroupLayout,
    post_layout: wgpu::BindGroupLayout,
    star_buffer: wgpu::Buffer,
    wave_buffer: wgpu::Buffer,
    glyph_buffer: wgpu::Buffer,
    word_buffer: wgpu::Buffer,
    textures: [wgpu::Texture; 2],
    cover_views: [wgpu::TextureView; 2],
    /// The clip's previous and newest frames. Stars blend from one to the other.
    clip_textures: [wgpu::Texture; 2],
    /// The last picture before a cut, which bursts while the new one gathers.
    clip_shard: wgpu::Texture,
    clip_frames: u64,
    clip_at: f32,
    clip_brightness: Vec<u8>,
    cut_at: f32,
    cut_strength: f32,
    /// How much the clip's picture is lifted now, and how much its current scene asks for.
    exposure: f32,
    exposure_target: f32,
    /// How much of the whole picture changed with the newest frame.
    picture_change: f32,
    /// The cover's colors, which the clip's colors replace while it plays.
    cover_a: [f32; 3],
    cover_b: [f32; 3],
    clip_colored: bool,
    last_dt: f32,
    sampler: wgpu::Sampler,
    wave: Vec<f32>,
    wave_clock: f32,
    newest: usize,
    has_cover: bool,
    time: f32,
    voice: f32,
    color_a: [f32; 3],
    color_b: [f32; 3],
    target_a: [f32; 3],
    target_b: [f32; 3],
}

/// Everything needed to draw on one screen.
pub struct View {
    width: u32,
    height: u32,
    params: wgpu::Buffer,
    light: wgpu::TextureView,
    bloom: Vec<wgpu::TextureView>,
    sky_group: wgpu::BindGroup,
    star_group: wgpu::BindGroup,
    down_groups: Vec<wgpu::BindGroup>,
    up_groups: Vec<wgpu::BindGroup>,
    composite_group: wgpu::BindGroup,
    card_groups: [wgpu::BindGroup; 2],
}

impl Galaxy {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue, output: wgpu::TextureFormat) -> Self {
        let prelude = format!(
            "const GRID: u32 = {GRID}u;\nconst WAVE: u32 = {WAVE}u;\nconst WORD_SLOTS: u32 = {WORD_SLOTS}u;\nconst STARS_PER_WORD: u32 = {STARS_PER_WORD}u;\nconst SCREEN_W: u32 = {SCREEN_W}u;\nconst SCREEN_H: u32 = {SCREEN_H}u;\n{COMMON}\n"
        );
        let module = |label: &'static str, body: &str| {
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(format!("{prelude}{body}").into()),
            })
        };
        let build_module = module("build", BUILD);
        let star_module = module("stars", STARS);
        let sky_module = module("sky", SKY);
        let post_module = module("post", POST);
        let filter_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("filter"),
            source: wgpu::ShaderSource::Wgsl(FILTER.into()),
        });

        let star_buffer = storage_buffer(&device, "stars", u64::from(STAR_COUNT) * STAR_BYTES, false);
        let wave = vec![0.; WAVE];
        let wave_buffer = storage_buffer(&device, "wave", (WAVE * 4) as u64, true);
        queue.write_buffer(&wave_buffer, 0, as_bytes(&wave));
        let glyph_buffer = storage_buffer(&device, "glyphs", (WORD_SLOTS * STARS_PER_WORD * 8) as u64, true);
        let word_buffer = storage_buffer(&device, "words", (WORD_SLOTS * WORD_FLOATS * 4) as u64, true);
        let build_params = uniform_buffer(&device, "story");
        let textures = [cover_texture(&device), cover_texture(&device)];
        let cover_views = [textures[0].create_view(&Default::default()), textures[1].create_view(&Default::default())];
        let clip_textures = [clip_texture(&device), clip_texture(&device)];
        let clip_views =
            [clip_textures[0].create_view(&Default::default()), clip_textures[1].create_view(&Default::default())];
        let clip_shard = clip_texture(&device);
        let shard_view = clip_shard.create_view(&Default::default());
        let heat_buffer = storage_buffer(&device, "heat", u64::from(GRAINS) * 4, false);
        let word_cut_buffer = storage_buffer(&device, "word cuts", (WORD_SLOTS * 4) as u64, false);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let compute = wgpu::ShaderStages::COMPUTE;
        let build_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("build"),
            entries: &[
                uniform_entry(0, compute),
                storage_entry(1, compute, false),
                texture_entry(2, compute),
                texture_entry(3, compute),
                sampler_entry(4, compute),
                storage_entry(5, compute, true),
                storage_entry(6, compute, true),
                storage_entry(7, compute, true),
                texture_entry(8, compute),
                texture_entry(9, compute),
                storage_entry(10, compute, false),
                texture_entry(11, compute),
                storage_entry(12, compute, false),
            ],
        });
        let build_group = |old: usize, new: usize| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("build"),
                layout: &build_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: build_params.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: star_buffer.as_entire_binding() },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&cover_views[old]),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(&cover_views[new]),
                    },
                    wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::Sampler(&sampler) },
                    wgpu::BindGroupEntry { binding: 5, resource: wave_buffer.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 6, resource: glyph_buffer.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 7, resource: word_buffer.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 8, resource: wgpu::BindingResource::TextureView(&clip_views[0]) },
                    wgpu::BindGroupEntry { binding: 9, resource: wgpu::BindingResource::TextureView(&clip_views[1]) },
                    wgpu::BindGroupEntry { binding: 10, resource: heat_buffer.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 11, resource: wgpu::BindingResource::TextureView(&shard_view) },
                    wgpu::BindGroupEntry { binding: 12, resource: word_cut_buffer.as_entire_binding() },
                ],
            })
        };
        let build_groups = [build_group(1, 0), build_group(0, 1)];
        let build_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("build"),
            bind_group_layouts: &[Some(&build_layout)],
            immediate_size: 0,
        });
        let build = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("build"),
            layout: Some(&build_pipeline_layout),
            module: &build_module,
            entry_point: Some("build"),
            compilation_options: Default::default(),
            cache: None,
        });
        let settle = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("settle words"),
            layout: Some(&build_pipeline_layout),
            module: &build_module,
            entry_point: Some("settle_words"),
            compilation_options: Default::default(),
            cache: None,
        });

        let fragment = wgpu::ShaderStages::FRAGMENT;
        let vertex = wgpu::ShaderStages::VERTEX;
        let sky_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sky"),
            entries: &[uniform_entry(0, fragment), storage_entry(1, fragment, true)],
        });
        let star_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("stars"),
            entries: &[uniform_entry(0, vertex), storage_entry(1, vertex, true)],
        });
        let filter_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("filter"),
            entries: &[texture_entry(0, fragment), sampler_entry(1, fragment)],
        });
        let post_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("post"),
            entries: &[
                uniform_entry(0, vertex | fragment),
                texture_entry(1, fragment),
                texture_entry(2, fragment),
                sampler_entry(3, fragment),
            ],
        });

        let full = wgpu::PrimitiveTopology::TriangleList;
        let strip = wgpu::PrimitiveTopology::TriangleStrip;
        let sky = draw_pipeline(
            &device,
            Draw {
                label: "sky",
                layout: &sky_layout,
                module: &sky_module,
                vertex: "vs_full",
                fragment: "fs_sky",
                format: HDR,
                blend: None,
                topology: full,
            },
        );
        let stars = draw_pipeline(
            &device,
            Draw {
                label: "stars",
                layout: &star_layout,
                module: &star_module,
                vertex: "vs_star",
                fragment: "fs_star",
                format: HDR,
                blend: Some(ADD),
                topology: strip,
            },
        );
        let cut = draw_pipeline(
            &device,
            Draw {
                label: "cut",
                layout: &star_layout,
                module: &star_module,
                vertex: "vs_cut",
                fragment: "fs_cut",
                format: HDR,
                blend: Some(DARKEN),
                topology: strip,
            },
        );
        let filter = |label, entry, blend| {
            draw_pipeline(
                &device,
                Draw {
                    label,
                    layout: &filter_layout,
                    module: &filter_module,
                    vertex: "vs_full",
                    fragment: entry,
                    format: HDR,
                    blend,
                    topology: full,
                },
            )
        };
        let bright = filter("bright", "fs_bright", None);
        let down = filter("down", "fs_down", None);
        let up = filter("up", "fs_up", Some(ADD));
        let composite = draw_pipeline(
            &device,
            Draw {
                label: "composite",
                layout: &post_layout,
                module: &post_module,
                vertex: "vs_full",
                fragment: "fs_composite",
                format: output,
                blend: None,
                topology: full,
            },
        );
        let card = draw_pipeline(
            &device,
            Draw {
                label: "card",
                layout: &post_layout,
                module: &post_module,
                vertex: "vs_card",
                fragment: "fs_card",
                format: output,
                blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                topology: strip,
            },
        );

        let palette = Palette::default();
        let (color_a, color_b) = (linear_rgb(palette.accent), linear_rgb(palette.second));
        Self {
            device,
            queue,
            build,
            settle,
            build_groups,
            build_params,
            sky,
            stars,
            cut,
            bright,
            down,
            up,
            composite,
            card,
            sky_layout,
            star_layout,
            filter_layout,
            post_layout,
            star_buffer,
            wave_buffer,
            glyph_buffer,
            word_buffer,
            textures,
            cover_views,
            clip_textures,
            clip_shard,
            clip_frames: 0,
            clip_at: 0.,
            clip_brightness: Vec::new(),
            cut_at: -100.,
            cut_strength: 0.,
            exposure: 1.,
            exposure_target: 1.,
            picture_change: 0.,
            cover_a: color_a,
            cover_b: color_b,
            clip_colored: false,
            last_dt: 1. / 60.,
            sampler,
            wave,
            wave_clock: 0.,
            newest: 0,
            has_cover: false,
            time: 0.,
            voice: 0.,
            color_a,
            color_b,
            target_a: color_a,
            target_b: color_b,
        }
    }

    pub fn view(&self, width: u32, height: u32) -> View {
        let device = &self.device;
        let params = uniform_buffer(device, "screen");
        let light = light_buffer(device, width, height);
        let bloom: Vec<wgpu::TextureView> = (1..=BLOOM_LEVELS)
            .map(|level| light_buffer(device, (width >> level).max(2), (height >> level).max(2)))
            .collect();
        let filter_group = |source: &wgpu::TextureView| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("filter"),
                layout: &self.filter_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(source) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                ],
            })
        };
        let down_groups =
            (0..BLOOM_LEVELS).map(|level| filter_group(if level == 0 { &light } else { &bloom[level - 1] })).collect();
        let up_groups = (0..BLOOM_LEVELS - 1).map(|level| filter_group(&bloom[level + 1])).collect();
        let post_group = |first: &wgpu::TextureView, second: &wgpu::TextureView| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("post"),
                layout: &self.post_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: params.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(first) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(second) },
                    wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                ],
            })
        };
        let composite_group = post_group(&light, &bloom[0]);
        let card_groups = [
            post_group(&self.cover_views[1], &self.cover_views[0]),
            post_group(&self.cover_views[0], &self.cover_views[1]),
        ];
        let sky_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sky"),
            layout: &self.sky_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: params.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: self.wave_buffer.as_entire_binding() },
            ],
        });
        let star_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("stars"),
            layout: &self.star_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: params.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: self.star_buffer.as_entire_binding() },
            ],
        });
        View {
            width,
            height,
            params,
            light,
            bloom,
            sky_group,
            star_group,
            down_groups,
            up_groups,
            composite_group,
            card_groups,
        }
    }

    fn write_cover(&self, index: usize, rgba: &[u8]) {
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.textures[index],
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * COVER_SIZE),
                rows_per_image: Some(COVER_SIZE),
            },
            wgpu::Extent3d { width: COVER_SIZE, height: COVER_SIZE, depth_or_array_layers: 1 },
        );
    }

    pub fn set_cover(&mut self, cover: &Cover) {
        if cover.rgba.len() != (4 * COVER_SIZE * COVER_SIZE) as usize {
            return;
        }
        if self.has_cover {
            self.newest = 1 - self.newest;
            self.write_cover(self.newest, &cover.rgba);
        } else {
            self.write_cover(0, &cover.rgba);
            self.write_cover(1, &cover.rgba);
            self.has_cover = true;
        }
        self.cover_a = linear_rgb(cover.palette.accent);
        self.cover_b = linear_rgb(cover.palette.second);
        if !self.clip_colored {
            self.target_a = self.cover_a;
            self.target_b = self.cover_b;
        }
    }

    /// While a clip plays, the galaxy takes the colors of its current scene.
    pub fn set_clip_palette(&mut self, palette: &Palette) {
        self.target_a = linear_rgb(palette.accent);
        self.target_b = linear_rgb(palette.second);
        self.clip_colored = true;
    }

    /// The clip is over: the galaxy goes back to the colors of the cover.
    pub fn end_clip_colors(&mut self) {
        if self.clip_colored {
            self.clip_colored = false;
            self.target_a = self.cover_a;
            self.target_b = self.cover_b;
        }
    }

    /// Shows the clip's newest frame, given as RGB. The previous frame stays a moment longer,
    /// so the grains blend from frame to frame instead of jumping.
    pub fn set_clip_picture(&mut self, picture: &ClipPicture) {
        let rgba = &picture.rgba;
        let extent = wgpu::Extent3d { width: CLIP_WIDTH, height: CLIP_HEIGHT, depth_or_array_layers: 1 };
        let layout = wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4 * CLIP_WIDTH),
            rows_per_image: Some(CLIP_HEIGHT),
        };
        // A cut keeps the last picture before it, which bursts while the new one gathers.
        let change = picture_change(&self.clip_brightness, &picture.brightness);
        let cut = self.clip_frames > 0 && change > CUT_CHANGE && self.time - self.cut_at > CUT_GAP;
        self.exposure_target = picture.exposure;
        if self.clip_frames == 0 {
            self.exposure = self.exposure_target;
        }
        self.clip_brightness.clone_from(&picture.brightness);
        self.picture_change = change;
        #[cfg(debug_assertions)]
        if cut {
            eprintln!("clip: cut, {:.0}% of the picture changed", change * 100.);
        }
        if self.clip_frames == 0 {
            self.queue.write_texture(self.clip_textures[0].as_image_copy(), rgba, layout, extent);
        } else {
            let mut encoder = self.device.create_command_encoder(&Default::default());
            if cut {
                encoder.copy_texture_to_texture(
                    self.clip_textures[1].as_image_copy(),
                    self.clip_shard.as_image_copy(),
                    extent,
                );
                self.cut_at = self.time;
                self.cut_strength = ((change - CUT_CHANGE) / 0.15).clamp(0.4, 1.);
            }
            encoder.copy_texture_to_texture(
                self.clip_textures[1].as_image_copy(),
                self.clip_textures[0].as_image_copy(),
                extent,
            );
            self.queue.submit([encoder.finish()]);
        }
        self.queue.write_texture(self.clip_textures[1].as_image_copy(), rgba, layout, extent);
        self.clip_frames += 1;
        self.clip_at = self.time;
    }

    /// Tests hand over pictures as RGB.
    #[cfg(test)]
    pub fn set_clip_frame(&mut self, rgb: &[u8]) {
        if let Some(picture) = ClipPicture::from_rgb(rgb) {
            self.set_clip_picture(&picture);
        }
    }

    /// Gives a word place the shape of the word that is about to appear there.
    pub fn write_word(&self, slot: usize, points: &[[f32; 2]]) {
        if !points.is_empty() {
            let offset = (slot * STARS_PER_WORD * 8) as u64;
            self.queue.write_buffer(&self.glyph_buffer, offset, as_bytes(&points[..points.len().min(STARS_PER_WORD)]));
        }
    }

    pub fn advance(&mut self, dt: f32, voice: f32) {
        self.time += dt;
        self.last_dt = dt;
        // Rings of light come from whole words, not single syllables, so the core never flickers.
        let speed = if voice > self.voice { 9. } else { 4. };
        self.voice = approach(self.voice, voice, speed, dt);
        self.wave_clock += dt * WAVE_RATE;
        while self.wave_clock >= 1. {
            self.wave_clock -= 1.;
            self.wave.copy_within(0..WAVE - 1, 1);
            self.wave[0] = self.voice;
        }
        // The lift follows the scenes gently, and settles quickly into a new one after a cut.
        let lift_speed = if self.time - self.cut_at < 0.4 { EXPOSURE_CUT_SPEED } else { EXPOSURE_SPEED };
        self.exposure = approach(self.exposure, self.exposure_target, lift_speed, dt);
        for channel in 0..3 {
            self.color_a[channel] = approach(self.color_a[channel], self.target_a[channel], 2.8, dt);
            self.color_b[channel] = approach(self.color_b[channel], self.target_b[channel], 2.8, dt);
        }
    }

    fn params(&self, pose: &Pose, size: (u32, u32), screen: Screen) -> [f32; PARAM_FLOATS] {
        let (a, b, shape) = (self.color_a, self.color_b, pose.shape);
        let (width, height) = (size.0.max(1) as f32, size.1.max(1) as f32);
        // The clip's depth only shows when the eye moves: the camera sways, and swings on the chorus.
        let sway = pose.clip * (SWAY_REST + SWAY_CHORUS * pose.chorus);
        let look =
            (screen.look.0 + sway * (self.time * 0.5).sin(), screen.look.1 + sway * 0.4 * (self.time * 0.33).sin());
        let since_frame = self.time - self.clip_at;
        let fresh = 1. - ((since_frame - 0.12) / 0.18).clamp(0., 1.);
        let calm = 1. - ((self.picture_change - CALM_CHANGE.0) / (CALM_CHANGE.1 - CALM_CHANGE.0)).clamp(0., 1.);
        let motion_gate = fresh * calm;
        [
            self.time,
            self.voice,
            pose.fade,
            pose.lyric,
            pose.twist,
            pose.tilt,
            pose.roll,
            pose.warp,
            pose.flight,
            pose.speed,
            pose.card,
            pose.collapse,
            pose.energy,
            pose.spin,
            pose.blend,
            pose.breath,
            shape.arms,
            shape.winding,
            shape.hand,
            shape.gather,
            shape.extent,
            shape.falloff,
            shape.thickness,
            pose.chorus,
            width / height,
            2. / height,
            look.0,
            look.1,
            screen.headroom,
            pose.shock,
            pose.clip,
            (since_frame / CLIP_BLEND).clamp(0., 1.),
            self.time - self.cut_at,
            self.cut_strength,
            motion_gate,
            (-self.last_dt * HEAT_COOL).exp(),
            self.exposure,
            self.last_dt,
            0.,
            0.,
            a[0],
            a[1],
            a[2],
            1.,
            b[0],
            b[1],
            b[2],
            1.,
        ]
    }

    /// Places every star for this frame. Screens share the result.
    pub fn build(&self, encoder: &mut wgpu::CommandEncoder, pose: &Pose) {
        let story = self.params(pose, (1, 1), Screen { look: (0., 0.), headroom: 1. });
        self.queue.write_buffer(&self.build_params, 0, as_bytes(&story));
        self.queue.write_buffer(&self.wave_buffer, 0, as_bytes(&self.wave));
        self.queue.write_buffer(&self.word_buffer, 0, as_bytes(&pose.words));
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
        pass.set_bind_group(0, &self.build_groups[self.newest], &[]);
        // Each word first settles how much it is cut out of the picture behind it; then every star is placed.
        pass.set_pipeline(&self.settle);
        pass.dispatch_workgroups(1, 1, 1);
        pass.set_pipeline(&self.build);
        pass.dispatch_workgroups(star_count(pose).div_ceil(WORKGROUP), 1, 1);
    }

    pub fn draw(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        view: &View,
        output: &wgpu::TextureView,
        pose: &Pose,
        screen: Screen,
    ) {
        if pose.fade < 0.002 {
            drop(render_pass(encoder, "clear", output, wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)));
            return;
        }
        let params = self.params(pose, (view.width, view.height), screen);
        self.queue.write_buffer(&view.params, 0, as_bytes(&params));
        {
            let mut pass = render_pass(encoder, "scene", &view.light, wgpu::LoadOp::Clear(wgpu::Color::BLACK));
            pass.set_pipeline(&self.sky);
            pass.set_bind_group(0, &view.sky_group, &[]);
            pass.draw(0..3, 0..1);
            if self.has_cover {
                pass.set_pipeline(&self.stars);
                pass.set_bind_group(0, &view.star_group, &[]);
                pass.draw(0..4, 0..star_count(pose));
                // Words over a bright picture are cut out of it. Their stars are among the galaxy's.
                if pose.clip > 0. {
                    pass.set_pipeline(&self.cut);
                    pass.draw(0..4, 0..GALAXY_STARS);
                }
            }
        }
        for level in 0..BLOOM_LEVELS {
            let mut pass = render_pass(encoder, "glow", &view.bloom[level], wgpu::LoadOp::Clear(wgpu::Color::BLACK));
            pass.set_pipeline(if level == 0 { &self.bright } else { &self.down });
            pass.set_bind_group(0, &view.down_groups[level], &[]);
            pass.draw(0..3, 0..1);
        }
        for level in (0..BLOOM_LEVELS - 1).rev() {
            let mut pass = render_pass(encoder, "glow", &view.bloom[level], wgpu::LoadOp::Load);
            pass.set_pipeline(&self.up);
            pass.set_bind_group(0, &view.up_groups[level], &[]);
            pass.draw(0..3, 0..1);
        }
        let mut pass = render_pass(encoder, "final", output, wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT));
        pass.set_pipeline(&self.composite);
        pass.set_bind_group(0, &view.composite_group, &[]);
        pass.draw(0..3, 0..1);
        if pose.card > 0.002 && self.has_cover {
            pass.set_pipeline(&self.card);
            pass.set_bind_group(0, &view.card_groups[self.newest], &[]);
            pass.draw(0..4, 0..1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        super::{
            glyphs, palette,
            story::{Input, Story},
        },
        *,
    };
    use crate::lyrics::TimedLine;

    const FRAME: f32 = 1. / 60.;

    fn bright_cover(flip: bool) -> Cover {
        let mut rgba = Vec::with_capacity((4 * COVER_SIZE * COVER_SIZE) as usize);
        for y in 0..COVER_SIZE {
            for x in 0..COVER_SIZE {
                let (u, v) = (x as f32 / COVER_SIZE as f32, y as f32 / COVER_SIZE as f32);
                let ring = ((u - 0.5).hypot(v - 0.5) * 22.).sin() * 0.5 + 0.5;
                let (r, g, b) = if flip { (ring, 0.3 + v * 0.6, 1. - u) } else { (1. - v * 0.3, ring * 0.7, u * 0.8) };
                rgba.extend_from_slice(&[(r * 255.) as u8, (g * 255.) as u8, (b * 255.) as u8, 255]);
            }
        }
        let palette = if flip {
            Palette { accent: gpui::rgb(0x35d0ff), second: gpui::rgb(0x8b5cff) }
        } else {
            Palette { accent: gpui::rgb(0xff6f4d), second: gpui::rgb(0xff2d8f) }
        };
        Cover { rgba, palette }
    }

    fn real_cover() -> Option<Cover> {
        let path = std::env::var("DRIFT_PREVIEW_COVER").ok()?;
        let bytes = std::fs::read(path).ok()?;
        let (palette, rgba) = palette::from_image(&bytes, COVER_SIZE)?;
        Some(Cover { rgba, palette })
    }

    /// Plays the story offscreen and photographs chosen moments.
    struct Rig {
        galaxy: Galaxy,
        view: View,
        story: Story,
        texture: wgpu::Texture,
        width: u32,
        height: u32,
        position: f64,
        duration: f64,
        seconds: f32,
        look: (f32, f32),
    }

    impl Rig {
        fn new(width: u32, height: u32) -> Option<Self> {
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
            let adapter =
                futures::executor::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
            let (device, queue) =
                futures::executor::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
            let format = wgpu::TextureFormat::Rgba8UnormSrgb;
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let galaxy = Galaxy::new(device, queue, format);
            let view = galaxy.view(width, height);
            Some(Self {
                galaxy,
                view,
                story: Story::new(),
                texture,
                width,
                height,
                position: 0.,
                duration: 180.,
                seconds: 0.,
                look: (0., 0.),
            })
        }

        fn run(&mut self, seconds: f32, cover: &mut Option<Cover>) {
            for _ in 0..(seconds / FRAME).round() as u32 {
                self.seconds += FRAME;
                self.position += f64::from(FRAME);
                let phrase = (self.seconds * 0.7).sin().abs();
                let syllable = (self.seconds * 8.4).sin() * 0.5 + 0.5;
                let voice = syllable * syllable * (0.35 + 0.65 * phrase);
                if cover.is_some() && self.story.can_swap_cover(0.) {
                    let next = cover.take().unwrap();
                    self.galaxy.set_cover(&next);
                    self.story.cover_swapped();
                }
                let level = 0.08 + 0.1 * (self.seconds * 0.31).sin().abs();
                self.story.advance(
                    FRAME,
                    &Input {
                        active: true,
                        position: self.position,
                        duration: self.duration,
                        level,
                        chorus: true,
                        clip: false,
                    },
                );
                if let Some(request) = self.story.take_request() {
                    let spelled = glyphs::spell(&request.words);
                    self.story.glyphs_ready(request.generation, spelled);
                }
                for (slot, points) in self.story.take_uploads() {
                    self.galaxy.write_word(slot, &points);
                }
                self.galaxy.advance(FRAME, voice);
            }
        }

        fn capture(&self) -> Vec<u8> {
            self.capture_pose(*self.story.pose())
        }

        fn capture_pose(&self, pose: Pose) -> Vec<u8> {
            let (width, height) = (self.width, self.height);
            let row = (4 * width).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
            let buffer = self.galaxy.device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: u64::from(row * height),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let target = self.texture.create_view(&Default::default());
            let mut encoder = self.galaxy.device.create_command_encoder(&Default::default());
            self.galaxy.build(&mut encoder, &pose);
            self.galaxy.draw(&mut encoder, &self.view, &target, &pose, Screen { look: self.look, headroom: 1. });
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(row),
                        rows_per_image: Some(height),
                    },
                },
                wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            );
            self.galaxy.queue.submit([encoder.finish()]);
            buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
            let _ = self.galaxy.device.poll(wgpu::PollType::wait_indefinitely());
            let data = buffer.slice(..).get_mapped_range().unwrap();
            let mut out = Vec::with_capacity((4 * width * height) as usize);
            for line in data.chunks_exact(row as usize) {
                out.extend_from_slice(&line[..(4 * width) as usize]);
            }
            out
        }

        fn save(&self, name: &str) {
            if let Ok(directory) = std::env::var("DRIFT_PREVIEW") {
                let pixels = self.capture();
                image::save_buffer(
                    format!("{directory}/{name}.png"),
                    &pixels,
                    self.width,
                    self.height,
                    image::ColorType::Rgba8,
                )
                .unwrap();
            }
        }
    }

    fn average(pixels: &[u8]) -> f32 {
        let sum: u64 =
            pixels.as_chunks::<4>().0.iter().map(|&[r, g, b, _]| u64::from(r) + u64::from(g) + u64::from(b)).sum();
        sum as f32 / (pixels.len() / 4 * 3) as f32
    }

    /// Clip frames become a screen of stars: red on the left of the picture, blue on the right.
    #[test]
    fn clip_frames_become_a_screen_of_stars() {
        let Some(mut rig) = Rig::new(1280, 800) else { return };
        let mut cover = Some(bright_cover(false));
        rig.story.new_track("clip");
        rig.run(12., &mut cover);
        let mut frame = Vec::with_capacity((3 * CLIP_WIDTH * CLIP_HEIGHT) as usize);
        for _ in 0..CLIP_HEIGHT {
            for x in 0..CLIP_WIDTH {
                frame.extend_from_slice(if x < CLIP_WIDTH / 2 { &[230, 40, 40] } else { &[40, 60, 230] });
            }
        }
        // DRIFT_PREVIEW_CLIP: a raw 160×90 RGB frame to look at instead of the test pattern.
        let preview = std::env::var("DRIFT_PREVIEW_CLIP").ok().and_then(|path| std::fs::read(path).ok());
        let shown = preview.as_deref().unwrap_or(&frame);
        rig.galaxy.set_clip_frame(shown);
        rig.galaxy.set_clip_frame(shown);
        rig.galaxy.advance(0.2, 0.);
        let mut pose = *rig.story.pose();
        pose.words = [[0.; WORD_FLOATS]; WORD_SLOTS];
        pose.clip = 1.;
        let pixels = rig.capture_pose(pose);
        if let Ok(directory) = std::env::var("DRIFT_PREVIEW") {
            image::save_buffer(
                format!("{directory}/clip_screen.png"),
                &pixels,
                rig.width,
                rig.height,
                image::ColorType::Rgba8,
            )
            .unwrap();
        }
        let (width, height) = (rig.width, rig.height);
        // The screen fills the frame; sample the middle of each half of the picture.
        let mean = |from: f32, to: f32| {
            let mut sum = [0u64; 3];
            let mut count = 0u64;
            for y in (height * 2 / 5)..(height * 3 / 5) {
                for x in (width as f32 * from) as u32..(width as f32 * to) as u32 {
                    let at = ((y * width + x) * 4) as usize;
                    for channel in 0..3 {
                        sum[channel] += u64::from(pixels[at + channel]);
                    }
                    count += 1;
                }
            }
            sum.map(|value| value as f32 / count as f32)
        };
        let (left, right) = (mean(0.26, 0.4), mean(0.6, 0.74));
        println!("clip screen: left {left:?}, right {right:?}");
        if preview.is_some() {
            return;
        }
        assert!(left[0] > left[2] * 1.4 && left[0] > 25., "the left of the picture is red: {left:?}");
        assert!(right[2] > right[0] * 1.4 && right[2] > 25., "the right of the picture is blue: {right:?}");
    }

    /// Movement and small changes are not cuts; a new scene is.
    #[test]
    fn tells_a_cut_from_movement() {
        let size = (CLIP_WIDTH * CLIP_HEIGHT) as usize;
        let calm: Vec<u8> = (0..size).map(|index| (index % 200) as u8).collect();
        let mut moved = calm.clone();
        for value in moved.iter_mut().take(size / 12) {
            *value = value.wrapping_add(90);
        }
        let other: Vec<u8> = calm.iter().map(|value| 255 - value).collect();
        assert!(picture_change(&calm, &moved) < CUT_CHANGE, "movement is not a cut: {}", picture_change(&calm, &moved));
        assert!(picture_change(&calm, &other) > CUT_CHANGE, "a new scene is a cut: {}", picture_change(&calm, &other));
        assert_eq!(picture_change(&[], &calm), 0., "the first frame is not a cut");
    }

    /// The page sends pictures as JPEG: they unpack to RGB of the clip's size, and nothing else is taken.
    #[test]
    fn clip_pictures_unpack_from_jpeg() {
        let encode = |width: u32, height: u32| {
            let mut rgb = Vec::with_capacity((width * height * 3) as usize);
            for _ in 0..height {
                for x in 0..width {
                    rgb.extend_from_slice(if x < width / 2 { &[230, 40, 40] } else { &[40, 60, 230] });
                }
            }
            let mut jpeg = Vec::new();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 85)
                .encode(&rgb, width, height, image::ExtendedColorType::Rgb8)
                .unwrap();
            jpeg
        };
        let picture = ClipPicture::from_jpeg(&encode(CLIP_WIDTH, CLIP_HEIGHT)).expect("a picture of the clip's size");
        let rgba = &picture.rgba;
        assert_eq!(rgba.len(), (CLIP_WIDTH * CLIP_HEIGHT * 4) as usize);
        let at = |x: u32| &rgba[((CLIP_HEIGHT / 2 * CLIP_WIDTH + x) * 4) as usize..][..3];
        assert!(at(CLIP_WIDTH / 4)[0] > 180 && at(CLIP_WIDTH / 4)[2] < 90, "left is red: {:?}", at(CLIP_WIDTH / 4));
        assert!(
            at(CLIP_WIDTH * 3 / 4)[2] > 180 && at(CLIP_WIDTH * 3 / 4)[0] < 90,
            "right is blue: {:?}",
            at(CLIP_WIDTH * 3 / 4)
        );
        assert!(ClipPicture::from_jpeg(&encode(192, 108)).is_none(), "a picture of another size is not taken");
        assert!(ClipPicture::from_jpeg(b"not a picture").is_none());
    }

    /// Dark scenes are lifted so they can be seen; bright and deeply colored scenes keep their light.
    #[test]
    fn dark_scenes_are_lifted_and_bright_ones_left_alone() {
        let frame = |pixel: &dyn Fn(usize) -> [u8; 3]| -> Vec<u8> {
            (0..(CLIP_WIDTH * CLIP_HEIGHT) as usize)
                .flat_map(|index| {
                    let [r, g, b] = pixel(index);
                    [r, g, b, 255]
                })
                .collect()
        };
        let bright = frame(&|index| [(index % 256) as u8; 3]);
        let dark = frame(&|index| [(index % 70) as u8; 3]);
        let deep = frame(&|index| {
            if index % (CLIP_WIDTH as usize) < (CLIP_WIDTH / 2) as usize { [230, 40, 40] } else { [40, 60, 230] }
        });
        let black: Vec<u8> = (0..(CLIP_WIDTH * CLIP_HEIGHT) as usize).flat_map(|_| [0, 0, 0, 255]).collect();
        assert_eq!(exposure_for(&bright), 1.);
        assert_eq!(exposure_for(&deep), 1., "deep red and blue are bright, not dark");
        let lifted = exposure_for(&dark);
        assert!(lifted > 1.8 && lifted <= EXPOSURE_MAX, "a dark scene is lifted: {lifted}");
        assert_eq!(exposure_for(&black), EXPOSURE_MAX, "black is lifted as far as allowed, and stays black");
        assert_eq!(exposure_for(&[]), 1.);
    }

    /// Debug only, with DRIFT_PREVIEW set: photographs the clip effects for a human to look at.
    #[test]
    fn clip_effects_preview() {
        let Ok(directory) = std::env::var("DRIFT_PREVIEW") else { return };
        let Some(mut rig) = Rig::new(1280, 800) else { return };
        let mut cover = Some(bright_cover(false));
        rig.story.new_track("clip");
        rig.run(12., &mut cover);
        let size = (CLIP_WIDTH * CLIP_HEIGHT) as usize;
        let picture = std::env::var("DRIFT_PREVIEW_CLIP")
            .ok()
            .and_then(|path| std::fs::read(path).ok())
            .filter(|bytes| bytes.len() == size * 3)
            .unwrap_or_else(|| (0..size * 3).map(|index| ((index * 7) % 256) as u8).collect());
        let save = |rig: &Rig, pose: Pose, name: &str| {
            let pixels = rig.capture_pose(pose);
            image::save_buffer(
                format!("{directory}/{name}.png"),
                &pixels,
                rig.width,
                rig.height,
                image::ColorType::Rgba8,
            )
            .unwrap();
        };
        let mut pose = *rig.story.pose();
        pose.words = [[0.; WORD_FLOATS]; WORD_SLOTS];
        pose.clip = 1.;
        pose.chorus = 0.;
        rig.galaxy.set_clip_frame(&picture);
        rig.galaxy.set_clip_frame(&picture);
        rig.galaxy.advance(0.3, 0.);
        rig.galaxy.time = 0.;
        // The galaxy pours into the clip, step by step.
        for (step, clip) in [0.0_f32, 0.2, 0.35, 0.5, 0.65, 0.8, 0.92, 1.0].into_iter().enumerate() {
            let mut pouring = pose;
            pouring.clip = clip;
            save(&rig, pouring, &format!("pour{step}"));
        }
        save(&rig, pose, "fx1_sweet_spot");
        rig.look = (0.24, 0.07);
        save(&rig, pose, "fx2_swung");
        rig.look = (0., 0.);
        // A cut: the inverted picture is a new scene.
        let other: Vec<u8> = picture.iter().map(|value| 255 - value).collect();
        rig.galaxy.advance(2., 0.);
        rig.galaxy.set_clip_frame(&other);
        rig.galaxy.advance(0.12, 0.);
        save(&rig, pose, "fx3_cut_burst");
        rig.galaxy.advance(0.33, 0.);
        save(&rig, pose, "fx4_cut_gather");
        rig.galaxy.advance(1.5, 0.);
        // Movement: a bright block slides across a dark frame.
        let block = |left: u32| -> Vec<u8> {
            let mut frame = vec![10u8; size * 3];
            for y in 30..60 {
                for x in left..left + 30 {
                    let at = ((y * CLIP_WIDTH + x) * 3) as usize;
                    frame[at..at + 3].copy_from_slice(&[255, 200, 120]);
                }
            }
            frame
        };
        for step in 0..6 {
            rig.galaxy.set_clip_frame(&block(40 + step * 6));
            for _ in 0..3 {
                rig.galaxy.advance(1. / 60., 0.);
                let mut encoder = rig.galaxy.device.create_command_encoder(&Default::default());
                rig.galaxy.build(&mut encoder, &pose);
                rig.galaxy.queue.submit([encoder.finish()]);
            }
        }
        save(&rig, pose, "fx5_motion");
    }

    /// Debug only, with DRIFT_PREVIEW set: a line of words over a bright, a white and a dark picture, given as
    /// raw RGB in DRIFT_PREVIEW_BRIGHT, DRIFT_PREVIEW_WHITE and DRIFT_PREVIEW_DARK.
    #[test]
    fn words_over_the_clip_preview() {
        let Ok(directory) = std::env::var("DRIFT_PREVIEW") else { return };
        let Some(mut rig) = Rig::new(1280, 800) else { return };
        let mut cover = Some(bright_cover(false));
        rig.story.new_track("clip");
        rig.run(12., &mut cover);
        let size = (CLIP_WIDTH * CLIP_HEIGHT * 3) as usize;
        let at = rig.position;
        rig.story.set_lines(vec![TimedLine::plain(at + 0.3, "letters over the light"), TimedLine::plain(at + 30., "")]);
        rig.run(1.8, &mut cover);
        for (name, variable) in [
            ("words_bright", "DRIFT_PREVIEW_BRIGHT"),
            ("words_white", "DRIFT_PREVIEW_WHITE"),
            ("words_dark", "DRIFT_PREVIEW_DARK"),
        ] {
            let Some(picture) = std::env::var(variable)
                .ok()
                .and_then(|path| std::fs::read(path).ok())
                .filter(|bytes| bytes.len() == size)
            else {
                continue;
            };
            rig.galaxy.set_clip_frame(&picture);
            rig.galaxy.set_clip_frame(&picture);
            rig.galaxy.advance(3., 0.);
            let mut pose = *rig.story.pose();
            pose.clip = 1.;
            let pixels = rig.capture_pose(pose);
            image::save_buffer(
                format!("{directory}/{name}.png"),
                &pixels,
                rig.width,
                rig.height,
                image::ColorType::Rgba8,
            )
            .unwrap();
        }
    }

    /// Words of a line must appear one after another, not all at once.
    #[test]
    fn words_gather_one_by_one() {
        let Some(mut rig) = Rig::new(1400, 910) else { return };
        let mut cover = Some(real_cover().unwrap_or_else(|| bright_cover(false)));
        rig.story.new_track("DFWOLUHYj0s");
        rig.run(12., &mut cover);
        let at = rig.position;
        rig.story.set_lines(vec![
            TimedLine::plain(at - 20., "one two three four five six"),
            TimedLine::plain(at - 16., "one two three four five six"),
            TimedLine::plain(at - 12., "one two three four five six"),
            TimedLine::plain(at - 8., "one two three four five six"),
            TimedLine::plain(at + 0.7, "and again I lie awake till dawn while you are out there"),
            TimedLine::plain(at + 5.2, ""),
        ]);
        let mut seen = Vec::new();
        for (index, step) in [0.8_f32, 0.35, 0.45, 0.6, 0.7, 0.9].into_iter().enumerate() {
            rig.run(step, &mut cover);
            rig.save(&format!("w{index}_words"));
            seen.push(rig.story.pose().words.iter().filter(|word| word[4] > 0.5).count());
        }
        println!("words on stage over time: {seen:?}");
        assert!(seen[0] < seen[3], "more words keep landing: {seen:?}");
        assert!(seen[1] <= seen[3] && seen[3] >= 3, "the line does not appear at once: {seen:?}");
    }

    /// The chorus should feel huge, but the galaxy must rise smoothly and never flash the screen.
    /// Words are left out here: a single big word lighting up is meant to be sharp.
    #[test]
    fn chorus_rises_without_a_flash() {
        let Some(mut rig) = Rig::new(1440, 936) else { return };
        let mut cover = Some(real_cover().unwrap_or_else(|| bright_cover(false)));
        rig.story.new_track("DFWOLUHYj0s");
        rig.run(12., &mut cover);
        let at = rig.position;
        let verse =
            ["the city fades outside", "I walk home on my own", "your words are in my head", "and the night is quiet"];
        let chorus =
            ["we fly into the dark", "the stars fall in our hands", "I will not let you go", "we burn up in the sky"];
        let other = [
            "the morning paints the ceiling",
            "the coffee cools on the table",
            "the phone is silent again",
            "I am tired of looking for you",
        ];
        let texts: Vec<&str> = [verse, chorus, other, chorus].concat();
        let mut lines: Vec<TimedLine> = texts
            .iter()
            .enumerate()
            .map(|(index, text)| TimedLine::plain(at + 0.5 + index as f64 * 2.4, *text))
            .collect();
        lines.push(TimedLine::plain(at + 0.5 + texts.len() as f64 * 2.4, ""));
        rig.story.set_lines(lines);
        rig.run(7.6, &mut cover);
        let sky = |rig: &Rig| {
            let mut pose = *rig.story.pose();
            pose.words = [[0.; WORD_FLOATS]; WORD_SLOTS];
            average(&rig.capture_pose(pose))
        };
        let before = sky(&rig);
        let (mut highest, mut steepest, mut previous) = (before, 0f32, before);
        for index in 0..34 {
            rig.run(0.1, &mut cover);
            let now = sky(&rig);
            highest = highest.max(now);
            steepest = steepest.max((now - previous).abs());
            previous = now;
            if index % 4 == 0 {
                rig.save(&format!("c{index:02}_chorus"));
            }
        }
        println!("chorus brightness: before {before:.1}, highest {highest:.1}, largest step per 0.1 s {steepest:.1}");
        assert!(steepest < 5.5, "too abrupt: {steepest}");
        assert!(highest < before * 2.2 + 8., "too bright: {highest} from {before}");
    }

    /// The jump between songs must never flash the whole screen.
    #[test]
    fn hyperspace_does_not_flash() {
        let Some(mut rig) = Rig::new(1280, 720) else { return };
        let mut cover = Some(real_cover().unwrap_or_else(|| bright_cover(false)));
        rig.story.new_track("first");
        rig.run(12., &mut cover);
        rig.position = rig.duration - 1.0;
        rig.run(0.9, &mut cover);
        let before = average(&rig.capture());
        rig.story.new_track("second");
        rig.position = 0.;
        let mut next = Some(bright_cover(true));
        let (mut highest, mut steepest, mut previous) = (0f32, 0f32, before);
        let mut arrived = false;
        for _ in 0..30 {
            rig.run(0.1, &mut next);
            // Once the cover itself appears, its brightness is the song's own.
            let warp = rig.story.pose().warp;
            arrived |= warp >= 1.;
            if arrived && warp < 0.4 {
                break;
            }
            let now = average(&rig.capture());
            highest = highest.max(now);
            steepest = steepest.max((now - previous).abs());
            previous = now;
        }
        println!("jump brightness: before {before:.1}, highest {highest:.1}, largest step per 0.1 s {steepest:.1}");
        assert!(highest < 70., "too bright: {highest}");
        assert!(steepest < 26., "too abrupt: {steepest}");
    }

    /// How long one frame takes on this GPU at the MacBook's real resolution.
    #[test]
    fn frame_cost() {
        if std::env::var_os("DRIFT_FRAME_COST").is_none() {
            return;
        }
        let Some(mut rig) = Rig::new(2700, 1753) else { return };
        let target = rig.texture.create_view(&Default::default());
        let measure = |rig: &Rig, name: &str| {
            let started = std::time::Instant::now();
            for _ in 0..60 {
                let mut encoder = rig.galaxy.device.create_command_encoder(&Default::default());
                let pose = *rig.story.pose();
                rig.galaxy.build(&mut encoder, &pose);
                rig.galaxy.draw(&mut encoder, &rig.view, &target, &pose, Screen { look: (0.02, 0.01), headroom: 2. });
                rig.galaxy.queue.submit([encoder.finish()]);
                let _ = rig.galaxy.device.poll(wgpu::PollType::wait_indefinitely());
            }
            println!("{name}: {:.2} ms per frame", started.elapsed().as_secs_f32() * 1000. / 60.);
        };
        let mut cover = Some(bright_cover(false));
        rig.story.new_track("cost");
        rig.run(13., &mut cover);
        measure(&rig, "galaxy");
        let at = rig.position;
        rig.story
            .set_lines(vec![TimedLine::plain(at + 0.8, "and again I lie awake till dawn while you are out there")]);
        rig.run(2., &mut cover);
        measure(&rig, "text");
        rig.story.set_lines(Vec::new());
        rig.story.new_track("cost 2");
        let mut next = Some(bright_cover(true));
        rig.run(1.25, &mut next);
        measure(&rig, "hyperspace");
    }

    /// Photographs several songs in full bloom, and the same one with the cursor at the edges.
    #[test]
    fn every_song_has_its_own_galaxy() {
        let Ok(_) = std::env::var("DRIFT_PREVIEW") else { return };
        let Some(mut rig) = Rig::new(1000, 650) else { return };
        let mut cover = Some(real_cover().unwrap_or_else(|| bright_cover(false)));
        for (index, id) in ["DFWOLUHYj0s", "k4V3Mo61fJM", "Zi_XLOBDo_Y", "kJQP7kiw5Fk", "3tmd-ClpJxA", "hT_nvWreIhg"]
            .iter()
            .enumerate()
        {
            rig.story.new_track(id);
            rig.position = 0.;
            if index > 0 {
                cover = Some(bright_cover(index % 2 == 0));
            }
            rig.run(14., &mut cover);
            rig.save(&format!("g{index}_song"));
        }
        if let Ok(directory) = std::env::var("DRIFT_PREVIEW_FULL") {
            let Some(mut full) = Rig::new(2700, 1753) else { return };
            let mut art = Some(real_cover().unwrap_or_else(|| bright_cover(false)));
            full.story.new_track("DFWOLUHYj0s");
            full.run(13., &mut art);
            let at = full.position;
            let pixels = full.capture();
            image::save_buffer(format!("{directory}/full_galaxy.png"), &pixels, 2700, 1753, image::ColorType::Rgba8)
                .unwrap();
            full.story.set_lines(vec![TimedLine::plain(at + 0.8, "you look like a cat")]);
            full.run(1.9, &mut art);
            let pixels = full.capture();
            image::save_buffer(format!("{directory}/full_text.png"), &pixels, 2700, 1753, image::ColorType::Rgba8)
                .unwrap();
            full.story.set_lines(Vec::new());
            full.story.new_track("Zi_XLOBDo_Y");
            let mut bright = Some(bright_cover(true));
            full.run(1.25, &mut bright);
            let pixels = full.capture();
            image::save_buffer(format!("{directory}/full_flight.png"), &pixels, 2700, 1753, image::ColorType::Rgba8)
                .unwrap();
            full.run(13., &mut bright);
            let pixels = full.capture();
            image::save_buffer(format!("{directory}/full_bright.png"), &pixels, 2700, 1753, image::ColorType::Rgba8)
                .unwrap();
        }
        rig.look = (-0.1, 0.07);
        rig.save("l0_cursor_left");
        rig.look = (0.1, -0.07);
        rig.save("l1_cursor_right");
    }

    /// Goes through a whole song and the jump to the next one, on real GPU frames.
    #[test]
    fn plays_a_song_from_arrival_to_the_next_jump() {
        let Some(mut rig) = Rig::new(1400, 910) else {
            eprintln!("no GPU available, skipping");
            return;
        };
        let mut cover = Some(real_cover().unwrap_or_else(|| bright_cover(false)));
        rig.story.new_track("DFWOLUHYj0s");
        rig.run(1.9, &mut cover);
        rig.save("s01_arrive");
        rig.run(1.0, &mut cover);
        rig.save("s02_cover");
        let on_cover = rig.capture();
        rig.run(2.0, &mut cover);
        rig.save("s03_unwind_start");
        rig.run(1.1, &mut cover);
        rig.save("s04_unwind_mid");
        rig.run(4.0, &mut cover);
        rig.save("s05_galaxy");
        let galaxy = rig.capture();
        assert!(average(&on_cover) > 4., "the cover is visible");
        assert!(average(&galaxy) > 1.5, "the galaxy is visible");
        assert_ne!(on_cover, galaxy);

        let at = rig.position;
        rig.story.set_lines(vec![
            TimedLine::plain(at + 1.0, "you look like a cat"),
            TimedLine::plain(at + 5.0, "and again I lie awake till dawn while you are out there"),
            TimedLine::plain(at + 9.0, ""),
        ]);
        rig.run(0.6, &mut cover);
        rig.save("s06_text_forming");
        rig.run(1.4, &mut cover);
        rig.save("s07_text");
        rig.run(3.4, &mut cover);
        rig.save("s08_text_long");
        rig.run(4.0, &mut cover);

        rig.position = rig.duration - 4.0;
        rig.run(1.8, &mut cover);
        rig.save("s09_collapse_mid");
        rig.run(1.7, &mut cover);
        rig.save("s10_collapse");

        rig.story.new_track("another-track");
        rig.position = 0.;
        rig.run(0.42, &mut cover);
        rig.save("s11_jump");
        rig.run(0.5, &mut cover);
        let mut next = Some(bright_cover(true));
        rig.run(0.05, &mut next);
        rig.save("s12_flight");
        rig.run(1.1, &mut next);
        rig.save("s13_arriving");
        rig.run(0.6, &mut next);
        rig.save("s14_next_cover");
        rig.run(8.0, &mut next);
        rig.save("s15_next_galaxy");
    }
}
