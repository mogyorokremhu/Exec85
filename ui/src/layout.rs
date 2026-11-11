//! Tiling: windows divide the screen into rectangles that never overlap.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect { pub x: usize, pub y: usize, pub w: usize, pub h: usize }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir { SideBySide, Stacked }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Win { Files, Details, App(usize) }

/// A layout is a tree: either one window, or a split into two parts.
pub enum Node<T> {
    Leaf(T),
    Split { dir: Dir, pct: usize, a: Box<Node<T>>, b: Box<Node<T>> },
}

impl<T: Copy> Node<T> {
    /// Work out each window's rectangle. The rectangles together cover `r` exactly.
    pub fn tiles(&self, r: Rect, out: &mut Vec<(T, Rect)>) {
        match self {
            Node::Leaf(t) => out.push((*t, r)),
            Node::Split { dir, pct, a, b } => match dir {
                Dir::SideBySide => {
                    let wa = split(r.w, *pct);
                    a.tiles(Rect { x: r.x, y: r.y, w: wa, h: r.h }, out);
                    b.tiles(Rect { x: r.x + wa, y: r.y, w: r.w - wa, h: r.h }, out);
                }
                Dir::Stacked => {
                    let ha = split(r.h, *pct);
                    a.tiles(Rect { x: r.x, y: r.y, w: r.w, h: ha }, out);
                    b.tiles(Rect { x: r.x, y: r.y + ha, w: r.w, h: r.h - ha }, out);
                }
            },
        }
    }
}

/// Size of the first part of a split: `pct` percent, but both parts get at least 1 cell when possible.
fn split(total: usize, pct: usize) -> usize {
    let a = total * pct / 100;
    if total >= 2 { a.clamp(1, total - 1) } else { total }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overlaps(a: Rect, b: Rect) -> bool {
        a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
    }

    fn tree(depth: u32, seed: usize) -> Node<usize> {
        if depth == 0 { return Node::Leaf(seed); }
        Node::Split {
            dir: if (seed + depth as usize) % 2 == 0 { Dir::SideBySide } else { Dir::Stacked },
            pct: 10 + (seed * 17 + depth as usize * 13) % 81,
            a: Box::new(tree(depth - 1, seed * 2 + 1)),
            b: Box::new(tree(depth - 1, seed * 2 + 2)),
        }
    }

    #[test]
    fn tiles_never_overlap_and_cover_the_area_exactly() {
        for seed in 0..40 {
            for (w, h) in [(80, 24), (85, 42), (7, 5), (200, 60), (3, 3)] {
                let area = Rect { x: 0, y: 5, w, h };
                let mut out = Vec::new();
                tree(3, seed).tiles(area, &mut out);
                let total: usize = out.iter().map(|(_, r)| r.w * r.h).sum();
                assert_eq!(total, w * h, "seed {seed} {w}x{h}: tiles do not cover the area");
                for (i, (_, a)) in out.iter().enumerate() {
                    assert!(a.x >= area.x && a.y >= area.y && a.x + a.w <= area.x + area.w && a.y + a.h <= area.y + area.h);
                    for (_, b) in out.iter().skip(i + 1) { assert!(!overlaps(*a, *b), "seed {seed}: {a:?} overlaps {b:?}"); }
                }
            }
        }
    }

    #[test]
    fn side_by_side_split_sizes() {
        let n = Node::Split { dir: Dir::SideBySide, pct: 60, a: Box::new(Node::Leaf(Win::Files)), b: Box::new(Node::Leaf(Win::Details)) };
        let mut out = Vec::new();
        n.tiles(Rect { x: 0, y: 0, w: 100, h: 30 }, &mut out);
        assert_eq!(out[0].1, Rect { x: 0, y: 0, w: 60, h: 30 });
        assert_eq!(out[1].1, Rect { x: 60, y: 0, w: 40, h: 30 });
    }
}
