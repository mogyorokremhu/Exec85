//! Calculator: type an expression, press Enter. Supports + - * / % ^, parentheses, constants
//! (pi, e, ans) and functions (sqrt, abs, sin, cos, tan, asin, acos, atan, ln, log, exp, floor, ceil, round).
use super::{Application, Env, Kind, Out};
use crate::keys::{code, Key};
use crate::layout::Rect;
use crate::paint::Painter;

// ---- the expression evaluator (pure, tested on its own) ------------------------------------------

struct Parser<'a> { s: &'a [u8], i: usize, ans: f64 }

type R = Result<f64, String>;

impl Parser<'_> {
    fn ws(&mut self) { while self.i < self.s.len() && self.s[self.i] == b' ' { self.i += 1; } }
    fn peek(&mut self) -> Option<u8> { self.ws(); self.s.get(self.i).copied() }
    fn eat(&mut self, c: u8) -> bool { if self.peek() == Some(c) { self.i += 1; true } else { false } }

    fn expr(&mut self) -> R {
        let mut v = self.term()?;
        loop {
            if self.eat(b'+') { v += self.term()?; } else if self.eat(b'-') { v -= self.term()?; } else { return Ok(v); }
        }
    }
    fn term(&mut self) -> R {
        let mut v = self.unary()?;
        loop {
            if self.eat(b'*') { v *= self.unary()?; }
            else if self.eat(b'/') { let d = self.unary()?; if d == 0.0 { return Err("Division by zero".into()); } v /= d; }
            else if self.eat(b'%') { let d = self.unary()?; if d == 0.0 { return Err("Division by zero".into()); } v %= d; }
            else { return Ok(v); }
        }
    }
    fn unary(&mut self) -> R {
        if self.eat(b'-') { return Ok(-self.unary()?); }
        if self.eat(b'+') { return self.unary(); }
        self.power()
    }
    fn power(&mut self) -> R {
        let base = self.atom()?;
        if self.eat(b'^') { let e = self.unary()?; return Ok(base.powf(e)); } // right-associative, binds tighter than unary minus on the left
        Ok(base)
    }
    fn atom(&mut self) -> R {
        match self.peek() {
            Some(b'(') => { self.i += 1; let v = self.expr()?; if !self.eat(b')') { return Err("Missing )".into()); } Ok(v) }
            Some(c) if c.is_ascii_digit() || c == b'.' => self.number(),
            Some(c) if c.is_ascii_alphabetic() => {
                let start = self.i;
                while self.i < self.s.len() && self.s[self.i].is_ascii_alphabetic() { self.i += 1; }
                let name = std::str::from_utf8(&self.s[start..self.i]).unwrap_or("").to_ascii_lowercase();
                if self.peek() == Some(b'(') {
                    self.i += 1;
                    let a = self.expr()?;
                    if !self.eat(b')') { return Err("Missing )".into()); }
                    return func(&name, a);
                }
                match name.as_str() { "pi" => Ok(std::f64::consts::PI), "e" => Ok(std::f64::consts::E), "ans" => Ok(self.ans), _ => Err(format!("Unknown name: {name}")) }
            }
            Some(_) => Err("Syntax error".into()),
            None => Err("Unexpected end".into()),
        }
    }
    fn number(&mut self) -> R {
        let start = self.i;
        while self.i < self.s.len() && (self.s[self.i].is_ascii_digit() || self.s[self.i] == b'.') { self.i += 1; }
        // exponent such as 1e3 or 2.5e-4, but only when digits really follow (so "2e" stays 2 times e)
        if self.i < self.s.len() && (self.s[self.i] == b'e' || self.s[self.i] == b'E') {
            let mut j = self.i + 1;
            if j < self.s.len() && (self.s[j] == b'+' || self.s[j] == b'-') { j += 1; }
            if j < self.s.len() && self.s[j].is_ascii_digit() {
                while j < self.s.len() && self.s[j].is_ascii_digit() { j += 1; }
                self.i = j;
            }
        }
        std::str::from_utf8(&self.s[start..self.i]).unwrap_or("").parse::<f64>().map_err(|_| "Bad number".to_string())
    }
}

fn func(name: &str, a: f64) -> R {
    let v = match name {
        "sqrt" => { if a < 0.0 { return Err("Square root of a negative number".into()); } a.sqrt() }
        "abs" => a.abs(), "sin" => a.sin(), "cos" => a.cos(), "tan" => a.tan(),
        "asin" => a.asin(), "acos" => a.acos(), "atan" => a.atan(),
        "ln" => { if a <= 0.0 { return Err("ln needs a positive number".into()); } a.ln() }
        "log" => { if a <= 0.0 { return Err("log needs a positive number".into()); } a.log10() }
        "exp" => a.exp(), "floor" => a.floor(), "ceil" => a.ceil(), "round" => a.round(),
        _ => return Err(format!("Unknown function: {name}")),
    };
    if v.is_nan() { Err("Not a number".into()) } else { Ok(v) }
}

/// Evaluate `text`; `ans` is the previous result.
pub fn evaluate(text: &str, ans: f64) -> Result<f64, String> {
    let mut p = Parser { s: text.as_bytes(), i: 0, ans };
    let v = p.expr()?;
    if p.peek().is_some() { return Err("Syntax error".into()); }
    if v.is_infinite() { return Err("Result too large".into()); }
    if v.is_nan() { return Err("Not a number".into()); }
    Ok(v)
}

/// 14 not 14.0000000000; 0.1+0.2 shows 0.3.
pub fn format_number(v: f64) -> String {
    if v == v.trunc() && v.abs() < 1e15 { return format!("{}", v as i64); }
    let s = format!("{:.10}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.len() > 20 || v.abs() >= 1e15 || (v != 0.0 && v.abs() < 1e-9) { return format!("{:e}", v); }
    s.to_string()
}

// ---- the app -----------------------------------------------------------------------------------------

pub struct Calculator {
    input: String,
    history: Vec<(String, String)>,
    ans: f64,
    error: Option<String>,
    recall: Option<usize>,
}

impl Calculator {
    pub fn new() -> Calculator { Calculator { input: String::new(), history: Vec::new(), ans: 0.0, error: None, recall: None } }

    fn enter(&mut self) {
        if self.input.trim().is_empty() { return; }
        match evaluate(&self.input, self.ans) {
            Ok(v) => {
                self.ans = v;
                self.history.push((self.input.clone(), format_number(v)));
                self.input.clear();
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
        self.recall = None;
    }
}

impl Application for Calculator {
    fn kind(&self) -> Kind { Kind::Calculator }
    fn title(&self) -> String { "Calculator".into() }

    fn handle(&mut self, k: Key, _env: &Env) -> Out {
        match k.code {
            code::ENTER => self.enter(),
            code::BACKSPACE => { self.input.pop(); self.error = None; }
            code::ESC => { self.input.clear(); self.error = None; self.recall = None; }
            code::UP | code::DOWN => {
                // walk back and forth through earlier expressions
                if self.history.is_empty() { return Out::None; }
                let n = self.history.len();
                let next = match (k.code, self.recall) {
                    (code::UP, None) => n - 1,
                    (code::UP, Some(i)) => i.saturating_sub(1),
                    (_, Some(i)) if i + 1 < n => i + 1,
                    _ => { self.recall = None; self.input.clear(); return Out::None; }
                };
                self.recall = Some(next);
                self.input = self.history[next].0.clone();
            }
            _ => if let Some(c) = k.ch() {
                if c.is_ascii_graphic() && self.input.len() < 200 { self.input.push(c); self.error = None; self.recall = None; }
            },
        }
        Out::None
    }

    fn draw(&self, p: &mut Painter, a: Rect, focused: bool) {
        let t = p.t;
        if a.h < 3 || a.w < 8 { return; }
        let rows = a.h - 2;
        let start = self.history.len().saturating_sub(rows);
        for (i, (expr, res)) in self.history[start..].iter().enumerate() {
            let y = a.y + rows - (self.history.len() - start) + i;
            p.right(a.x, y, a.w, &format!("= {res}"), t.text, t.panel); // (clears the row first)
            p.text(a.x, y, &expr.chars().take(a.w.saturating_sub(res.chars().count() + 4)).collect::<String>(), t.dim, None);
        }
        let y = a.y + a.h - 2;
        if let Some(e) = &self.error { p.clip(a.x, y, a.w, e, t.accent, t.panel); } else { p.clip(a.x, y, a.w, "", t.text, t.panel); }
        p.field(a.x, a.y + a.h - 1, a.w, &self.input, focused, false);
    }

    fn state(&self) -> String {
        format!("name=calculator input={} result={} history={} error={}",
            if self.input.is_empty() { "-".into() } else { self.input.replace(' ', "_") },
            self.history.last().map_or("-".to_string(), |h| h.1.clone()), self.history.len(),
            self.error.as_deref().map_or("none".to_string(), |e| e.replace(' ', "_")))
    }
    fn hint(&self) -> &'static str { "Type an expression, Enter calculates   Up/Down earlier   Esc clear   F4 Close" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::parse_key;

    fn ev(s: &str) -> Result<f64, String> { evaluate(s, 0.0) }
    fn close(a: f64, b: f64) -> bool { (a - b).abs() < 1e-9 }

    #[test]
    fn arithmetic_and_precedence() {
        assert_eq!(ev("2+3*4"), Ok(14.0));
        assert_eq!(ev("(2+3)*4"), Ok(20.0));
        assert_eq!(ev("10/4"), Ok(2.5));
        assert_eq!(ev("10 % 3"), Ok(1.0));
        assert_eq!(ev("2^3^2"), Ok(512.0), "powers group to the right");
        assert_eq!(ev("-2^2"), Ok(-4.0), "unary minus applies after the power");
        assert_eq!(ev("2*-3"), Ok(-6.0));
        assert_eq!(ev("--4"), Ok(4.0));
        assert_eq!(ev("  7 - 2 - 1 "), Ok(4.0), "subtraction groups to the left");
        assert_eq!(ev("1.5+.5"), Ok(2.0));
        assert_eq!(ev("1e3+1"), Ok(1001.0));
        assert_eq!(ev("2.5e-1"), Ok(0.25));
        assert!(close(ev("0.1+0.2").unwrap(), 0.3));
    }

    #[test]
    fn functions_and_constants() {
        assert!(close(ev("sqrt(16)").unwrap(), 4.0));
        assert!(close(ev("sin(0)").unwrap(), 0.0));
        assert!(close(ev("cos(pi)").unwrap(), -1.0));
        assert!(close(ev("ln(e)").unwrap(), 1.0));
        assert!(close(ev("log(1000)").unwrap(), 3.0));
        assert!(close(ev("2pi").unwrap_or(0.0), 0.0) || ev("2pi").is_err(), "no implicit multiplication");
        assert_eq!(ev("abs(-3)*2"), Ok(6.0));
        assert_eq!(ev("round(2.5)+floor(2.9)+ceil(2.1)"), Ok(8.0));
        assert_eq!(evaluate("ans*2", 21.0), Ok(42.0));
        assert!(close(ev("2e").unwrap_or(f64::NAN), 0.0) || ev("2e").is_err());
    }

    #[test]
    fn errors_are_readable() {
        assert_eq!(ev("1/0"), Err("Division by zero".into()));
        assert_eq!(ev("5%0"), Err("Division by zero".into()));
        assert_eq!(ev("sqrt(-1)"), Err("Square root of a negative number".into()));
        assert_eq!(ev("ln(0)"), Err("ln needs a positive number".into()));
        assert_eq!(ev("(1+2"), Err("Missing )".into()));
        assert_eq!(ev("1+"), Err("Unexpected end".into()));
        assert_eq!(ev("1 2"), Err("Syntax error".into()));
        assert_eq!(ev("foo(1)"), Err("Unknown function: foo".into()));
        assert_eq!(ev("bar"), Err("Unknown name: bar".into()));
        assert_eq!(ev("9^999"), Err("Result too large".into()));
        assert_eq!(ev(""), Err("Unexpected end".into()));
    }

    #[test]
    fn numbers_are_shown_tidily() {
        assert_eq!(format_number(14.0), "14");
        assert_eq!(format_number(-0.5), "-0.5");
        assert_eq!(format_number(0.1 + 0.2), "0.3");
        assert_eq!(format_number(1.0 / 3.0), "0.3333333333");
        assert_eq!(format_number(1e20), "1e20");
        assert_eq!(format_number(0.0), "0");
    }

    fn type_keys(c: &mut Calculator, s: &str) { for ch in s.chars() { c.handle(parse_key(&ch.to_string()).unwrap(), &Env::for_tests()); } }

    #[test]
    fn typing_and_history() {
        let mut c = Calculator::new();
        let env = Env::for_tests();
        type_keys(&mut c, "2+3*4");
        assert!(c.state().contains("input=2+3*4"));
        c.handle(parse_key("enter").unwrap(), &env);
        assert!(c.state().contains("result=14") && c.state().contains("history=1"));
        type_keys(&mut c, "ans*2");
        c.handle(parse_key("enter").unwrap(), &env);
        assert!(c.state().contains("result=28"));
        // a mistake shows an error and keeps the text so it can be fixed
        type_keys(&mut c, "1/0");
        c.handle(parse_key("enter").unwrap(), &env);
        assert!(c.state().contains("error=Division_by_zero") && c.state().contains("input=1/0"));
        c.handle(parse_key("backspace").unwrap(), &env);
        assert!(c.state().contains("error=none"));
        c.handle(parse_key("esc").unwrap(), &env);
        // Up recalls earlier expressions
        c.handle(parse_key("up").unwrap(), &env);
        assert!(c.state().contains("input=ans*2"));
        c.handle(parse_key("up").unwrap(), &env);
        assert!(c.state().contains("input=2+3*4"));
        c.handle(parse_key("down").unwrap(), &env);
        c.handle(parse_key("down").unwrap(), &env);
        assert!(c.state().contains("input=-"));
    }
}
