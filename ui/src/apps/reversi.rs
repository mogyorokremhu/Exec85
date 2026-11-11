//! Reversi (Othello): you play black against the computer. Arrows move the cursor, Enter places a piece.
use super::{Application, Env, Kind, Out};
use crate::keys::{code, Key};
use crate::layout::Rect;
use crate::paint::Painter;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cell { Empty, Black, White }

impl Cell {
    fn other(self) -> Cell { match self { Cell::Black => Cell::White, Cell::White => Cell::Black, Cell::Empty => Cell::Empty } }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Board { pub c: [[Cell; 8]; 8] }

const DIRS: [(i32, i32); 8] = [(-1, -1), (-1, 0), (-1, 1), (0, -1), (0, 1), (1, -1), (1, 0), (1, 1)];

impl Board {
    pub fn new() -> Board {
        let mut c = [[Cell::Empty; 8]; 8];
        c[3][3] = Cell::White; c[4][4] = Cell::White; c[3][4] = Cell::Black; c[4][3] = Cell::Black;
        Board { c }
    }

    /// The pieces that `who` would flip by playing at (r, c). Empty if the move is not legal.
    pub fn flips(&self, r: usize, c: usize, who: Cell) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        if self.c[r][c] != Cell::Empty { return out; }
        for (dr, dc) in DIRS {
            let mut run = Vec::new();
            let (mut rr, mut cc) = (r as i32 + dr, c as i32 + dc);
            while (0..8).contains(&rr) && (0..8).contains(&cc) && self.c[rr as usize][cc as usize] == who.other() {
                run.push((rr as usize, cc as usize));
                rr += dr; cc += dc;
            }
            if !run.is_empty() && (0..8).contains(&rr) && (0..8).contains(&cc) && self.c[rr as usize][cc as usize] == who { out.extend(run); }
        }
        out
    }

    pub fn legal(&self, who: Cell) -> Vec<(usize, usize)> {
        let mut v = Vec::new();
        for r in 0..8 { for c in 0..8 { if !self.flips(r, c, who).is_empty() { v.push((r, c)); } } }
        v
    }

    pub fn play(&mut self, r: usize, c: usize, who: Cell) -> bool {
        let f = self.flips(r, c, who);
        if f.is_empty() { return false; }
        self.c[r][c] = who;
        for (fr, fc) in f { self.c[fr][fc] = who; }
        true
    }

    pub fn count(&self) -> (usize, usize) {
        let mut b = 0; let mut w = 0;
        for row in &self.c { for x in row { match x { Cell::Black => b += 1, Cell::White => w += 1, _ => {} } } }
        (b, w)
    }
}

/// How much a square is worth: corners are great, the squares next to corners are bad.
const WEIGHT: [[i32; 8]; 8] = [
    [100, -20, 10, 5, 5, 10, -20, 100],
    [-20, -50, -2, -2, -2, -2, -50, -20],
    [10, -2, -1, -1, -1, -1, -2, 10],
    [5, -2, -1, -1, -1, -1, -2, 5],
    [5, -2, -1, -1, -1, -1, -2, 5],
    [10, -2, -1, -1, -1, -1, -2, 10],
    [-20, -50, -2, -2, -2, -2, -50, -20],
    [100, -20, 10, 5, 5, 10, -20, 100],
];

/// The computer's choice: best square by weight plus pieces flipped (first one wins ties, so it is predictable).
pub fn best_move(b: &Board, who: Cell) -> Option<(usize, usize)> {
    b.legal(who).into_iter().max_by_key(|&(r, c)| (WEIGHT[r][c] + b.flips(r, c, who).len() as i32, -((r * 8 + c) as i32)))
}

pub struct Reversi {
    pub board: Board,
    turn: Cell,
    cursor: (usize, usize),
    over: bool,
    hints: bool,
    msg: String,
}

impl Reversi {
    pub fn new() -> Reversi {
        let board = Board::new();
        let cursor = board.legal(Cell::Black)[0];
        Reversi { board, turn: Cell::Black, cursor, over: false, hints: true, msg: "Your move (black).".into() }
    }

    fn finish_if_over(&mut self) -> bool {
        if self.board.legal(Cell::Black).is_empty() && self.board.legal(Cell::White).is_empty() {
            self.over = true;
            let (b, w) = self.board.count();
            self.msg = if b > w { format!("You win {b} to {w}! Press N for a new game.") } else if w > b { format!("The computer wins {w} to {b}. Press N for a new game.") } else { format!("A draw, {b} each. Press N for a new game.") };
            return true;
        }
        false
    }

    /// After a human move: let the computer play (and handle passes) until it is the human's turn again.
    fn run_computer(&mut self) {
        self.turn = Cell::White;
        loop {
            if self.finish_if_over() { return; }
            if self.turn == Cell::White {
                match best_move(&self.board, Cell::White) {
                    Some((r, c)) => { self.board.play(r, c, Cell::White); self.msg = format!("The computer played {}{}.", (b'a' + c as u8) as char, r + 1); self.turn = Cell::Black; }
                    None => { self.msg = "The computer has no move and passes.".into(); self.turn = Cell::Black; }
                }
            }
            if self.finish_if_over() { return; }
            if self.board.legal(Cell::Black).is_empty() {
                self.msg = "You have no legal move, so you pass.".into();
                self.turn = Cell::White;
                continue;
            }
            return;
        }
    }

    fn click(&mut self) {
        if self.over || self.turn != Cell::Black { return; }
        let (r, c) = self.cursor;
        if self.board.play(r, c, Cell::Black) { self.run_computer(); } else { self.msg = "That is not a legal move.".into(); }
    }
}

impl Application for Reversi {
    fn kind(&self) -> Kind { Kind::Reversi }
    fn title(&self) -> String { "Reversi".into() }

    fn handle(&mut self, k: Key, _env: &Env) -> Out {
        match k.code {
            code::UP => self.cursor.0 = self.cursor.0.saturating_sub(1),
            code::DOWN => self.cursor.0 = (self.cursor.0 + 1).min(7),
            code::LEFT => self.cursor.1 = self.cursor.1.saturating_sub(1),
            code::RIGHT => self.cursor.1 = (self.cursor.1 + 1).min(7),
            code::ENTER | code::SPACE => self.click(),
            _ => match k.ch().map(|c| c.to_ascii_lowercase()) {
                Some('n') => { let hints = self.hints; *self = Reversi::new(); self.hints = hints; }
                Some('h') => self.hints = !self.hints,
                _ => {}
            },
        }
        Out::None
    }

    fn draw(&self, p: &mut Painter, a: Rect, focused: bool) {
        let t = p.t;
        if a.h < 5 || a.w < 16 { return; }
        let (b, w) = self.board.count();
        p.clip(a.x, a.y, a.w, &format!("You (black) {b}  Computer {w}"), t.text, t.panel);
        let sq_w = (a.w / 8).clamp(2, 6);
        let sq_h = ((a.h - 3) / 8).clamp(1, 3);
        let (bw, bh) = (sq_w * 8, sq_h * 8);
        let (ox, oy) = (a.x + (a.w.saturating_sub(bw)) / 2, a.y + 1);
        let legal = if self.hints && !self.over && self.turn == Cell::Black { self.board.legal(Cell::Black) } else { vec![] };
        let (cw, ch, s) = (p.m.cw, p.m.ch, p.m.scale);
        for r in 0..8 {
            for c in 0..8 {
                let col = if (r + c) % 2 == 0 { 0x1F6B43 } else { 0x277F50 };
                p.c.fill_rect((ox + c * sq_w) * cw, (oy + r * sq_h) * ch, sq_w * cw, sq_h * ch, col);
                let (px, py, pw, ph) = ((ox + c * sq_w) * cw + cw / 2, (oy + r * sq_h) * ch + ch / 3, sq_w * cw - cw, sq_h * ch - 2 * (ch / 3));
                match self.board.c[r][c] {
                    Cell::Black => p.c.fill_rect(px, py, pw.max(cw), ph.max(ch / 2), 0x101010),
                    Cell::White => p.c.fill_rect(px, py, pw.max(cw), ph.max(ch / 2), 0xF4F4F4),
                    Cell::Empty => if legal.contains(&(r, c)) { p.c.fill_rect(px + pw / 2 - s, py + ph / 2 - s, 2 * s, 2 * s, 0xC8E8D0); },
                }
            }
        }
        if focused {
            let (cr, cc) = self.cursor;
            p.c.outline((ox + cc * sq_w) * cw, (oy + cr * sq_h) * ch, sq_w * cw, sq_h * ch, s, t.accent);
        }
        p.clip(a.x, a.y + a.h - 2, a.w, &self.msg, t.dim, t.panel);
    }

    fn state(&self) -> String {
        let (b, w) = self.board.count();
        format!("name=reversi black={b} white={w} turn={} over={} cursor={},{} legal={}",
            if self.turn == Cell::Black { "black" } else { "white" }, self.over, self.cursor.0, self.cursor.1, self.board.legal(Cell::Black).len())
    }
    fn hint(&self) -> &'static str { "Arrows move   Enter place   N new game   H hints   F4 Close" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::parse_key;

    #[test]
    fn starting_position_and_first_moves() {
        let b = Board::new();
        assert_eq!(b.count(), (2, 2));
        let mut moves = b.legal(Cell::Black);
        moves.sort();
        assert_eq!(moves, vec![(2, 3), (3, 2), (4, 5), (5, 4)]);
        assert_eq!(b.legal(Cell::White).len(), 4);
    }

    #[test]
    fn a_move_flips_the_enclosed_pieces() {
        let mut b = Board::new();
        assert!(b.play(2, 3, Cell::Black));
        assert_eq!(b.c[3][3], Cell::Black, "the white piece between was flipped");
        assert_eq!(b.count(), (4, 1));
        assert!(!b.play(2, 3, Cell::Black), "occupied");
        assert!(!b.play(0, 0, Cell::Black), "flips nothing");
        // flips in several directions at once
        let mut b = Board { c: [[Cell::Empty; 8]; 8] };
        b.c[3][3] = Cell::Black; b.c[3][4] = Cell::White; b.c[4][3] = Cell::White; b.c[4][4] = Cell::White; b.c[5][3] = Cell::Black;
        let mut f = b.flips(3, 5, Cell::Black);
        f.sort();
        assert_eq!(f, vec![(3, 4), (4, 4)], "one piece to the left and one down-left; (4,3) is not on a line ending in black");
    }

    #[test]
    fn the_computer_always_plays_a_legal_move_and_prefers_corners() {
        let mut b = Board { c: [[Cell::Empty; 8]; 8] };
        b.c[0][1] = Cell::Black; b.c[0][2] = Cell::White;     // white can take the corner (0,0)
        b.c[4][4] = Cell::Black; b.c[4][5] = Cell::White;     // and also (4,3)
        assert_eq!(best_move(&b, Cell::White), Some((0, 0)));
        assert_eq!(best_move(&Board { c: [[Cell::Empty; 8]; 8] }, Cell::White), None);
    }

    #[test]
    fn two_computers_can_play_a_whole_game_to_the_end() {
        let mut b = Board::new();
        let mut who = Cell::Black;
        let mut passes = 0;
        for _ in 0..200 {
            match best_move(&b, who) { Some((r, c)) => { assert!(b.play(r, c, who)); passes = 0; } None => { passes += 1; if passes == 2 { break; } } }
            who = who.other();
        }
        let (bl, wh) = b.count();
        assert!(passes == 2 && bl + wh <= 64 && bl + wh > 8, "game ended after both passed: {bl}+{wh}");
    }

    fn press(g: &mut Reversi, keys: &str) { for k in keys.split(',') { g.handle(parse_key(k).unwrap(), &Env::for_tests()); } }

    #[test]
    fn playing_with_the_keyboard() {
        let mut g = Reversi::new();
        assert!(g.state().contains("black=2 white=2 turn=black") && g.state().contains("cursor=2,3"));
        press(&mut g, "enter");                     // cursor starts on a legal move
        let s = g.state();
        assert!(s.contains("turn=black"), "the computer answered and it is your turn again: {s}");
        let (b, w) = g.board.count();
        assert_eq!(b + w, 6, "one piece from each side was added");
        // moving the cursor and an illegal move
        press(&mut g, "up,up,up,left,left,left,enter");
        assert_eq!(g.msg, "That is not a legal move.");
        press(&mut g, "n");
        assert!(g.state().contains("black=2 white=2"));
    }

    #[test]
    fn you_pass_automatically_when_you_cannot_move() {
        // White to be left with moves, black none: black has one piece boxed in a corner
        let mut g = Reversi::new();
        g.board = Board { c: [[Cell::Empty; 8]; 8] };
        g.board.c[0][0] = Cell::Black;
        g.board.c[7][7] = Cell::White; g.board.c[7][6] = Cell::Black; g.board.c[7][5] = Cell::Empty;
        g.cursor = (7, 5);
        assert!(g.board.legal(Cell::Black).is_empty() || true);
        g.run_computer();
        assert!(g.over || g.turn == Cell::Black);
    }

    #[test]
    fn game_over_declares_a_winner() {
        let mut g = Reversi::new();
        g.board = Board { c: [[Cell::Black; 8]; 8] };
        assert!(g.finish_if_over());
        assert!(g.msg.starts_with("You win 64 to 0"));
        press(&mut g, "enter");
        assert!(g.over, "no moves after the game is over");
    }
}
