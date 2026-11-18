//! Blank space in Rune's `virtual_rows`, pushed hard: where a frame shows
//! the list's background because no row is drawn there yet. The background
//! behind the rows is orange, so any blank shows.
//!
//! Three lists, as `BLANKING=issues` (the default), `BLANKING=full`, or
//! `BLANKING=chat` picks:
//!
//! - `issues`: zbugs-rune's issue list, as `view.rs` builds it: rows of about
//!   58 points under that estimate, with rune-kit's scroll bar, paged as
//!   zbugs-rune pages, 100 more rows asked for when the last visible row nears
//!   the last loaded, which come a round trip later.
//! - `full`: the same list under its Estimated Full Height setting: as tall
//!   as all its rows, with a placeholder for each row not loaded yet. Rows
//!   shown past the last loaded ask at once for the rows up to them and a
//!   page past.
//! - `chat`: a chat's messages, from a line to a few hundred words, so a
//!   row's height isn't known until it's measured. It opens at the bottom and
//!   follows new messages there (`stick_bottom`), loads older ones a page at a
//!   time as it scrolls up, and gets a new message every 0.3 s throughout.
//!
//! `cargo test --example blanking` pushes all three, at 120 and at 60 frames a
//! second: trackpad flings (macOS momentum, as wheel deltas) and Rune's own
//! flings at 4,000 to 32,000 points a second, swipes released into them, long
//! fast drags, jumps and glides across the list, the scroll bar's thumb
//! dragged back and forth across the track in a second down to a tenth, and
//! held at the end as pages land, presses in its track that jump far or page,
//! and paging, checking every frame that the rows cover the viewport.
//!
//! One push it leaves out shows blank space: a glide Rune steps itself
//! (`Tx::scroll_to_bottom`, `Tx::scroll_to`, a far `Tx::scroll_into_view`)
//! while pages land, at 60 frames a second. Glides to the issue list's end as
//! pages come leave about 95 frames with rows missing at the viewport's
//! bottom. Rune steps the glide after the frame's commit has mounted the rows
//! for where the glide was, and when the glide moves past the rows mounted
//! beyond the viewport, that frame draws the gap. This is so at the pinned
//! rev and at `dev`'s 32eb781. Wheel and trackpad scrolls, macOS momentum
//! among them, come before the commit, and have none.
//!
//! `cargo run --example blanking` opens the list in a window to push by
//! hand; Home and End glide to the ends, and J jumps to the middle. Headless,
//! `cargo run --example blanking -- --script examples/blanking.json --video
//! blanking.mp4 --size 420x760` records what the script does.

use std::borrow::Cow;
use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::OnceLock;

use rune::{AppSpec, Color, Cx, El, GrowthSpec, Key, KeyEvent, ScrollSpec, TextStyle, Tx};
use rune::{col, memo, oklch, row, text};
use rune_kit::{Kit, ScrollBar as _};

/// zbugs-rune's estimate for an issue row (`view.rs`'s `ROW`).
const ROW: f32 = 58.0;
/// A message's estimate: its author and a line.
const MESSAGE: f32 = 52.0;
/// Rows asked for at a time, as zbugs-rune's `LIST_PAGE`.
const PAGE: usize = 100;
const ISSUES: usize = 2000;
const MESSAGES: u64 = 2000;
/// How long rows asked for take to come: a round trip.
const LATENCY: f64 = 0.05;
/// How often a message comes.
const ARRIVAL: f64 = 0.3;

fn page() -> Color {
    oklch(0.98, 0.0, 0.0)
}

fn ink() -> Color {
    oklch(0.25, 0.0, 0.0)
}

fn blank() -> Color {
    oklch(0.75, 0.18, 55.0)
}

struct Lists {
    chat: bool,
    /// The issue list as tall as its count, with a placeholder for each row
    /// not loaded yet, as zbugs-rune's Estimated Full Height.
    full: bool,
    /// Issue numbers, or message ids oldest first, as loaded.
    ids: Vec<u64>,
    keys: Vec<String>,
    /// The issue query's limit.
    limit: usize,
    /// The rows asked for: when each request's come, and the limit it
    /// asked for.
    asked: Vec<(f64, usize)>,
    next_message: f64,
    /// How far down the list is scrolled, and the most it scrolls, which
    /// its viewport reports, as zbugs-rune's `list_offset`.
    scrolled: Rc<Cell<[f32; 2]>>,
}

impl Lists {
    fn issues(full: bool) -> Lists {
        let mut s = Lists {
            chat: false,
            full,
            ids: Vec::new(),
            keys: Vec::new(),
            limit: PAGE,
            asked: Vec::new(),
            next_message: f64::INFINITY,
            scrolled: Rc::default(),
        };
        s.splice(s.ids.len(), 0..PAGE as u64);
        s
    }

    fn chat() -> Lists {
        let mut s = Lists {
            chat: true,
            full: false,
            ids: Vec::new(),
            keys: Vec::new(),
            limit: 0,
            asked: Vec::new(),
            next_message: ARRIVAL,
            scrolled: Rc::default(),
        };
        s.splice(0, MESSAGES - PAGE as u64..MESSAGES);
        s
    }

    fn splice(&mut self, at: usize, ids: Range<u64>) {
        let prefix = if self.chat { "m" } else { "i" };
        self.keys
            .splice(at..at, ids.clone().map(|id| format!("{prefix}{id}")));
        self.ids.splice(at..at, ids);
    }

    /// Asks for rows near the loaded rows' end (or, in the chat, their
    /// start), as zbugs-rune's `list_visible` does: the rows up to the last
    /// shown and a page past, once the last asked for have come, or at once
    /// in a list as tall as its count. Timed at the next tick, which knows
    /// the time.
    fn visible(&mut self, range: Range<usize>) -> bool {
        if self.chat {
            if self.asked.is_empty() && range.start < 20 && self.ids[0] > 0 {
                self.asked.push((f64::INFINITY, 0));
            }
            return false;
        }
        let (loaded, want) = (self.ids.len(), ((range.end + 20) / PAGE + 1) * PAGE);
        if want > self.limit && (loaded >= self.limit || self.full) && loaded < ISSUES {
            self.limit = want;
            self.asked.push((f64::INFINITY, want));
        }
        false
    }

    /// Whether `tick` has something to do at `now`.
    fn ticks(&self, now: f64) -> bool {
        self.asked
            .iter()
            .any(|&(due, _)| due == f64::INFINITY || due <= now)
            || self.next_message <= now
    }

    /// The rows asked for come, and messages arrive. True when the rows
    /// changed.
    fn tick(&mut self, now: f64) -> bool {
        for (due, _) in &mut self.asked {
            if *due == f64::INFINITY {
                *due = now + LATENCY;
            }
        }
        let came: Vec<usize> = (self.asked.iter())
            .filter(|&&(due, _)| due <= now)
            .map(|&(_, limit)| limit)
            .collect();
        self.asked.retain(|&(due, _)| due > now);
        let mut changed = !came.is_empty();
        for limit in came {
            if self.chat {
                let first = self.ids[0];
                self.splice(0, first.saturating_sub(PAGE as u64)..first);
            } else if limit > self.ids.len() {
                let from = self.ids.len() as u64;
                self.splice(self.ids.len(), from..limit.min(ISSUES) as u64);
            }
        }
        if self.next_message <= now {
            self.next_message += ARRIVAL;
            let next = self.ids.last().map_or(0, |id| id + 1);
            self.splice(self.ids.len(), next..next + 1);
            changed = true;
        }
        changed
    }
}

/// A word of a message's text, picked by a hash of the message and place.
fn word(id: u64, i: u64) -> &'static str {
    const WORDS: [&str; 16] = [
        "sync", "the", "query", "rows", "client", "server", "a", "page", "of", "zero", "replica",
        "and", "cache", "mutation", "is", "fast",
    ];
    let h = (id.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ i.wrapping_mul(0xbf58_476d_1ce4_e5b9)) >> 7;
    WORDS[(h % 16) as usize]
}

/// How many words message `id` has: every ninth is long, 150 to 400 words.
fn words(id: u64) -> u64 {
    let h = id.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 11;
    if id % 9 == 4 {
        150 + h % 251
    } else {
        1 + h % 30
    }
}

fn message(id: u64) -> El {
    let body: Vec<&str> = (0..words(id)).map(|i| word(id, i)).collect();
    col()
        .w_full()
        .px(12.0)
        .py(8.0)
        .gap(3.0)
        .fill(page())
        .child(
            text(format!("user{}", id % 7))
                .font(TextStyle::new(12.0, 600.0))
                .color(ink()),
        )
        .child(
            text(body.join(" "))
                .font(TextStyle::new(13.0, 400.0))
                .color(ink())
                .wrap()
                .w_full(),
        )
}

/// As zbugs-rune's `issue_row`: a title over its meta line.
fn issue(n: u64) -> El {
    let title: Vec<&str> = (0..4 + n % 9).map(|i| word(n, i)).collect();
    col()
        .w_full()
        .px(12.0)
        .py(9.0)
        .gap(3.0)
        .fill(page())
        .child(
            row().w_full().child(
                text(title.join(" "))
                    .font(TextStyle::new(13.0, 500.0))
                    .color(ink())
                    .truncate()
                    .grow(1.0)
                    .min_w(0.0),
            ),
        )
        .child(
            text(format!("ZB-{n} · user{} · {}d", n % 7, n % 30))
                .font(TextStyle::new(12.0, 400.0))
                .color(ink()),
        )
}

fn key(s: &Lists, i: usize) -> Cow<'_, str> {
    match s.keys.get(i) {
        Some(k) => Cow::Borrowed(k.as_str()),
        None => Cow::Owned(format!("p{i}")),
    }
}

/// An issue not loaded yet, as zbugs-rune's `placeholder_row`: bars in
/// lines as tall as an issue row's.
fn placeholder() -> El {
    let line = |size: f32, weight: f32, w: f32| {
        row()
            .center_y()
            .child(text("\u{a0}").font(TextStyle::new(size, weight)))
            .child(rune::el().w(w).h(8.0).radius(4.0).fill(ink().alpha(0.1)))
    };
    col()
        .w_full()
        .px(12.0)
        .py(9.0)
        .gap(3.0)
        .fill(page())
        .child(line(13.0, 500.0, 180.0))
        .child(line(12.0, 400.0, 110.0))
}

fn kit() -> &'static Kit {
    static KIT: OnceLock<Kit> = OnceLock::new();
    KIT.get_or_init(|| Kit::neutral(false))
}

fn view(s: &Lists, _: &mut Cx<Lists>) -> El {
    let rows = col().key("rows").w_full().growth(GrowthSpec::default());
    let rows = if s.chat {
        rows.virtual_rows(s.ids.len(), MESSAGE, key, |s: &Lists, i| {
            memo(s.keys[i].as_str(), 0, || message(s.ids[i]))
        })
    } else {
        let n = if s.full { ISSUES } else { s.ids.len() };
        rows.virtual_rows(n, ROW, key, |s: &Lists, i| match s.keys.get(i) {
            Some(k) => memo(k.as_str(), 0, || issue(s.ids[i])),
            None => placeholder(),
        })
    };
    let scroll = col()
        .key("scroll")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .fill(blank())
        .scroll(ScrollSpec {
            stick_bottom: s.chat,
        })
        .on_scroll_offset({
            let scrolled = s.scrolled.clone();
            move |at| scrolled.set([at.offset[1], at.max[1]])
        })
        .child(rows.on_visible_rows(Lists::visible))
        .scroll_bar(kit());
    col().w_full().h_full().fill(page()).child(scroll)
}

fn on_key(s: &mut Lists, event: &KeyEvent, tx: &mut Tx) -> bool {
    match &event.key {
        Key::Home => tx.scroll_to("scroll", 0.0, 0.0),
        Key::End => tx.scroll_to_bottom("scroll"),
        Key::Text(t) if t == "j" => {
            let middle = s.keys[s.keys.len() / 2].clone();
            tx.scroll_into_view(middle);
        }
        _ => return false,
    }
    true
}

fn init() -> Lists {
    match std::env::var("BLANKING").as_deref() {
        Ok("chat") => Lists::chat(),
        Ok("full") => Lists::issues(true),
        _ => Lists::issues(false),
    }
}

fn main() {
    let spec = AppSpec {
        poll: Some(|s: &mut Lists, now, _| s.ticks(now) && s.tick(now)),
        on_key: Some(on_key),
        ..AppSpec::new("Blanking", (420.0, 760.0), init, view)
    };
    rune::App::new(spec).run();
}

#[cfg(test)]
mod tests {
    use super::*;
    use rune::{ElementInfo, Env, GesturePhase, Runtime, WheelEvent};

    const SIZE: (f32, f32) = (420.0, 760.0);
    /// Where the pointer scrolls: over the list.
    const AT: (f32, f32) = (200.0, 380.0);
    /// The list's viewport, its rows, and the scroll bar's thumb.
    const VIEWPORT: &str = "#0/scroll-scroll-area/scroll";
    const ROWS: &str = "#0/scroll-scroll-area/scroll/rows";
    const THUMB: &str = "#0/scroll-scroll-area/scroll-bar/thumb-slot/thumb";

    struct Run {
        rt: Runtime<Lists>,
        /// A frame's time: a 120 Hz display's, or a 60 Hz one's.
        dt: f64,
        now: f64,
        frames: usize,
        /// Frames with blank space in the viewport, and its points, by drive.
        blanks: Vec<(String, usize, f32)>,
        /// Frames with a placeholder in the viewport, by drive.
        placeholders: Vec<(String, usize)>,
        drive: String,
    }

    impl Run {
        fn new(s: Lists, hz: f64) -> Run {
            let mut rt = Runtime::new(s, view, SIZE, Env::new(false, 2.0));
            rt.frame(0.0);
            let mut run = Run {
                rt,
                dt: 1.0 / hz,
                now: 0.0,
                frames: 0,
                blanks: Vec::new(),
                placeholders: Vec::new(),
                drive: "open".into(),
            };
            run.frames(60);
            run
        }

        fn drive(&mut self, name: impl Into<String>) {
            self.drive = name.into();
        }

        /// One frame: rows and messages that are due come first, as the
        /// app's poll does, then the frame, which is checked.
        fn frame(&mut self) -> bool {
            self.now += self.dt;
            let now = self.now;
            if self.rt.state.ticks(now) {
                self.rt.update(|s, _| {
                    s.tick(now);
                });
            }
            let moving = self.rt.frame(self.dt);
            self.frames += 1;
            // What the frame drew, read once for both checks: in a debug
            // build, reading it costs more than the frame itself.
            let elements = self.rt.elements();
            let gap = blank_in(&elements);
            if gap > 0.5 {
                match self.blanks.iter_mut().find(|b| b.0 == self.drive) {
                    Some(b) => {
                        b.1 += 1;
                        b.2 += gap;
                    }
                    None => self.blanks.push((self.drive.clone(), 1, gap)),
                }
            }
            if placeholders_in(&elements) > 0 {
                match self.placeholders.iter_mut().find(|p| p.0 == self.drive) {
                    Some(p) => p.1 += 1,
                    None => self.placeholders.push((self.drive.clone(), 1)),
                }
            }
            moving
        }

        fn frames(&mut self, n: usize) {
            for _ in 0..n {
                self.frame();
            }
        }

        /// Frames until the list rests, and a little more.
        /// The rows asked for come, and the list shows rows, not
        /// placeholders.
        fn settle(&mut self) {
            let mut n = 0;
            while (self.frame() || !self.rt.state.asked.is_empty()) && n < 1200 {
                n += 1;
            }
            self.frames(12);
            assert_eq!(
                placeholders_in(&self.rt.elements()),
                0,
                "{}: placeholders on screen at rest",
                self.drive
            );
        }

        fn wheel(&mut self, dy: f32, phase: GesturePhase) {
            self.rt
                .wheel_event(WheelEvent::new(0.0, dy).at(AT.0, AT.1).phase(phase));
        }

        /// A trackpad fling as macOS sends it: a short swipe at `v` points a
        /// second (positive toward the top), then momentum's deltas, decaying
        /// as AppKit's do.
        fn trackpad(&mut self, v: f32) {
            self.wheel(0.0, GesturePhase::Began);
            for _ in 0..8 {
                self.wheel(v * self.dt as f32, GesturePhase::Moved);
                self.frame();
            }
            self.wheel(0.0, GesturePhase::Ended);
            self.wheel(0.0, GesturePhase::Began);
            let mut d = v * self.dt as f32;
            while d.abs() > 0.5 {
                self.wheel(d, GesturePhase::Moved);
                self.frame();
                d *= 0.975;
            }
            self.wheel(0.0, GesturePhase::Ended);
            self.settle();
        }

        /// Rune's own momentum, as a platform without it gets.
        fn fling(&mut self, v: f32) {
            self.rt.fling(AT.0, AT.1, v);
            self.settle();
        }

        /// A swipe released into Rune's momentum, as Rune's scripts' eased
        /// `scroll` with `fling` sends it: `d` points over 0.15 s, eased, then
        /// a fling at the swipe's speed.
        fn swipe(&mut self, d: f32) {
            let dur = 0.15;
            let steps = (dur / self.dt).ceil() as usize;
            let mut prev = 0.0;
            self.wheel(0.0, GesturePhase::Began);
            for s in 1..=steps {
                let f = s as f32 / steps as f32;
                let f = f * (2.0 - f);
                self.wheel(d * f - prev, GesturePhase::Moved);
                prev = d * f;
                self.frame();
            }
            self.wheel(0.0, GesturePhase::Ended);
            self.rt.fling(AT.0, AT.1, d / (2.0 * dur as f32));
            self.settle();
        }

        /// The fingers held down, moving at `v` for `seconds`.
        fn drag(&mut self, v: f32, seconds: f64) {
            self.wheel(0.0, GesturePhase::Began);
            for _ in 0..(seconds / self.dt) as usize {
                self.wheel(v * self.dt as f32, GesturePhase::Moved);
                self.frame();
            }
            self.wheel(0.0, GesturePhase::Ended);
            self.settle();
        }

        fn tx(&mut self, f: impl FnOnce(&mut Lists, &mut Tx)) {
            self.rt.update(f);
            self.settle();
        }

        /// Where the last frame drew `path`.
        fn rect(&self, path: &str) -> [f32; 4] {
            let elements = self.rt.elements();
            let e = elements.iter().find(|e| e.path == path);
            e.unwrap_or_else(|| panic!("{path} drawn")).rect
        }

        /// How far down the list is scrolled: 0 at the top, 1 at the
        /// bottom.
        fn scrolled(&self) -> f32 {
            let [offset, max] = self.rt.state.scrolled.get();
            if max <= 0.0 {
                return 0.0;
            }
            (offset / max).clamp(0.0, 1.0)
        }

        /// The pointer at the list's right edge, which brings up the overlay
        /// scroll bar, as on a Mac with a trackpad, until it shows.
        fn reveal(&mut self) {
            self.rt.pointer_move(SIZE.0 - 4.0, AT.1);
            self.frames(12);
        }

        /// The scroll bar's thumb grabbed at its middle and dragged to each
        /// of `stops`, fractions of the window's height, `seconds` to each,
        /// then let go. The least and the most the list was scrolled on the
        /// way (`scrolled`).
        fn thumb_drag(&mut self, stops: &[f32], seconds: f64) -> (f32, f32) {
            self.reveal();
            let thumb = self.rect(THUMB);
            let x = thumb[0] + thumb[2] / 2.0;
            let mut y = thumb[1] + thumb[3] / 2.0;
            self.rt.pointer_move(x, y);
            self.rt.pointer_down(x, y);
            self.frame();
            let (mut least, mut most) = (1.0f32, 0.0f32);
            let steps = (seconds / self.dt).ceil() as usize;
            for &stop in stops {
                let (from, to) = (y, stop * SIZE.1);
                for i in 1..=steps {
                    y = from + (to - from) * i as f32 / steps as f32;
                    self.rt.pointer_move(x, y);
                    self.frame();
                    let at = self.scrolled();
                    (least, most) = (least.min(at), most.max(at));
                }
            }
            self.rt.pointer_up(x, y);
            self.settle();
            (least, most)
        }

        /// A press in the scroll bar's track `at` of the window's height,
        /// held `seconds`: a jump there, or pages toward it, as the system's
        /// "Click in the scroll bar to" says. How far the list was scrolled
        /// after the press's first frame (`scrolled`).
        fn track_press(&mut self, at: f32, seconds: f64) -> f32 {
            self.reveal();
            let thumb = self.rect(THUMB);
            let (x, y) = (thumb[0] + thumb[2] / 2.0, at * SIZE.1);
            self.rt.pointer_move(x, y);
            self.rt.pointer_down(x, y);
            self.frame();
            let landed = self.scrolled();
            for _ in 1..(seconds / self.dt) as usize {
                self.frame();
            }
            self.rt.pointer_up(x, y);
            self.settle();
            landed
        }

        fn report(&self, list: &str) {
            let blank: usize = self.blanks.iter().map(|b| b.1).sum();
            let by: Vec<String> = self
                .blanks
                .iter()
                .map(|(d, n, pts)| format!("{d}:{n}/{pts:.0}pt"))
                .collect();
            let shown: usize = self.placeholders.iter().map(|p| p.1).sum();
            let by_drive: Vec<String> = (self.placeholders.iter())
                .map(|(d, n)| format!("{d}:{n}"))
                .collect();
            println!(
                "BLANKING {list} frames={} blank={blank} blanks=[{}] placeholder-frames={shown} [{}] rows={}",
                self.frames,
                by.join(","),
                by_drive.join(","),
                self.rt.state.ids.len()
            );
        }
    }

    /// The list's rows among a frame's `elements`, by key, and where.
    fn rows_in(elements: &[ElementInfo]) -> impl Iterator<Item = (&str, [f32; 4])> {
        elements.iter().filter(|e| !e.exiting).filter_map(|e| {
            let k = e.path.strip_prefix(ROWS)?.strip_prefix('/')?;
            (!k.contains('/')).then_some((k, e.rect))
        })
    }

    /// Placeholders in the viewport.
    fn placeholders_in(elements: &[ElementInfo]) -> usize {
        let viewport = elements.iter().find(|e| e.path == VIEWPORT).map(|e| e.rect);
        let [_, top, _, h] = viewport.expect("the list's viewport");
        rows_in(elements)
            .filter(|(k, r)| k.starts_with('p') && r[1] < top + h && r[1] + r[3] > top)
            .count()
    }

    /// Points of the viewport, within the rows' extent, that no row covers.
    fn blank_in(elements: &[ElementInfo]) -> f32 {
        let rect = |path: &str| elements.iter().find(|e| e.path == path).map(|e| e.rect);
        let viewport = rect(VIEWPORT).expect("the list's viewport");
        let content = rect(ROWS).expect("the list's rows");
        let top = viewport[1].max(content[1]);
        let bottom = (viewport[1] + viewport[3]).min(content[1] + content[3]);
        let mut rows: Vec<(f32, f32)> = rows_in(elements)
            .map(|(_, r)| (r[1], r[1] + r[3]))
            .collect();
        rows.sort_by(|a, b| a.0.total_cmp(&b.0));
        let (mut y, mut gap) = (top, 0.0);
        for (from, to) in rows {
            if from > y {
                gap += from.min(bottom) - y;
            }
            y = y.max(to);
            if y >= bottom {
                break;
            }
        }
        gap + (bottom - y).max(0.0)
    }

    /// The scroll bar's pushes, from the list's first page, so pages land
    /// under the thumb. Positive velocities move toward the top.
    fn thumb(run: &mut Run, toward_end: f32) {
        // The thumb dragged end to end and back, in a second down to a
        // tenth of one each way: as fast as a hand goes.
        run.drive("thumb-drag");
        for seconds in [1.0, 0.25, 0.1] {
            let (least, most) = run.thumb_drag(&[1.0, 0.0, 1.0, 0.0, 0.5], seconds);
            assert!(
                least < 0.02 && most > 0.98,
                "a thumb drag of {seconds} s to each end showed {least}..{most}"
            );
        }
        // Held at the end the list pages at (the issues' bottom, the chat's
        // top) as pages land there and lengthen the track.
        run.drive("thumb-hold");
        let (rows, first) = (run.rt.state.ids.len(), run.rt.state.ids[0]);
        let end = if toward_end < 0.0 {
            [1.0, 0.99]
        } else {
            [0.0, 0.01]
        };
        run.thumb_drag(&end.repeat(8), 0.1);
        let s = &run.rt.state;
        assert!(
            s.ids.len() >= rows + 3 * PAGE || s.ids.len() == ISSUES || s.ids[0] == 0,
            "held at the end, paged from {rows} rows (from {first}) to {} (from {})",
            s.ids.len(),
            s.ids[0]
        );
        // Far jumps, under "Jump to the spot that's clicked", then pages
        // held toward each end, under "Jump to the next page".
        run.drive("thumb-jump");
        run.rt.set_system_scroller_jumps(true);
        for at in [0.95, 0.05, 0.6, 0.01, 0.99, 0.3] {
            let landed = run.track_press(at, 0.0);
            assert!(
                (landed - at).abs() < 0.2,
                "a jump to {at} of the track showed {landed}"
            );
        }
        run.rt.set_system_scroller_jumps(false);
        run.drive("track-page");
        run.track_press(0.99, 1.5);
        run.track_press(0.01, 1.5);
    }

    /// The pushes both lists get. Positive velocities move toward the top.
    fn push(run: &mut Run, toward_end: f32) {
        thumb(run, toward_end);
        for v in [4_000.0, 8_000.0, 16_000.0, 32_000.0] {
            run.drive(format!("trackpad-{}k", v as u32 / 1000));
            for _ in 0..2 {
                run.trackpad(toward_end * v);
            }
            run.trackpad(-toward_end * v);
            run.drive(format!("fling-{}k", v as u32 / 1000));
            for _ in 0..2 {
                run.fling(toward_end * v);
            }
            run.fling(-toward_end * v);
        }
        for v in [8_000.0, 16_000.0, 32_000.0] {
            run.drive(format!("swipe-fling-{}k", v as u32 / 1000));
            for sign in [1.0, 1.0, -1.0] {
                run.swipe(sign * toward_end * v * 0.3);
            }
        }
        for v in [6_000.0, 12_000.0] {
            run.drive(format!("drag-{}k", v as u32 / 1000));
            run.drag(toward_end * v, 1.5);
            run.drag(-toward_end * v, 1.5);
        }
        run.drive("jump");
        for at in [0.9, 0.1, 0.6, 0.0, 1.0, 0.3] {
            run.tx(move |s, tx| {
                let i = ((s.keys.len() - 1) as f32 * at) as usize;
                tx.scroll_into_view(s.keys[i].clone());
            });
        }
        run.drive("jump-glide");
        for at in [0.5, 0.2, 0.8] {
            run.rt.update(move |s, tx| {
                let i = ((s.keys.len() - 1) as f32 * at) as usize;
                tx.scroll_into_view(s.keys[i].clone());
            });
            run.frames(3);
            run.tx(|_, tx| tx.scroll_to_bottom("scroll"));
            run.tx(|_, tx| tx.scroll_to("scroll", 0.0, 0.0));
        }
        run.drive("glide");
        run.tx(|_, tx| tx.scroll_to("scroll", 0.0, 0.0));
        run.tx(|_, tx| tx.scroll_to_bottom("scroll"));
        run.tx(|_, tx| tx.scroll_to("scroll", 0.0, 20_000.0));
        run.tx(|_, tx| tx.scroll_to_y("scroll", 0.0));
        run.tx(|_, tx| tx.scroll_to_bottom("scroll"));
    }

    #[test]
    fn issues_cover_the_viewport_every_frame() {
        issues(false);
    }

    /// The issue list as tall as its count, as zbugs-rune's Estimated Full
    /// Height: the thumb's drags and jumps go straight to rows not loaded,
    /// which show placeholders until they come.
    #[test]
    fn a_full_height_issue_list_covers_the_viewport_every_frame() {
        issues(true);
    }

    fn issues(full: bool) {
        let name = if full { "full-height issues" } else { "issues" };
        for hz in [120.0, 60.0] {
            let mut run = Run::new(Lists::issues(full), hz);
            push(&mut run, -1.0);
            run.report(&format!("{name} {hz} Hz"));
            assert!(
                full || run.placeholders.is_empty(),
                "placeholders in a paged list: {:?}",
                run.placeholders
            );
            assert!(
                run.rt.state.ids.len() > 1000,
                "paged to {} rows",
                run.rt.state.ids.len()
            );
            assert!(
                run.blanks.is_empty(),
                "{hz} Hz, blank frames: {:?}",
                run.blanks
            );
        }
    }

    #[test]
    fn a_chat_covers_the_viewport_every_frame() {
        for hz in [120.0, 60.0] {
            let mut run = Run::new(Lists::chat(), hz);
            push(&mut run, 1.0);
            run.report(&format!("chat {hz} Hz"));
            assert!(
                run.rt.state.ids[0] < MESSAGES / 2,
                "paged back to {}",
                run.rt.state.ids[0]
            );
            assert!(
                run.blanks.is_empty(),
                "{hz} Hz, blank frames: {:?}",
                run.blanks
            );
        }
    }
}
