//! Companion-side script evaluator (v1 subset, `std`).
//!
//! The `no_std` core cannot host a parser, so scripts evaluate here, one
//! sample at a time, and the outputs drive replayed strokes. Language (the
//! documented v1 subset; deviations from any proprietary engine are ours):
//!
//! - `;`-separated statements, `name = expression`; `//` and `/* */` comments.
//! - Inputs (read-only): `x y p d s nx ny` (position, pressure, travel,
//!   speed px/ms, last-segment normal); constants `pi e`.
//! - Outputs: `ox oy op` (at least one must be assigned); unassigned outputs
//!   pass their input through (`ox=x, oy=y, op=p`).
//! - User variables persist across samples and read as 0 before first write.
//! - Operators `+ - * / ^`, comparisons returning 1/0, `&& || !` (short
//!   circuit), `?:` (lazy branches). `f32` throughout.
//! - Functions: `sine square triangle saw sigmoid noise cell_noise mix
//!   clamp smooth_step` (the core's wave table) plus `abs min max`.
//! - Pen-up rule: `p == 0` forces `op = 0`, whatever the script wrote.
//! - A non-finite output aborts the run naming the sample (silent NaNs would
//!   poison the replay).

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::waves;



/// One sample's inputs.
#[derive(Clone, Copy, Debug)]
pub struct Sample {
    pub x: f32,
    pub y: f32,
    pub p: f32,
    pub d: f32,
    pub s: f32,
    pub nx: f32,
    pub ny: f32,
}

/// One sample's outputs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Outputs {
    pub ox: f32,
    pub oy: f32,
    pub op: f32,
}

/// Minimal point view the evaluator needs (mirrors the host `StrokePoint`
/// fields it reads; the engine maps to this).
#[derive(Clone, Copy, Debug)]
pub struct ScriptInput {
    pub x: f64,
    pub y: f64,
    pub pressure: f32,
    pub time: f64,
}

/// Derive per-sample inputs from recorded points (travel, speed px/ms, and
/// segment normals included).
pub fn samples_from_points(pts: &[ScriptInput]) -> Vec<Sample> {
    let mut out = Vec::with_capacity(pts.len());
    let mut travel = 0.0f32;
    let mut prev: Option<(f32, f32, f64)> = None;
    for p in pts {
        let (x, y) = (p.x as f32, p.y as f32);
        let (dist, dt, nx, ny) = match prev {
            Some((qx, qy, qt)) => {
                let (dx, dy) = (x - qx, y - qy);
                let dist = (dx * dx + dy * dy).sqrt();
                let dt = (p.time - qt).max(0.0);
                let (nx, ny) = if dist > 1e-9 { (dy / dist, -dx / dist) } else { (0.0, 0.0) };
                (dist, dt as f32, nx, ny)
            }
            None => (0.0, 0.0, 0.0, 0.0),
        };
        travel += dist;
        let s = if dt > 0.0 { dist / dt } else { 0.0 };
        prev = Some((x, y, p.time));
        out.push(Sample { x, y, p: p.pressure, d: travel, s, nx, ny });
    }
    out
}

const INPUTS: &[&str] = &["x", "y", "p", "d", "s", "nx", "ny", "pi", "e"];
const OUTPUTS: &[&str] = &["ox", "oy", "op"];

#[derive(Clone, Debug)]
enum Expr {
    Num(f32),
    Var(String),
    Not(Box<Expr>),
    Neg(Box<Expr>),
    Bin(BinOp, Box<Expr>, Box<Expr>),
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    Call(String, Vec<Expr>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    And,
    Or,
}

#[derive(Clone, Debug)]
enum Tok {
    Num(f32),
    Ident(String),
    Sym(Sym),
    Semi,
    End,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sym {
    Plus,
    Minus,
    Star,
    Slash,
    Caret,
    Bang,
    LParen,
    RParen,
    Comma,
    Assign,
    Q,
    Colon,
    Lt,
    Le,
    Gt,
    Ge,
    EqEq,
    Ne,
    AndAnd,
    OrOr,
}

struct Lex<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Lex<'a> {
    fn new(src: &'a str) -> Self {
        Self { b: src.as_bytes(), i: 0 }
    }

    fn error(&self, msg: &str) -> String {
        format!("offset {}: {msg}", self.i.min(self.b.len()))
    }

    fn skip_ws_and_comments(&mut self) -> Result<(), String> {
        loop {
            while let Some(c) = self.b.get(self.i) {
                if c.is_ascii_whitespace() {
                    self.i += 1;
                } else {
                    break;
                }
            }
            if self.b.get(self.i) == Some(&b'/') {
                match self.b.get(self.i + 1) {
                    Some(b'/') => {
                        self.i += 2;
                        while self.b.get(self.i).is_some_and(|c| *c != b'\n') {
                            self.i += 1;
                        }
                        continue;
                    }
                    Some(b'*') => {
                        self.i += 2;
                        loop {
                            match self.b.get(self.i) {
                                None => return Err(self.error("unterminated /* comment")),
                                Some(b'*') if self.b.get(self.i + 1) == Some(&b'/') => {
                                    self.i += 2;
                                    break;
                                }
                                _ => self.i += 1,
                            }
                        }
                        continue;
                    }
                    _ => {}
                }
            }
            return Ok(());
        }
    }

    fn next_tok(&mut self) -> Result<(Tok, usize), String> {
        self.skip_ws_and_comments()?;
        let at = self.i;
        let Some(&c) = self.b.get(self.i) else { return Ok((Tok::End, at)) };
        // Numbers: digits, at most one dot, optional exponent.
        if c.is_ascii_digit() || (c == b'.' && self.b.get(self.i + 1).is_some_and(|d| d.is_ascii_digit())) {
            let mut j = self.i;
            while self.b.get(j).is_some_and(|d| d.is_ascii_digit()) {
                j += 1;
            }
            if self.b.get(j) == Some(&b'.') {
                j += 1;
                while self.b.get(j).is_some_and(|d| d.is_ascii_digit()) {
                    j += 1;
                }
            }
            if self.b.get(j) == Some(&b'e') || self.b.get(j) == Some(&b'E') {
                let mut k = j + 1;
                if self.b.get(k) == Some(&b'+') || self.b.get(k) == Some(&b'-') {
                    k += 1;
                }
                if self.b.get(k).is_some_and(|d| d.is_ascii_digit()) {
                    j = k;
                    while self.b.get(j).is_some_and(|d| d.is_ascii_digit()) {
                        j += 1;
                    }
                }
            }
            let text = core::str::from_utf8(self.b.get(self.i..j).unwrap_or(&[])).unwrap_or("");
            let v: f32 = text.parse().map_err(|_| format!("offset {at}: bad number {text:?}"))?;
            self.i = j;
            return Ok((Tok::Num(v), at));
        }
        if c.is_ascii_alphabetic() || c == b'_' {
            let mut j = self.i;
            while self.b.get(j).is_some_and(|d| d.is_ascii_alphanumeric() || *d == b'_') {
                j += 1;
            }
            let name = core::str::from_utf8(self.b.get(self.i..j).unwrap_or(&[])).unwrap_or("").to_string();
            self.i = j;
            return Ok((Tok::Ident(name), at));
        }
        let two = || self.b.get(self.i + 1).copied();
        let sym = match (c, two()) {
            (b'&', Some(b'&')) => {
                self.i += 2;
                Sym::AndAnd
            }
            (b'|', Some(b'|')) => {
                self.i += 2;
                Sym::OrOr
            }
            (b'=', Some(b'=')) => {
                self.i += 2;
                Sym::EqEq
            }
            (b'!', Some(b'=')) => {
                self.i += 2;
                Sym::Ne
            }
            (b'<', Some(b'=')) => {
                self.i += 2;
                Sym::Le
            }
            (b'>', Some(b'=')) => {
                self.i += 2;
                Sym::Ge
            }
            (b'+', _) => {
                self.i += 1;
                Sym::Plus
            }
            (b'-', _) => {
                self.i += 1;
                Sym::Minus
            }
            (b'*', _) => {
                self.i += 1;
                Sym::Star
            }
            (b'/', _) => {
                self.i += 1;
                Sym::Slash
            }
            (b'^', _) => {
                self.i += 1;
                Sym::Caret
            }
            (b'!', _) => {
                self.i += 1;
                Sym::Bang
            }
            (b'(', _) => {
                self.i += 1;
                Sym::LParen
            }
            (b')', _) => {
                self.i += 1;
                Sym::RParen
            }
            (b',', _) => {
                self.i += 1;
                Sym::Comma
            }
            (b'=', _) => {
                self.i += 1;
                Sym::Assign
            }
            (b'?', _) => {
                self.i += 1;
                Sym::Q
            }
            (b':', _) => {
                self.i += 1;
                Sym::Colon
            }
            (b'<', _) => {
                self.i += 1;
                Sym::Lt
            }
            (b'>', _) => {
                self.i += 1;
                Sym::Gt
            }
            (b';', _) => {
                self.i += 1;
                return Ok((Tok::Semi, at));
            }
            _ => return Err(format!("offset {at}: unexpected character {:?}", c as char)),
        };
        Ok((Tok::Sym(sym), at))
    }
}

struct Parser<'a> {
    lex: Lex<'a>,
    cur: Tok,
    at: usize,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str) -> Result<Self, String> {
        let mut lex = Lex::new(src);
        let (cur, at) = lex.next_tok()?;
        Ok(Self { lex, cur, at })
    }

    fn bump(&mut self) -> Result<(), String> {
        let (cur, at) = self.lex.next_tok()?;
        self.cur = cur;
        self.at = at;
        Ok(())
    }

    fn eat(&mut self, s: Sym) -> Result<bool, String> {
        if self.cur_sym() == Some(s) {
            self.bump()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn cur_sym(&self) -> Option<Sym> {
        match &self.cur {
            Tok::Sym(s) => Some(*s),
            _ => None,
        }
    }

    fn expect(&mut self, s: Sym, what: &str) -> Result<(), String> {
        if self.eat(s)? {
            Ok(())
        } else {
            Err(format!("offset {}: expected {what}", self.at))
        }
    }

    fn program(&mut self) -> Result<Vec<(String, Expr)>, String> {
        let mut stmts = Vec::new();
        while !matches!(self.cur, Tok::End) {
            if self.eat_sym_semi()? {
                continue;
            }
            let name = match &self.cur {
                Tok::Ident(n) => n.clone(),
                _ => return Err(format!("offset {}: expected `name = expression`", self.at)),
            };
            self.bump()?;
            self.expect(Sym::Assign, "`=`")?;
            let e = self.expr()?;
            if INPUTS.contains(&name.as_str()) {
                return Err(format!("offset {}: `{name}` is read-only", self.at));
            }
            stmts.push((name, e));
            if !matches!(self.cur, Tok::End) {
                self.expect_semi()?;
            }
        }
        if !stmts.iter().any(|(n, _)| OUTPUTS.contains(&n.as_str())) {
            return Err("no output assigned: write at least one of ox, oy, op".to_string());
        }
        Ok(stmts)
    }

    fn eat_sym_semi(&mut self) -> Result<bool, String> {
        if matches!(self.cur, Tok::Semi) {
            self.bump()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn expect_semi(&mut self) -> Result<(), String> {
        if matches!(self.cur, Tok::Semi) {
            self.bump()?;
            Ok(())
        } else {
            Err(format!("offset {}: expected `;` between statements", self.at))
        }
    }

    fn expr(&mut self) -> Result<Expr, String> {
        let c = self.or()?;
        if self.eat(Sym::Q)? {
            let t = self.expr()?;
            self.expect(Sym::Colon, "`:`")?;
            let f = self.expr()?;
            return Ok(Expr::Ternary(Box::new(c), Box::new(t), Box::new(f)));
        }
        Ok(c)
    }

    fn or(&mut self) -> Result<Expr, String> {
        let mut e = self.and()?;
        while self.eat(Sym::OrOr)? {
            let r = self.and()?;
            e = Expr::Bin(BinOp::Or, Box::new(e), Box::new(r));
        }
        Ok(e)
    }

    fn and(&mut self) -> Result<Expr, String> {
        let mut e = self.cmp()?;
        while self.eat(Sym::AndAnd)? {
            let r = self.cmp()?;
            e = Expr::Bin(BinOp::And, Box::new(e), Box::new(r));
        }
        Ok(e)
    }

    fn cmp(&mut self) -> Result<Expr, String> {
        let mut e = self.add()?;
        loop {
            let op = match self.cur_sym() {
                Some(Sym::Lt) => BinOp::Lt,
                Some(Sym::Le) => BinOp::Le,
                Some(Sym::Gt) => BinOp::Gt,
                Some(Sym::Ge) => BinOp::Ge,
                Some(Sym::EqEq) => BinOp::Eq,
                Some(Sym::Ne) => BinOp::Ne,
                _ => break,
            };
            self.bump()?;
            let r = self.add()?;
            e = Expr::Bin(op, Box::new(e), Box::new(r));
        }
        Ok(e)
    }

    fn add(&mut self) -> Result<Expr, String> {
        let mut e = self.mul()?;
        loop {
            if self.eat(Sym::Plus)? {
                let r = self.mul()?;
                e = Expr::Bin(BinOp::Add, Box::new(e), Box::new(r));
            } else if self.eat(Sym::Minus)? {
                let r = self.mul()?;
                e = Expr::Bin(BinOp::Sub, Box::new(e), Box::new(r));
            } else {
                break;
            }
        }
        Ok(e)
    }

    fn mul(&mut self) -> Result<Expr, String> {
        let mut e = self.pow()?;
        loop {
            if self.eat(Sym::Star)? {
                let r = self.pow()?;
                e = Expr::Bin(BinOp::Mul, Box::new(e), Box::new(r));
            } else if self.eat(Sym::Slash)? {
                let r = self.pow()?;
                e = Expr::Bin(BinOp::Div, Box::new(e), Box::new(r));
            } else {
                break;
            }
        }
        Ok(e)
    }

    fn pow(&mut self) -> Result<Expr, String> {
        let b = self.unary()?;
        if self.eat(Sym::Caret)? {
            let e = self.pow()?;
            return Ok(Expr::Bin(BinOp::Pow, Box::new(b), Box::new(e)));
        }
        Ok(b)
    }

    fn unary(&mut self) -> Result<Expr, String> {
        if self.eat(Sym::Minus)? {
            return Ok(Expr::Neg(Box::new(self.unary()?)));
        }
        if self.eat(Sym::Plus)? {
            return self.unary();
        }
        if self.eat(Sym::Bang)? {
            return Ok(Expr::Not(Box::new(self.unary()?)));
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<Expr, String> {
        match &self.cur {
            Tok::Num(v) => {
                let v = *v;
                self.bump()?;
                Ok(Expr::Num(v))
            }
            Tok::Ident(name) => {
                let name = name.clone();
                self.bump()?;
                if self.eat(Sym::LParen)? {
                    let mut args = Vec::new();
                    if !self.eat(Sym::RParen)? {
                        loop {
                            args.push(self.expr()?);
                            if self.eat(Sym::Comma)? {
                                continue;
                            }
                            self.expect(Sym::RParen, "`)`")?;
                            break;
                        }
                    }
                    check_arity(&name, args.len(), self.at)?;
                    Ok(Expr::Call(name, args))
                } else {
                    Ok(Expr::Var(name))
                }
            }
            _ => {
                if self.eat(Sym::LParen)? {
                    let e = self.expr()?;
                    self.expect(Sym::RParen, "`)`")?;
                    Ok(e)
                } else {
                    Err(format!("offset {}: expected a value", self.at))
                }
            }
        }
    }
}

fn arity(name: &str) -> Option<usize> {
    match name {
        "sine" | "triangle" | "saw" | "sigmoid" | "noise" | "cell_noise" | "abs" | "sqrt" => Some(1),
        "square" | "min" | "max" => Some(2),
        "mix" | "clamp" | "smooth_step" => Some(3),
        _ => None,
    }
}

fn check_arity(name: &str, n: usize, at: usize) -> Result<(), String> {
    match arity(name) {
        Some(a) if a == n => Ok(()),
        Some(a) => Err(format!("offset {at}: `{name}` takes {a} arguments, got {n}")),
        None => Err(format!("offset {at}: unknown function `{name}`")),
    }
}

/// A compiled script: statements plus the persistent variable store.
pub struct Script {
    stmts: Vec<(String, Expr)>,
    vars: BTreeMap<String, f32>,
}

/// Compile script source.
pub fn compile(src: &str) -> Result<Script, String> {
    let mut p = Parser::new(src)?;
    Ok(Script { stmts: p.program()?, vars: BTreeMap::new() })
}

fn truth(v: f32) -> bool {
    v != 0.0 && !v.is_nan()
}

impl Script {
    fn eval_expr(&self, e: &Expr, s: &Sample, vars: &BTreeMap<String, f32>) -> f32 {
        match e {
            Expr::Num(v) => *v,
            Expr::Var(n) => read_var(n, s, vars),
            Expr::Not(x) => {
                if truth(self.eval_expr(x, s, vars)) {
                    0.0
                } else {
                    1.0
                }
            }
            Expr::Neg(x) => -self.eval_expr(x, s, vars),
            Expr::Bin(op, l, r) => match op {
                BinOp::And => {
                    if truth(self.eval_expr(l, s, vars)) && truth(self.eval_expr(r, s, vars)) {
                        1.0
                    } else {
                        0.0
                    }
                }
                BinOp::Or => {
                    if truth(self.eval_expr(l, s, vars)) || truth(self.eval_expr(r, s, vars)) {
                        1.0
                    } else {
                        0.0
                    }
                }
                _ => {
                    let (a, b) = (self.eval_expr(l, s, vars), self.eval_expr(r, s, vars));
                    apply_bin(*op, a, b)
                }
            },
            Expr::Ternary(c, t, f) => {
                if truth(self.eval_expr(c, s, vars)) {
                    self.eval_expr(t, s, vars)
                } else {
                    self.eval_expr(f, s, vars)
                }
            }
            Expr::Call(n, args) => {
                let v: Vec<f32> = args.iter().map(|a| self.eval_expr(a, s, vars)).collect();
                apply_call(n, &v)
            }
        }
    }

    /// Run one sample; user variables carry over from previous samples.
    pub fn eval(&mut self, s: &Sample) -> Result<Outputs, String> {
        // Snapshot vars for reads; writes land directly (later statements in
        // the same sample see earlier ones: evaluate against a working copy).
        let mut vars = self.vars.clone();
        for (name, e) in &self.stmts {
            let v = self.eval_expr(e, s, &vars);
            vars.insert(name.clone(), v);
        }
        self.vars = vars;
        let get = |n: &str, dflt: f32| self.vars.get(n).copied().unwrap_or(dflt);
        let (ox, oy, mut op) = (get("ox", s.x), get("oy", s.y), get("op", s.p));
        if s.p == 0.0 {
            op = 0.0;
        }
        for (v, n) in [(ox, "ox"), (oy, "oy"), (op, "op")] {
            if !v.is_finite() {
                return Err(format!("{n} is non-finite (got {v}); check divisions and function inputs"));
            }
        }
        Ok(Outputs { ox, oy, op })
    }
}

fn read_var(name: &str, s: &Sample, vars: &BTreeMap<String, f32>) -> f32 {
    match name {
        "x" => s.x,
        "y" => s.y,
        "p" => s.p,
        "d" => s.d,
        "nx" => s.nx,
        "ny" => s.ny,
        "s" => s.s,
        "pi" => core::f32::consts::PI,
        "e" => core::f32::consts::E,
        _ => vars.get(name).copied().unwrap_or(0.0),
    }
}

fn apply_bin(op: BinOp, a: f32, b: f32) -> f32 {
    match op {
        BinOp::Add => a + b,
        BinOp::Sub => a - b,
        BinOp::Mul => a * b,
        BinOp::Div => a / b,
        BinOp::Pow => lib_pow(a, b),
        BinOp::Lt => bool01(a < b),
        BinOp::Le => bool01(a <= b),
        BinOp::Gt => bool01(a > b),
        BinOp::Ge => bool01(a >= b),
        BinOp::Eq => bool01(a == b),
        BinOp::Ne => bool01(a != b),
        BinOp::And | BinOp::Or => 0.0,
    }
}

fn bool01(b: bool) -> f32 {
    if b {
        1.0
    } else {
        0.0
    }
}

fn lib_pow(a: f32, b: f32) -> f32 {
    if !a.is_finite() || !b.is_finite() {
        return f32::NAN;
    }
    (a).powf(b)
}

fn apply_call(name: &str, v: &[f32]) -> f32 {
    let a = |i: usize| v.get(i).copied().unwrap_or(f32::NAN);
    match name {
        "sine" => waves::sine(a(0)),
        "triangle" => waves::triangle(a(0)),
        "saw" => waves::saw(a(0)),
        "sigmoid" => waves::sigmoid(a(0)),
        "noise" => waves::noise(a(0)),
        "cell_noise" => waves::cell_noise(a(0)),
        "square" => waves::square(a(0), a(1)),
        "min" => a(0).min(a(1)),
        "max" => a(0).max(a(1)),
        "abs" => a(0).abs(),
        "sqrt" => a(0).sqrt(),
        "mix" => waves::mix(a(0), a(1), a(2)),
        "clamp" => waves::clamp(a(0), a(1), a(2)),
        "smooth_step" => waves::smooth_step(a(0), a(1), a(2)),
        _ => f32::NAN,
    }
}

/// Run a whole stroke, tracking the sample index for error messages.
pub fn eval_stroke(script: &mut Script, samples: &[Sample]) -> Result<Vec<Outputs>, String> {
    let mut out = Vec::with_capacity(samples.len());
    for (i, s) in samples.iter().enumerate() {
        out.push(script.eval(s).map_err(|e| format!("sample {i}: {e}"))?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(x: f32, y: f32, p: f32) -> Sample {
        Sample { x, y, p, d: 0.0, s: 0.0, nx: 0.0, ny: 0.0 }
    }

    fn run(src: &str, s: Sample) -> Result<Outputs, String> {
        compile(src)?.eval(&s)
    }

    fn ox(src: &str, s: Sample) -> f32 {
        run(src, s).unwrap().ox
    }

    #[test]
    fn arithmetic_and_precedence() {
        let s = sample(0.0, 0.0, 1.0);
        assert!((ox("ox = 2 + 3 * 4;", s) - 14.0).abs() < 1e-5);
        assert!((ox("ox = (2 + 3) * 4;", s) - 20.0).abs() < 1e-5);
        assert!((ox("ox = 2 ^ 3 ^ 2;", s) - 512.0).abs() < 1e-3, "power is right-associative");
        assert!((ox("ox = -2 * 3;", s) + 6.0).abs() < 1e-5);
        assert!((ox("ox = 7 / 2;", s) - 3.5).abs() < 1e-5);
    }

    #[test]
    fn ternary_comparisons_and_logic() {
        let s = sample(3.0, 0.0, 1.0);
        assert!((ox("ox = x > 2 ? 10 : 20;", s) - 10.0).abs() < 1e-5);
        assert!((ox("ox = x < 2 ? 10 : 20;", s) - 20.0).abs() < 1e-5);
        assert!((ox("ox = x >= 3 && p == 1 ? 1 : 0;", s) - 1.0).abs() < 1e-5);
        assert!((ox("ox = x != 3 || p != 1 ? 1 : 0;", s) - 0.0).abs() < 1e-5);
        assert!((ox("ox = !0;", s) - 1.0).abs() < 1e-5);
        // Short circuit: the dead branch would divide by zero.
        assert!((ox("ox = 1 || 1 / 0 > 5 ? 7 : 8;", s) - 7.0).abs() < 1e-5);
        assert!((ox("ox = 0 && 1 / 0 > 5 ? 7 : 8;", s) - 8.0).abs() < 1e-5);
    }

    #[test]
    fn comments_and_separators() {
        let s = sample(1.0, 2.0, 1.0);
        let src = "// lead\n ox = x + 1; /* mid */ oy = y * 2;; op = p;";
        let o = run(src, s).unwrap();
        assert!((o.ox - 2.0).abs() < 1e-5 && (o.oy - 4.0).abs() < 1e-5 && (o.op - 1.0).abs() < 1e-5);
    }

    #[test]
    fn state_carries_across_samples() {
        let mut sc = compile("ox = n; n = n + 1;").unwrap();
        let s = sample(0.0, 0.0, 1.0);
        assert!((sc.eval(&s).unwrap().ox - 0.0).abs() < 1e-5, "unwritten reads as 0");
        assert!((sc.eval(&s).unwrap().ox - 1.0).abs() < 1e-5);
        assert!((sc.eval(&s).unwrap().ox - 2.0).abs() < 1e-5);
    }

    #[test]
    fn pen_up_forces_zero_and_defaults_pass_through() {
        let down = sample(5.0, 6.0, 0.5);
        let o = run("ox = x * 2;", down).unwrap();
        assert!((o.ox - 10.0).abs() < 1e-5);
        assert!((o.oy - 6.0).abs() < 1e-5, "unassigned oy passes y through");
        assert!((o.op - 0.5).abs() < 1e-5, "unassigned op passes p through");
        let up = sample(5.0, 6.0, 0.0);
        let o = run("ox = x * 2; op = 0.9;", up).unwrap();
        assert_eq!(o.op, 0.0, "pen-up rule overrides the script");
    }

    #[test]
    fn functions_match_the_core_table() {
        let s = sample(0.0, 0.0, 1.0);
        assert!((ox("ox = sine(0.25);", s) - 1.0).abs() < 1e-4);
        assert!((ox("ox = mix(10, 20, 0.25);", s) - 12.5).abs() < 1e-4);
        assert!((ox("ox = clamp(9, 0, 5);", s) - 5.0).abs() < 1e-4);
        assert!((ox("ox = smooth_step(0, 1, 0.5);", s) - 0.5).abs() < 1e-4);
        assert!((ox("ox = min(3, 7) + max(3, 7);", s) - 10.0).abs() < 1e-4);
        // Deterministic noise: same input, same output, twice.
        assert_eq!(ox("ox = noise(d);", s), ox("ox = noise(d);", s));
    }

    #[test]
    fn compile_errors_are_actionable() {
        assert!(compile("ox = 1 +;").is_err());
        assert!(compile("ox = unknown_fn(2);").is_err());
        assert!(compile("ox = mix(1, 2);").is_err(), "arity checked");
        assert!(compile("oy = 1;").is_ok(), "oy alone satisfies the output rule");
        assert!(compile("tmp = 1;").is_err(), "at least one output required");
        assert!(compile("x = 2; ox = x;").is_err(), "inputs are read-only");
        assert!(compile("ox = 1; /* open").is_err(), "unterminated comment");
        assert!(compile("ox = 1 @ 2;").is_err(), "bad character");
    }

    #[test]
    fn runtime_errors_name_the_sample() {
        let mut sc = compile("ox = 1 / (x - x);").unwrap();
        let s = sample(4.0, 0.0, 1.0);
        let err = sc.eval(&s).unwrap_err();
        assert!(err.contains("ox") && err.contains("non-finite"), "{err}");
        let samples = vec![sample(1.0, 0.0, 1.0), sample(2.0, 0.0, 1.0)];
        let err = eval_stroke(&mut compile("op = p / (x - 2);").unwrap(), &samples).unwrap_err();
        assert!(err.contains("sample 1"), "{err}");
    }

    #[test]
    fn samples_derive_travel_speed_and_normals() {
        let pts = vec![
            ScriptInput { x: 0.0, y: 0.0, pressure: 1.0, time: 0.0 },
            ScriptInput { x: 3.0, y: 4.0, pressure: 0.5, time: 10.0 },
        ];
        let s = samples_from_points(&pts);
        assert!((s[1].d - 5.0).abs() < 1e-5);
        assert!((s[1].s - 0.5).abs() < 1e-5, "5 px in 10 ms");
        assert!((s[1].nx - 0.8).abs() < 1e-5 && (s[1].ny + 0.6).abs() < 1e-5, "normal of (3,4)");
    }
}
