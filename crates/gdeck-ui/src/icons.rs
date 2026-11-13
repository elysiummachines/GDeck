//! Small vector glyphs drawn with the painter.
//!
//! Drawn rather than loaded so the binary carries no image assets and the
//! icons stay crisp at any scale.

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};

fn stroke(color: Color32) -> Stroke {
    Stroke::new(1.6, color)
}

/// Lightning bolt: launch.
pub fn launch(painter: &egui::Painter, rect: Rect, color: Color32) {
    let p = |x: f32, y: f32| Pos2::new(rect.left() + rect.width() * x, rect.top() + rect.height() * y);
    let points = vec![
        p(0.62, 0.05),
        p(0.20, 0.55),
        p(0.46, 0.55),
        p(0.38, 0.95),
        p(0.80, 0.45),
        p(0.54, 0.45),
    ];
    painter.add(egui::Shape::convex_polygon(
        points,
        Color32::TRANSPARENT,
        stroke(color),
    ));
}

/// Folder: open a path or URL.
pub fn open(painter: &egui::Painter, rect: Rect, color: Color32) {
    let p = |x: f32, y: f32| Pos2::new(rect.left() + rect.width() * x, rect.top() + rect.height() * y);
    painter.add(egui::Shape::closed_line(
        vec![
            p(0.10, 0.80),
            p(0.10, 0.22),
            p(0.42, 0.22),
            p(0.52, 0.34),
            p(0.90, 0.34),
            p(0.90, 0.80),
        ],
        stroke(color),
    ));
}

/// Page with a folded corner: note.
pub fn note(painter: &egui::Painter, rect: Rect, color: Color32) {
    let p = |x: f32, y: f32| Pos2::new(rect.left() + rect.width() * x, rect.top() + rect.height() * y);
    painter.add(egui::Shape::closed_line(
        vec![
            p(0.18, 0.10),
            p(0.66, 0.10),
            p(0.82, 0.30),
            p(0.82, 0.90),
            p(0.18, 0.90),
        ],
        stroke(color),
    ));
    painter.line_segment([p(0.32, 0.46), p(0.68, 0.46)], stroke(color));
    painter.line_segment([p(0.32, 0.64), p(0.56, 0.64)], stroke(color));
}

/// Console window: shell command.
pub fn shell(painter: &egui::Painter, rect: Rect, color: Color32) {
    let p = |x: f32, y: f32| Pos2::new(rect.left() + rect.width() * x, rect.top() + rect.height() * y);
    painter.rect_stroke(
        Rect::from_two_pos(p(0.10, 0.18), p(0.90, 0.82)),
        egui::CornerRadius::same(2),
        stroke(color),
        egui::StrokeKind::Inside,
    );
    painter.add(egui::Shape::line(
        vec![p(0.26, 0.38), p(0.44, 0.50), p(0.26, 0.62)],
        stroke(color),
    ));
    painter.line_segment([p(0.52, 0.64), p(0.72, 0.64)], stroke(color));
}

/// Plus: nothing bound yet.
pub fn empty(painter: &egui::Painter, rect: Rect, color: Color32) {
    let centre = rect.center();
    let arm = rect.width() * 0.30;
    painter.line_segment(
        [centre - Vec2::new(arm, 0.0), centre + Vec2::new(arm, 0.0)],
        stroke(color),
    );
    painter.line_segment(
        [centre - Vec2::new(0.0, arm), centre + Vec2::new(0.0, arm)],
        stroke(color),
    );
}

/// Warning triangle, for a key that runs a shell command.
pub fn warning(painter: &egui::Painter, rect: Rect, color: Color32) {
    let p = |x: f32, y: f32| Pos2::new(rect.left() + rect.width() * x, rect.top() + rect.height() * y);
    painter.add(egui::Shape::closed_line(
        vec![p(0.5, 0.12), p(0.95, 0.88), p(0.05, 0.88)],
        Stroke::new(1.4, color),
    ));
    painter.line_segment([p(0.5, 0.40), p(0.5, 0.62)], Stroke::new(1.4, color));
    painter.line_segment([p(0.5, 0.74), p(0.5, 0.76)], Stroke::new(1.6, color));
}

/// Draw the glyph matching an action type name.
pub fn for_action(painter: &egui::Painter, rect: Rect, kind: &str, color: Color32) {
    match kind {
        "launch" => launch(painter, rect, color),
        "open" => open(painter, rect, color),
        "note" => note(painter, rect, color),
        "shell" => shell(painter, rect, color),
        _ => empty(painter, rect, color),
    }
}
