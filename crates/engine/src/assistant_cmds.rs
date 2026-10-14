//! Drawing assistants: document-stored snapping guides (resolving #2293).
//!
//! Assistants live in [`photocraft_doc::DrawingAssistants`] on the document, so adding, moving or
//! deleting one is a history step (undo/redo free through [`Session::edit`]) and saves to
//! `.pcraft`. The stroke pipeline ([`snap_points`]) runs every brush point through the set
//! before dab generation, keeping pressure, tilt, rotation, wheel and time untouched. The canvas
//! overlay reads the same set through [`photocraft_doc::DrawingAssistant::overlay_primitives`].

use photocraft_doc::assistants::is_valid_snap_radius;
use photocraft_doc::{
    AssistantId, BezierAssistant, ConcentricAssistant, DrawingAssistantItem, DrawingAssistantKind, EllipseAssistant, FisheyeAssistant, LabelColor,
    MAX_ASSISTANTS, MAX_SNAP_RADIUS, ParallelAssistant, PerspectiveAssistant, PolygonAssistant, StraightLineAssistant,
};
use photocraft_paint::StrokePoint;
use serde_json::{Value, json};

use crate::brush_cmds::MAX_COORD;
use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

fn has_doc(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document open".into())
}

/// Snap `pts` onto an assistant set, preserving every stylus channel but the position.
/// Points that snap outside the brush pipeline's coordinate range (a projection can overshoot
/// its input along a far anchor's line) keep their raw position instead: the pipeline never
/// sees coordinates it did not validate.
pub fn snap_stroke_points(set: &photocraft_doc::DrawingAssistants, pts: &[StrokePoint]) -> Vec<StrokePoint> {
    if !set.enabled || set.items.iter().all(|a| !a.visible || !a.snapping) {
        return pts.to_vec();
    }
    pts.iter()
        .map(|p| match set.snap_point([p.x, p.y]) {
            Some(hit) if hit.point[0].abs() <= MAX_COORD && hit.point[1].abs() <= MAX_COORD => StrokePoint { x: hit.point[0], y: hit.point[1], ..*p },
            _ => *p,
        })
        .collect()
}

/// [`snap_stroke_points`] against the active document's assistants (a no-op without one).
pub fn snap_points(s: &Session, pts: &[StrokePoint]) -> Vec<StrokePoint> {
    let Some(st) = s.active() else { return pts.to_vec() };
    snap_stroke_points(&st.doc.assistants, pts)
}

fn id_param(p: &Value, cmd: &str) -> Result<AssistantId> {
    p.get("id").and_then(Value::as_u64).map(AssistantId).ok_or_else(|| bad(cmd, "missing `id`"))
}

fn item_json(a: &DrawingAssistantItem) -> Value {
    let (kind, detail) = match &a.kind {
        DrawingAssistantKind::StraightLine(l) => ("straightLine", json!({"anchor": l.anchor, "angleDeg": l.angle_deg, "length": l.length})),
        DrawingAssistantKind::Perspective(p) => (
            "perspective",
            json!({"points": p.vps, "horizonY": p.horizon_y, "length": p.length, "pinned": p.pinned, "rays": p.rays.iter().map(|r| json!({"vp": r.vp, "angleDeg": r.angle_deg})).collect::<Vec<_>>()}),
        ),
        DrawingAssistantKind::Bezier(b) => ("bezier", json!({"points": b.points})),
        DrawingAssistantKind::Parallel(p) => {
            ("parallel", json!({"anchor": p.anchor, "angleDeg": p.angle_deg, "spacing": p.spacing, "length": p.length, "count": p.count}))
        }
        DrawingAssistantKind::Ellipse(e) => ("ellipse", json!({"center": e.center, "radii": e.radii, "rotationDeg": e.rotation_deg})),
        DrawingAssistantKind::Concentric(c) => ("concentric", json!({"center": c.center, "spacing": c.spacing})),
        DrawingAssistantKind::Polygon(g) => ("polygon", json!({"center": g.center, "radius": g.radius, "sides": g.sides, "rotationDeg": g.rotation_deg})),
        DrawingAssistantKind::Fisheye(f) => (
            "fisheye",
            json!({"center": f.center, "radius": f.radius, "strength": f.strength, "points": f.vps, "horizonY": f.horizon_y, "pinned": f.pinned,
                "rays": f.rays.iter().map(|r| json!({"vp": r.vp, "angleDeg": r.angle_deg})).collect::<Vec<_>>()}),
        ),
    };
    json!({"id": a.id.0, "name": a.name, "visible": a.visible, "snapping": a.snapping, "color": a.color.id(), "kind": kind, "detail": detail})
}

fn list_json(s: &Session) -> Result<Value> {
    let st = s.active().ok_or(EngineError::NoDocument)?;
    Ok(json!({
        "enabled": st.doc.assistants.enabled,
        "snapRadius": st.doc.assistants.snap_radius,
        "items": st.doc.assistants.items.iter().map(item_json).collect::<Vec<_>>(),
    }))
}

fn check_capacity(s: &Session, cmd: &str) -> Result<()> {
    let st = s.active().ok_or(EngineError::NoDocument)?;
    if st.doc.assistants.items.len() >= MAX_ASSISTANTS {
        return Err(bad(cmd, format!("the document already holds {MAX_ASSISTANTS} assistants")));
    }
    Ok(())
}

/// Default guide length: the canvas diagonal, so new rulers span the document finitely.
fn diagonal(s: &Session) -> Result<f64> {
    let st = s.active().ok_or(EngineError::NoDocument)?;
    Ok(f64::from(st.doc.size.width).hypot(f64::from(st.doc.size.height)))
}

fn next_name(s: &Session, prefix: &str) -> String {
    // Number within the document's assistants of any kind ("Perspective 3").
    let n = s.active().map(|st| st.doc.assistants.items.len() + 1).unwrap_or(1);
    format!("{prefix} {n}")
}

fn name_param(s: &Session, p: &Value, prefix: &str) -> String {
    p.get("name").and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty() && n.len() <= 120).map(str::to_string).unwrap_or_else(|| next_name(s, prefix))
}

fn anchor_param(p: &Value, cmd: &str, key: &str, fallback: [f64; 2]) -> Result<[f64; 2]> {
    match p.get(key) {
        None => Ok(fallback),
        Some(v) => {
            let a = v.as_array().filter(|a| a.len() == 2).ok_or_else(|| bad(cmd, format!("`{key}` must be [x, y]")))?;
            let x = a.first().and_then(Value::as_f64).ok_or_else(|| bad(cmd, format!("`{key}` must be [x, y]")))?;
            let y = a.get(1).and_then(Value::as_f64).ok_or_else(|| bad(cmd, format!("`{key}` must be [x, y]")))?;
            Ok([x, y])
        }
    }
}

fn angle_param(p: &Value, cmd: &str, key: &str, fallback: f64) -> Result<f64> {
    match p.get(key) {
        None => Ok(fallback),
        Some(v) => v.as_f64().ok_or_else(|| bad(cmd, format!("`{key}` must be a number (degrees)"))),
    }
}

fn add_straight(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "assist.addStraightLine";
    check_capacity(s, CMD)?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let center = [f64::from(st.doc.size.width) / 2.0, f64::from(st.doc.size.height) / 2.0];
    let anchor = anchor_param(p, CMD, "anchor", center)?;
    let angle = angle_param(p, CMD, "angleDeg", 0.0)?;
    let mut line = StraightLineAssistant::new(anchor, angle)
        .ok_or_else(|| bad(CMD, "`anchor` must be finite coordinates within ±1000000 and `angleDeg` a finite angle"))?;
    line.length = diagonal(s)?;
    let name = name_param(s, p, "Straight Line");
    let mut out = Value::Null;
    s.edit("Add Assistant", |doc, _| {
        let item = DrawingAssistantItem::new(name, DrawingAssistantKind::StraightLine(line));
        out = item_json(&item);
        doc.assistants.items.push(item);
        Ok(())
    })?;
    Ok(out)
}

fn points_param(p: &Value, cmd: &str, fallback: Vec<[f64; 2]>) -> Result<Vec<[f64; 2]>> {
    match p.get("points") {
        None => Ok(fallback),
        Some(v) => {
            let a = v.as_array().ok_or_else(|| bad(cmd, "`points` must be a list of [x, y]"))?;
            if a.is_empty() {
                return Err(bad(cmd, "`points` must name at least one vanishing point"));
            }
            a.iter()
                .map(|q| {
                    let pair = q.as_array().filter(|a| a.len() == 2).ok_or_else(|| bad(cmd, "`points` must be a list of [x, y]"))?;
                    let x = pair.first().and_then(Value::as_f64).ok_or_else(|| bad(cmd, "`points` must be a list of [x, y]"))?;
                    let y = pair.get(1).and_then(Value::as_f64).ok_or_else(|| bad(cmd, "`points` must be a list of [x, y]"))?;
                    Ok([x, y])
                })
                .collect()
        }
    }
}

fn add_perspective(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "assist.addPerspective";
    check_capacity(s, CMD)?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let center = [f64::from(st.doc.size.width) / 2.0, f64::from(st.doc.size.height) / 2.0];
    let points = points_param(p, CMD, vec![center])?;
    let horizon = match p.get("horizonY") {
        None => None,
        Some(v) => Some(v.as_f64().ok_or_else(|| bad(CMD, "`horizonY` must be a number"))?),
    };
    let step = match p.get("rayStep") {
        None => 15.0,
        Some(v) => v.as_f64().ok_or_else(|| bad(CMD, "`rayStep` must be a number (degrees)"))?,
    };
    let mut guide = PerspectiveAssistant::with_default_rays(points, horizon, step)
        .ok_or_else(|| bad(CMD, "need 1–8 finite vanishing points within ±1000000, a finite `horizonY`, and a `rayStep` between 0° and 180°"))?;
    guide.length = diagonal(s)?;
    let name = name_param(s, p, "Perspective");
    let mut out = Value::Null;
    s.edit("Add Assistant", |doc, _| {
        let item = DrawingAssistantItem::new(name, DrawingAssistantKind::Perspective(guide));
        out = item_json(&item);
        doc.assistants.items.push(item);
        Ok(())
    })?;
    Ok(out)
}

fn get_mut(s: &mut Session, cmd: &str, p: &Value) -> Result<AssistantId> {
    let id = id_param(p, cmd)?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    if st.doc.assistants.get(id).is_none() {
        return Err(bad(cmd, format!("no assistant with id {}", id.0)));
    }
    Ok(id)
}

fn remove(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "assist.remove";
    let id = get_mut(s, CMD, p)?;
    s.edit("Delete Assistant", |doc, _| {
        doc.assistants.items.retain(|a| a.id != id);
        Ok(())
    })?;
    Ok(json!({"removed": id.0}))
}

fn set_visible(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "assist.setVisible";
    let id = get_mut(s, CMD, p)?;
    let visible = p.get("visible").and_then(Value::as_bool).ok_or_else(|| bad(CMD, "`visible` must be a boolean"))?;
    s.edit(if visible { "Show Assistant" } else { "Hide Assistant" }, |doc, _| {
        if let Some(a) = doc.assistants.get_mut(id) {
            a.visible = visible;
        }
        Ok(())
    })?;
    Ok(json!({"id": id.0, "visible": visible}))
}

fn set_snap(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "assist.setSnap";
    let enabled = match p.get("enabled") {
        None => None,
        Some(v) => Some(v.as_bool().ok_or_else(|| bad(CMD, "`enabled` must be a boolean"))?),
    };
    let radius = match p.get("radius") {
        None => None,
        Some(v) => {
            let r = v.as_f64().ok_or_else(|| bad(CMD, "`radius` must be a number (document pixels)"))?;
            if !is_valid_snap_radius(r) {
                return Err(bad(CMD, format!("`radius` must be between 0 and {MAX_SNAP_RADIUS}")));
            }
            Some(r)
        }
    };
    if enabled.is_none() && radius.is_none() {
        return Err(bad(CMD, "give `enabled` and/or `radius`"));
    }
    let label = match (enabled, radius) {
        (Some(false), _) => "Disable Assistant Snapping",
        _ => "Assistant Snapping",
    };
    s.edit(label, |doc, _| {
        if let Some(e) = enabled {
            doc.assistants.enabled = e;
        }
        if let Some(r) = radius {
            doc.assistants.snap_radius = r;
        }
        Ok(())
    })?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    Ok(json!({"enabled": st.doc.assistants.enabled, "radius": st.doc.assistants.snap_radius}))
}

fn clear(s: &mut Session, _p: &Value) -> Result<Value> {
    s.active().ok_or(EngineError::NoDocument)?;
    s.edit("Delete Assistants", |doc, _| {
        doc.assistants.items.clear();
        Ok(())
    })?;
    Ok(json!({"cleared": true}))
}

/// Move a straight-line/parallel anchor, or a concentric/fisheye center (canvas handle
/// drag; pass `"coalesce": "assist-drag"` so a drag is one undo step).
fn move_anchor(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "assist.moveAnchor";
    let id = get_mut(s, CMD, p)?;
    let anchor = anchor_param(p, CMD, "anchor", [f64::NAN, f64::NAN])?;
    if p.get("anchor").is_none() {
        return Err(bad(CMD, "missing `anchor`"));
    }
    if !anchor[0].is_finite() || !anchor[1].is_finite() {
        return Err(bad(CMD, "`anchor` must be finite coordinates"));
    }
    s.edit("Move Assistant", |doc, _| {
        let Some(a) = doc.assistants.get_mut(id) else { return Ok(()) };
        match &mut a.kind {
            DrawingAssistantKind::StraightLine(line) => {
                let len = line.length;
                *line = StraightLineAssistant::new(anchor, line.angle_deg).ok_or_else(|| bad(CMD, "`anchor` must be within ±1000000"))?;
                line.length = len;
            }
            DrawingAssistantKind::Parallel(par) => {
                let len = par.length;
                *par = ParallelAssistant::new(anchor, par.angle_deg, par.spacing, par.count).ok_or_else(|| bad(CMD, "`anchor` must be within ±1000000"))?;
                par.length = len;
            }
            DrawingAssistantKind::Concentric(con) => {
                *con = ConcentricAssistant::new(anchor, con.spacing).ok_or_else(|| bad(CMD, "`anchor` must be within ±1000000"))?;
            }
            DrawingAssistantKind::Fisheye(fish) => {
                *fish = FisheyeAssistant::new(anchor, fish.radius, fish.strength, fish.vps.clone(), fish.horizon_y, fish.rays.clone())
                    .ok_or_else(|| bad(CMD, "`anchor` must be within ±1000000"))?;
            }
            _ => return Err(EngineError::BadParams { cmd: CMD.into(), msg: "this assistant has no anchor".into() }),
        }
        Ok(())
    })?;
    Ok(json!({"id": id.0, "anchor": anchor}))
}

/// Rotate a straight-line/parallel assistant about its anchor (canvas angle-handle drag).
fn set_angle(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "assist.setAngle";
    let id = get_mut(s, CMD, p)?;
    let angle = angle_param(p, CMD, "angleDeg", f64::NAN)?;
    if p.get("angleDeg").is_none() {
        return Err(bad(CMD, "missing `angleDeg`"));
    }
    s.edit("Rotate Assistant", |doc, _| {
        let Some(a) = doc.assistants.get_mut(id) else { return Ok(()) };
        match &mut a.kind {
            DrawingAssistantKind::StraightLine(line) => {
                let len = line.length;
                *line = StraightLineAssistant::new(line.anchor, angle).ok_or_else(|| bad(CMD, "`angleDeg` must be a finite angle"))?;
                line.length = len;
            }
            DrawingAssistantKind::Parallel(par) => {
                let len = par.length;
                *par = ParallelAssistant::new(par.anchor, angle, par.spacing, par.count).ok_or_else(|| bad(CMD, "`angleDeg` must be a finite angle"))?;
                par.length = len;
            }
            _ => return Err(EngineError::BadParams { cmd: CMD.into(), msg: "this assistant has no angle".into() }),
        }
        Ok(())
    })?;
    Ok(json!({"id": id.0, "angleDeg": angle}))
}

/// Move one vanishing point (canvas handle drag); its rays follow.
fn move_vanishing_point(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "assist.moveVanishingPoint";
    let id = get_mut(s, CMD, p)?;
    let index = p.get("index").and_then(Value::as_u64).ok_or_else(|| bad(CMD, "`index` must be a whole number"))?;
    let point = anchor_param(p, CMD, "point", [f64::NAN, f64::NAN])?;
    if p.get("point").is_none() {
        return Err(bad(CMD, "missing `point`"));
    }
    s.edit("Move Vanishing Point", |doc, _| {
        let Some(a) = doc.assistants.get_mut(id) else { return Ok(()) };
        let i = index as usize;
        let pinned = match &a.kind {
            DrawingAssistantKind::Perspective(guide) => guide.vp_pinned(i),
            DrawingAssistantKind::Fisheye(fish) => fish.vp_pinned(i),
            _ => return Err(EngineError::BadParams { cmd: CMD.into(), msg: "this assistant has no vanishing points".into() }),
        };
        if pinned {
            return Err(EngineError::BadParams { cmd: CMD.into(), msg: format!("vanishing point {index} is fixed (unpin it first)") });
        }
        let vps = match &mut a.kind {
            DrawingAssistantKind::Perspective(guide) => &mut guide.vps,
            DrawingAssistantKind::Fisheye(fish) => &mut fish.vps,
            _ => return Err(EngineError::BadParams { cmd: CMD.into(), msg: "this assistant has no vanishing points".into() }),
        };
        let slot = vps.get_mut(i).ok_or_else(|| EngineError::BadParams { cmd: CMD.into(), msg: format!("vanishing point {index} does not exist") })?;
        if !(point[0].is_finite()
            && point[1].is_finite()
            && point[0].abs() <= photocraft_doc::MAX_ASSISTANT_COORDINATE
            && point[1].abs() <= photocraft_doc::MAX_ASSISTANT_COORDINATE)
        {
            return Err(EngineError::BadParams { cmd: CMD.into(), msg: "`point` must be finite coordinates within ±1000000".into() });
        }
        *slot = point;
        Ok(())
    })?;
    Ok(json!({"id": id.0, "index": index, "point": point}))
}

fn vp_index_param(p: &Value, cmd: &str) -> Result<usize> {
    p.get("index").and_then(Value::as_u64).and_then(|n| usize::try_from(n).ok()).ok_or_else(|| bad(cmd, "`index` must be a whole number"))
}

fn fan_assistant_mut(s: &mut Session, cmd: &str, p: &Value) -> Result<AssistantId> {
    let id = get_mut(s, cmd, p)?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    match &st.doc.assistants.get(id).map(|a| &a.kind) {
        Some(DrawingAssistantKind::Perspective(_)) | Some(DrawingAssistantKind::Fisheye(_)) => Ok(id),
        _ => Err(bad(cmd, "only perspective and fisheye assistants have vanishing points and guides")),
    }
}

/// Append a vanishing point (default: canvas centre — drag its diamond to place it).
fn add_vanishing_point(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "assist.addVanishingPoint";
    let id = fan_assistant_mut(s, CMD, p)?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let center = [f64::from(st.doc.size.width) / 2.0, f64::from(st.doc.size.height) / 2.0];
    let point = anchor_param(p, CMD, "point", center)?;
    let mut index = 0;
    s.edit("Add Vanishing Point", |doc, _| {
        let Some(a) = doc.assistants.get_mut(id) else { return Ok(()) };
        index = match &mut a.kind {
            DrawingAssistantKind::Perspective(guide) => guide.add_vanishing_point(point),
            DrawingAssistantKind::Fisheye(fish) => fish.add_vanishing_point(point),
            _ => return Err(EngineError::BadParams { cmd: CMD.into(), msg: "only perspective and fisheye assistants have vanishing points".into() }),
        }
        .map_err(|msg| EngineError::BadParams { cmd: CMD.into(), msg: msg.into() })?;
        Ok(())
    })?;
    Ok(json!({"id": id.0, "index": index, "point": point}))
}

/// Delete a vanishing point with its guides (keeps at least one; higher indices remap).
fn remove_vanishing_point(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "assist.removeVanishingPoint";
    let id = fan_assistant_mut(s, CMD, p)?;
    let index = vp_index_param(p, CMD)?;
    s.edit("Delete Vanishing Point", |doc, _| {
        let Some(a) = doc.assistants.get_mut(id) else { return Ok(()) };
        match &mut a.kind {
            DrawingAssistantKind::Perspective(guide) => guide.remove_vanishing_point(index),
            DrawingAssistantKind::Fisheye(fish) => fish.remove_vanishing_point(index),
            _ => return Err(EngineError::BadParams { cmd: CMD.into(), msg: "only perspective and fisheye assistants have vanishing points".into() }),
        }
        .map_err(|msg| EngineError::BadParams { cmd: CMD.into(), msg: msg.into() })?;
        Ok(())
    })?;
    Ok(json!({"id": id.0, "index": index}))
}

/// Append one guide through a vanishing point (defaults: its first free 15° step).
fn add_guide(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "assist.addGuide";
    let id = fan_assistant_mut(s, CMD, p)?;
    let vp = p.get("vp").and_then(Value::as_u64).and_then(|n| usize::try_from(n).ok()).unwrap_or(0);
    let angle = match p.get("angleDeg") {
        None => None,
        Some(v) => Some(v.as_f64().ok_or_else(|| bad(CMD, "`angleDeg` must be a number (degrees)"))?),
    };
    let mut index = 0;
    s.edit("Add Guide", |doc, _| {
        let Some(a) = doc.assistants.get_mut(id) else { return Ok(()) };
        let (vps_len, free) = match &a.kind {
            DrawingAssistantKind::Perspective(guide) => (guide.vps.len(), guide.free_guide_angle(vp)),
            DrawingAssistantKind::Fisheye(fish) => (fish.vps.len(), fish.free_guide_angle(vp)),
            _ => return Err(EngineError::BadParams { cmd: CMD.into(), msg: "only perspective and fisheye assistants have guides".into() }),
        };
        if vp >= vps_len {
            return Err(EngineError::BadParams { cmd: CMD.into(), msg: "vanishing point does not exist".into() });
        }
        let angle = angle.unwrap_or(free);
        index = match &mut a.kind {
            DrawingAssistantKind::Perspective(guide) => guide.add_guide(vp, angle),
            DrawingAssistantKind::Fisheye(fish) => fish.add_guide(vp, angle),
            _ => return Err(EngineError::BadParams { cmd: CMD.into(), msg: "only perspective and fisheye assistants have guides".into() }),
        }
        .map_err(|msg| EngineError::BadParams { cmd: CMD.into(), msg: msg.into() })?;
        Ok(())
    })?;
    Ok(json!({"id": id.0, "index": index, "vp": vp}))
}

/// Delete one guide by ray index (canvas-selected guides delete through this).
fn remove_guide(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "assist.removeGuide";
    let id = fan_assistant_mut(s, CMD, p)?;
    let index = vp_index_param(p, CMD)?;
    s.edit("Delete Guide", |doc, _| {
        let Some(a) = doc.assistants.get_mut(id) else { return Ok(()) };
        match &mut a.kind {
            DrawingAssistantKind::Perspective(guide) => guide.remove_guide(index),
            DrawingAssistantKind::Fisheye(fish) => fish.remove_guide(index),
            _ => return Err(EngineError::BadParams { cmd: CMD.into(), msg: "only perspective and fisheye assistants have guides".into() }),
        }
        .map_err(|msg| EngineError::BadParams { cmd: CMD.into(), msg: msg.into() })?;
        Ok(())
    })?;
    Ok(json!({"id": id.0, "index": index}))
}

/// Rotate one guide about its vanishing point (canvas guide drags commit this).
fn rotate_guide(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "assist.rotateGuide";
    let id = fan_assistant_mut(s, CMD, p)?;
    let index = vp_index_param(p, CMD)?;
    let angle = angle_param(p, CMD, "angleDeg", f64::NAN)?;
    if p.get("angleDeg").is_none() {
        return Err(bad(CMD, "missing `angleDeg`"));
    }
    s.edit("Rotate Guide", |doc, _| {
        let Some(a) = doc.assistants.get_mut(id) else { return Ok(()) };
        match &mut a.kind {
            DrawingAssistantKind::Perspective(guide) => guide.rotate_guide(index, angle),
            DrawingAssistantKind::Fisheye(fish) => fish.rotate_guide(index, angle),
            _ => return Err(EngineError::BadParams { cmd: CMD.into(), msg: "only perspective and fisheye assistants have guides".into() }),
        }
        .map_err(|msg| EngineError::BadParams { cmd: CMD.into(), msg: msg.into() })?;
        Ok(())
    })?;
    Ok(json!({"id": id.0, "index": index, "angleDeg": angle}))
}

/// Pin or unpin a vanishing point (Clip Studio Paint's fixed vanishing points: canvas drags
/// refuse pinned points, explicit deletion still removes them).
fn set_vanishing_point_fixed(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "assist.setVanishingPointFixed";
    let id = fan_assistant_mut(s, CMD, p)?;
    let index = vp_index_param(p, CMD)?;
    let fixed = p.get("fixed").and_then(Value::as_bool).ok_or_else(|| bad(CMD, "`fixed` must be a boolean"))?;
    s.edit(if fixed { "Fix Vanishing Point" } else { "Unfix Vanishing Point" }, |doc, _| {
        let Some(a) = doc.assistants.get_mut(id) else { return Ok(()) };
        match &mut a.kind {
            DrawingAssistantKind::Perspective(guide) => guide.set_vp_pinned(index, fixed),
            DrawingAssistantKind::Fisheye(fish) => fish.set_vp_pinned(index, fixed),
            _ => return Err(EngineError::BadParams { cmd: CMD.into(), msg: "only perspective and fisheye assistants have vanishing points".into() }),
        }
        .map_err(|msg| EngineError::BadParams { cmd: CMD.into(), msg: msg.into() })?;
        Ok(())
    })?;
    Ok(json!({"id": id.0, "index": index, "fixed": fixed}))
}

/// Rename an assistant (options-bar popup inline edit).
fn rename(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "assist.rename";
    let id = get_mut(s, CMD, p)?;
    let name = p
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|n| !n.is_empty() && n.len() <= 120)
        .map(str::to_string)
        .ok_or_else(|| bad(CMD, "`name` must be 1–120 characters"))?;
    s.edit("Rename Assistant", |doc, _| {
        if let Some(a) = doc.assistants.get_mut(id) {
            a.name = name.clone();
        }
        Ok(())
    })?;
    Ok(json!({"id": id.0, "name": name}))
}

/// Toggle one assistant's snapping without hiding its overlay (drawn dimmed while off).
fn set_snapping(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "assist.setSnapping";
    let id = get_mut(s, CMD, p)?;
    let snapping = p.get("snapping").and_then(Value::as_bool).ok_or_else(|| bad(CMD, "`snapping` must be a boolean"))?;
    s.edit(if snapping { "Enable Assistant Snapping" } else { "Disable Assistant Snapping" }, |doc, _| {
        if let Some(a) = doc.assistants.get_mut(id) {
            a.snapping = snapping;
        }
        Ok(())
    })?;
    Ok(json!({"id": id.0, "snapping": snapping}))
}

/// Rebuild a perspective assistant's ray fan at a new step (the popup's density slider):
/// vanishing points and horizon stay, only the fan density changes.
fn set_ray_step(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "assist.setRayStep";
    let id = get_mut(s, CMD, p)?;
    let step = p.get("rayStep").and_then(Value::as_f64).ok_or_else(|| bad(CMD, "`rayStep` must be a number (degrees)"))?;
    s.edit("Assistant Ray Density", |doc, _| {
        let Some(a) = doc.assistants.get_mut(id) else { return Ok(()) };
        let (vps, horizon, rays) = match &mut a.kind {
            DrawingAssistantKind::Perspective(guide) => (guide.vps.clone(), guide.horizon_y, &mut guide.rays),
            DrawingAssistantKind::Fisheye(fish) => (fish.vps.clone(), fish.horizon_y, &mut fish.rays),
            _ => return Err(EngineError::BadParams { cmd: CMD.into(), msg: "only perspective and fisheye assistants have a ray fan".into() }),
        };
        let rebuilt = PerspectiveAssistant::with_default_rays(vps, horizon, step)
            .ok_or_else(|| EngineError::BadParams { cmd: CMD.into(), msg: "`rayStep` must be between 0° and 180° (exclusive of 0)".into() })?;
        *rays = rebuilt.rays;
        Ok(())
    })?;
    Ok(json!({"id": id.0, "rayStep": step}))
}

fn num_param(p: &Value, cmd: &str, key: &str, fallback: f64) -> Result<f64> {
    match p.get(key) {
        None => Ok(fallback),
        Some(v) => v.as_f64().ok_or_else(|| bad(cmd, format!("`{key}` must be a number"))),
    }
}

fn points4_param(p: &Value, cmd: &str, fallback: [[f64; 2]; 4]) -> Result<[[f64; 2]; 4]> {
    match p.get("points") {
        None => Ok(fallback),
        Some(v) => {
            let a = v.as_array().filter(|a| a.len() == 4).ok_or_else(|| bad(cmd, "`points` must be 4 control points"))?;
            let mut out = [[0.0; 2]; 4];
            for (i, q) in a.iter().enumerate() {
                let pair = q.as_array().filter(|a| a.len() == 2).ok_or_else(|| bad(cmd, "`points` must be 4 control points"))?;
                out[i] = [
                    pair.first().and_then(Value::as_f64).ok_or_else(|| bad(cmd, "`points` must be 4 control points"))?,
                    pair.get(1).and_then(Value::as_f64).ok_or_else(|| bad(cmd, "`points` must be 4 control points"))?,
                ];
            }
            Ok(out)
        }
    }
}

fn sides_param(p: &Value, cmd: &str, fallback: u32) -> Result<u32> {
    match p.get("sides") {
        None => Ok(fallback),
        Some(v) => v.as_u64().and_then(|n| u32::try_from(n).ok()).ok_or_else(|| bad(cmd, "`sides` must be a whole number 3–64")),
    }
}

fn count_param(p: &Value, cmd: &str) -> Result<u32> {
    match p.get("count") {
        None => Ok(2),
        Some(v) => {
            v.as_u64().and_then(|n| u32::try_from(n).ok()).filter(|n| (1..=16).contains(n)).ok_or_else(|| bad(cmd, "`count` must be a whole number 1–16"))
        }
    }
}

fn add_bezier(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "assist.addBezier";
    check_capacity(s, CMD)?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let center = [f64::from(st.doc.size.width) / 2.0, f64::from(st.doc.size.height) / 2.0];
    let fallback = [[center[0] - 90.0, center[1]], [center[0] - 30.0, center[1]], [center[0] + 30.0, center[1]], [center[0] + 90.0, center[1]]];
    let points = points4_param(p, CMD, fallback)?;
    let curve = BezierAssistant::new(points).ok_or_else(|| bad(CMD, "control points must be finite coordinates within ±1000000"))?;
    let name = name_param(s, p, "Bezier");
    let mut out = Value::Null;
    s.edit("Add Assistant", |doc, _| {
        let item = DrawingAssistantItem::new(name, DrawingAssistantKind::Bezier(curve));
        out = item_json(&item);
        doc.assistants.items.push(item);
        Ok(())
    })?;
    Ok(out)
}

/// Move one kind-specific control handle (canvas drag): Bézier point 0–3, ellipse
/// center/rx/ry (0–2), polygon center/corner (0–1), fisheye lens radius (0).
fn move_control_point(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "assist.moveControlPoint";
    let id = get_mut(s, CMD, p)?;
    let index = p.get("index").and_then(Value::as_u64).ok_or_else(|| bad(CMD, "`index` must be a whole number"))?;
    let point = anchor_param(p, CMD, "point", [f64::NAN, f64::NAN])?;
    if p.get("point").is_none() {
        return Err(bad(CMD, "missing `point`"));
    }
    if !point[0].is_finite() || !point[1].is_finite() {
        return Err(bad(CMD, "`point` must be finite coordinates"));
    }
    s.edit("Move Assistant Handle", |doc, _| {
        let Some(a) = doc.assistants.get_mut(id) else { return Ok(()) };
        match &mut a.kind {
            DrawingAssistantKind::Bezier(b) => {
                let slot = b.points.get_mut(index as usize).ok_or_else(|| bad(CMD, format!("bezier control point {index} does not exist")))?;
                *slot = point;
                if !b.points.iter().all(|q| q[0].abs() <= photocraft_doc::MAX_ASSISTANT_COORDINATE && q[1].abs() <= photocraft_doc::MAX_ASSISTANT_COORDINATE) {
                    return Err(bad(CMD, "`point` must be within ±1000000"));
                }
            }
            DrawingAssistantKind::Ellipse(e) => match index {
                0 => *e = EllipseAssistant::new(point, e.radii, e.rotation_deg).ok_or_else(|| bad(CMD, "`point` must be within ±1000000"))?,
                1 | 2 => {
                    let r = (point[0] - e.center[0]).hypot(point[1] - e.center[1]);
                    let mut radii = e.radii;
                    radii[index as usize - 1] = r;
                    *e = EllipseAssistant::new(e.center, radii, e.rotation_deg).ok_or_else(|| bad(CMD, "radius must be positive and within ±1000000"))?;
                }
                _ => return Err(bad(CMD, format!("ellipse handle {index} does not exist"))),
            },
            DrawingAssistantKind::Polygon(g) => match index {
                0 => *g = PolygonAssistant::new(point, g.radius, g.sides, g.rotation_deg).ok_or_else(|| bad(CMD, "`point` must be within ±1000000"))?,
                1 => {
                    let dx = [point[0] - g.center[0], point[1] - g.center[1]];
                    let r = dx[0].hypot(dx[1]);
                    let rot = dx[1].atan2(dx[0]).to_degrees();
                    *g = PolygonAssistant::new(g.center, r, g.sides, rot).ok_or_else(|| bad(CMD, "radius must be positive and within ±1000000"))?;
                }
                _ => return Err(bad(CMD, format!("polygon handle {index} does not exist"))),
            },
            DrawingAssistantKind::Fisheye(f) => {
                if index != 0 {
                    return Err(bad(CMD, format!("fisheye handle {index} does not exist")));
                }
                let r = (point[0] - f.center[0]).hypot(point[1] - f.center[1]);
                *f = FisheyeAssistant::new(f.center, r, f.strength, f.vps.clone(), f.horizon_y, f.rays.clone())
                    .ok_or_else(|| bad(CMD, "lens radius must be positive and within ±1000000"))?;
            }
            _ => return Err(EngineError::BadParams { cmd: CMD.into(), msg: "this assistant has no control points".into() }),
        }
        Ok(())
    })?;
    Ok(json!({"id": id.0, "index": index, "point": point}))
}

fn add_parallel(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "assist.addParallel";
    check_capacity(s, CMD)?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let center = [f64::from(st.doc.size.width) / 2.0, f64::from(st.doc.size.height) / 2.0];
    let anchor = anchor_param(p, CMD, "anchor", center)?;
    let angle = angle_param(p, CMD, "angleDeg", 0.0)?;
    let spacing = num_param(p, CMD, "spacing", 20.0)?;
    let count = count_param(p, CMD)?;
    let mut par = ParallelAssistant::new(anchor, angle, spacing, count).ok_or_else(|| {
        bad(CMD, "need a finite `anchor` within ±1000000, a finite `angleDeg`, a `spacing` between 0 and ±1000000 (exclusive of 0), and a `count` 1–16")
    })?;
    par.length = diagonal(s)?;
    let name = name_param(s, p, "Parallel");
    let mut out = Value::Null;
    s.edit("Add Assistant", |doc, _| {
        let item = DrawingAssistantItem::new(name, DrawingAssistantKind::Parallel(par));
        out = item_json(&item);
        doc.assistants.items.push(item);
        Ok(())
    })?;
    Ok(out)
}

/// How many parallel lines exist, base included (the popup Lines slider).
fn set_parallel_count(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "assist.setParallelCount";
    let id = get_mut(s, CMD, p)?;
    if p.get("count").is_none() {
        return Err(bad(CMD, "missing `count`"));
    }
    let count = count_param(p, CMD)?;
    s.edit("Parallel Line Count", |doc, _| {
        if let Some(a) = doc.assistants.get_mut(id) {
            let DrawingAssistantKind::Parallel(par) = &mut a.kind else {
                return Err(EngineError::BadParams { cmd: CMD.into(), msg: "only parallel assistants have a line count".into() });
            };
            par.count = count;
        }
        Ok(())
    })?;
    Ok(json!({"id": id.0, "count": count}))
}

/// Guide spacing for parallel-line and concentric assistants (the popup slider).
fn set_spacing(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "assist.setSpacing";
    let id = get_mut(s, CMD, p)?;
    let spacing = num_param(p, CMD, "spacing", f64::NAN)?;
    if p.get("spacing").is_none() {
        return Err(bad(CMD, "missing `spacing`"));
    }
    s.edit("Assistant Spacing", |doc, _| {
        let Some(a) = doc.assistants.get_mut(id) else { return Ok(()) };
        match &mut a.kind {
            DrawingAssistantKind::Parallel(par) => {
                *par = ParallelAssistant::new(par.anchor, par.angle_deg, spacing, par.count)
                    .ok_or_else(|| bad(CMD, "`spacing` must be between 0 and ±1000000 (exclusive of 0)"))?;
            }
            DrawingAssistantKind::Concentric(con) => {
                *con = ConcentricAssistant::new(con.center, spacing).ok_or_else(|| bad(CMD, "`spacing` must be between 0 and ±1000000 (exclusive of 0)"))?;
            }
            _ => return Err(EngineError::BadParams { cmd: CMD.into(), msg: "only parallel and concentric assistants have a spacing".into() }),
        }
        Ok(())
    })?;
    Ok(json!({"id": id.0, "spacing": spacing}))
}

fn add_ellipse(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "assist.addEllipse";
    check_capacity(s, CMD)?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let center = [f64::from(st.doc.size.width) / 2.0, f64::from(st.doc.size.height) / 2.0];
    let c = anchor_param(p, CMD, "center", center)?;
    let radii: [f64; 2] = match p.get("radii") {
        None => [60.0, 40.0],
        Some(v) => {
            let a = v.as_array().filter(|a| a.len() == 2).ok_or_else(|| bad(CMD, "`radii` must be [rx, ry]"))?;
            [
                a.first().and_then(Value::as_f64).ok_or_else(|| bad(CMD, "`radii` must be [rx, ry]"))?,
                a.get(1).and_then(Value::as_f64).ok_or_else(|| bad(CMD, "`radii` must be [rx, ry]"))?,
            ]
        }
    };
    let rotation = angle_param(p, CMD, "rotationDeg", 0.0)?;
    let e = EllipseAssistant::new(c, radii, rotation)
        .ok_or_else(|| bad(CMD, "need a finite `center` within ±1000000, positive `radii` within ±1000000, and a finite `rotationDeg`"))?;
    let name = name_param(s, p, "Ellipse");
    let mut out = Value::Null;
    s.edit("Add Assistant", |doc, _| {
        let item = DrawingAssistantItem::new(name, DrawingAssistantKind::Ellipse(e));
        out = item_json(&item);
        doc.assistants.items.push(item);
        Ok(())
    })?;
    Ok(out)
}

fn set_ellipse_rotation(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "assist.setEllipseRotation";
    let id = get_mut(s, CMD, p)?;
    let rotation = angle_param(p, CMD, "rotationDeg", f64::NAN)?;
    if p.get("rotationDeg").is_none() {
        return Err(bad(CMD, "missing `rotationDeg`"));
    }
    s.edit("Rotate Assistant", |doc, _| {
        let Some(a) = doc.assistants.get_mut(id) else { return Ok(()) };
        let DrawingAssistantKind::Ellipse(e) = &mut a.kind else {
            return Err(EngineError::BadParams { cmd: CMD.into(), msg: "only ellipse assistants rotate".into() });
        };
        *e = EllipseAssistant::new(e.center, e.radii, rotation).ok_or_else(|| bad(CMD, "`rotationDeg` must be finite"))?;
        Ok(())
    })?;
    Ok(json!({"id": id.0, "rotationDeg": rotation}))
}

fn add_concentric(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "assist.addConcentric";
    check_capacity(s, CMD)?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let center = [f64::from(st.doc.size.width) / 2.0, f64::from(st.doc.size.height) / 2.0];
    let c = anchor_param(p, CMD, "center", center)?;
    let spacing = num_param(p, CMD, "spacing", 20.0)?;
    let con = ConcentricAssistant::new(c, spacing)
        .ok_or_else(|| bad(CMD, "need a finite `center` within ±1000000 and a `spacing` between 0 and ±1000000 (exclusive of 0)"))?;
    let name = name_param(s, p, "Concentric");
    let mut out = Value::Null;
    s.edit("Add Assistant", |doc, _| {
        let item = DrawingAssistantItem::new(name, DrawingAssistantKind::Concentric(con));
        out = item_json(&item);
        doc.assistants.items.push(item);
        Ok(())
    })?;
    Ok(out)
}

fn add_polygon(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "assist.addPolygon";
    check_capacity(s, CMD)?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let center = [f64::from(st.doc.size.width) / 2.0, f64::from(st.doc.size.height) / 2.0];
    let c = anchor_param(p, CMD, "center", center)?;
    let radius = num_param(p, CMD, "radius", 60.0)?;
    let sides = sides_param(p, CMD, 5)?;
    let rotation = angle_param(p, CMD, "rotationDeg", -90.0)?;
    let g = PolygonAssistant::new(c, radius, sides, rotation)
        .ok_or_else(|| bad(CMD, "need a finite `center` within ±1000000, a positive `radius` within ±1000000, `sides` 3–64, and a finite `rotationDeg`"))?;
    let name = name_param(s, p, "Polygon");
    let mut out = Value::Null;
    s.edit("Add Assistant", |doc, _| {
        let item = DrawingAssistantItem::new(name, DrawingAssistantKind::Polygon(g));
        out = item_json(&item);
        doc.assistants.items.push(item);
        Ok(())
    })?;
    Ok(out)
}

fn set_polygon_sides(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "assist.setPolygonSides";
    let id = get_mut(s, CMD, p)?;
    let sides = sides_param(p, CMD, 0)?;
    if p.get("sides").is_none() {
        return Err(bad(CMD, "missing `sides`"));
    }
    if !(3..=64).contains(&sides) {
        return Err(bad(CMD, "`sides` must be 3–64"));
    }
    s.edit("Assistant Sides", |doc, _| {
        if let Some(a) = doc.assistants.get_mut(id) {
            let DrawingAssistantKind::Polygon(g) = &mut a.kind else {
                return Err(EngineError::BadParams { cmd: CMD.into(), msg: "only polygon assistants have sides".into() });
            };
            g.sides = sides;
        }
        Ok(())
    })?;
    Ok(json!({"id": id.0, "sides": sides}))
}

fn add_fisheye(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "assist.addFisheye";
    check_capacity(s, CMD)?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let (w, h) = (f64::from(st.doc.size.width), f64::from(st.doc.size.height));
    let center = [w / 2.0, h / 2.0];
    let c = anchor_param(p, CMD, "center", center)?;
    let radius = num_param(p, CMD, "radius", w.min(h) * 0.25)?;
    let strength = num_param(p, CMD, "strength", 0.65)?;
    let points = points_param(p, CMD, vec![center])?;
    let horizon = match p.get("horizonY") {
        None => None,
        Some(v) => Some(v.as_f64().ok_or_else(|| bad(CMD, "`horizonY` must be a number"))?),
    };
    let step = num_param(p, CMD, "rayStep", 15.0)?;
    let fan = PerspectiveAssistant::with_default_rays(points, horizon, step)
        .ok_or_else(|| bad(CMD, "need 1–8 finite vanishing points within ±1000000, a finite `horizonY`, and a `rayStep` between 0° and 180°"))?;
    let fish = FisheyeAssistant::new(c, radius, strength, fan.vps, fan.horizon_y, fan.rays)
        .ok_or_else(|| bad(CMD, "need a finite `center` within ±1000000, a positive `radius` within ±1000000, and a `strength` between 0 and 1"))?;
    let name = name_param(s, p, "Fisheye");
    let mut out = Value::Null;
    s.edit("Add Assistant", |doc, _| {
        let item = DrawingAssistantItem::new(name, DrawingAssistantKind::Fisheye(fish));
        out = item_json(&item);
        doc.assistants.items.push(item);
        Ok(())
    })?;
    Ok(out)
}

fn set_fisheye_strength(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "assist.setFisheyeStrength";
    let id = get_mut(s, CMD, p)?;
    let strength = num_param(p, CMD, "strength", f64::NAN)?;
    if p.get("strength").is_none() {
        return Err(bad(CMD, "missing `strength`"));
    }
    if !strength.is_finite() || !(0.0..=1.0).contains(&strength) {
        return Err(bad(CMD, "`strength` must be between 0 and 1"));
    }
    s.edit("Fisheye Strength", |doc, _| {
        if let Some(a) = doc.assistants.get_mut(id) {
            let DrawingAssistantKind::Fisheye(f) = &mut a.kind else {
                return Err(EngineError::BadParams { cmd: CMD.into(), msg: "only fisheye assistants have a strength".into() });
            };
            f.strength = strength;
        }
        Ok(())
    })?;
    Ok(json!({"id": id.0, "strength": strength}))
}

/// Recolor one assistant's overlay (its label colour; `"none"` returns it to the theme's
/// assistant magenta).
fn set_color(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "assist.setColor";
    let id = get_mut(s, CMD, p)?;
    let color = p
        .get("color")
        .and_then(Value::as_str)
        .and_then(LabelColor::from_id)
        .ok_or_else(|| bad(CMD, "`color` must be a label colour id (\"red\", …, \"none\")"))?;
    s.edit("Assistant Color", |doc, _| {
        if let Some(a) = doc.assistants.get_mut(id) {
            a.color = color;
        }
        Ok(())
    })?;
    Ok(json!({"id": id.0, "color": color.id()}))
}

/// Resize a straight-line, parallel or perspective guide (the popup Length slider).
/// Positive and finite; the horizon and the lens-bounded fisheye need no length.
fn set_length(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "assist.setLength";
    let id = get_mut(s, CMD, p)?;
    let length = num_param(p, CMD, "length", f64::NAN)?;
    if p.get("length").is_none() {
        return Err(bad(CMD, "missing `length`"));
    }
    if !length.is_finite() || length <= 0.0 || length > photocraft_doc::MAX_ASSISTANT_COORDINATE {
        return Err(bad(CMD, "`length` must be positive and within ±1000000"));
    }
    s.edit("Resize Assistant", |doc, _| {
        let Some(a) = doc.assistants.get_mut(id) else { return Ok(()) };
        match &mut a.kind {
            DrawingAssistantKind::StraightLine(line) => line.length = length,
            DrawingAssistantKind::Parallel(par) => par.length = length,
            DrawingAssistantKind::Perspective(guide) => guide.length = length,
            _ => return Err(EngineError::BadParams { cmd: CMD.into(), msg: "only straight-line, parallel and perspective guides have a length".into() }),
        }
        Ok(())
    })?;
    Ok(json!({"id": id.0, "length": length}))
}

/// Assistant command specs.
pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "assist.addStraightLine",
            label: "Add Straight Line Assistant",
            menu: &[],
            shortcut: None,
            params: r##"{"anchor":[x,y]?=canvas centre,"angleDeg":degrees=0,"name":string?} — a snapping ruler through `anchor` at `angleDeg`"##,
            enabled: has_doc,
            run: add_straight,
            journal: true,
        },
        CommandSpec {
            id: "assist.addPerspective",
            label: "Add Perspective Assistant",
            menu: &[],
            shortcut: None,
            params: r##"{"points":[[x,y],…]?=canvas centre,"horizonY":px?=through the points,"rayStep":degrees=15,"name":string?} — 1- or 2-point perspective snapping"##,
            enabled: has_doc,
            run: add_perspective,
            journal: true,
        },
        CommandSpec {
            id: "assist.list",
            label: "List Drawing Assistants",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: has_doc,
            run: |s, _| list_json(s),
            journal: false,
        },
        CommandSpec {
            id: "assist.remove",
            label: "Delete Drawing Assistant",
            menu: &[],
            shortcut: None,
            params: r##"{"id":assistant id}"##,
            enabled: has_doc,
            run: remove,
            journal: true,
        },
        CommandSpec {
            id: "assist.setVisible",
            label: "Show/Hide Drawing Assistant",
            menu: &[],
            shortcut: None,
            params: r##"{"id":assistant id,"visible":bool}"##,
            enabled: has_doc,
            run: set_visible,
            journal: true,
        },
        CommandSpec {
            id: "assist.setSnap",
            label: "Drawing Assistant Snapping",
            menu: &[],
            shortcut: None,
            params: r##"{"enabled":bool?,"radius":document px 0..500?} — master switch and magnetism"##,
            enabled: has_doc,
            run: set_snap,
            journal: true,
        },
        CommandSpec {
            id: "assist.clear",
            label: "Delete All Drawing Assistants",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: has_doc,
            run: clear,
            journal: true,
        },
        CommandSpec {
            id: "assist.moveAnchor",
            label: "Move Assistant Anchor",
            menu: &[],
            shortcut: None,
            params: r##"{"id":assistant id,"anchor":[x,y]}"##,
            enabled: has_doc,
            run: move_anchor,
            journal: true,
        },
        CommandSpec {
            id: "assist.setAngle",
            label: "Rotate Assistant",
            menu: &[],
            shortcut: None,
            params: r##"{"id":assistant id,"angleDeg":degrees}"##,
            enabled: has_doc,
            run: set_angle,
            journal: true,
        },
        CommandSpec {
            id: "assist.moveVanishingPoint",
            label: "Move Vanishing Point",
            menu: &[],
            shortcut: None,
            params: r##"{"id":assistant id,"index":vanishing point,"point":[x,y]} — refuses pinned points"##,
            enabled: has_doc,
            run: move_vanishing_point,
            journal: true,
        },
        CommandSpec {
            id: "assist.addVanishingPoint",
            label: "Add Vanishing Point",
            menu: &[],
            shortcut: None,
            params: r##"{"id":assistant id,"point":[x,y]?=canvas centre} — drag its diamond to place it"##,
            enabled: has_doc,
            run: add_vanishing_point,
            journal: true,
        },
        CommandSpec {
            id: "assist.removeVanishingPoint",
            label: "Delete Vanishing Point",
            menu: &[],
            shortcut: None,
            params: r##"{"id":assistant id,"index":vanishing point} — keeps at least one, higher guides remap"##,
            enabled: has_doc,
            run: remove_vanishing_point,
            journal: true,
        },
        CommandSpec {
            id: "assist.addGuide",
            label: "Add Guide",
            menu: &[],
            shortcut: None,
            params: r##"{"id":assistant id,"vp":vanishing point=0,"angleDeg":degrees?=first free 15° step}"##,
            enabled: has_doc,
            run: add_guide,
            journal: true,
        },
        CommandSpec {
            id: "assist.removeGuide",
            label: "Delete Guide",
            menu: &[],
            shortcut: None,
            params: r##"{"id":assistant id,"index":guide ray} — canvas-selected guides delete through this"##,
            enabled: has_doc,
            run: remove_guide,
            journal: true,
        },
        CommandSpec {
            id: "assist.rotateGuide",
            label: "Rotate Guide",
            menu: &[],
            shortcut: None,
            params: r##"{"id":assistant id,"index":guide ray,"angleDeg":degrees} — canvas guide drags commit this"##,
            enabled: has_doc,
            run: rotate_guide,
            journal: true,
        },
        CommandSpec {
            id: "assist.setVanishingPointFixed",
            label: "Fix Vanishing Point",
            menu: &[],
            shortcut: None,
            params: r##"{"id":assistant id,"index":vanishing point,"fixed":bool} — canvas drags refuse pinned points"##,
            enabled: has_doc,
            run: set_vanishing_point_fixed,
            journal: true,
        },
        CommandSpec {
            id: "assist.rename",
            label: "Rename Drawing Assistant",
            menu: &[],
            shortcut: None,
            params: r##"{"id":assistant id,"name":1–120 characters}"##,
            enabled: has_doc,
            run: rename,
            journal: true,
        },
        CommandSpec {
            id: "assist.setSnapping",
            label: "Toggle One Assistant's Snapping",
            menu: &[],
            shortcut: None,
            params: r##"{"id":assistant id,"snapping":bool} — overlay stays, drawn dimmed while off"##,
            enabled: has_doc,
            run: set_snapping,
            journal: true,
        },
        CommandSpec {
            id: "assist.setRayStep",
            label: "Assistant Ray Density",
            menu: &[],
            shortcut: None,
            params: r##"{"id":perspective/fisheye assistant id,"rayStep":degrees 0..180} — rebuilds the ray fan"##,
            enabled: has_doc,
            run: set_ray_step,
            journal: true,
        },
        CommandSpec {
            id: "assist.addBezier",
            label: "Add Bezier Assistant",
            menu: &[],
            shortcut: None,
            params: r##"{"points":4 control points?=S-curve at the canvas centre,"name":string?} — a snapping cubic curve"##,
            enabled: has_doc,
            run: add_bezier,
            journal: true,
        },
        CommandSpec {
            id: "assist.moveControlPoint",
            label: "Move Assistant Control Point",
            menu: &[],
            shortcut: None,
            params: r##"{"id":assistant id,"index":control point,"point":[x,y]} — bezier 0–3, ellipse 0–2, polygon 0–1, fisheye 0"##,
            enabled: has_doc,
            run: move_control_point,
            journal: true,
        },
        CommandSpec {
            id: "assist.addParallel",
            label: "Add Parallel Assistant",
            menu: &[],
            shortcut: None,
            params: r##"{"anchor":[x,y]?,"angleDeg":degrees=0,"spacing":px=20,"count":1–16=2,"name":string?} — snap onto the parallel lines"##,
            enabled: has_doc,
            run: add_parallel,
            journal: true,
        },
        CommandSpec {
            id: "assist.setSpacing",
            label: "Assistant Guide Spacing",
            menu: &[],
            shortcut: None,
            params: r##"{"id":parallel/concentric assistant id,"spacing":px} — distance between family lines"##,
            enabled: has_doc,
            run: set_spacing,
            journal: true,
        },
        CommandSpec {
            id: "assist.setParallelCount",
            label: "Parallel Line Count",
            menu: &[],
            shortcut: None,
            params: r##"{"id":parallel assistant id,"count":1–16} — total lines, base included"##,
            enabled: has_doc,
            run: set_parallel_count,
            journal: true,
        },
        CommandSpec {
            id: "assist.addEllipse",
            label: "Add Ellipse Assistant",
            menu: &[],
            shortcut: None,
            params: r##"{"center":[x,y]?,"radii":[rx,ry]?=[60,40],"rotationDeg":degrees=0,"name":string?} — snap onto the ellipse"##,
            enabled: has_doc,
            run: add_ellipse,
            journal: true,
        },
        CommandSpec {
            id: "assist.setEllipseRotation",
            label: "Rotate Ellipse Assistant",
            menu: &[],
            shortcut: None,
            params: r##"{"id":ellipse assistant id,"rotationDeg":degrees}"##,
            enabled: has_doc,
            run: set_ellipse_rotation,
            journal: true,
        },
        CommandSpec {
            id: "assist.addConcentric",
            label: "Add Concentric Assistant",
            menu: &[],
            shortcut: None,
            params: r##"{"center":[x,y]?,"spacing":px=20,"name":string?} — snap onto concentric rings"##,
            enabled: has_doc,
            run: add_concentric,
            journal: true,
        },
        CommandSpec {
            id: "assist.addPolygon",
            label: "Add Polygon Assistant",
            menu: &[],
            shortcut: None,
            params: r##"{"center":[x,y]?,"radius":px=60,"sides":3–64=5,"rotationDeg":degrees=-90,"name":string?} — snap onto the edges"##,
            enabled: has_doc,
            run: add_polygon,
            journal: true,
        },
        CommandSpec {
            id: "assist.setPolygonSides",
            label: "Assistant Polygon Sides",
            menu: &[],
            shortcut: None,
            params: r##"{"id":polygon assistant id,"sides":3–64}"##,
            enabled: has_doc,
            run: set_polygon_sides,
            journal: true,
        },
        CommandSpec {
            id: "assist.addFisheye",
            label: "Add Fisheye Assistant",
            menu: &[],
            shortcut: None,
            params: r##"{"center":[x,y]?,"radius":px?=quarter of the canvas,"strength":0–1=0.65,"points":[[x,y],…]?,"horizonY":px?,"rayStep":degrees=15,"name":string?} — curvilinear perspective inside the lens"##,
            enabled: has_doc,
            run: add_fisheye,
            journal: true,
        },
        CommandSpec {
            id: "assist.setFisheyeStrength",
            label: "Fisheye Lens Strength",
            menu: &[],
            shortcut: None,
            params: r##"{"id":fisheye assistant id,"strength":0–1} — 0 is linear, 1 is full fisheye"##,
            enabled: has_doc,
            run: set_fisheye_strength,
            journal: true,
        },
        CommandSpec {
            id: "assist.setColor",
            label: "Assistant Overlay Color",
            menu: &[],
            shortcut: None,
            params: r##"{"id":assistant id,"color":label id ("red", …, "none")} — "none" is the theme magenta"##,
            enabled: has_doc,
            run: set_color,
            journal: true,
        },
        CommandSpec {
            id: "assist.setLength",
            label: "Resize Assistant Guide",
            menu: &[],
            shortcut: None,
            params: r##"{"id":straight-line/parallel/perspective assistant id,"length":px} — finite extent, never infinite"##,
            enabled: has_doc,
            run: set_length,
            journal: true,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn session() -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 100, "height": 100, "background": "white"})).unwrap();
        s
    }

    fn px(s: &Session, x: i32, y: i32) -> Vec<f32> {
        let st = s.active().unwrap();
        st.doc.layer(st.active_layer.unwrap()).unwrap().surface().unwrap().pixel(x, y)
    }

    fn add_horizontal(s: &mut Session, y: f64) -> u64 {
        s.execute("assist.addStraightLine", json!({"anchor": [50.0, y], "angleDeg": 0.0})).unwrap()["id"].as_u64().unwrap()
    }

    /// Five raw pointer samples crossing just below the guide at y = 50.
    fn crossing_points() -> Value {
        json!([[10.0, 53.0], [30.0, 52.0], [50.0, 54.0], [70.0, 51.0], [90.0, 53.0]])
    }

    #[test]
    fn stroke_snaps_to_guide_preserving_pressure_and_tilt() {
        let mut s = session();
        add_horizontal(&mut s, 50.0);
        // Stylus metadata survives snapping untouched.
        let pts = vec![
            StrokePoint { x: 10.0, y: 53.0, pressure: 0.4, tilt_x: 12.0, tilt_y: -7.0, rotation: 30.0, wheel: 0.5, time: 11.0 },
            StrokePoint { x: 90.0, y: 53.0, pressure: 0.9, ..Default::default() },
        ];
        let snapped = snap_points(&s, &pts);
        assert_eq!((snapped[0].x, snapped[0].y), (10.0, 50.0));
        assert_eq!((snapped[1].x, snapped[1].y), (90.0, 50.0));
        for (a, b) in pts.iter().zip(&snapped) {
            assert_eq!((b.pressure, b.tilt_x, b.tilt_y, b.rotation, b.wheel, b.time), (a.pressure, a.tilt_x, a.tilt_y, a.rotation, a.wheel, a.time));
        }
        // The committed stroke paints the guide, not the raw samples (size 3, hard: the raw
        // y ≈ 52–54 would leave y = 50 white).
        s.execute("paint.stroke", json!({"points": crossing_points(), "size": 3.0, "hardness": 1.0, "color": "#000000", "smoothing": 0.0})).unwrap();
        assert!(px(&s, 50, 50)[0] < 0.2, "snapped stroke paints the guide: {:?}", px(&s, 50, 50));
        assert_eq!(px(&s, 50, 80), vec![1.0; 4], "far from the guide stays white");
    }

    #[test]
    fn snapping_off_leaves_points_unadjusted() {
        let mut s = session();
        add_horizontal(&mut s, 50.0);
        s.execute("assist.setSnap", json!({"enabled": false})).unwrap();
        let pts = vec![StrokePoint::new(10.0, 53.0, 0.5)];
        assert_eq!(snap_points(&s, &pts), pts);
        s.execute("paint.stroke", json!({"points": crossing_points(), "size": 3.0, "hardness": 1.0, "color": "#000000", "smoothing": 0.0})).unwrap();
        assert_eq!(px(&s, 50, 50), vec![1.0; 4], "guide row untouched with snapping off");
        assert!(px(&s, 50, 53)[0] < 0.5, "raw samples paint instead: {:?}", px(&s, 50, 53));
        // Hidden assistants are skipped too, while the set stays enabled.
        s.execute("assist.setSnap", json!({"enabled": true})).unwrap();
        let id = s.active().unwrap().doc.assistants.items[0].id.0;
        s.execute("assist.setVisible", json!({"id": id, "visible": false})).unwrap();
        assert_eq!(snap_points(&s, &pts), pts);
    }

    #[test]
    fn rename_validates_and_persists_through_undo() {
        let mut s = session();
        let id = add_horizontal(&mut s, 50.0);
        let r = s.execute("assist.rename", json!({"id": id, "name": "  Horizon  "})).unwrap();
        assert_eq!(r["name"], json!("Horizon"), "trims whitespace");
        assert!(s.execute("assist.rename", json!({"id": id, "name": "   "})).is_err(), "blank rejected");
        assert!(s.execute("assist.rename", json!({"id": id, "name": "x".repeat(121)})).is_err(), "overlong rejected");
        assert!(s.execute("assist.rename", json!({"id": 9999, "name": "Ghost"})).is_err(), "unknown id rejected");
        s.undo();
        assert_eq!(s.active().unwrap().doc.assistants.get(AssistantId(id)).unwrap().name, "Straight Line 1");
        s.redo();
        assert_eq!(s.active().unwrap().doc.assistants.get(AssistantId(id)).unwrap().name, "Horizon");
    }

    #[test]
    fn per_item_snapping_toggle_skips_only_that_assistant() {
        let mut s = session();
        let id = add_horizontal(&mut s, 50.0);
        s.execute("assist.setSnapping", json!({"id": id, "snapping": false})).unwrap();
        let pts = vec![StrokePoint::new(10.0, 53.0, 0.5)];
        assert_eq!(snap_points(&s, &pts), pts, "pipeline skips a non-snapping item");
        assert!(s.execute("assist.setSnapping", json!({"id": id})).is_err(), "missing flag rejected");
        s.execute("assist.setSnapping", json!({"id": id, "snapping": true})).unwrap();
        let snapped = snap_points(&s, &pts);
        assert_eq!((snapped[0].x, snapped[0].y), (10.0, 50.0));
        let list = s.execute("assist.list", json!({})).unwrap();
        assert_eq!(list["items"][0]["snapping"], json!(true));
    }

    #[test]
    fn ray_step_rebuilds_only_the_fan() {
        let mut s = session();
        let id = s.execute("assist.addPerspective", json!({"points": [[50.0, 50.0]], "rayStep": 90.0})).unwrap()["id"].as_u64().unwrap();
        let rays_before = match &s.active().unwrap().doc.assistants.get(AssistantId(id)).unwrap().kind {
            DrawingAssistantKind::Perspective(p) => p.rays.len(),
            _ => panic!("not perspective"),
        };
        assert_eq!(rays_before, 2);
        s.execute("assist.setRayStep", json!({"id": id, "rayStep": 45.0})).unwrap();
        let st = s.active().unwrap();
        let p = match &st.doc.assistants.get(AssistantId(id)).unwrap().kind {
            DrawingAssistantKind::Perspective(p) => p.clone(),
            _ => panic!("not perspective"),
        };
        assert_eq!(p.rays.len(), 4, "45° fan doubles the rays");
        assert_eq!(p.vps, vec![[50.0, 50.0]], "vanishing point untouched");
        assert_eq!(p.horizon_y, Some(50.0), "horizon untouched");
        assert!(s.execute("assist.setRayStep", json!({"id": id, "rayStep": 0.0})).is_err(), "zero step rejected");
        assert!(s.execute("assist.setRayStep", json!({"id": id, "rayStep": 200.0})).is_err(), "over-180 rejected");
        let lid = add_horizontal(&mut s, 10.0);
        assert!(s.execute("assist.setRayStep", json!({"id": lid, "rayStep": 45.0})).is_err(), "straight lines have no fan");
    }

    #[test]
    fn perspective_assistant_snaps_strokes_to_rays() {
        let mut s = session();
        // One vanishing point at the canvas centre with the default fan: horizontal/vertical rays.
        s.execute("assist.addPerspective", json!({"points": [[50.0, 50.0]], "rayStep": 90.0})).unwrap();
        // Just below the horizontal ray: snaps up onto y = 50.
        let snapped = snap_points(&s, &[StrokePoint::new(20.0, 53.0, 1.0)]);
        assert_eq!((snapped[0].x, snapped[0].y), (20.0, 50.0));
        // Far from every ray and the horizon: untouched.
        let far = snap_points(&s, &[StrokePoint::new(10.0, 10.0, 1.0)]);
        assert_eq!((far[0].x, far[0].y), (10.0, 10.0));
    }

    #[test]
    fn assistant_config_undoes_redoes_and_lists() {
        let mut s = session();
        let before = s.active().unwrap().history.past_len();
        let id = add_horizontal(&mut s, 50.0);
        assert_eq!(s.execute("assist.list", json!({})).unwrap()["items"].as_array().unwrap().len(), 1);
        assert!(s.undo());
        assert_eq!(s.execute("assist.list", json!({})).unwrap()["items"].as_array().unwrap().len(), 0);
        assert!(s.redo());
        let items = s.execute("assist.list", json!({})).unwrap()["items"].as_array().unwrap().clone();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["id"].as_u64().unwrap(), id);
        assert_eq!(items[0]["kind"].as_str().unwrap(), "straightLine");
        assert_eq!(s.active().unwrap().history.past_len(), before + 1, "one undo step for the add");
        // Moving the anchor is undoable too.
        s.execute("assist.moveAnchor", json!({"id": id, "anchor": [10.0, 60.0]})).unwrap();
        assert!(s.undo());
        let anchor = s.execute("assist.list", json!({})).unwrap()["items"][0]["detail"]["anchor"].clone();
        assert_eq!(anchor, json!([50.0, 50.0]));
    }

    #[test]
    fn handle_moves_rotate_and_follow_rays() {
        let mut s = session();
        let id = add_horizontal(&mut s, 50.0);
        // Rotate to vertical through the same anchor.
        s.execute("assist.setAngle", json!({"id": id, "angleDeg": 90.0})).unwrap();
        let snapped = snap_points(&s, &[StrokePoint::new(53.0, 20.0, 1.0)]);
        assert_eq!((snapped[0].x, snapped[0].y), (50.0, 20.0));
        // A coalesced drag is one undo step.
        let steps = s.active().unwrap().history.past_len();
        for x in [51.0, 52.0, 53.0] {
            s.execute("assist.moveAnchor", json!({"id": id, "anchor": [x, 50.0], "coalesce": "assist-drag"})).unwrap();
        }
        assert_eq!(s.active().unwrap().history.past_len(), steps + 1);
        assert!(s.undo());
        // Vanishing points move with their rays: shift the VP down 10 px, the horizontal ray
        // follows, and strokes snap to the new ray.
        s.execute("assist.addPerspective", json!({"points": [[50.0, 50.0]], "rayStep": 90.0})).unwrap();
        let pid = s.execute("assist.list", json!({})).unwrap()["items"][1]["id"].as_u64().unwrap();
        s.execute("assist.moveVanishingPoint", json!({"id": pid, "index": 0, "point": [50.0, 60.0], "coalesce": "vp-drag"})).unwrap();
        let snapped = snap_points(&s, &[StrokePoint::new(20.0, 63.0, 1.0)]);
        assert_eq!((snapped[0].x, snapped[0].y), (20.0, 60.0));
    }

    #[test]
    fn snap_radius_boundaries_are_accepted_and_undoable() {
        let mut s = session();
        for radius in [0.0, MAX_SNAP_RADIUS] {
            let before = s.active().unwrap().doc.assistants.snap_radius;
            s.execute("assist.setSnap", json!({"radius": radius})).unwrap();
            assert_eq!(s.active().unwrap().doc.assistants.snap_radius, radius);
            assert!(s.undo());
            assert_eq!(s.active().unwrap().doc.assistants.snap_radius, before);
        }
    }

    #[test]
    fn bad_params_fail_without_history_steps() {
        let mut s = session();
        let steps = s.active().unwrap().history.past_len();
        for (id, params) in [
            ("assist.addStraightLine", json!({"angleDeg": "flat"})),
            ("assist.addStraightLine", json!({"anchor": [0.0]})),
            ("assist.addPerspective", json!({"points": []})),
            ("assist.addPerspective", json!({"rayStep": 0.0})),
            ("assist.addPerspective", json!({"points": [[0.0, 0.0], [1.0, 1.0], [2.0, 2.0], [3.0, 3.0], [4.0, 4.0]]})),
            ("assist.remove", json!({})),
            ("assist.remove", json!({"id": 999})),
            ("assist.setVisible", json!({"id": 1})),
            ("assist.setSnap", json!({})),
            ("assist.setSnap", json!({"radius": -1.0})),
            ("assist.setSnap", json!({"radius": MAX_SNAP_RADIUS + 1.0})),
            ("assist.setSnap", json!({"radius": 600.0})),
            ("assist.setSnap", json!({"radius": f64::NAN})),
            ("assist.moveAnchor", json!({"id": 1})),
            ("assist.setAngle", json!({"id": 1})),
            ("assist.moveVanishingPoint", json!({"id": 1, "index": 0})),
            ("assist.addVanishingPoint", json!({"id": 1, "point": [f64::NAN, 0.0]})),
            ("assist.removeVanishingPoint", json!({"id": 1})),
            ("assist.removeVanishingPoint", json!({"id": 1, "index": 99})),
            ("assist.addGuide", json!({"id": 1})),
            ("assist.addGuide", json!({"id": 1, "vp": 99, "angleDeg": 0.0})),
            ("assist.removeGuide", json!({"id": 1})),
            ("assist.rotateGuide", json!({"id": 1, "index": 0})),
            ("assist.rotateGuide", json!({"id": 1, "index": 0, "angleDeg": f64::INFINITY})),
            ("assist.setVanishingPointFixed", json!({"id": 1, "index": 0})),
        ] {
            assert!(s.execute(id, params).is_err(), "{id}");
        }
        // Wrong-kind handle commands fail gracefully too.
        let straight = add_horizontal(&mut s, 50.0);
        assert!(s.execute("assist.moveVanishingPoint", json!({"id": straight, "index": 0, "point": [0.0, 0.0]})).is_err());
        s.execute("assist.addPerspective", json!({"points": [[10.0, 10.0]]})).unwrap();
        let pid = s.execute("assist.list", json!({})).unwrap()["items"][1]["id"].as_u64().unwrap();
        assert!(s.execute("assist.moveAnchor", json!({"id": pid, "anchor": [0.0, 0.0]})).is_err());
        assert!(s.execute("assist.setAngle", json!({"id": pid, "angleDeg": 10.0})).is_err());
        assert!(s.execute("assist.moveVanishingPoint", json!({"id": pid, "index": 9, "point": [0.0, 0.0]})).is_err());
        assert_eq!(s.active().unwrap().history.past_len(), steps + 2, "only the two valid adds stepped");
    }

    #[test]
    fn live_stroke_preview_matches_the_snapped_commit() {
        use crate::brush_cmds::LiveStroke;
        let mut s = session();
        add_horizontal(&mut s, 50.0);
        let params = json!({"points": [[10.0, 53.0]], "size": 3.0, "hardness": 1.0, "color": "#000000", "smoothing": 0.0});
        let mut live = LiveStroke::begin(&s, &params).unwrap();
        live.push(&[StrokePoint::new(50.0, 54.0, 1.0), StrokePoint::new(90.0, 53.0, 1.0)]).unwrap();
        let seed = live.seed;
        let layer = s.active().unwrap().active_layer.unwrap();
        let expected = live.doc.layer(layer).unwrap().surface().unwrap().rgba(50, 50);
        assert!(expected[0] < 0.2, "preview snaps: {expected:?}");
        s.offer_live_stroke(live).unwrap();
        let mut commit = json!({"points": [[10.0, 53.0], [50.0, 54.0], [90.0, 53.0]], "size": 3.0, "hardness": 1.0, "color": "#000000", "smoothing": 0.0});
        commit["seed"] = json!(seed);
        let r = s.execute("paint.stroke", commit).unwrap();
        assert_eq!(r["fromLive"], json!(true));
        let actual = s.active().unwrap().doc.layer(layer).unwrap().surface().unwrap().rgba(50, 50);
        assert!((0..4).all(|i| (expected[i] - actual[i]).abs() < 0.02), "commit matches preview: {actual:?} vs {expected:?}");
    }
}

#[cfg(test)]
mod retouch_tests {
    use super::super::retouch_cmds::{LiveDab, LiveRetouch};
    use super::*;
    use photocraft_geom::Rect;
    use serde_json::json;

    fn session() -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 100, "height": 100, "background": "white"})).unwrap();
        s
    }

    fn px(s: &Session, x: i32, y: i32) -> Vec<f32> {
        let st = s.active().unwrap();
        st.doc.layer(st.active_layer.unwrap()).unwrap().surface().unwrap().pixel(x, y)
    }

    fn paint_red_left(s: &mut Session) {
        s.edit("setup", |doc, active| {
            doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap().fill_rect(Rect::new(0, 0, 50, 100), &[1.0, 0.0, 0.0, 1.0]);
            Ok(())
        })
        .unwrap();
    }

    fn paint_gray(s: &mut Session) {
        s.edit("setup", |doc, active| {
            doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap().fill_rect(Rect::new(0, 0, 100, 100), &[0.5, 0.5, 0.5, 1.0]);
            Ok(())
        })
        .unwrap();
    }

    fn add_horizontal(s: &mut Session, y: f64) {
        s.execute("assist.addStraightLine", json!({"anchor": [50.0, y], "angleDeg": 0.0})).unwrap();
    }

    #[test]
    fn clone_stamp_snaps_to_guide_and_samples_snapped_offset() {
        let mut s = session();
        paint_red_left(&mut s);
        add_horizontal(&mut s, 50.0);
        let r = s.execute("paint.cloneStamp", json!({"points": [[60, 53], [80, 53]], "source": [10, 50], "size": 8, "hardness": 100})).unwrap();
        assert_eq!(r["offset"], json!([-50, 0]), "the source mapping follows the snapped first point");
        let p = px(&s, 70, 50);
        assert!(p[0] > 0.99 && p[1] < 0.01 && p[3] > 0.99, "cloned red onto the guide: {p:?}");
        assert_eq!(px(&s, 70, 80), vec![1.0; 4], "far from the guide untouched");
    }

    #[test]
    fn dodge_snaps_to_guide() {
        let mut s = session();
        paint_gray(&mut s);
        add_horizontal(&mut s, 50.0);
        s.execute("paint.dodge", json!({"points": [[10, 53], [90, 53]], "size": 4, "hardness": 100})).unwrap();
        assert!(px(&s, 50, 50)[0] > 0.53, "the guide row dodges: {:?}", px(&s, 50, 50));
        assert!((px(&s, 50, 80)[0] - 0.5).abs() < 0.01, "far from the guide untouched");
    }

    #[test]
    fn live_retouch_preview_snaps() {
        let mut s = session();
        paint_red_left(&mut s);
        add_horizontal(&mut s, 50.0);
        let mut live = LiveRetouch::begin(&s, "paint.cloneStamp", &json!({"points": [[60, 53]], "source": [10, 50], "size": 8, "hardness": 100})).unwrap();
        live.push(&[StrokePoint::new(80.0, 53.0, 1.0)]).unwrap();
        assert_eq!(live.points().iter().map(|p| [p.x, p.y]).collect::<Vec<_>>(), vec![[60.0, 50.0], [80.0, 50.0]]);
        let st = s.active().unwrap();
        let p = live.doc.layer(st.active_layer.unwrap()).unwrap().surface().unwrap().rgba(70, 50);
        assert!(p[0] > 0.99 && p[1] < 0.01, "preview clones onto the guide: {p:?}");
    }

    #[test]
    fn live_dab_preview_snaps() {
        let mut s = session();
        paint_gray(&mut s);
        add_horizontal(&mut s, 50.0);
        let mut live = LiveDab::begin(&s, "paint.dodge", &json!({"points": [[50, 53]], "size": 4, "hardness": 100})).unwrap();
        live.push(&[StrokePoint::new(70.0, 53.0, 1.0)]).unwrap();
        let st = s.active().unwrap();
        let surf = live.doc.layer(st.active_layer.unwrap()).unwrap().surface().unwrap();
        assert!(surf.rgba(50, 50)[0] > 0.53, "preview dodges the guide");
        assert!((surf.rgba(50, 80)[0] - 0.5).abs() < 0.01, "far from the guide untouched");
    }

    #[test]
    fn retouch_snapping_off_leaves_points_unadjusted() {
        let mut s = session();
        paint_red_left(&mut s);
        add_horizontal(&mut s, 50.0);
        s.execute("assist.setSnap", json!({"enabled": false})).unwrap();
        // A small brush (radius 2): the raw y = 53 samples leave the y = 50 row untouched.
        let r = s.execute("paint.cloneStamp", json!({"points": [[60, 53], [80, 53]], "source": [10, 50], "size": 4, "hardness": 100})).unwrap();
        assert_eq!(r["offset"], json!([-50, -3]), "the raw first point maps the source");
        let p = px(&s, 70, 53);
        assert!(p[0] > 0.99 && p[1] < 0.01, "the raw row paints instead: {p:?}");
    }
}
