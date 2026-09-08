//! Offscreen checks exercise the production GPU path without touching a user's PTY/settings.
use super::*;
use crate::terminal::{CursorPatch, RgbColor, RgbaColor, RowPatch, SearchSnapshot};
use std::{
    future::Future,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
};

fn block_on<T>(future: impl Future<Output = T>) -> T {
    struct ThreadWake(std::thread::Thread);
    impl Wake for ThreadWake {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(ThreadWake(std::thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(v) => return v,
            Poll::Pending => std::thread::park(),
        }
    }
}
fn pixels(renderer: &GpuTerminalRenderer) -> Vec<u8> {
    let stride = (renderer.width * 4).div_ceil(256) * 256;
    let buffer = renderer.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("test readback"),
        size: u64::from(stride * renderer.height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = renderer.device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &renderer.texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(stride),
                rows_per_image: Some(renderer.height),
            },
        },
        wgpu::Extent3d {
            width: renderer.width,
            height: renderer.height,
            depth_or_array_layers: 1,
        },
    );
    renderer.queue.submit(Some(encoder.finish()));
    let (tx, rx) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| tx.send(result).unwrap());
    renderer
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    rx.recv().unwrap().unwrap();
    let mapped = buffer.slice(..).get_mapped_range();
    mapped
        .chunks_exact(stride as usize)
        .flat_map(|row| row[..renderer.width as usize * 4].iter().copied())
        .collect()
}
fn fixture() -> FramePatch {
    let fg = RgbColor {
        red: 242,
        green: 242,
        blue: 247,
    };
    let bg = RgbColor {
        red: 28,
        green: 28,
        blue: 30,
    };
    let lines = [
        "PS C:\\Users\\js\\Desktop\\projects> dddddddddddddddddd1",
        "ASCII: ddddddddddddddddddddddddddddddddddddddddddddd",
        "Mixed: A中B文C e\u{301} o\u{308} -> != === => ffi",
        "Colors: != === => 中文 👩‍💻 🙂",
        "IME: abcdefghijklmnopqrstuvwxyz",
    ];
    FramePatch {
        generation: 1,
        columns: 64,
        rows: lines.len(),
        full_redraw: true,
        full_redraw_reason: Some(crate::terminal::FullRedrawReason::RendererRequest),
        metadata_only: false,
        changed_rows: lines
            .iter()
            .enumerate()
            .map(|(row, line)| {
                let (mut cells, _) = text::compose_preedit(&[], 64, 0, line, fg);
                for cell in &mut cells {
                    cell.background = bg;
                    if row == 0 && cell.column >= 34 {
                        cell.foreground = RgbColor {
                            red: 235,
                            green: 185,
                            blue: 40,
                        };
                    }
                    if row == 3 {
                        cell.foreground = if cell.column % 2 == 0 {
                            RgbColor {
                                red: 255,
                                green: 100,
                                blue: 100,
                            }
                        } else {
                            RgbColor {
                                red: 90,
                                green: 190,
                                blue: 255,
                            }
                        };
                    }
                }
                RowPatch {
                    row,
                    start_column: 0,
                    end_column: 64,
                    cells,
                }
            })
            .collect(),
        cursor: CursorPatch {
            column: 8,
            row: 4,
            visible: true,
            shape: 0,
            blinking: false,
        },
        scroll_offset: 0,
        scroll_history_lines: 0,
        search: SearchSnapshot::default(),
        decorations: Vec::new(),
        title: None,
        exit_message: None,
    }
}
#[test]
#[ignore = "requires a GPU adapter; writes target/render-grid.png"]
fn gpu_grid_ime_and_incremental_match_full_render() {
    let instance = wgpu::Instance::default();
    let adapter = block_on(instance.request_adapter(&Default::default())).expect("GPU adapter");
    let (device, queue) = block_on(adapter.request_device(&Default::default())).unwrap();
    let fg = RgbColor {
        red: 242,
        green: 242,
        blue: 247,
    };
    let bg = RgbColor {
        red: 28,
        green: 28,
        blue: 30,
    };
    let highlight = RgbaColor {
        red: 10,
        green: 132,
        blue: 255,
        alpha: 115,
    };
    let theme = TerminalTheme {
        foreground: fg,
        background: bg,
        selection_background: highlight,
        search_match_background: highlight,
        search_current_background: highlight,
    };
    let m = text::measure_cell("Maple Mono NF CN", 26.0);
    let mut gpu = GpuTerminalRenderer::new(
        &device,
        &queue,
        64 * m.width,
        5 * m.height,
        64,
        5,
        m.width as f32,
        m.height as f32,
        26.0,
        "Maple Mono NF CN",
        theme,
    );
    let mut frame = fixture();
    assert!(gpu.apply_frame(&frame));
    gpu.render_if_needed();
    let initial = pixels(&gpu);
    assert!(initial.as_chunks::<4>().0.iter().any(|p| p[0] > 100));
    gpu.set_ime_preedit("输入中");
    gpu.render_if_needed();
    let composed = pixels(&gpu);
    assert_ne!(initial, composed);
    // Independently draw a terminal frame whose cells already contain the preedit.
    let (mut cells, width) =
        text::compose_preedit(&frame.changed_rows[4].cells, 64, 8, "输入中", fg);
    for cell in &mut cells {
        if cell.column >= 8 && cell.column < 8 + width {
            cell.background = bg;
            cell.underline_style = 1;
        }
    }
    let mut expected_frame = fixture();
    expected_frame.changed_rows[4].cells = cells;
    let mut expected = GpuTerminalRenderer::new(
        &device,
        &queue,
        64 * m.width,
        5 * m.height,
        64,
        5,
        m.width as f32,
        m.height as f32,
        26.0,
        "Maple Mono NF CN",
        theme,
    );
    assert!(expected.apply_frame(&expected_frame));
    expected.render_if_needed();
    let reference = pixels(&expected);
    image::save_buffer(
        "target/render-ime-reference.png",
        &reference,
        gpu.width,
        gpu.height,
        image::ColorType::Rgba8,
    )
    .unwrap();
    image::save_buffer(
        "target/render-grid.png",
        &composed,
        gpu.width,
        gpu.height,
        image::ColorType::Rgba8,
    )
    .unwrap();
    assert!(
        reference == composed,
        "IME pixel differences: {}",
        reference
            .iter()
            .zip(&composed)
            .filter(|(a, b)| a != b)
            .count()
    );
    image::save_buffer(
        "target/render-grid.png",
        &composed,
        gpu.width,
        gpu.height,
        image::ColorType::Rgba8,
    )
    .unwrap();
    gpu.set_ime_preedit("");
    gpu.render_if_needed();
    assert!(
        pixels(&gpu) == initial,
        "IME cancellation must restore the exact image"
    );
    frame.changed_rows[1].cells[10].character = 'X';
    let mut delta = fixture();
    delta.full_redraw = false;
    delta.full_redraw_reason = None;
    delta.changed_rows = vec![RowPatch {
        row: 1,
        start_column: 10,
        end_column: 11,
        cells: vec![frame.changed_rows[1].cells[10].clone()],
    }];
    assert!(gpu.apply_frame(&delta));
    gpu.render_if_needed();
    let incremental = pixels(&gpu);
    assert_ne!(incremental, initial);
    assert!(gpu.apply_frame(&frame));
    gpu.render_if_needed();
    assert!(
        pixels(&gpu) == incremental,
        "partial update must equal full rendering"
    );
    // Exercise atlas eviction without changing the visible terminal.
    gpu.glyph_renderer.clear_cache();
    gpu.clear_pending = true;
    gpu.render_pending = true;
    gpu.render_if_needed();
    assert!(pixels(&gpu) == incremental);
    // Font/DPI changes must rebuild grid placement and active composition together.
    gpu.set_ime_preedit("中e\u{301}");
    let resized = text::measure_cell("Consolas", 19.5);
    gpu.sync_surface(
        64 * resized.width,
        5 * resized.height,
        resized.width as f32,
        resized.height as f32,
        19.5,
        "Consolas",
    );
    gpu.render_if_needed();
    let mut fresh = GpuTerminalRenderer::new(
        &device,
        &queue,
        64 * resized.width,
        5 * resized.height,
        64,
        5,
        resized.width as f32,
        resized.height as f32,
        19.5,
        "Consolas",
        theme,
    );
    assert!(fresh.apply_frame(&frame));
    fresh.set_ime_preedit("中e\u{301}");
    fresh.render_if_needed();
    assert!(
        pixels(&gpu) == pixels(&fresh),
        "resizing with IME must equal a fresh render"
    );
}
