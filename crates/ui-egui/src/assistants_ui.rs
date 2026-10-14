//! Drawing assistants: the canvas overlay, handle drags and the options-bar popup.
//!
//! Guides draw through the document's own
//! [`photocraft_doc::DrawingAssistant::overlay_primitives`] (the UI never computes guide
//! geometry); handle drags commit the `assist.*` commands, coalesced so one drag is one undo
//! step. Strokes themselves snap in the engine pipeline, so the overlay and the paint agree.

use crate::{
    PhotocraftApp,
    canvas::{ToolEvent, ViewXform},
    theme::Tokens,
    widgets,
};
use egui::{Pos2, Stroke};
use photocraft_doc::{AssistantId, DrawingAssistant, DrawingAssistantKind, GuidePrimitive, HandleKind};
use serde_json::json;

/// An assistant handle drag in progress (cleared when the assistant goes away).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AssistantDrag {
    pub id: u64,
    pub handle: HandleKind,
}

/// Screen points of tolerance around a handle (`8 / zoom` document pixels).
fn tolerance(xf: &ViewXform) -> f64 {
    8.0 / f64::from(xf.zoom.max(0.001))
}

/// Viewport (document pixels) covering the canvas rect, through rotation and flip.
fn viewport(xf: &ViewXform) -> [f64; 4] {
    let corners = [
        xf.to_doc(xf.rect.min),
        xf.to_doc(Pos2::new(xf.rect.max.x, xf.rect.min.y)),
        xf.to_doc(xf.rect.max),
        xf.to_doc(Pos2::new(xf.rect.min.x, xf.rect.max.y)),
    ];
    [
        corners.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min),
        corners.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min),
        corners.iter().map(|p| p[0]).fold(f64::NEG_INFINITY, f64::max),
        corners.iter().map(|p| p[1]).fold(f64::NEG_INFINITY, f64::max),
    ]
}

fn shown(app: &PhotocraftApp) -> bool {
    app.ui.view.shows(app.ui.view.show.assistants)
}

/// Every visible assistant's handles as (id, handle, document position).
fn handles(app: &PhotocraftApp, vp: [f64; 4]) -> Vec<(u64, HandleKind, [f64; 2])> {
    let Some(st) = app.session.active() else { return Vec::new() };
    if !st.doc.assistants.enabled {
        return Vec::new();
    }
    st.doc
        .assistants
        .items
        .iter()
        .filter(|a| a.visible)
        .flat_map(|a| {
            a.kind.overlay_primitives(vp).into_iter().filter_map(|p| match p {
                GuidePrimitive::Handle { pos, kind } => Some((a.id.0, kind, pos)),
                GuidePrimitive::Segment { .. } => None,
            })
        })
        .collect()
}

fn drag_of(app: &mut PhotocraftApp) -> Option<AssistantDrag> {
    let d = app.ui.assistant_drag?;
    // The assistant may have been deleted or hidden mid-drag.
    let ok = app.session.active().is_some_and(|st| {
        st.doc.assistants.get(AssistantId(d.id)).is_some_and(|a| {
            a.visible
                && match (d.handle, &a.kind) {
                    (HandleKind::Anchor | HandleKind::Angle, DrawingAssistantKind::StraightLine(_) | DrawingAssistantKind::Parallel(_)) => true,
                    (HandleKind::Anchor, DrawingAssistantKind::Concentric(_) | DrawingAssistantKind::Fisheye(_)) => true,
                    (HandleKind::VanishingPoint(i), DrawingAssistantKind::Perspective(p)) => i < p.vps.len(),
                    (HandleKind::VanishingPoint(i), DrawingAssistantKind::Fisheye(f)) => i < f.vps.len(),
                    (HandleKind::ControlPoint(i), DrawingAssistantKind::Bezier(_)) => i < 4,
                    (HandleKind::ControlPoint(i), DrawingAssistantKind::Ellipse(_)) => i < 3,
                    (HandleKind::ControlPoint(i), DrawingAssistantKind::Polygon(_)) => i < 2,
                    (HandleKind::ControlPoint(i), DrawingAssistantKind::Fisheye(_)) => i < 1,
                    _ => false,
                }
        })
    });
    if ok {
        Some(d)
    } else {
        app.ui.assistant_drag = None;
        None
    }
}

/// Move the dragged handle to document point `p` (one coalesced history step per drag).
fn drag_to(app: &mut PhotocraftApp, d: AssistantDrag, p: [f64; 2], snap_angle: bool) {
    let anchor = match d.handle {
        HandleKind::Angle => app.session.active().and_then(|st| st.doc.assistants.get(AssistantId(d.id))).and_then(|a| match &a.kind {
            DrawingAssistantKind::StraightLine(l) => Some(l.anchor),
            DrawingAssistantKind::Parallel(p) => Some(p.anchor),
            _ => None,
        }),
        _ => None,
    };
    let (id, params) = match (d.handle, anchor) {
        (HandleKind::Anchor, _) => ("assist.moveAnchor", json!({"id": d.id, "anchor": p})),
        (HandleKind::Angle, Some(a)) => {
            let mut deg = (p[1] - a[1]).atan2(p[0] - a[0]).to_degrees();
            if snap_angle {
                deg = (deg / 15.0).round() * 15.0;
            }
            ("assist.setAngle", json!({"id": d.id, "angleDeg": deg}))
        }
        (HandleKind::Angle, None) => return,
        (HandleKind::VanishingPoint(i), _) => ("assist.moveVanishingPoint", json!({"id": d.id, "index": i, "point": p})),
        (HandleKind::ControlPoint(i), _) => ("assist.moveControlPoint", json!({"id": d.id, "index": i, "point": p})),
    };
    let mut params = params;
    params["coalesce"] = json!("assist-drag");
    let _ = app.run(id, params);
}

/// Canvas pointer handling: drag assistant handles. Returns true while a drag owns the pointer
/// (so a handle grab never starts a stroke).
pub fn pointer(app: &mut PhotocraftApp, ev: ToolEvent, mods: egui::Modifiers) -> bool {
    if drag_of(app).is_none() {
        // Not dragging: only a press on a handle starts one.
        let ToolEvent::Down { x, y, .. } = ev else { return false };
        if !shown(app) || !app.active_tool().is_brushlike() {
            return false;
        }
        let Some(xf) = ViewXform::active(app) else { return false };
        let tol = tolerance(&xf);
        let hit = handles(app, viewport(&xf))
            .into_iter()
            .filter(|(_, _, h)| (h[0] - x).hypot(h[1] - y) <= tol)
            .min_by(|(_, _, a), (_, _, b)| (a[0] - x).hypot(a[1] - y).partial_cmp(&(b[0] - x).hypot(b[1] - y)).unwrap_or(std::cmp::Ordering::Equal));
        match hit {
            Some((id, handle, _)) => {
                app.ui.assistant_drag = Some(AssistantDrag { id, handle });
                true
            }
            None => false,
        }
    } else {
        match ev {
            ToolEvent::Down { x, y, .. } => {
                // Re-grab: switch to whatever handle is under the press.
                app.ui.assistant_drag = None;
                pointer(app, ToolEvent::Down { x, y, pressure: 1.0 }, mods)
            }
            ToolEvent::Move { x, y, .. } => {
                if let Some(d) = drag_of(app) {
                    drag_to(app, d, [x, y], mods.shift);
                }
                true
            }
            ToolEvent::Up { .. } => {
                app.ui.assistant_drag = None;
                true
            }
        }
    }
}

/// Canvas overlay: guide segments and drag handles of every visible assistant.
///
/// Major guides (the straight line, the horizon) draw solid; the secondary ray fan draws
/// dotted. Everything uses the assistants' own theme colour, so rulers never read as paths,
/// selections or reference guides. Lines stay thin (no halo): the magenta is rare enough in
/// artwork to read on its own, and halos turned the dotted fan into caterpillars.
/// Assistants whose own snapping is off draw dimmed: still editable, visibly inert.
pub fn draw(app: &PhotocraftApp, painter: &egui::Painter, xf: &ViewXform) {
    if !shown(app) {
        return;
    }
    let Some(st) = app.session.active() else { return };
    if !st.doc.assistants.enabled || st.doc.assistants.is_empty() {
        return;
    }
    let vp = viewport(xf);
    let t = Tokens::get(painter.ctx());
    let painter = painter.with_clip_rect(painter.clip_rect().intersect(xf.rect));
    let screen = |p: [f64; 2]| xf.to_screen(p[0] as f32, p[1] as f32);
    let dragging = app.ui.assistant_drag;
    for item in st.doc.assistants.items.iter().filter(|a| a.visible) {
        let color = if item.snapping { t.assistant } else { t.assistant.gamma_multiply(0.4) };
        for p in item.kind.overlay_primitives(vp) {
            match p {
                GuidePrimitive::Segment { a, b, major } => {
                    let (a, b) = (screen(a), screen(b));
                    if major {
                        painter.line_segment([a, b], Stroke::new(1.0, color));
                    } else {
                        painter.add(egui::Shape::dotted_line(&[a, b], color, 6.0, 1.0));
                    }
                }
                GuidePrimitive::Handle { pos, kind } => {
                    let selected = dragging.is_some_and(|d| d.id == item.id.0 && d.handle == kind);
                    draw_handle(&painter, screen(pos), kind, selected, color);
                }
            }
        }
    }
}

/// One assistant handle: circle for the line anchor, square for the angle, diamond for a
/// vanishing point — never the path tool's anchor, so rulers and paths can't be confused.
/// White-filled (assistant-filled when grabbed) with a thin assistant outline.
fn draw_handle(painter: &egui::Painter, c: Pos2, kind: HandleKind, selected: bool, color: egui::Color32) {
    let r = if selected { 5.0 } else { 4.0 };
    let fill = if selected { color } else { egui::Color32::WHITE };
    match kind {
        HandleKind::Anchor => {
            painter.circle_filled(c, r, fill);
            painter.circle_stroke(c, r, Stroke::new(1.0, color));
        }
        HandleKind::Angle => {
            let rect = egui::Rect::from_center_size(c, egui::vec2(2.0 * r, 2.0 * r));
            painter.rect_filled(rect, 0.0, fill);
            painter.rect_stroke(rect, 0.0, Stroke::new(1.0, color), egui::StrokeKind::Inside);
        }
        HandleKind::VanishingPoint(_) => {
            let d = r + 1.0;
            let pts = vec![c + egui::vec2(0.0, -d), c + egui::vec2(d, 0.0), c + egui::vec2(0.0, d), c + egui::vec2(-d, 0.0)];
            painter.add(egui::Shape::convex_polygon(pts, fill, Stroke::new(1.0, color)));
        }
        // Kind-specific control points (Bézier, ellipse, polygon, fisheye radius): a solid
        // dot — smaller than the anchor ring, no white to confuse with path anchors.
        HandleKind::ControlPoint(_) => {
            painter.circle_filled(c, if selected { r } else { r - 1.0 }, color);
        }
    }
}

/// Which slider rows a popup item gets under its row.
#[derive(Clone, Copy, PartialEq)]
enum Extras {
    None,
    /// Ray-fan density (perspective).
    Rays,
    /// Family-line spacing (parallel, concentric).
    Spacing,
    /// Ray density plus lens strength (fisheye).
    Fisheye,
}

/// The popup slider's current spacing for a parallel/concentric item.
fn spacing_of(app: &PhotocraftApp, id: u64) -> Option<f64> {
    let st = app.session.active()?;
    match &st.doc.assistants.get(AssistantId(id))?.kind {
        DrawingAssistantKind::Parallel(p) => Some(p.spacing),
        DrawingAssistantKind::Concentric(c) => Some(c.spacing),
        _ => None,
    }
}

/// The popup slider's current lens strength for a fisheye item.
fn strength_of(app: &PhotocraftApp, id: u64) -> Option<f64> {
    let st = app.session.active()?;
    match &st.doc.assistants.get(AssistantId(id))?.kind {
        DrawingAssistantKind::Fisheye(f) => Some(f.strength),
        _ => None,
    }
}

/// The popup slider's current value: the smallest angular gap in the first vanishing
/// point's fan (exact for default fans, a reasonable proxy for hand-built ones).
fn ray_step_of(app: &PhotocraftApp, id: u64) -> Option<f64> {
    let st = app.session.active()?;
    let item = st.doc.assistants.get(AssistantId(id))?;
    let rays = match &item.kind {
        DrawingAssistantKind::Perspective(p) => &p.rays,
        DrawingAssistantKind::Fisheye(f) => &f.rays,
        _ => return None,
    };
    let mut angles: Vec<f64> = rays.iter().filter(|r| r.vp == 0).map(|r| r.angle_deg).collect();
    if angles.len() < 2 {
        return None;
    }
    angles.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut best = f64::INFINITY;
    for w in angles.windows(2) {
        let gap = (w[1] - w[0]).abs();
        if gap > 1e-9 && gap < best {
            best = gap;
        }
    }
    best.is_finite().then_some(best)
}

/// Options-bar popup: the "Snap to Assistants" switch, magnetism, and assistant management.
pub fn menu(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let count = app.session.active().map(|st| st.doc.assistants.items.len()).unwrap_or(0);
    let on = app.session.active().is_some_and(|st| st.doc.assistants.enabled);
    let resp = crate::icons::button_with_icon_size(ui, "ruler", 24.0, 14.0, on && count > 0, tl!("Drawing assistants: snap strokes to guides"));
    let resp = crate::brush_picker::named(resp, "Drawing assistants: snap strokes to guides");
    egui::Popup::menu(&resp).show(|ui| {
        ui.set_min_width(240.0);
        widgets::menu_scroll(ui, |ui| {
            let mut run: Vec<(&str, serde_json::Value)> = Vec::new();
            let (mut enabled, radius) = app
                .session
                .active()
                .map(|st| (st.doc.assistants.enabled, st.doc.assistants.snap_radius))
                .unwrap_or((true, photocraft_doc::DEFAULT_SNAP_RADIUS));
            if ui.checkbox(&mut enabled, tl!("Snap to Assistants")).changed() {
                run.push(("assist.setSnap", json!({"enabled": enabled})));
            }
            ui.horizontal(|ui| {
                ui.label(tl!("Magnetism"));
                let mut px = radius;
                let resp = ui.add(egui::DragValue::new(&mut px).range(0.0..=photocraft_doc::MAX_SNAP_RADIUS).speed(0.5).suffix(" px"));
                if resp.changed() {
                    let mut p = json!({"radius": px});
                    p["coalesce"] = json!("assist-radius");
                    run.push(("assist.setSnap", p));
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                for (label, cmd) in [
                    (tl!("Straight Line"), "assist.addStraightLine"),
                    (tl!("Perspective"), "assist.addPerspective"),
                    (tl!("Bezier"), "assist.addBezier"),
                    (tl!("Parallel"), "assist.addParallel"),
                ] {
                    if ui.button(label).clicked() {
                        run.push((cmd, json!({})));
                        ui.close();
                    }
                }
            });
            ui.horizontal(|ui| {
                for (label, cmd) in [
                    (tl!("Ellipse"), "assist.addEllipse"),
                    (tl!("Concentric"), "assist.addConcentric"),
                    (tl!("Polygon"), "assist.addPolygon"),
                    (tl!("Fisheye"), "assist.addFisheye"),
                ] {
                    if ui.button(label).clicked() {
                        run.push((cmd, json!({})));
                        ui.close();
                    }
                }
            });
            // (id, name, visible, snapping, kind label, extras row).
            let items: Vec<(u64, String, bool, bool, String, Extras)> = app
                .session
                .active()
                .map(|st| {
                    st.doc
                        .assistants
                        .items
                        .iter()
                        .map(|a| {
                            let (kind, extras) = match &a.kind {
                                DrawingAssistantKind::StraightLine(_) => (tl!("Line").to_string(), Extras::None),
                                DrawingAssistantKind::Perspective(_) => (tl!("Perspective").to_string(), Extras::Rays),
                                DrawingAssistantKind::Bezier(_) => (tl!("Bezier").to_string(), Extras::None),
                                DrawingAssistantKind::Parallel(_) => (tl!("Parallel").to_string(), Extras::Spacing),
                                DrawingAssistantKind::Ellipse(_) => (tl!("Ellipse").to_string(), Extras::None),
                                DrawingAssistantKind::Concentric(_) => (tl!("Concentric").to_string(), Extras::Spacing),
                                DrawingAssistantKind::Polygon(_) => (tl!("Polygon").to_string(), Extras::None),
                                DrawingAssistantKind::Fisheye(_) => (tl!("Fisheye").to_string(), Extras::Fisheye),
                            };
                            (a.id.0, a.name.clone(), a.visible, a.snapping, kind, extras)
                        })
                        .collect()
                })
                .unwrap_or_default();
            if !items.is_empty() {
                ui.separator();
            }
            for (id, name, visible, snapping, kind, extras) in items {
                ui.horizontal(|ui| {
                    let icon = if visible { "eye" } else { "eye-off" };
                    if crate::icons::button(ui, icon, 22.0, visible, tl!("Show/hide this assistant")).clicked() {
                        run.push(("assist.setVisible", json!({"id": id, "visible": !visible})));
                    }
                    // Per-assistant snapping: the overlay dims while off, strokes pass through.
                    let mut snap = snapping;
                    if crate::widgets::checkbox(ui, &mut snap, "").on_hover_text(tl!("Snap strokes to this assistant")).changed() {
                        run.push(("assist.setSnapping", json!({"id": id, "snapping": snap})));
                    }
                    // Inline rename: pencil toggles a one-line edit, ↩ commits, Esc cancels.
                    let editing = app.ui.assistant_rename.as_ref().is_some_and(|(eid, _)| *eid == id);
                    if editing {
                        let draft = app.ui.assistant_rename.as_mut().map(|(_, d)| d).expect("editing");
                        let resp = ui.add(egui::TextEdit::singleline(draft).desired_width(120.0).hint_text(kind.as_str()));
                        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            let done = app.ui.assistant_rename.take().map(|(_, d)| d).unwrap_or_default();
                            if !done.trim().is_empty() && done.trim() != name {
                                run.push(("assist.rename", json!({"id": id, "name": done.trim()})));
                            }
                        }
                        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            app.ui.assistant_rename = None;
                        }
                    } else {
                        ui.label(format!("{name} ({kind})"));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if crate::icons::button(ui, "trash", 22.0, false, tl!("Delete this assistant")).clicked() {
                            run.push(("assist.remove", json!({"id": id})));
                        }
                        if crate::icons::button(ui, "pencil", 22.0, false, tl!("Rename this assistant")).clicked() {
                            app.ui.assistant_rename = Some((id, name.clone()));
                        }
                    });
                });
                // Per-kind sliders under the row: ray density, guide spacing, lens strength.
                match extras {
                    Extras::None => {}
                    Extras::Rays => {
                        ui.horizontal(|ui| {
                            ui.label(tl!("Ray step"));
                            let mut step = ray_step_of(app, id).unwrap_or(15.0);
                            let resp = ui.add(egui::DragValue::new(&mut step).range(1.0..=180.0).speed(0.5).suffix("°"));
                            if resp.changed() {
                                let mut p = json!({"id": id, "rayStep": step});
                                p["coalesce"] = json!("assist-raystep");
                                run.push(("assist.setRayStep", p));
                            }
                        });
                    }
                    Extras::Spacing => {
                        ui.horizontal(|ui| {
                            ui.label(tl!("Spacing"));
                            let mut gap = spacing_of(app, id).unwrap_or(20.0);
                            let resp = ui.add(egui::DragValue::new(&mut gap).range(1.0..=1000000.0).speed(0.5).suffix(" px"));
                            if resp.changed() {
                                let mut p = json!({"id": id, "spacing": gap});
                                p["coalesce"] = json!("assist-spacing");
                                run.push(("assist.setSpacing", p));
                            }
                        });
                    }
                    Extras::Fisheye => {
                        ui.horizontal(|ui| {
                            ui.label(tl!("Ray step"));
                            let mut step = ray_step_of(app, id).unwrap_or(15.0);
                            let resp = ui.add(egui::DragValue::new(&mut step).range(1.0..=180.0).speed(0.5).suffix("°"));
                            if resp.changed() {
                                let mut p = json!({"id": id, "rayStep": step});
                                p["coalesce"] = json!("assist-raystep");
                                run.push(("assist.setRayStep", p));
                            }
                            ui.label(tl!("Strength"));
                            let mut k = strength_of(app, id).unwrap_or(0.65);
                            let resp = ui.add(egui::DragValue::new(&mut k).range(0.0..=1.0).speed(0.01));
                            if resp.changed() {
                                let mut p = json!({"id": id, "strength": k});
                                p["coalesce"] = json!("assist-strength");
                                run.push(("assist.setFisheyeStrength", p));
                            }
                        });
                    }
                }
            }
            for (id, params) in run {
                let _ = app.run(id, params);
            }
        });
    });
}

#[cfg(test)]
mod tests;
