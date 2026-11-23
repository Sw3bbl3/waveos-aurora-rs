//! Several displays in one desktop: the arrangement math, host-tested and
//! shared by the compositor (Lumen Server) and Settings.
//!
//! Every display is a rectangle in one global coordinate space, in points
//! (a display at scale 2 shows its points with 2×2 device pixels). The
//! primary display — the one with the menu bar and Harbor — is at (0, 0);
//! the others touch it, or each other, along an edge (no overlaps, no
//! islands), as on macOS and Windows. The pointer crosses where displays
//! touch and stops at the outer edges; a window belongs to the display it
//! overlaps most.
//!
//! Settings stores each display's place, resolution, scale and wallpaper
//! under a preference named after the display's identity ([`pref_key`]),
//! so a monitor gets its arrangement back whenever it is plugged in.

use crate::geom::Rect;
use alloc::string::String;
use alloc::vec::Vec;

/// Index of the display that shows point (`x`, `y`).
pub fn at(displays: &[Rect], x: i32, y: i32) -> Option<usize> {
    displays.iter().position(|d| d.contains(x, y))
}

/// The point of `r` closest to (`x`, `y`).
pub fn clamp_point(r: Rect, x: i32, y: i32) -> (i32, i32) {
    (x.clamp(r.x, (r.right() - 1).max(r.x)), y.clamp(r.y, (r.bottom() - 1).max(r.y)))
}

fn dist2(a: (i32, i32), b: (i32, i32)) -> i64 {
    let (dx, dy) = ((a.0 - b.0) as i64, (a.1 - b.1) as i64);
    dx * dx + dy * dy
}

/// The display closest to (`x`, `y`) (the first one when there are none closer).
pub fn nearest(displays: &[Rect], x: i32, y: i32) -> usize {
    (0..displays.len()).min_by_key(|&i| dist2(clamp_point(displays[i], x, y), (x, y))).unwrap_or(0)
}

/// Where the pointer goes when moved to (`x`, `y`): there, if a display
/// shows that point; otherwise the closest point a display shows. So it
/// crosses from one display to the next only where they touch, and stops
/// at the outer edges and in the corners between displays of different
/// sizes.
pub fn constrain(displays: &[Rect], x: i32, y: i32) -> (i32, i32) {
    if displays.is_empty() || at(displays, x, y).is_some() {
        return (x, y);
    }
    clamp_point(displays[nearest(displays, x, y)], x, y)
}

/// The display `r` belongs to: the one it overlaps most, else the one
/// nearest its centre.
pub fn home(displays: &[Rect], r: Rect) -> usize {
    let best = (0..displays.len()).max_by_key(|&i| (displays[i].intersect(&r).area(), core::cmp::Reverse(i)));
    match best {
        Some(i) if displays[i].intersects(&r) => i,
        _ => {
            let (cx, cy) = r.center();
            nearest(displays, cx, cy)
        }
    }
}

/// `r` taken from display `from` to display `to`: at the same place
/// relative to the display (proportionally, so a window in the right half
/// stays in the right half), its size kept where it fits, inside `to`.
pub fn carry(r: Rect, from: Rect, to: Rect) -> Rect {
    let (w, h) = (r.w.min(to.w), r.h.min(to.h));
    // How far into its free room on `from` it was, as far into the room on `to`.
    let along = |pos: i32, start: i32, room: i32, to_start: i32, to_room: i32| {
        let room = room.max(0) as i64;
        let into = (pos - start).clamp(0, room as i32) as i64;
        let to_room = to_room.max(0) as i64;
        to_start + if room == to_room { into } else if room == 0 { 0 } else { into * to_room / room } as i32
    };
    Rect::new(along(r.x, from.x, from.w - r.w, to.x, to.w - w), along(r.y, from.y, from.h - r.h, to.y, to.h - h), w, h)
}

/// The rectangle around every display.
pub fn bounds(displays: &[Rect]) -> Rect {
    displays.iter().fold(Rect::default(), |acc, d| acc.union(d))
}

/// Shifts every display so that display `primary` is at (0, 0).
pub fn normalize(displays: &mut [Rect], primary: usize) {
    let Some(p) = displays.get(primary).copied() else { return };
    for d in displays.iter_mut() {
        *d = d.offset(-p.x, -p.y);
    }
}

/// Where a display goes next to another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
    Above,
    Below,
}

impl Side {
    pub const ALL: [Side; 4] = [Side::Left, Side::Right, Side::Above, Side::Below];

    pub fn name(self) -> &'static str {
        match self {
            Side::Left => "Left",
            Side::Right => "Right",
            Side::Above => "Above",
            Side::Below => "Below",
        }
    }
}

/// The top-left corner for a `size` display on `side` of `anchor`: beside
/// it with the top edges lined up, or above or below it with the left
/// edges lined up.
pub fn beside(anchor: Rect, size: (i32, i32), side: Side) -> (i32, i32) {
    match side {
        Side::Left => (anchor.x - size.0, anchor.y),
        Side::Right => (anchor.right(), anchor.y),
        Side::Above => (anchor.x, anchor.y - size.1),
        Side::Below => (anchor.x, anchor.bottom()),
    }
}

/// The side of `anchor` that `r` touches, if it touches one along an edge
/// (sharing at least one point of it).
pub fn side_of(anchor: Rect, r: Rect) -> Option<Side> {
    let rows = r.y < anchor.bottom() && r.bottom() > anchor.y;
    let columns = r.x < anchor.right() && r.right() > anchor.x;
    if rows && r.right() == anchor.x {
        Some(Side::Left)
    } else if rows && r.x == anchor.right() {
        Some(Side::Right)
    } else if columns && r.bottom() == anchor.y {
        Some(Side::Above)
    } else if columns && r.y == anchor.bottom() {
        Some(Side::Below)
    } else {
        None
    }
}

/// Where a `size` display goes when put at `want` among `others`: touching
/// one of them along an edge (by at least a tenth of the shorter side, so
/// the pointer has room to cross), overlapping none, as close to `want` as
/// possible. With no others it stays at `want`.
pub fn snap(others: &[Rect], size: (i32, i32), want: (i32, i32)) -> (i32, i32) {
    if others.is_empty() {
        return want;
    }
    let (w, h) = size;
    let mut best: Option<((i32, i32), i64)> = None;
    for o in others {
        // At least this much of the shared edge.
        let share_y = (h.min(o.h) / 10).max(1);
        let share_x = (w.min(o.w) / 10).max(1);
        let y = want.1.clamp(o.y - h + share_y, o.bottom() - share_y);
        let x = want.0.clamp(o.x - w + share_x, o.right() - share_x);
        for pos in [(o.x - w, y), (o.right(), y), (x, o.y - h), (x, o.bottom())] {
            let r = Rect::new(pos.0, pos.1, w, h);
            if others.iter().any(|q| q.intersects(&r)) {
                continue;
            }
            let d = dist2(pos, want);
            if best.is_none_or(|(_, bd)| d < bd) {
                best = Some((pos, d));
            }
        }
    }
    best.map(|(p, _)| p).unwrap_or_else(|| {
        let b = bounds(others);
        (b.right(), b.y)
    })
}

/// Lays displays out from their sizes (points) and saved places: the
/// primary at (0, 0), then each other display at its saved place, moved
/// as little as needed to touch the ones placed before it without
/// overlapping them; a display without a saved place goes to the right of
/// everything, top edges lined up with the primary.
pub fn arrange(sizes: &[(i32, i32)], saved: &[Option<(i32, i32)>], primary: usize) -> Vec<Rect> {
    let mut out = alloc::vec![Rect::default(); sizes.len()];
    if sizes.is_empty() {
        return out;
    }
    let primary = primary.min(sizes.len() - 1);
    out[primary] = Rect::new(0, 0, sizes[primary].0, sizes[primary].1);
    let mut placed = alloc::vec![out[primary]];
    for i in (0..sizes.len()).filter(|&i| i != primary) {
        let want = match saved.get(i).copied().flatten() {
            Some(p) => p,
            None => (bounds(&placed).right(), 0),
        };
        let (x, y) = snap(&placed, sizes[i], want);
        out[i] = Rect::new(x, y, sizes[i].0, sizes[i].1);
        placed.push(out[i]);
    }
    out
}

/// Every display reaches every other through shared edges.
pub fn connected(displays: &[Rect]) -> bool {
    if displays.is_empty() {
        return true;
    }
    let mut seen = alloc::vec![false; displays.len()];
    let mut stack = alloc::vec![0];
    seen[0] = true;
    while let Some(i) = stack.pop() {
        for j in 0..displays.len() {
            if !seen[j] && side_of(displays[i], displays[j]).is_some() {
                seen[j] = true;
                stack.push(j);
            }
        }
    }
    seen.iter().all(|&s| s)
}

/// A window taken off a display that went away: that display's identity
/// and where the window was on it (relative to its top-left corner), to
/// put it back when the display returns.
pub type Away = Option<(u32, Rect)>;

/// Where a window goes when the displays change from `old` to `new` (each
/// an identity and a rectangle; `primary` indexes `new`):
/// - its display is still there: it moves with it (and is carried to the
///   new size if that changed);
/// - its display went away: it moves to the primary, remembering where it was;
/// - it was taken off a display that is back: it returns there.
pub fn migrate(old: &[(u32, Rect)], new: &[(u32, Rect)], primary: usize, r: Rect, away: Away) -> (Rect, Away) {
    if new.is_empty() {
        return (r, away);
    }
    if let Some((key, rel)) = away {
        if let Some(&(_, d)) = new.iter().find(|(k, _)| *k == key) {
            let back = Rect::new(d.x + rel.x, d.y + rel.y, rel.w.min(d.w), rel.h.min(d.h));
            return (carry(back, d, d), None);
        }
    }
    let rects: Vec<Rect> = old.iter().map(|(_, d)| *d).collect();
    if rects.is_empty() {
        return (r, away);
    }
    let (key, from) = old[home(&rects, r)];
    match new.iter().find(|(k, _)| *k == key) {
        Some(&(_, to)) if to == from => (r, away),
        Some(&(_, to)) => (carry(r, from, to), away),
        None => {
            let to = new[primary.min(new.len() - 1)].1;
            let remembered = away.or(Some((key, r.offset(-from.x, -from.y))));
            (carry(r, from, to), remembered)
        }
    }
}

/// What Settings saves for a display: its place (points, relative to the
/// primary display), resolution (device pixels; 0×0 for the display's
/// preferred one), scale (0 for automatic) and wallpaper (-1 for the main
/// display's). Stored as `x,y,width,height,scale,wallpaper`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Record {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale: u8,
    pub wallpaper: i16,
}

impl Default for Record {
    fn default() -> Record {
        Record { x: 0, y: 0, width: 0, height: 0, scale: 0, wallpaper: -1 }
    }
}

impl Record {
    pub fn parse(s: &str) -> Option<Record> {
        let mut f = s.split(',').map(str::trim);
        let mut next = || f.next();
        let r = Record {
            x: next()?.parse().ok()?,
            y: next()?.parse().ok()?,
            width: next()?.parse().ok()?,
            height: next()?.parse().ok()?,
            scale: next()?.parse().ok()?,
            wallpaper: next().and_then(|v| v.parse().ok()).unwrap_or(-1),
        };
        let size_ok = (r.width, r.height) == (0, 0) || ((320..=16384).contains(&r.width) && (200..=16384).contains(&r.height));
        let place_ok = r.x.abs() <= 1 << 20 && r.y.abs() <= 1 << 20;
        (size_ok && place_ok && r.scale <= 2).then_some(r)
    }

    pub fn format(&self) -> String {
        alloc::format!("{},{},{},{},{},{}", self.x, self.y, self.width, self.height, self.scale, self.wallpaper)
    }
}

/// A 32-bit FNV-1a hash of a display's identity (its EDID's maker,
/// product, serial number and name, and the connector it is on).
pub fn identity_hash(identity: &str) -> u32 {
    identity.bytes().fold(0x811C_9DC5u32, |h, b| (h ^ b as u32).wrapping_mul(0x0100_0193))
}

/// The preference that holds the [`Record`] of the display with this identity hash.
pub fn pref_key(hash: u32) -> String {
    alloc::format!("display_{hash:08x}")
}

/// Preference: the identity hash (8 hex digits) of the display chosen as
/// the main one (the menu bar and Harbor go there).
pub const PRIMARY_PREF: &str = "display_primary";
/// Preference: "1" shows the main display's picture on every display.
pub const MIRROR_PREF: &str = "display_mirror";

#[cfg(test)]
mod tests {
    use super::*;

    fn three() -> Vec<Rect> {
        // 1280x800 primary, a 1024x768 to its right and a 1920x1080 to its left.
        alloc::vec![Rect::new(0, 0, 1280, 800), Rect::new(1280, 0, 1024, 768), Rect::new(-1920, -100, 1920, 1080)]
    }

    #[test]
    fn points_belong_to_the_display_that_shows_them() {
        let d = three();
        assert_eq!(at(&d, 0, 0), Some(0));
        assert_eq!(at(&d, 1280, 10), Some(1));
        assert_eq!(at(&d, -1, 900), Some(2));
        assert_eq!(at(&d, 1500, 790), None);
        assert_eq!(nearest(&d, 1500, 790), 1);
        assert_eq!(bounds(&d), Rect::new(-1920, -100, 1280 + 1024 + 1920, 1080));
        assert!(connected(&d));
        assert!(!connected(&[Rect::new(0, 0, 100, 100), Rect::new(300, 0, 100, 100)]));
    }

    #[test]
    fn the_pointer_crosses_only_where_displays_touch() {
        let d = three();
        // Rightwards across the shared edge at y = 100.
        assert_eq!(constrain(&d, 1290, 100), (1290, 100));
        // Below the smaller right display: it stays on the primary's edge…
        assert_eq!(constrain(&d, 1300, 790), (1279, 790));
        // …or the right display's bottom edge, whichever is closer.
        assert_eq!(constrain(&d, 1400, 770), (1400, 767));
        // Off the outer edges.
        assert_eq!(constrain(&d, 5000, 10), (2303, 10));
        assert_eq!(constrain(&d, -2500, 2000), (-1920, 979));
        assert_eq!(constrain(&d, 100, -5), (100, 0));
    }

    #[test]
    fn windows_belong_where_they_are_mostly() {
        let d = three();
        assert_eq!(home(&d, Rect::new(1100, 100, 400, 300)), 1);
        assert_eq!(home(&d, Rect::new(1100, 100, 300, 300)), 0);
        assert_eq!(home(&d, Rect::new(5000, 5000, 10, 10)), 1);
    }

    #[test]
    fn carried_windows_keep_their_relative_place() {
        let from = Rect::new(1280, 0, 1024, 768);
        let to = Rect::new(0, 0, 2048, 1536);
        // In the right half, it stays in the right half.
        let r = carry(Rect::new(1280 + 624, 0, 400, 300), from, to);
        assert_eq!(r, Rect::new(2048 - 400, 0, 400, 300));
        // Too large for a small display: it shrinks to fit.
        let r = carry(Rect::new(0, 0, 2000, 1500), to, from);
        assert_eq!((r.w, r.h), (1024, 768));
        assert_eq!((r.x, r.y), (1280, 0));
    }

    #[test]
    fn snapping_touches_without_overlap() {
        let p = Rect::new(0, 0, 1280, 800);
        // Dropped slightly overlapping on the right: pushed out to touch.
        assert_eq!(snap(&[p], (1024, 768), (1200, 30)), (1280, 30));
        // Dropped far below: it comes up to touch the bottom edge.
        assert_eq!(snap(&[p], (1024, 768), (100, 2000)), (100, 800));
        // Dropped far off to the upper right: it keeps at least a tenth of an edge.
        let (x, y) = snap(&[p], (1024, 768), (3000, -3000));
        let r = Rect::new(x, y, 1024, 768);
        assert!(side_of(p, r).is_some() && !r.intersects(&p), "{r:?}");
        // Between two displays, it doesn't overlap either.
        let q = Rect::new(1280, 0, 1024, 768);
        let (x, y) = snap(&[p, q], (800, 600), (1000, 100));
        let r = Rect::new(x, y, 800, 600);
        assert!(!r.intersects(&p) && !r.intersects(&q));
        assert!(side_of(p, r).is_some() || side_of(q, r).is_some());
        assert_eq!(beside(p, (1024, 768), Side::Left), (-1024, 0));
        assert_eq!(beside(p, (1024, 768), Side::Below), (0, 800));
        assert_eq!(side_of(p, Rect::new(-1024, 0, 1024, 768)), Some(Side::Left));
        assert_eq!(side_of(p, Rect::new(0, 800, 10, 10)), Some(Side::Below));
        assert_eq!(side_of(p, Rect::new(1280, 800, 10, 10)), None);
    }

    #[test]
    fn arranging_from_saved_places() {
        let sizes = [(1280, 800), (1024, 768), (1920, 1080)];
        // Nothing saved: left to right.
        let d = arrange(&sizes, &[None, None, None], 0);
        assert_eq!(d, alloc::vec![Rect::new(0, 0, 1280, 800), Rect::new(1280, 0, 1024, 768), Rect::new(2304, 0, 1920, 1080)]);
        // The second saved on the left; the third saved overlapping: moved to touch.
        let d = arrange(&sizes, &[None, Some((-1024, 0)), Some((500, 500))], 0);
        assert_eq!(d[1], Rect::new(-1024, 0, 1024, 768));
        assert!(!d[2].intersects(&d[0]) && !d[2].intersects(&d[1]));
        assert!(connected(&d));
        // Another primary: it is the one at the origin.
        let d = arrange(&sizes, &[None, None, None], 2);
        assert_eq!(d[2], Rect::new(0, 0, 1920, 1080));
        assert!(connected(&d));
        let mut n = alloc::vec![Rect::new(-100, -50, 10, 10), Rect::new(0, 0, 5, 5)];
        normalize(&mut n, 0);
        assert_eq!(n[1], Rect::new(100, 50, 5, 5));
    }

    #[test]
    fn windows_leave_with_their_display_and_come_back() {
        let a = Rect::new(0, 0, 1280, 800);
        let b = Rect::new(1280, 0, 1024, 768);
        let old = [(1, a), (2, b)];
        let w = Rect::new(1380, 100, 400, 300);
        // Display 2 unplugged: the window moves to the primary and remembers.
        let (moved, away) = migrate(&old, &[(1, a)], 0, w, None);
        assert!(a.intersect(&moved) == moved, "{moved:?}");
        assert_eq!(away, Some((2, Rect::new(100, 100, 400, 300))));
        // Plugged in again (now on the left): back to the same place on it.
        let b2 = Rect::new(-1024, 0, 1024, 768);
        let (back, away) = migrate(&[(1, a)], &[(1, a), (2, b2)], 0, moved, away);
        assert_eq!(back, Rect::new(-924, 100, 400, 300));
        assert_eq!(away, None);
        // A window on a display that stays put doesn't move.
        let still = Rect::new(10, 10, 100, 100);
        assert_eq!(migrate(&old, &[(1, a)], 0, still, None), (still, None));
        // A display that moved takes its windows along.
        let (moved, _) = migrate(&old, &[(1, a), (2, b2)], 0, w, None);
        assert_eq!(moved, Rect::new(-924, 100, 400, 300));
    }

    #[test]
    fn records_round_trip() {
        let r = Record { x: -1024, y: 16, width: 1920, height: 1080, scale: 2, wallpaper: 3 };
        assert_eq!(Record::parse(&r.format()), Some(r));
        assert_eq!(Record::parse("0,0,0,0,0"), Some(Record::default()));
        assert_eq!(Record::parse("1,2,10,10,0,1"), None);
        assert_eq!(Record::parse("1,2,800,600,3,1"), None);
        assert_eq!(Record::parse("garbage"), None);
        assert_eq!(pref_key(identity_hash("QEMU@Virtual-2")).len(), 16);
        assert_ne!(identity_hash("QEMU@Virtual-1"), identity_hash("QEMU@Virtual-2"));
    }
}
