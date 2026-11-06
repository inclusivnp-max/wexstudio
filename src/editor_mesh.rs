//! Horizontal text-mesh culling and exact single-row hit testing.
//! Font shaping is unchanged; this avoids submitting every off-screen glyph each frame.
use eframe::egui::{
    self, Color32, Pos2, Rect, Vec2,
    epaint::{Galley, Mesh, Vertex},
};
use std::sync::{Arc, Weak};
const BLOCK: usize = 256;
#[derive(Clone, Debug)]
struct Block {
    start: usize,
    end: usize,
    min: f32,
    max: f32,
    prefix_max: f32,
    suffix_min: f32,
    hit_max: f32,
}
struct RowIndex {
    blocks: Vec<Block>,
}
impl RowIndex {
    fn new(row: &egui::epaint::text::PlacedRow) -> Self {
        let mut blocks = vec![];
        let mut prefix = f32::NEG_INFINITY;
        let mut hit = f32::NEG_INFINITY;
        for start in (0..row.glyphs.len()).step_by(BLOCK) {
            let end = (start + BLOCK).min(row.glyphs.len());
            let mut min = f32::INFINITY;
            let mut max = f32::NEG_INFINITY;
            for g in &row.glyphs[start..end] {
                hit = hit.max(g.logical_rect().center().x);
                if !g.uv_rect.is_nothing() {
                    let a = g.first_vertex as usize;
                    if let Some(v) = row.visuals.mesh.vertices.get(a..a + 4) {
                        for v in v {
                            min = min.min(v.pos.x);
                            max = max.max(v.pos.x);
                        }
                    }
                }
            }
            prefix = prefix.max(max);
            blocks.push(Block {
                start,
                end,
                min,
                max,
                prefix_max: prefix,
                suffix_min: f32::INFINITY,
                hit_max: hit,
            });
        }
        let mut suffix = f32::INFINITY;
        for b in blocks.iter_mut().rev() {
            suffix = suffix.min(b.min);
            b.suffix_min = suffix;
        }
        Self { blocks }
    }
    fn hit(&self, row: &egui::epaint::text::PlacedRow, x: f32) -> usize {
        let i = self.blocks.partition_point(|b| b.hit_max <= x);
        let Some(b) = self.blocks.get(i) else {
            return row.glyphs.len();
        };
        for n in b.start..b.end {
            if x < row.glyphs[n].logical_rect().center().x {
                return n;
            }
        }
        row.glyphs.len()
    }
}
struct Entry {
    galley: Weak<Galley>,
    rows: Vec<RowIndex>,
    used: u64,
}
#[derive(Default)]
pub struct Cache {
    entries: Vec<Entry>,
    frame: u64,
}
#[derive(Default, Debug, Clone, serde::Serialize)]
pub struct Stats {
    pub source_vertices: usize,
    pub submitted_vertices: usize,
    pub candidate_glyphs: usize,
    pub candidate_blocks: usize,
    pub newly_indexed_glyphs: usize,
}
impl Cache {
    /// Build the immutable index on a layout worker before publication.
    pub fn prepare(&mut self, g: &Arc<Galley>) {
        self.index(g);
    }
    pub fn begin_frame(&mut self, frame: u64) {
        self.frame = frame;
        self.entries
            .retain(|e| frame.saturating_sub(e.used) <= 2 && e.galley.strong_count() > 0);
    }
    fn index(&mut self, g: &Arc<Galley>) -> (usize, usize) {
        if let Some(i) = self
            .entries
            .iter()
            .position(|e| e.galley.ptr_eq(&Arc::downgrade(g)))
        {
            self.entries[i].used = self.frame;
            return (i, 0);
        }
        if self.entries.len() >= 128 {
            let i = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, e)| e.used)
                .unwrap()
                .0;
            self.entries.swap_remove(i);
        }
        let n = g.rows.iter().map(|r| r.glyphs.len()).sum();
        self.entries.push(Entry {
            galley: Arc::downgrade(g),
            rows: g.rows.iter().map(RowIndex::new).collect(),
            used: self.frame,
        });
        (self.entries.len() - 1, n)
    }
    /// Equivalent to Galley::cursor_from_pos for single unwrapped editor rows.
    pub fn cursor_from_pos(&mut self, g: &Arc<Galley>, pos: Vec2) -> egui::text::CCursor {
        if g.rows.len() != 1 || g.rows[0].glyphs.len() < BLOCK {
            return g.cursor_from_pos(pos);
        }
        let row = &g.rows[0];
        if pos.y < row.min_y() - 5. {
            return g.begin();
        }
        if pos.y > row.max_y() + 5. {
            return g.end();
        }
        let (i, _) = self.index(g);
        let n = self.entries[i].rows[0].hit(row, pos.x - row.pos.x);
        egui::text::CCursor {
            index: n,
            prefer_next_row: n < row.glyphs.len(),
        }
    }
    /// Exact existing colors/UVs/quads, including backgrounds and decorations.
    /// Only editor TextShapes (angle0/opacity1/no override) are handled here.
    pub fn mesh(
        &mut self,
        g: &Arc<Galley>,
        pos: Pos2,
        clip: Rect,
        texture: [usize; 2],
        fallback: Color32,
    ) -> (Mesh, Stats) {
        let (i, indexed) = self.index(g);
        let mut out = Mesh::default();
        let mut stats = Stats {
            source_vertices: g.num_vertices,
            newly_indexed_glyphs: indexed,
            ..Default::default()
        };
        let scale = g.pixels_per_point;
        let origin = Pos2::new(
            (pos.x * scale).round() / scale,
            (pos.y * scale).round() / scale,
        );
        let normalize = Vec2::new(1. / texture[0] as f32, 1. / texture[1] as f32);
        for (row, idx) in g.rows.iter().zip(&self.entries[i].rows) {
            let offset = origin + row.pos.to_vec2();
            let bounds = row.visuals.mesh_bounds.translate(offset.to_vec2());
            if !clip.intersects(bounds) {
                continue;
            }
            let left = clip.left() - offset.x;
            let right = clip.right() - offset.x;
            let src = &row.visuals.mesh;
            let first = row.visuals.glyph_index_start;
            let last = first + row.visuals.glyph_vertex_range.len() / 4 * 6;
            copy_triangles(
                &mut out,
                src,
                &src.indices[..first],
                offset,
                clip,
                normalize,
                fallback,
            );
            let start = idx.blocks.partition_point(|b| b.prefix_max < left);
            let end = idx.blocks.partition_point(|b| b.suffix_min <= right);
            for b in &idx.blocks[start.min(end)..end] {
                stats.candidate_blocks += 1;
                if b.max < left || b.min > right {
                    continue;
                }
                for glyph in &row.glyphs[b.start..b.end] {
                    stats.candidate_glyphs += 1;
                    if glyph.uv_rect.is_nothing() {
                        continue;
                    }
                    let a = glyph.first_vertex as usize;
                    let quad = &src.vertices[a..a + 4];
                    if !bounds_of(quad).translate(offset.to_vec2()).intersects(clip) {
                        continue;
                    }
                    let base = out.vertices.len() as u32;
                    out.indices.extend_from_slice(&[
                        base,
                        base + 1,
                        base + 2,
                        base + 2,
                        base + 1,
                        base + 3,
                    ]);
                    out.vertices.extend(
                        quad.iter()
                            .map(|v| convert(*v, offset, normalize, fallback)),
                    );
                }
            }
            copy_triangles(
                &mut out,
                src,
                &src.indices[last..],
                offset,
                clip,
                normalize,
                fallback,
            );
        }
        stats.submitted_vertices = out.vertices.len();
        (out, stats)
    }
}
fn convert(v: Vertex, offset: Pos2, n: Vec2, fallback: Color32) -> Vertex {
    Vertex {
        pos: offset + v.pos.to_vec2(),
        uv: Pos2::new(v.uv.x * n.x, v.uv.y * n.y),
        color: if v.color == Color32::PLACEHOLDER {
            fallback
        } else {
            v.color
        },
    }
}
fn bounds_of(v: &[Vertex]) -> Rect {
    let mut r = Rect::NOTHING;
    for v in v {
        r.extend_with(v.pos);
    }
    r
}
fn copy_triangles(
    out: &mut Mesh,
    src: &Mesh,
    indices: &[u32],
    offset: Pos2,
    clip: Rect,
    n: Vec2,
    fallback: Color32,
) {
    for t in indices.as_chunks::<3>().0 {
        let v = [
            src.vertices[t[0] as usize],
            src.vertices[t[1] as usize],
            src.vertices[t[2] as usize],
        ];
        if bounds_of(&v).translate(offset.to_vec2()).intersects(clip) {
            let b = out.vertices.len() as u32;
            out.indices.extend_from_slice(&[b, b + 1, b + 2]);
            out.vertices
                .extend(v.into_iter().map(|v| convert(v, offset, n, fallback)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::epaint::{
        TessellationOptions, Tessellator, TextShape,
        text::{Fonts, LayoutJob, TextFormat},
    };
    fn layout(text: &str, ppp: f32) -> (Arc<Galley>, [usize; 2]) {
        let mut fonts = Fonts::new(8192, Default::default(), egui::FontDefinitions::default());
        let mut job = LayoutJob::default();
        let split = text
            .char_indices()
            .nth(text.chars().count() / 2)
            .map_or(text.len(), |(i, _)| i);
        job.append(
            &text[..split],
            0.,
            TextFormat {
                font_id: egui::FontId::monospace(16.),
                color: Color32::PLACEHOLDER,
                italics: true,
                ..Default::default()
            },
        );
        job.append(
            &text[split..],
            0.,
            TextFormat {
                font_id: egui::FontId::monospace(18.),
                color: Color32::LIGHT_GREEN,
                background: Color32::DARK_BLUE,
                underline: egui::Stroke::new(1.5_f32, Color32::YELLOW),
                strikethrough: egui::Stroke::new(1_f32, Color32::RED),
                ..Default::default()
            },
        );
        job.wrap.max_width = f32::INFINITY;
        let g = fonts.with_pixels_per_point(ppp).layout_job(job);
        (g, fonts.font_image_size())
    }
    fn full(g: &Arc<Galley>, pos: Pos2, tex: [usize; 2]) -> Mesh {
        let mut mesh = Mesh::default();
        Tessellator::new(
            g.pixels_per_point,
            TessellationOptions::default(),
            tex,
            vec![],
        )
        .tessellate_text(&TextShape::new(pos, g.clone(), Color32::WHITE), &mut mesh);
        mesh
    }
    type Triangle = [(u32, u32, u32, u32, [u8; 4]); 3];
    fn visible(mesh: &Mesh, clip: Rect) -> Vec<Triangle> {
        mesh.indices
            .as_chunks::<3>()
            .0
            .iter()
            .filter_map(|t| {
                let v = [
                    mesh.vertices[t[0] as usize],
                    mesh.vertices[t[1] as usize],
                    mesh.vertices[t[2] as usize],
                ];
                bounds_of(&v).intersects(clip).then(|| {
                    v.map(|v| {
                        (
                            v.pos.x.to_bits(),
                            v.pos.y.to_bits(),
                            v.uv.x.to_bits(),
                            v.uv.y.to_bits(),
                            v.color.to_array(),
                        )
                    })
                })
            })
            .collect()
    }
    #[test]
    fn cropped_triangles_match_actual_epaint_tessellator_styles_uvs_and_fractional_dpi() {
        let text = "abc\tЖ界😀e\u{301} 0123456789 ".repeat(700);
        for ppp in [1., 1.25, 2.] {
            let (g, tex) = layout(&text, ppp);
            let pos = Pos2::new(-12.31, 3.79);
            let original = full(&g, pos, tex);
            let mut c = Cache::default();
            for x in [0., 147., g.size().x / 2., g.size().x - 500.] {
                let clip = Rect::from_min_size(Pos2::new(x, 0.), Vec2::new(477., 53.));
                let (mesh, stats) = c.mesh(&g, pos, clip, tex, Color32::WHITE);
                assert_eq!(visible(&mesh, clip), visible(&original, clip));
                assert!(stats.submitted_vertices < stats.source_vertices / 10);
            }
        }
    }
    #[test]
    fn multiline_rows_backgrounds_and_offscreen_rows_match_full_renderer() {
        let (g, tex) = layout(&"hello Ж界\t😀\nsecond long line\nthird\n".repeat(30), 1.);
        let original = full(&g, Pos2::new(10.2, -24.3), tex);
        let clip = Rect::from_min_size(Pos2::ZERO, Vec2::new(130., 91.));
        let (mesh, _) =
            Cache::default().mesh(&g, Pos2::new(10.2, -24.3), clip, tex, Color32::WHITE);
        assert_eq!(visible(&mesh, clip), visible(&original, clip));
    }
    #[test]
    fn exact_binary_block_hits_match_epaint_at_every_boundary_and_far_tail() {
        let (g, _) = layout(&"abcЖ界😀e\u{301}\t ".repeat(1000), 1.25);
        let row = &g.rows[0];
        let mut cache = Cache::default();
        for glyph in &row.glyphs {
            for dx in [-0.001, 0., 0.001] {
                let p = Vec2::new(
                    row.pos.x + glyph.logical_rect().center().x + dx,
                    row.min_y() + 2.,
                );
                assert_eq!(cache.cursor_from_pos(&g, p), g.cursor_from_pos(p));
            }
        }
        for x in [-100., g.size().x + 100.] {
            let p = Vec2::new(x, row.min_y() + 3.);
            assert_eq!(cache.cursor_from_pos(&g, p), g.cursor_from_pos(p));
        }
    }
    #[test]
    fn nonmonotone_ink_overhang_and_hit_positions_are_not_silently_dropped() {
        let (g, tex) = layout(&"abcdef ".repeat(300), 1.);
        let mut changed = (*g).clone();
        let row = Arc::make_mut(&mut changed.rows[0].row);
        let v = row.glyphs[1000].first_vertex as usize;
        for point in &mut row.visuals.mesh.vertices[v..v + 4] {
            point.pos.x -= row.glyphs[1000].pos.x;
        }
        row.glyphs[1001].pos.x = -3.;
        let changed = Arc::new(changed);
        let original = full(&changed, Pos2::ZERO, tex);
        let clip = Rect::from_min_size(Pos2::ZERO, Vec2::new(100., 60.));
        let mut c = Cache::default();
        let (mesh, _) = c.mesh(&changed, Pos2::ZERO, clip, tex, Color32::WHITE);
        assert_eq!(visible(&mesh, clip), visible(&original, clip));
        for x in [-10., 0., 3., 100., 5000.] {
            let p = Vec2::new(x, 10.);
            assert_eq!(c.cursor_from_pos(&changed, p), changed.cursor_from_pos(p));
        }
    }
    #[test]
    fn real_half_million_glyphs_submit_only_visible_geometry_and_reuse_index() {
        let (g, tex) = layout(&"x".repeat(500000), 1.);
        let mut cache = Cache::default();
        cache.begin_frame(1);
        let clip = Rect::from_min_size(Pos2::new(g.size().x - 800., 0.), Vec2::new(700., 40.));
        let (_, first) = cache.mesh(&g, Pos2::ZERO, clip, tex, Color32::WHITE);
        assert_eq!(first.newly_indexed_glyphs, 500000);
        assert!(
            first.source_vertices >= 2000000
                && first.submitted_vertices < 1000
                && first.candidate_glyphs <= 512
        );
        cache.begin_frame(2);
        let (_, again) = cache.mesh(&g, Pos2::ZERO, clip, tex, Color32::WHITE);
        assert_eq!(again.newly_indexed_glyphs, 0);
        drop(g);
        cache.begin_frame(3);
        assert!(cache.entries.is_empty());
    }
}
