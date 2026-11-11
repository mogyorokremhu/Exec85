//! Snake: steer with the arrow keys, eat the food, do not hit the walls or yourself.
use super::{Application, Env, Kind, Out};
use crate::keys::{code, Key};
use crate::layout::Rect;
use crate::paint::Painter;
use std::collections::VecDeque;
use std::time::Duration;

type Pos = (i32, i32);

pub struct Snake {
    w: i32,
    h: i32,
    body: VecDeque<Pos>, // front = head
    dir: Pos,
    pending: Option<Pos>,
    food: Pos,
    score: u32,
    alive: bool,
    won: bool,
    started: bool,
    paused: bool,
    ticks: u32,
    rng: u64,
}

impl Snake {
    pub fn new(cols: usize, rows: usize) -> Snake { Snake::with_seed(cols, rows, crate::sys::now() as u64 ^ 0x9E37_79B9_7F4A_7C15) }

    pub fn with_seed(cols: usize, rows: usize, seed: u64) -> Snake {
        let (w, h) = (((cols / 2).max(8)) as i32, (rows.saturating_sub(2).max(6)) as i32);
        let mut s = Snake { w, h, body: VecDeque::new(), dir: (1, 0), pending: None, food: (0, 0), score: 0, alive: true, won: false, started: false, paused: false, ticks: 0, rng: seed | 1 };
        let (cx, cy) = (w / 2, h / 2);
        for i in 0..3 { s.body.push_back((cx - i, cy)); }
        s.spawn_food();
        s
    }

    fn rand(&mut self) -> u64 { self.rng ^= self.rng << 13; self.rng ^= self.rng >> 7; self.rng ^= self.rng << 17; self.rng }

    fn spawn_food(&mut self) {
        let free = (self.w * self.h) as usize - self.body.len();
        if free == 0 { self.won = true; self.alive = false; return; }
        let mut n = (self.rand() % free as u64) as usize;
        for y in 0..self.h { for x in 0..self.w {
            if self.body.contains(&(x, y)) { continue; }
            if n == 0 { self.food = (x, y); return; }
            n -= 1;
        }}
    }

    fn set_dir(&mut self, d: Pos) {
        let cur = self.pending.unwrap_or(self.dir);
        if (d.0 == -cur.0 && d.1 == -cur.1) && self.body.len() > 1 { return; } // cannot turn straight back
        self.pending = Some(d);
        self.started = true;
        self.paused = false;
    }

    /// Move one step.
    pub fn step(&mut self) {
        if !self.alive { return; }
        if let Some(d) = self.pending.take() { self.dir = d; }
        let head = *self.body.front().unwrap();
        let next = (head.0 + self.dir.0, head.1 + self.dir.1);
        self.ticks += 1;
        if next.0 < 0 || next.1 < 0 || next.0 >= self.w || next.1 >= self.h { self.alive = false; return; }
        let eating = next == self.food;
        // the tail moves away this step unless we are growing, so it is not an obstacle
        let hits = self.body.iter().enumerate().any(|(i, p)| *p == next && (eating || i + 1 != self.body.len()));
        if hits { self.alive = false; return; }
        self.body.push_front(next);
        if eating { self.score += 1; self.spawn_food(); } else { self.body.pop_back(); }
    }

    fn restart(&mut self) { let (cols, rows) = ((self.w * 2) as usize, (self.h + 2) as usize); let seed = self.rng; *self = Snake::with_seed(cols, rows, seed); }
}

impl Application for Snake {
    fn kind(&self) -> Kind { Kind::Snake }
    fn title(&self) -> String { "Snake".into() }

    fn interval(&self) -> Option<Duration> {
        if self.alive && self.started && !self.paused { Some(Duration::from_millis((150u64).saturating_sub(self.score as u64 * 4).max(60))) } else { None }
    }
    fn tick(&mut self, _env: &Env) { self.step(); }

    fn resize(&mut self, cols: usize, rows: usize) {
        let (w, h) = (((cols / 2).max(8)) as i32, (rows.saturating_sub(2).max(6)) as i32);
        if (w, h) != (self.w, self.h) && !self.started { let seed = self.rng; *self = Snake::with_seed(cols, rows, seed); }
    }

    fn handle(&mut self, k: Key, _env: &Env) -> Out {
        match k.code {
            code::UP => self.set_dir((0, -1)),
            code::DOWN => self.set_dir((0, 1)),
            code::LEFT => self.set_dir((-1, 0)),
            code::RIGHT => self.set_dir((1, 0)),
            code::SPACE => if self.alive && self.started { self.paused = !self.paused; },
            code::ENTER => if !self.alive { self.restart(); },
            _ => match k.ch().map(|c| c.to_ascii_lowercase()) {
                Some('p') => if self.alive && self.started { self.paused = !self.paused; },
                Some('n') => self.restart(),
                _ => {}
            },
        }
        Out::None
    }

    fn draw(&self, p: &mut Painter, a: Rect, _focused: bool) {
        let t = p.t;
        if a.h < 8 || a.w < 18 { return; }
        let msg = if !self.alive { if self.won { "You filled the whole board! Enter: play again" } else { "Game over. Enter: play again" } } else if !self.started { "Press an arrow key to start" } else if self.paused { "Paused (P)" } else { "" };
        p.clip(a.x, a.y, a.w, &format!("Score {}   {}", self.score, msg), t.text, t.panel);
        let (cw, ch) = (p.m.cw, p.m.ch);
        let (ox, oy) = (a.x, a.y + 1);
        p.c.fill_rect(ox * cw, oy * ch, (self.w as usize * 2) * cw, self.h as usize * ch, t.bg);
        let cell = |p: &mut Painter, (x, y): Pos, color: u32| p.c.fill_rect((ox + x as usize * 2) * cw + 1, (oy + y as usize) * ch + 1, 2 * cw - 2, ch - 2, color);
        cell(p, self.food, t.accent);
        for (i, b) in self.body.iter().enumerate() { cell(p, *b, if i == 0 { 0x6FDC6F } else { 0x3FA34D }); }
        p.outline(Rect { x: ox, y: oy, w: self.w as usize * 2, h: self.h as usize }, t.border);
    }

    fn state(&self) -> String {
        format!("name=snake len={} score={} alive={} started={} paused={} ticks={} dir={}", self.body.len(), self.score, self.alive, self.started, self.paused, self.ticks,
            match self.dir { (1, 0) => "right", (-1, 0) => "left", (0, -1) => "up", _ => "down" })
    }
    fn hint(&self) -> &'static str { "Arrows steer   P pause   N new game   F4 Close" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::parse_key;

    fn game() -> Snake { Snake::with_seed(40, 20, 12345) }
    fn press(s: &mut Snake, keys: &str) { for k in keys.split(',') { s.handle(parse_key(k).unwrap(), &Env::for_tests()); } }

    #[test]
    fn waits_for_the_first_key_then_moves() {
        let mut s = game();
        assert_eq!(s.interval(), None, "no timer before the game starts");
        let head = *s.body.front().unwrap();
        press(&mut s, "right");
        assert!(s.interval().is_some());
        s.step();
        assert_eq!(*s.body.front().unwrap(), (head.0 + 1, head.1));
        assert_eq!(s.body.len(), 3, "moving does not grow");
    }

    #[test]
    fn eating_grows_and_scores_and_moves_the_food() {
        let mut s = game();
        let head = *s.body.front().unwrap();
        s.food = (head.0 + 1, head.1);
        press(&mut s, "right");
        s.step();
        assert_eq!((s.score, s.body.len()), (1, 4));
        assert!(!s.body.contains(&s.food), "new food never appears on the snake");
        assert!(s.food.0 >= 0 && s.food.0 < s.w && s.food.1 >= 0 && s.food.1 < s.h);
    }

    #[test]
    fn walls_and_yourself_are_fatal() {
        let mut s = game();
        press(&mut s, "up");
        for _ in 0..100 { s.step(); }
        assert!(!s.alive && s.interval().is_none(), "ran into the top wall");
        // self collision: a long snake turning into itself
        let mut s = game();
        s.body = (0..8).map(|i| (10 - i, 5)).collect();
        s.food = (30, 15);
        s.dir = (1, 0);
        press(&mut s, "down");
        s.step();
        press(&mut s, "left");
        s.step();
        press(&mut s, "up");
        s.step();
        assert!(!s.alive, "turned into its own body");
    }

    #[test]
    fn cannot_reverse_into_yourself_and_the_tail_cell_is_free() {
        let mut s = game();
        press(&mut s, "right");
        press(&mut s, "left");
        s.step();
        assert!(s.alive, "the reverse turn was ignored");
        assert_eq!(s.state().contains("dir=right"), true);
        // chasing the tail around a 2x2 loop is legal: the tail leaves its cell as the head arrives
        let mut s = game();
        s.body = VecDeque::from(vec![(5, 5), (5, 6), (4, 6), (4, 5)]);
        s.food = (30, 15);
        s.dir = (0, -1);
        s.pending = Some((-1, 0));
        s.step();
        assert!(s.alive, "moving into the cell the tail is leaving is fine");
    }

    #[test]
    fn pause_restart_and_speed() {
        let mut s = game();
        press(&mut s, "right,p");
        assert_eq!(s.interval(), None);
        press(&mut s, "p");
        assert!(s.interval().is_some());
        let slow = s.interval().unwrap();
        s.score = 20;
        assert!(s.interval().unwrap() < slow, "it speeds up as you score");
        s.score = 500;
        assert_eq!(s.interval(), Some(Duration::from_millis(60)), "but never gets unplayably fast");
        s.alive = false;
        press(&mut s, "enter");
        assert!(s.alive && s.score == 0 && !s.started);
    }

    #[test]
    fn filling_the_board_wins() {
        let mut s = Snake::with_seed(16, 8, 7);   // 8 x 6 board
        s.body = (0..47).map(|i| (i % 8, i / 8)).collect();
        // food is the last free cell
        s.food = (7, 5);
        s.spawn_food();
        assert!(!s.won);
        let free: Vec<Pos> = (0..s.h).flat_map(|y| (0..s.w).map(move |x| (x, y))).filter(|p| !s.body.contains(p)).collect();
        assert_eq!(free.len(), 1);
        assert_eq!(s.food, free[0]);
    }
}
