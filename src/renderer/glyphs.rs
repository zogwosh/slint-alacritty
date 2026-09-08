//! Rasterize explicitly positioned glyphs and batch their clipped, colored quads.
use super::{resources::TEXTURE_FORMAT, text::ShapedRun};
use bytemuck::{Pod, Zeroable};
use cosmic_text::{CacheKey, FontSystem, SwashCache, SwashContent};
use slint::wgpu_29::wgpu;
use std::{collections::HashMap, ops::Range};

const PAGE_SIZE: u32 = 2048;
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    position: [f32; 2],
    uv: [f32; 2],
    color: [f32; 4],
}
#[derive(Clone, Copy)]
struct Sprite {
    page: usize,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    left: i32,
    top: i32,
    colored: bool,
}
struct Page {
    texture: wgpu::Texture,
    bind: wgpu::BindGroup,
    x: u32,
    y: u32,
    row_height: u32,
}
pub(super) struct GlyphRenderer {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    pages: Vec<Page>,
    sprites: HashMap<CacheKey, Option<Sprite>>,
    vertices: wgpu::Buffer,
    batches: Vec<(usize, Range<u32>)>,
}
impl GlyphRenderer {
    pub(super) fn new(device: &wgpu::Device) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("terminal glyph atlas"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor::default());
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("terminal glyphs"),
            source: wgpu::ShaderSource::Wgsl(include_str!("glyphs.wgsl").into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("terminal positioned glyphs"), layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs_main"), compilation_options: Default::default(), buffers: &[wgpu::VertexBufferLayout { array_stride: 32, step_mode: wgpu::VertexStepMode::Vertex, attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4] }] },
            fragment: Some(wgpu::FragmentState { module: &shader, entry_point: Some("fs_main"), compilation_options: Default::default(), targets: &[Some(wgpu::ColorTargetState { format: TEXTURE_FORMAT, blend: Some(wgpu::BlendState::ALPHA_BLENDING), write_mask: wgpu::ColorWrites::ALL })] }),
            primitive: Default::default(), depth_stencil: None, multisample: Default::default(), multiview_mask: None, cache: None,
        });
        let vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 32,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            pipeline,
            layout,
            sampler,
            pages: Vec::new(),
            sprites: HashMap::new(),
            vertices,
            batches: Vec::new(),
        }
    }
    pub(super) fn clear_cache(&mut self) {
        self.pages.clear();
        self.sprites.clear();
    }
    fn add_page(&mut self, device: &wgpu::Device) {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("terminal glyph page"),
            size: wgpu::Extent3d {
                width: PAGE_SIZE,
                height: PAGE_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TEXTURE_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        self.pages.push(Page {
            texture,
            bind,
            x: 0,
            y: 0,
            row_height: 0,
        });
    }
    fn sprite(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        fs: &mut FontSystem,
        cache: &mut SwashCache,
        key: CacheKey,
    ) -> Option<Sprite> {
        if let Some(sprite) = self.sprites.get(&key) {
            return *sprite;
        }
        let image = cache.get_image_uncached(fs, key);
        let sprite = image.and_then(|image| {
            let p = image.placement;
            if p.width == 0 || p.height == 0 || p.width > PAGE_SIZE || p.height > PAGE_SIZE {
                return None;
            }
            let colored = image.content == SwashContent::Color;
            let data = match image.content {
                SwashContent::Color => image.data,
                SwashContent::Mask => image
                    .data
                    .into_iter()
                    .flat_map(|a| [255, 255, 255, a])
                    .collect(),
                SwashContent::SubpixelMask => image
                    .data
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .flat_map(|p| [255, 255, 255, p[0].max(p[1]).max(p[2])])
                    .collect(),
            };
            if self.pages.is_empty() {
                self.add_page(device);
            }
            let page = self.pages.last_mut().unwrap();
            if page.x + p.width > PAGE_SIZE {
                page.x = 0;
                page.y += page.row_height;
                page.row_height = 0;
            }
            if page.y + p.height > PAGE_SIZE {
                self.add_page(device);
            }
            let index = self.pages.len() - 1;
            let page = &mut self.pages[index];
            let sprite = Sprite {
                page: index,
                x: page.x,
                y: page.y,
                width: p.width,
                height: p.height,
                left: p.left,
                top: p.top,
                colored,
            };
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &page.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: page.x,
                        y: page.y,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                &data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(p.width * 4),
                    rows_per_image: Some(p.height),
                },
                wgpu::Extent3d {
                    width: p.width,
                    height: p.height,
                    depth_or_array_layers: 1,
                },
            );
            page.x += p.width;
            page.row_height = page.row_height.max(p.height);
            Some(sprite)
        });
        self.sprites.insert(key, sprite);
        sprite
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) fn prepare<'a>(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        fs: &mut FontSystem,
        cache: &mut SwashCache,
        rows: impl Iterator<Item = (usize, &'a [ShapedRun])>,
        size: (u32, u32),
        cell: (f32, f32),
        baseline: f32,
    ) {
        // Invalidate only between frames. Rendered terminal pixels don't depend on atlas residency.
        if self.pages.len() > 4 || self.sprites.len() > 16384 {
            self.clear_cache();
        }
        self.batches.clear();
        let mut vertices = Vec::new();
        for (row, runs) in rows {
            let row_top = row as f32 * cell.1;
            for run in runs {
                for glyph in &run.glyphs {
                    let physical = glyph.layout.physical((0.0, row_top + baseline), 1.0);
                    let Some(sprite) = self.sprite(device, queue, fs, cache, physical.cache_key)
                    else {
                        continue;
                    };
                    let left = (physical.x + sprite.left) as f32;
                    let top = (physical.y - sprite.top) as f32;
                    // Contextual ligature fonts may keep separate clusters while their ink
                    // extends across neighboring cells. Slice the ink, not just the cluster box.
                    let paint_start = glyph
                        .start_column
                        .min((left / cell.0).floor().max(0.0) as usize);
                    let paint_end = glyph
                        .end_column
                        .max(((left + sprite.width as f32) / cell.0).ceil().max(0.0) as usize);
                    let first = run
                        .cells
                        .partition_point(|c| c.column + c.width_in_columns <= paint_start);
                    let last = run.cells.partition_point(|c| c.column < paint_end);
                    for paint in &run.cells[first..last] {
                        let x0 = left.max(paint.column as f32 * cell.0).max(0.0);
                        let x1 = (left + sprite.width as f32)
                            .min((paint.column + paint.width_in_columns) as f32 * cell.0)
                            .min(size.0 as f32);
                        let y0 = top.max(row_top).max(0.0);
                        let y1 = (top + sprite.height as f32)
                            .min(row_top + cell.1)
                            .min(size.1 as f32);
                        if x1 <= x0 || y1 <= y0 {
                            continue;
                        }
                        let c = paint.foreground;
                        let color = if sprite.colored {
                            [1.0; 4]
                        } else {
                            [
                                c.red as f32 / 255.0,
                                c.green as f32 / 255.0,
                                c.blue as f32 / 255.0,
                                1.0,
                            ]
                        };
                        let start = vertices.len() as u32;
                        for (x, y) in [(x0, y0), (x1, y0), (x0, y1), (x0, y1), (x1, y0), (x1, y1)] {
                            vertices.push(Vertex {
                                position: [
                                    2.0 * x / size.0 as f32 - 1.0,
                                    1.0 - 2.0 * y / size.1 as f32,
                                ],
                                uv: [
                                    (sprite.x as f32 + x - left) / PAGE_SIZE as f32,
                                    (sprite.y as f32 + y - top) / PAGE_SIZE as f32,
                                ],
                                color,
                            });
                        }
                        if let Some((page, range)) = self.batches.last_mut()
                            && *page == sprite.page
                        {
                            range.end += 6;
                        } else {
                            self.batches.push((sprite.page, start..start + 6));
                        }
                    }
                }
            }
        }
        if !vertices.is_empty() {
            let data = bytemuck::cast_slice(&vertices);
            if self.vertices.size() < data.len() as u64 {
                self.vertices = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("terminal glyph vertices"),
                    size: (data.len() as u64).next_power_of_two(),
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
            }
            queue.write_buffer(&self.vertices, 0, data);
        }
    }
    pub(super) fn render(&self, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_pipeline(&self.pipeline);
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        for (page, range) in &self.batches {
            pass.set_bind_group(0, &self.pages[*page].bind, &[]);
            pass.draw(range.clone(), 0..1);
        }
    }
}
