use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Arc;

use ddtrust_core::DataStore;
use eframe::egui;

pub struct Animation {
    frames: Vec<egui::TextureHandle>,
    delays: Vec<f32>,
    total: f32,
    pub size: egui::Vec2,
}

impl Animation {
    pub fn frame_at(&self, time: f64) -> Option<&egui::TextureHandle> {
        if self.frames.is_empty() {
            return None;
        }
        if self.frames.len() == 1 || self.total <= 0.0 {
            return self.frames.first();
        }
        let mut t = (time as f32) % self.total;
        for (i, d) in self.delays.iter().enumerate() {
            if t < *d {
                return self.frames.get(i).or_else(|| self.frames.first());
            }
            t -= *d;
        }
        self.frames.last()
    }
}

pub struct AssetCache {
    store: Arc<DataStore>,
    images: HashMap<String, Option<Animation>>,
}

impl AssetCache {
    pub fn new(store: Arc<DataStore>) -> Self {
        Self {
            store,
            images: HashMap::new(),
        }
    }

    pub fn ensure(&mut self, ctx: &egui::Context, name: &str) {
        if self.images.contains_key(name) {
            return;
        }
        let loaded = self.load(ctx, name);
        self.images.insert(name.to_string(), loaded);
    }

    pub fn get(&self, name: &str) -> Option<&Animation> {
        self.images.get(name).and_then(|a| a.as_ref())
    }

    fn load(&self, ctx: &egui::Context, name: &str) -> Option<Animation> {
        let bytes = self.store.graphic(name)?;
        if bytes.len() >= 6 && (&bytes[..6] == b"GIF87a" || &bytes[..6] == b"GIF89a") {
            return decode_gif(ctx, name, &bytes);
        }
        let img = image::load_from_memory(&bytes).ok()?.to_rgba8();
        let size = [img.width() as usize, img.height() as usize];
        let image = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
        let tex = ctx.load_texture(name, image, egui::TextureOptions::LINEAR);
        let v = tex.size_vec2();
        Some(Animation {
            frames: vec![tex],
            delays: vec![1.0],
            total: 1.0,
            size: v,
        })
    }
}

fn decode_gif(ctx: &egui::Context, name: &str, bytes: &[u8]) -> Option<Animation> {
    let mut opts = gif::DecodeOptions::new();
    opts.set_color_output(gif::ColorOutput::RGBA);
    let mut decoder = opts.read_info(Cursor::new(bytes)).ok()?;
    let screen_w = decoder.width() as usize;
    let screen_h = decoder.height() as usize;
    if screen_w == 0 || screen_h == 0 {
        return None;
    }
    let mut canvas = vec![0u8; screen_w * screen_h * 4];
    let mut frames = Vec::new();
    let mut delays = Vec::new();
    let mut index = 0;
    while let Some(frame) = decoder.read_next_frame().ok().flatten() {
        let fw = frame.width as usize;
        let fh = frame.height as usize;
        let left = frame.left as usize;
        let top = frame.top as usize;
        if frame.buffer.len() >= fw * fh * 4 {
            for y in 0..fh {
                let dy = top + y;
                if dy >= screen_h {
                    break;
                }
                for x in 0..fw {
                    let dx = left + x;
                    if dx >= screen_w {
                        break;
                    }
                    let src = (y * fw + x) * 4;
                    let dst = (dy * screen_w + dx) * 4;
                    canvas[dst..dst + 4].copy_from_slice(&frame.buffer[src..src + 4]);
                }
            }
        }
        let color = egui::ColorImage::from_rgba_unmultiplied([screen_w, screen_h], &canvas);
        let tex = ctx.load_texture(
            format!("{name}#{index}"),
            color,
            egui::TextureOptions::LINEAR,
        );
        frames.push(tex);
        let delay = (frame.delay as f32 / 100.0).max(0.06);
        delays.push(delay);
        index += 1;
        if index >= 80 {
            break;
        }
    }
    if frames.is_empty() {
        return None;
    }
    let total = delays.iter().sum::<f32>().max(0.06);
    Some(Animation {
        frames,
        delays,
        total,
        size: egui::vec2(screen_w as f32, screen_h as f32),
    })
}
