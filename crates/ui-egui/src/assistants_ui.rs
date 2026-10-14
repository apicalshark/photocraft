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
use photocraft_doc::{AssistantId, DrawingAssistant, DrawingAssistantKind, GuidePrimitive, HandleKind, LabelColor};
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
/// All guides draw as thin solid lines in the assistant's own colour (its label colour, or
/// the theme magenta when unlabelled), so rulers never read as paths, selections or
/// reference guides. Assistants whose own snapping is off draw dimmed: still editable,
/// visibly inert.
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
        let mut color = t.assistant_color(item.color);
        if !item.snapping {
            color = color.gamma_multiply(0.4);
        }
        for p in item.kind.overlay_primitives(vp) {
            match p {
                GuidePrimitive::Segment { a, b, .. } => {
                    let (a, b) = (screen(a), screen(b));
                    painter.line_segment([a, b], Stroke::new(1.0, color));
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
struct Extras {
    rays: bool,
    spacing: bool,
    strength: bool,
    length: bool,
    count: bool,
}

/// One brush-picker-style parameter row: dim label left, value field right, and a bare
/// full-width slider below — the same chrome as the brush Size/Hardness block. Values cross
/// as `f32` (plenty for UI ranges); the run queue carries the `f64` back to the command.
#[allow(clippy::too_many_arguments)]
fn slider_row(
    ui: &mut egui::Ui,
    label: &str,
    value: f64,
    range: std::ops::RangeInclusive<f32>,
    suffix: &str,
    decimals: Option<u32>,
    run: &mut Vec<(&str, serde_json::Value)>,
    id: u64,
    cmd: &'static str,
    key: &'static str,
    coalesce: &'static str,
) {
    let mut v = value as f32;
    let resp = widgets::slider_row_prec(ui, label, &mut v, range, suffix, None, widgets::RowGestures::default(), decimals);
    if resp.changed() {
        let mut p = json!({"id": id});
        p[key] = json!(f64::from(v));
        p["coalesce"] = json!(coalesce);
        run.push((cmd, p));
    }
}

/// Guide length: value field plus a logarithmic slider (brush-Size style: small lengths get
/// most of the travel, huge canvases stay reachable).
fn length_row(ui: &mut egui::Ui, id: u64, value: f64, run: &mut Vec<(&str, serde_json::Value)>) {
    let t = Tokens::get(ui.ctx());
    let mut v = (value.max(1.0) as f32).clamp(1.0, 1_000_000.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(tl!("Length")).color(t.text_dim));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if widgets::value_field(ui, &mut v, 1.0..=1_000_000.0, " px", 72.0).changed() {
                push_num(run, id, "assist.setLength", "length", "assist-length", f64::from(v));
            }
        });
    });
    let mut lv = v.max(1.0).ln();
    if widgets::slider(ui, &mut lv, 0.0..=1_000_000.0_f32.ln(), None).changed() {
        push_num(run, id, "assist.setLength", "length", "assist-length", f64::from(lv.exp()));
    }
}

/// Parallel line count: whole lines 1–16.
fn count_row(ui: &mut egui::Ui, id: u64, value: u32, run: &mut Vec<(&str, serde_json::Value)>) {
    let mut v = value as f32;
    let resp = widgets::slider_row_prec(ui, tl!("Lines"), &mut v, 1.0..=16.0, "", None, widgets::RowGestures::default(), Some(0));
    if resp.changed() {
        let mut p = json!({"id": id});
        p["count"] = json!(v.round() as u32);
        p["coalesce"] = json!("assist-count");
        run.push(("assist.setParallelCount", p));
    }
}

fn push_num(run: &mut Vec<(&str, serde_json::Value)>, id: u64, cmd: &'static str, key: &'static str, coalesce: &'static str, value: f64) {
    let mut p = json!({"id": id});
    p[key] = json!(value);
    p["coalesce"] = json!(coalesce);
    run.push((cmd, p));
}

/// The popup slider's current guide length, if the kind has one.
fn length_of(app: &PhotocraftApp, id: u64) -> Option<f64> {
    let st = app.session.active()?;
    match &st.doc.assistants.get(AssistantId(id))?.kind {
        DrawingAssistantKind::StraightLine(l) => Some(l.length),
        DrawingAssistantKind::Parallel(p) => Some(p.length),
        DrawingAssistantKind::Perspective(p) => Some(p.length),
        _ => None,
    }
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

/// The popup slider's current line count for a parallel item.
fn count_of(app: &PhotocraftApp, id: u64) -> Option<u32> {
    let st = app.session.active()?;
    match &st.doc.assistants.get(AssistantId(id))?.kind {
        DrawingAssistantKind::Parallel(p) => Some(p.count.max(1)),
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

/// The row's color chip: the assistant's label colour (theme magenta when unlabelled).
/// Opens the shared label-colour choices, like the Layers panel does.
fn color_swatch(app: &PhotocraftApp, ui: &mut egui::Ui, id: u64, current: LabelColor, run: &mut Vec<(&str, serde_json::Value)>) {
    let t = Tokens::get(ui.ctx());
    let chip = t.assistant_color(current);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::click());
    ui.painter().rect_filled(rect, 2.0, chip);
    ui.painter().rect_stroke(rect, 2.0, Stroke::new(1.0, t.field_border), egui::StrokeKind::Inside);
    let resp = resp.on_hover_text(format!("{}: {}", tl!("Color"), crate::layer_menu_ui::color_name(current)));
    egui::Popup::menu(&resp).show(|ui| {
        widgets::menu_scroll(ui, |ui| {
            ui.set_min_width(150.0);
            for color in LabelColor::ALL {
                let (text, checked) = (crate::layer_menu_ui::color_name(color), color == current);
                if ui.add(egui::Button::selectable(checked, text)).clicked() {
                    run.push(("assist.setColor", json!({"id": id, "color": color.id()})));
                    ui.close();
                }
            }
        });
    });
}

/// Options-bar popup: snap switch, magnetism, and the assistant list.
///
/// Fixed 280 px card: the header (switch + magnetism + New dropdown) stays put while
/// only the assistant list scrolls. Each assistant is one section — header row, snap
/// row, then its parameter sliders indented — separated by hairlines, never bare
/// separators. All chrome comes from `widgets`/`Tokens`, like the brush picker.
pub fn menu(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    const WIDTH: f32 = 280.0;
    let count = app.session.active().map(|st| st.doc.assistants.items.len()).unwrap_or(0);
    let on = app.session.active().is_some_and(|st| st.doc.assistants.enabled);
    let resp = crate::icons::button_with_icon_size(ui, "ruler", 24.0, 14.0, on && count > 0, tl!("Drawing assistants: snap strokes to guides"));
    let resp = crate::brush_picker::named(resp, "Drawing assistants: snap strokes to guides");
    egui::Popup::menu(&resp).show(|ui| {
        ui.set_min_width(WIDTH);
        ui.set_max_width(WIDTH);
        let mut run: Vec<(&str, serde_json::Value)> = Vec::new();
        let (mut enabled, radius) = app
            .session
            .active()
            .map(|st| (st.doc.assistants.enabled, st.doc.assistants.snap_radius))
            .unwrap_or((true, photocraft_doc::DEFAULT_SNAP_RADIUS));
        // Header: global switch, then magnetism — the brush picker's own chrome.
        if crate::widgets::checkbox(ui, &mut enabled, tl!("Snap to Assistants")).changed() {
            run.push(("assist.setSnap", json!({"enabled": enabled})));
        }
        {
            let mut px = radius as f32;
            let resp = widgets::slider_row(ui, tl!("Magnetism"), &mut px, 0.0..=photocraft_doc::MAX_SNAP_RADIUS as f32, " px", None);
            if resp.changed() {
                let mut p = json!({"radius": f64::from(px)});
                p["coalesce"] = json!("assist-radius");
                run.push(("assist.setSnap", p));
            }
        }
        widgets::hairline(ui);
        // New-assistant dropdown: the studio chevron combo, full width, placeholder text.
        {
            let kinds: [(&str, &str); 8] = [
                (tl!("Straight Line"), "assist.addStraightLine"),
                (tl!("Perspective"), "assist.addPerspective"),
                (tl!("Bezier"), "assist.addBezier"),
                (tl!("Parallel"), "assist.addParallel"),
                (tl!("Ellipse"), "assist.addEllipse"),
                (tl!("Concentric"), "assist.addConcentric"),
                (tl!("Polygon"), "assist.addPolygon"),
                (tl!("Fisheye"), "assist.addFisheye"),
            ];
            egui::ComboBox::from_id_salt("assist-new")
                .selected_text(tl!("New Assistant"))
                .width(WIDTH - 16.0)
                .icon(widgets::chevron_icon)
                .show_ui(ui, |ui| {
                    for (label, cmd) in kinds {
                        if ui.selectable_label(false, label).clicked() {
                            run.push((cmd, json!({})));
                        }
                    }
                });
        }
        widgets::hairline(ui);
        // (id, name, visible, snapping, color, kind label, extras row).
        let items: Vec<(u64, String, bool, bool, LabelColor, String, Extras)> = app
            .session
            .active()
            .map(|st| {
                st.doc
                    .assistants
                    .items
                    .iter()
                    .map(|a| {
                        let none = Extras { rays: false, spacing: false, strength: false, length: false, count: false };
                        let (kind, extras) = match &a.kind {
                            DrawingAssistantKind::StraightLine(_) => (tl!("Line").to_string(), Extras { length: true, ..none }),
                            DrawingAssistantKind::Perspective(_) => (tl!("Perspective").to_string(), Extras { rays: true, length: true, ..none }),
                            DrawingAssistantKind::Bezier(_) => (tl!("Bezier").to_string(), none),
                            DrawingAssistantKind::Parallel(_) => (tl!("Parallel").to_string(), Extras { spacing: true, length: true, count: true, ..none }),
                            DrawingAssistantKind::Ellipse(_) => (tl!("Ellipse").to_string(), none),
                            DrawingAssistantKind::Concentric(_) => (tl!("Concentric").to_string(), Extras { spacing: true, ..none }),
                            DrawingAssistantKind::Polygon(_) => (tl!("Polygon").to_string(), none),
                            DrawingAssistantKind::Fisheye(_) => (tl!("Fisheye").to_string(), Extras { rays: true, strength: true, ..none }),
                        };
                        (a.id.0, a.name.clone(), a.visible, a.snapping, a.color, kind, extras)
                    })
                    .collect()
            })
            .unwrap_or_default();
        if items.is_empty() {
            let t = Tokens::get(ui.ctx());
            ui.label(egui::RichText::new(tl!("No assistants yet — pick a kind above.")).color(t.text_dim).small());
        } else {
            widgets::menu_scroll(ui, |ui| {
                ui.set_min_width(WIDTH - 16.0);
                let last = items.len() - 1;
                for (n, (id, name, visible, snapping, color, kind, extras)) in items.into_iter().enumerate() {
                    // Header row: chip, elided name (click to rename), eye + delete right.
                    ui.horizontal(|ui| {
                        color_swatch(app, ui, id, color, &mut run);
                        let editing = app.ui.assistant_rename.as_ref().is_some_and(|(eid, _)| *eid == id);
                        if editing {
                            let draft = app.ui.assistant_rename.as_mut().map(|(_, d)| d).expect("editing");
                            let resp = ui.add(egui::TextEdit::singleline(draft).desired_width(140.0).hint_text(kind.as_str()));
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
                            // Name elides; the kind rides beside it as a dim caption.
                            let t = Tokens::get(ui.ctx());
                            let resp = crate::widgets::elided_label_sensed(ui, name.as_str(), 130.0, egui::Sense::click());
                            if resp.clicked() {
                                app.ui.assistant_rename = Some((id, name.clone()));
                            }
                            ui.label(egui::RichText::new(kind.as_str()).color(t.text_dim).small());
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if crate::icons::button(ui, "trash", 22.0, false, tl!("Delete this assistant")).clicked() {
                                run.push(("assist.remove", json!({"id": id})));
                            }
                            let icon = if visible { "eye" } else { "eye-off" };
                            if crate::icons::button(ui, icon, 22.0, visible, tl!("Show/hide this assistant")).clicked() {
                                run.push(("assist.setVisible", json!({"id": id, "visible": !visible})));
                            }
                        });
                    });
                    // Snap row: a labelled checkbox, not a bare box — its meaning is visible.
                    {
                        let mut snap = snapping;
                        if crate::widgets::checkbox(ui, &mut snap, tl!("Snap")).changed() {
                            run.push(("assist.setSnapping", json!({"id": id, "snapping": snap})));
                        }
                    }
                    // Per-kind parameters, indented under their assistant.
                    ui.scope(|ui| {
                        ui.spacing_mut().indent = 20.0;
                        ui.indent(format!("assist-extras-{id}"), |ui| {
                            if extras.rays {
                                slider_row(
                                    ui,
                                    tl!("Ray step"),
                                    ray_step_of(app, id).unwrap_or(15.0),
                                    1.0..=180.0,
                                    "°",
                                    None,
                                    &mut run,
                                    id,
                                    "assist.setRayStep",
                                    "rayStep",
                                    "assist-raystep",
                                );
                            }
                            if extras.spacing {
                                slider_row(
                                    ui,
                                    tl!("Spacing"),
                                    spacing_of(app, id).unwrap_or(20.0),
                                    1.0..=500.0,
                                    " px",
                                    None,
                                    &mut run,
                                    id,
                                    "assist.setSpacing",
                                    "spacing",
                                    "assist-spacing",
                                );
                            }
                            if extras.count {
                                count_row(ui, id, count_of(app, id).unwrap_or(2), &mut run);
                            }
                            if extras.strength {
                                slider_row(
                                    ui,
                                    tl!("Strength"),
                                    strength_of(app, id).unwrap_or(0.65),
                                    0.0..=1.0,
                                    "",
                                    Some(2),
                                    &mut run,
                                    id,
                                    "assist.setFisheyeStrength",
                                    "strength",
                                    "assist-strength",
                                );
                            }
                            if extras.length {
                                length_row(ui, id, length_of(app, id).unwrap_or(100.0), &mut run);
                            }
                        });
                    });
                    if n != last {
                        widgets::hairline(ui);
                    }
                }
            });
        }
        for (id, params) in run {
            let _ = app.run(id, params);
        }
    });
}

#[cfg(test)]
mod tests;
