//! Drawing what a save has built with the game's own models seen from above
//! (`fb_model::world::Sprites`, rendered by the icon helper): each building
//! at true size and facing, belts and pipes as their real straight piece
//! repeated along the curve. Falls back to icons and lines while the renders
//! aren't there, or when they'd be too small to see.

use crate::assets::Assets;
use crate::theme::*;
use egui::{pos2, Color32, Mesh, Painter, Pos2, Rect, Shape, Stroke, TextureId};
use fb_model::world::{Carrier, Sprite, Sprites, Strip};
use std::collections::HashMap;

/// Below this many pixels across, a render shows nothing useful.
const MIN_SPRITE_PX: f32 = 8.0;
const MIN_STRIP_PX: f32 = 2.5;
/// Lines for belts and pipes when there's no render to use.
const BELT: Color32 = HEADER;
const PIPE: Color32 = Color32::from_rgb(0x4C, 0x6F, 0x78);

/// World (cm) to screen, and back.
#[derive(Clone, Copy)]
pub struct View {
    pub rect: Rect,
    pub center: (f64, f64),
    pub scale: f64,
}

impl View {
    pub fn screen(&self, x: f64, y: f64) -> Pos2 {
        let c = self.rect.center();
        pos2(c.x + ((x - self.center.0) * self.scale) as f32, c.y + ((y - self.center.1) * self.scale) as f32)
    }
}

/// Local (x, y) rotated by `yaw` degrees about Z (Unreal: X forward, Y right).
fn rotate(yaw: f32, x: f32, y: f32) -> (f32, f32) {
    let (s, c) = yaw.to_radians().sin_cos();
    (x * c - y * s, x * s + y * c)
}

/// A texture for a render, once it's loaded.
fn texture(ctx: &egui::Context, assets: &mut Assets, file: &str) -> Option<TextureId> {
    let uri = assets.sprite_uri(file)?;
    match ctx.try_load_texture(&uri, egui::TextureOptions::LINEAR, egui::SizeHint::default()) {
        Ok(egui::load::TexturePoll::Ready { texture }) => Some(texture.id),
        _ => None,
    }
}

/// Screen corners of a building's footprint (local min..max, rotated, placed).
pub fn corners(view: &View, s: &Sprite, x: f32, y: f32, yaw: f32) -> [Pos2; 4] {
    [(s.min_x, s.min_y), (s.max_x, s.min_y), (s.max_x, s.max_y), (s.min_x, s.max_y)].map(|(lx, ly)| {
        let (rx, ry) = rotate(yaw, lx, ly);
        view.screen((x + rx) as f64, (y + ry) as f64)
    })
}

/// Quads batched per texture: one mesh per image, however many buildings.
#[derive(Default)]
pub struct Batch {
    meshes: HashMap<TextureId, Mesh>,
}

impl Batch {
    fn quad(&mut self, tex: TextureId, c: [Pos2; 4], u1: f32) {
        let mesh = self.meshes.entry(tex).or_insert_with(|| Mesh::with_texture(tex));
        let i = mesh.vertices.len() as u32;
        let uv = [pos2(0.0, 0.0), pos2(u1, 0.0), pos2(u1, 1.0), pos2(0.0, 1.0)];
        for (p, t) in c.iter().zip(uv) {
            mesh.vertices.push(egui::epaint::Vertex { pos: *p, uv: t, color: Color32::WHITE });
        }
        mesh.indices.extend_from_slice(&[i, i + 1, i + 2, i, i + 2, i + 3]);
    }

    pub fn paint(self, painter: &Painter) {
        for (_, m) in self.meshes {
            painter.add(Shape::mesh(m));
        }
    }
}

/// Draws a building: its render when there is one big enough, otherwise
/// returns false so the caller draws an icon or a dot.
#[allow(clippy::too_many_arguments)]
pub fn building(ctx: &egui::Context, assets: &mut Assets, sprites: Option<&Sprites>, batch: &mut Batch, outline: &mut Vec<[Pos2; 4]>, view: &View, class: &str, x: f32, y: f32, yaw: f32, selected: bool) -> bool {
    let Some(s) = sprites.and_then(|sp| sp.buildings.get(class)) else { return false };
    let across = ((s.max_x - s.min_x).max(s.max_y - s.min_y) as f64 * view.scale) as f32;
    if across < MIN_SPRITE_PX {
        return false;
    }
    let Some(tex) = texture(ctx, assets, &s.file) else { return false };
    let c = corners(view, s, x, y, yaw);
    if !view.rect.intersects(Rect::from_points(&c)) {
        return true;
    }
    batch.quad(tex, c, 1.0);
    if selected {
        outline.push(c);
    }
    true
}

/// Outlines for selected buildings, drawn over the renders.
pub fn outlines(painter: &Painter, outline: Vec<[Pos2; 4]>) {
    for c in outline {
        painter.add(Shape::convex_polygon(c.to_vec(), ACCENT.gamma_multiply(0.25), Stroke::new(2.0_f32, ACCENT)));
    }
}

/// Draws a belt or pipe: its rendered piece repeated along the curve, or a
/// line as wide as the real thing when the render is missing or tiny.
pub fn carrier(ctx: &egui::Context, assets: &mut Assets, sprites: Option<&Sprites>, batch: &mut Batch, painter: &Painter, view: &View, c: &Carrier) {
    let strip: Option<&Strip> = sprites.and_then(|sp| sp.strips.get(&c.class));
    let width_px = strip.map(|s| (s.width_cm as f64 * view.scale) as f32);
    if let (Some(s), Some(w)) = (strip, width_px) {
        if w >= MIN_STRIP_PX {
            if let Some(tex) = texture(ctx, assets, &s.file) {
                for seg in c.points.windows(2) {
                    pieces(batch, tex, view, s, seg[0], seg[1]);
                }
                return;
            }
        }
    }
    let width = width_px.unwrap_or(((view.scale * 60.0) as f32).clamp(0.8, 3.0)).max(1.0);
    let pts: Vec<Pos2> = c.points.iter().map(|p| view.screen(p[0] as f64, p[1] as f64)).collect();
    painter.add(Shape::line(pts, Stroke::new(width, if c.fluid { PIPE } else { BELT })));
}

/// One straight stretch of belt: as many rendered pieces as fit, the last
/// one cut short.
fn pieces(batch: &mut Batch, tex: TextureId, view: &View, s: &Strip, a: [f32; 2], b: [f32; 2]) {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1.0 {
        return;
    }
    let (ux, uy) = (dx / len, dy / len);
    // Across the belt: local +Y is the image's down (see `Strip`).
    let (nx, ny) = (-uy * s.width_cm / 2.0, ux * s.width_cm / 2.0);
    let mut t = 0.0;
    while t < len {
        let t1 = (t + s.length_cm).min(len);
        let (x0, y0) = (a[0] + ux * t, a[1] + uy * t);
        let (x1, y1) = (a[0] + ux * t1, a[1] + uy * t1);
        let c = [
            view.screen((x0 - nx) as f64, (y0 - ny) as f64),
            view.screen((x1 - nx) as f64, (y1 - ny) as f64),
            view.screen((x1 + nx) as f64, (y1 + ny) as f64),
            view.screen((x0 + nx) as f64, (y0 + ny) as f64),
        ];
        batch.quad(tex, c, (t1 - t) / s.length_cm);
        t = t1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_follows_unreal_yaw() {
        // Yaw 90: local +X (forward) points to world +Y.
        let (x, y) = rotate(90.0, 100.0, 0.0);
        assert!(x.abs() < 1e-3 && (y - 100.0).abs() < 1e-3);
    }

    #[test]
    fn a_belt_is_cut_into_rendered_pieces() {
        let view = View { rect: Rect::from_min_size(Pos2::ZERO, egui::vec2(1000.0, 1000.0)), center: (0.0, 0.0), scale: 1.0 };
        let s = Strip { file: "b.png".into(), length_cm: 200.0, width_cm: 120.0 };
        let mut batch = Batch::default();
        let tex = TextureId::Managed(7);
        pieces(&mut batch, tex, &view, &s, [0.0, 0.0], [500.0, 0.0]);
        let m = &batch.meshes[&tex];
        assert_eq!(m.vertices.len(), 12, "200 + 200 + 100 cm");
        assert!((m.vertices[9].uv.x - 0.5).abs() < 1e-6, "the last piece is half a texture");
        // The view's centre (world 0, 0) is the screen's (500, 500).
        let ys: Vec<f32> = m.vertices.iter().map(|v| v.pos.y - 500.0).collect();
        assert!(ys.iter().all(|y| (y.abs() - 60.0).abs() < 1e-3), "120 cm wide, centred");
    }
}
