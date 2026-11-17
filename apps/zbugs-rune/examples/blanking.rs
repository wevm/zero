//! Blank space in Rune's `virtual_rows`, pushed hard: where a frame shows
//! the list's background because no row is drawn there yet. The background
//! behind the rows is orange, so any blank shows.
//!
//! Two lists, as `BLANKING=issues` (the default) or `BLANKING=chat` picks:
//!
//! - `issues`: zbugs-rune's issue list, as `view.rs` builds it: rows of about
//!   58 points under that estimate, with rune-kit's scroll bar, paged as
//!   zbugs-rune pages, 100 more rows asked for when the last visible row nears
//!   the last loaded, which come a round trip later.
//! - `chat`: a chat's messages, from a line to a few hundred words, so a
//!   row's height isn't known until it's measured. It opens at the bottom and
//!   follows new messages there (`stick_bottom`), loads older ones a page at a
//!   time as it scrolls up, and gets a new message every 0.3 s throughout.
//!
//! `cargo test --example blanking` pushes both, at 120 and at 60 frames a
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
    /// Issue numbers, or message ids oldest first, as loaded.
    ids: Vec<u64>,
    keys: Vec<String>,
    /// The issue query's limit.
    limit: usize,
    /// When the rows asked for come.
    due: Option<f64>,
    next_message: f64,
    /// How far down the list is scrolled, and the most it scrolls, which
    /// its viewport reports, as zbugs-rune's `list_offset`.
    scrolled: Rc<Cell<[f32; 2]>>,
}

impl Lists {
    fn issues() -> Lists {
        let mut s = Lists {
            chat: false,
            ids: Vec::new(),
            keys: Vec::new(),
            limit: PAGE,
            due: None,
            next_message: f64::INFINITY,
            scrolled: Rc::default(),
        };
        s.splice(s.ids.len(), 0..PAGE as u64);
        s
    }

    fn chat() -> Lists {
        let mut s = Lists {
            chat: true,
            ids: Vec::new(),
            keys: Vec::new(),
            limit: 0,
            due: None,
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

    /// Asks for the next page near the loaded rows' end (or, in the chat,
    /// their start), as zbugs-rune's `list_visible` does.
    fn visible(&mut self, range: Range<usize>) -> bool {
        if self.due.is_some() {
            return false;
        }
        let more = if self.chat {
            range.start < 20 && self.ids[0] > 0
        } else {
            self.ids.len() >= self.limit
                && range.end + 20 >= self.ids.len()
                && self.ids.len() < ISSUES
        };
        if more {
            self.limit += PAGE;
            // Timed at the next tick, which knows the time.
            self.due = Some(f64::INFINITY);
        }
        false
    }

    /// Whether `tick` has something to do at `now`.
    fn ticks(&self, now: f64) -> bool {
        self.due
            .is_some_and(|due| due == f64::INFINITY || due <= now)
            || self.next_message <= now
    }

    /// The rows asked for come, and messages arrive. True when the rows
    /// changed.
    fn tick(&mut self, now: f64) -> bool {
        let mut changed = false;
        match self.due {
            Some(due) if due == f64::INFINITY => self.due = Some(now + LATENCY),
            Some(due) if due <= now => {
                self.due = None;
                changed = true;
                if self.chat {
                    let first = self.ids[0];
                    self.splice(0, first.saturating_sub(PAGE as u64)..first);
                } else {
                    let from = self.ids.len() as u64;
                    self.splice(self.ids.len(), from..self.limit.min(ISSUES) as u64);
                }
            }
            _ => {}
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
    Cow::Borrowed(s.keys[i].as_str())
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
        rows.virtual_rows(s.ids.len(), ROW, key, |s: &Lists, i| {
            memo(s.keys[i].as_str(), 0, || issue(s.ids[i]))
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
    if std::env::var("BLANKING").as_deref() == Ok("chat") {
        Lists::chat()
    } else {
        Lists::issues()
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
    use rune::{Env, GesturePhase, Runtime, WheelEvent};

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
            let gap = blank_in(&self.rt);
            if gap > 0.5 {
                match self.blanks.iter_mut().find(|b| b.0 == self.drive) {
                    Some(b) => {
                        b.1 += 1;
                        b.2 += gap;
                    }
                    None => self.blanks.push((self.drive.clone(), 1, gap)),
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
        fn settle(&mut self) {
            let mut n = 0;
            while self.frame() && n < 1200 {
                n += 1;
            }
            self.frames(12);
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
            println!(
                "BLANKING {list} frames={} blank={blank} blanks=[{}] rows={}",
                self.frames,
                by.join(","),
                self.rt.state.ids.len()
            );
        }
    }

    /// Points of the viewport, within the rows' extent, that no row covers.
    fn blank_in(rt: &Runtime<Lists>) -> f32 {
        let elements = rt.elements();
        let rect = |path: &str| elements.iter().find(|e| e.path == path).map(|e| e.rect);
        let viewport = rect(VIEWPORT).expect("the list's viewport");
        let content = rect(ROWS).expect("the list's rows");
        let top = viewport[1].max(content[1]);
        let bottom = (viewport[1] + viewport[3]).min(content[1] + content[3]);
        let mut rows: Vec<(f32, f32)> = elements
            .iter()
            .filter(|e| {
                !e.exiting
                    && e.path
                        .strip_prefix(ROWS)
                        .and_then(|k| k.strip_prefix('/'))
                        .is_some_and(|k| !k.contains('/'))
            })
            .map(|e| (e.rect[1], e.rect[1] + e.rect[3]))
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
        for hz in [120.0, 60.0] {
            let mut run = Run::new(Lists::issues(), hz);
            push(&mut run, -1.0);
            run.report(&format!("issues {hz} Hz"));
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
