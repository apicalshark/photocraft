//! Scripted stroke transform (slice of P3): run a v1 distortion script over a
//! point list and return the transformed points. Pure query (no document,
//! no painting): feed the result into `paint.stroke` to paint it, or preview
//! it anywhere points render. The evaluator lives in `photocraft-stroke`;
//! L-Systems stay out (they need an iterative turtle vehicle, not a
//! single-pass mapping).

use photocraft_stroke::script::{ScriptInput, compile, eval_stroke, samples_from_points};
use serde_json::{Value, json};

use crate::{EngineError, Result, Session, commands::CommandSpec};

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

/// Largest accepted script source (a runaway paste must not balloon memory).
const MAX_SCRIPT_LEN: usize = 65536;

fn script_points(_s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "paint.scriptPoints";
    let code = p.get("script").and_then(Value::as_str).filter(|s| !s.trim().is_empty()).ok_or_else(|| bad(cmd, "missing `script`"))?;
    if code.len() > MAX_SCRIPT_LEN {
        return Err(bad(cmd, "script too long (max 64 KiB)"));
    }
    let pts = crate::brush_cmds::parse_points(p, cmd)?;
    let inputs: Vec<ScriptInput> =
        pts.iter().map(|q| ScriptInput { x: q.x, y: q.y, pressure: q.pressure, time: q.time }).collect();
    let samples = samples_from_points(&inputs);
    let mut script = compile(code).map_err(|e| bad(cmd, e))?;
    let outs = eval_stroke(&mut script, &samples).map_err(|e| bad(cmd, e))?;
    // 1:1 with the input by construction; zip truncates rather than panics.
    let points: Vec<Value> = pts
        .iter()
        .zip(outs.iter())
        .map(|(q, o)| {
            json!({
                "x": f64::from(o.ox),
                "y": f64::from(o.oy),
                "pressure": o.op,
                "tiltX": q.tilt_x,
                "tiltY": q.tilt_y,
                "rotation": q.rotation,
                "wheel": q.wheel,
                "time": q.time,
            })
        })
        .collect();
    Ok(json!({ "points": points }))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "paint.scriptPoints",
        label: "Script Points",
        menu: &[],
        shortcut: None,
        params: r#"{"script":code,"points":paint.stroke-style} → {points:[{x,y,pressure,tiltX,tiltY,rotation,wheel,time}]} (v1 subset: ox/oy/op assignments over x y p d s nx ny + wave/math fns; p==0 forces op=0)"#,
        enabled: |_| Ok(()),
        run: script_points,
        journal: false,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pts() -> Value {
        json!([[0.0, 0.0], [5.0, 0.0], [10.0, 0.0]])
    }

    #[test]
    fn sine_wave_displaces_and_round_trips() {
        // sine() runs full cycles: d = 0/5/10 px at 0.05 waves/px gives 0/1/0.
        let mut s = Session::new();
        let r = s
            .execute("paint.scriptPoints", json!({"script": "oy = y + sine(d * 0.05) * 4 * p; op = p;", "points": pts()}))
            .unwrap();
        let out = r["points"].as_array().unwrap();
        assert_eq!(out.len(), 3);
        assert!(out[0]["y"].as_f64().unwrap().abs() < 1e-6);
        assert!((out[1]["y"].as_f64().unwrap() - 4.0).abs() < 1e-4, "quarter wave lifts: {out:?}");
        assert!(out[2]["y"].as_f64().unwrap().abs() < 1e-4, "half wave rests: {out:?}");
        assert!((out[2]["pressure"].as_f64().unwrap() - 1.0).abs() < 1e-6);
        // Twice is identical (state resets per call).
        let r2 = s.execute("paint.scriptPoints", json!({"script": "oy = y + sine(d * 0.05) * 4 * p; op = p;", "points": pts()})).unwrap();
        assert_eq!(r, r2);
    }

    #[test]
    fn pen_up_and_errors() {
        let mut s = Session::new();
        // Pen-up forces op = 0 whatever the script writes.
        let r = s
            .execute("paint.scriptPoints", json!({"script": "op = 0.9;", "points": [[0, 0, 0.0], [10, 0, 0.8]]}))
            .unwrap();
        assert_eq!(r["points"][0]["pressure"].as_f64().unwrap(), 0.0);
        assert!((r["points"][1]["pressure"].as_f64().unwrap() - 0.9).abs() < 1e-6);
        // Compile errors name the offset; runtime blowups name the sample.
        assert!(s.execute("paint.scriptPoints", json!({"script": "oy = = 3;", "points": pts()})).is_err());
        let e = s
            .execute("paint.scriptPoints", json!({"script": "op = p / (x - 10);", "points": [[0, 0], [10, 0], [20, 0]]}))
            .unwrap_err();
        assert!(e.to_string().contains("sample 1"), "{e}");
        // Hostile params error, never panic.
        assert!(s.execute("paint.scriptPoints", json!({})).is_err());
        assert!(s.execute("paint.scriptPoints", json!({"script": "ox = x;"})).is_err());
        assert!(s.execute("paint.scriptPoints", json!({"script": "", "points": pts()})).is_err());
        assert!(s.execute("paint.scriptPoints", json!({"script": 42, "points": pts()})).is_err());
        assert!(s.execute("paint.scriptPoints", json!({"script": "ox = x;", "points": []})).is_err());
        let long = "x".repeat(70000);
        assert!(s.execute("paint.scriptPoints", json!({"script": long, "points": pts()})).is_err());
    }
}
