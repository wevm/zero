//! Blank space in GPUI's lists, pushed hard: where a frame shows the list's
//! background because no row is painted there yet. The background behind the
//! rows is orange, so any blank shows.
//!
//! Two lists, as `BLANKING=issues` (the default) or `BLANKING=chat` picks:
//!
//! - `issues`: zbugs-gpui's issue list, as `app.rs` builds it: a
//!   `uniform_list` of 56-point rows with GPUI Component's scroll bar that
//!   asks for 100 more rows when the rows it renders near the last loaded,
//!   which come a round trip later.
//! - `chat`: a chat in GPUI's `list`, bottom-aligned, of messages from a line
//!   to a few hundred words, so a row's height isn't known until it renders.
//!   It opens at the bottom and follows new messages there, loads older ones a
//!   page at a time as it scrolls up, and gets a new message every 0.3 s
//!   throughout. It has the same scroll bar.
//!
//! `cargo test --example blanking` pushes both, headless: the scroll bar's
//! thumb dragged back and forth across the track in a second down to a
//! tenth, and held at the end the list pages at as pages land, clicks in its
//! track that jump far, trackpad flings (momentum, as the platform's scroll
//! events) at 4,000 to 32,000 points a second, long fast drags, jumps across
//! the list, and paging, checking every frame that the rows painted cover the
//! viewport. `cargo run --example blanking` does the same in a window, on the
//! display's frames, and prints what it found.

use gpui_kit::component::scroll::{ScrollableElement as _, ScrollbarHandle};
use gpui_kit::*;
use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// zbugs-gpui's row height (`app.rs`'s `ROW_HEIGHT`).
const ROW_HEIGHT: f32 = 56.;
/// Rows asked for at a time, as zbugs-gpui's `PAGE`.
const PAGE: usize = 100;
const ISSUES: usize = 2000;
const MESSAGES: u64 = 2000;
/// How long rows asked for take to come: a round trip.
const LATENCY: Duration = Duration::from_millis(50);
/// How often a message comes.
const ARRIVAL: Duration = Duration::from_millis(300);
const SIZE: (f32, f32) = (420., 760.);
/// One frame, at 120 Hz.
const FRAME: Duration = Duration::from_micros(8333);

/// What the last frame painted: the viewport, and each row's bounds.
#[derive(Default)]
struct Painted {
    viewport: Option<Bounds<Pixels>>,
    rows: Vec<(usize, Bounds<Pixels>)>,
}

struct Lists {
    chat: bool,
    /// Issue numbers, or message ids oldest first, as loaded.
    ids: Vec<u64>,
    /// The issue query's limit.
    limit: usize,
    /// Rows asked for, and when they come.
    due: Option<Duration>,
    asked: bool,
    next_message: Duration,
    scroll: UniformListScrollHandle,
    state: ListState,
    painted: Rc<RefCell<Painted>>,
    /// Where the pointer is.
    pointer: Point<Pixels>,
}

impl Lists {
    fn new(chat: bool) -> Lists {
        let (ids, next_message) = if chat {
            ((MESSAGES - PAGE as u64..MESSAGES).collect(), ARRIVAL)
        } else {
            ((0..PAGE as u64).collect(), Duration::MAX)
        };
        let state = ListState::new(PAGE, ListAlignment::Bottom, px(1000.));
        Lists {
            chat,
            ids,
            limit: PAGE,
            due: None,
            asked: false,
            next_message,
            scroll: UniformListScrollHandle::new(),
            state,
            painted: Rc::default(),
            pointer: pointer(),
        }
    }

    /// Near the loaded rows' end (in the chat, their start): ask for the
    /// next page, as zbugs-gpui's `grow` does.
    fn visible(&mut self, range: Range<usize>) {
        if self.asked || self.due.is_some() {
            return;
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
            self.asked = true;
        }
    }

    /// The rows asked for come, and messages arrive, at `now` since the
    /// start.
    fn tick(&mut self, now: Duration, cx: &mut Context<Self>) {
        if self.chat {
            let top = self.state.logical_scroll_top().item_ix;
            self.visible(top..top + 1);
        }
        if std::mem::take(&mut self.asked) {
            self.due = Some(now + LATENCY);
        }
        if self.due.is_some_and(|due| due <= now) {
            self.due = None;
            if self.chat {
                let first = self.ids[0];
                let older: Vec<u64> = (first.saturating_sub(PAGE as u64)..first).collect();
                self.state.splice(0..0, older.len());
                self.ids.splice(0..0, older);
            } else {
                let from = self.ids.len() as u64;
                self.ids.extend(from..self.limit.min(ISSUES) as u64);
            }
            cx.notify();
        }
        if self.next_message <= now {
            self.next_message += ARRIVAL;
            let next = self.ids.last().map_or(0, |id| id + 1);
            let len = self.ids.len();
            self.state.splice(len..len, 1);
            self.ids.push(next);
            cx.notify();
        }
    }

    /// Records where a row paints.
    fn recorder(&self, ix: usize) -> impl IntoElement + use<> {
        let painted = self.painted.clone();
        canvas(
            move |bounds, _, _| painted.borrow_mut().rows.push((ix, bounds)),
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }

    fn issue(&self, ix: usize) -> impl IntoElement + use<> {
        let n = self.ids[ix];
        let title: Vec<&str> = (0..4 + n % 9).map(|i| word(n, i)).collect();
        div()
            .relative()
            .w_full()
            .h(px(ROW_HEIGHT))
            .px_3()
            .py_2()
            .flex()
            .flex_col()
            .gap_1()
            .bg(page())
            .text_size(px(13.))
            .child(div().truncate().child(title.join(" ")))
            .child(format!("ZB-{n} · user{} · {}d", n % 7, n % 30))
            .child(self.recorder(ix))
    }

    fn message(&self, ix: usize) -> impl IntoElement + use<> {
        let id = self.ids[ix];
        let body: Vec<&str> = (0..words(id)).map(|i| word(id, i)).collect();
        // At least the height the text wraps to in the window, so heights
        // differ as much where text has no metrics (tests).
        let lines = (words(id) as f32 * 6.5 / 66.).ceil();
        div()
            .relative()
            .w_full()
            .min_h(px(28. + 15. * lines))
            .px_3()
            .py_2()
            .bg(page())
            .text_size(px(13.))
            .child(format!("user{}", id % 7))
            .child(body.join(" "))
            .child(self.recorder(ix))
    }

    /// Points of the viewport, within the rows' extent, that the last frame
    /// painted no row over.
    fn blank(&self) -> f32 {
        let painted = self.painted.borrow();
        let viewport = painted.viewport.expect("the list's viewport painted");
        let len = self.ids.len();
        let mut rows: Vec<(f32, f32)> = Vec::new();
        let (mut top, mut bottom) = (viewport.top(), viewport.bottom());
        for (ix, b) in &painted.rows {
            if *ix == 0 {
                top = top.max(b.top());
            }
            if *ix + 1 == len {
                bottom = bottom.min(b.bottom());
            }
            rows.push((b.top().as_f32(), b.bottom().as_f32()));
        }
        let (top, bottom) = (top.as_f32(), bottom.as_f32());
        rows.sort_by(|a, b| a.0.total_cmp(&b.0));
        let (mut y, mut gap) = (top, 0.);
        for (from, to) in rows {
            if from > y {
                gap += from.min(bottom) - y;
            }
            y = y.max(to);
            if y >= bottom {
                break;
            }
        }
        gap + (bottom - y).max(0.)
    }

    /// The list's scroll, as its scroll bar sees it.
    fn handle(&self) -> &dyn ScrollbarHandle {
        if self.chat { &self.state } else { &self.scroll }
    }

    /// How far down the list is scrolled: 0 at the top, 1 at the bottom.
    fn scrolled(&self) -> f32 {
        let handle = self.handle();
        let max = handle.content_size().height - handle.viewport_bounds().size.height;
        if max <= px(0.) {
            return 0.;
        }
        (-handle.offset().y / max).clamp(0., 1.)
    }

    /// The middle of the scroll bar's thumb, which GPUI Component draws
    /// rather than lays out: as long as the share of the list shown, 48
    /// points at least, down the bar as far as the list is scrolled.
    fn thumb(&self) -> Point<Pixels> {
        let viewport = self.handle().viewport_bounds();
        let track = viewport.size.height;
        let content = self.handle().content_size().height;
        let len = (track / content * track).max(px(48.)).min(track);
        let y = viewport.top() + (track - len) * self.scrolled() + len / 2.;
        point(viewport.right() - px(8.), y)
    }

    /// The pointer's input for `step`, if it has one.
    fn input(&mut self, step: Step) -> Option<PlatformInput> {
        let (button, modifiers) = (MouseButton::Left, Modifiers::default());
        let position = match step {
            Step::Wheel(dy, phase) => return Some(PlatformInput::Scroll(scroll_event(dy, phase))),
            Step::Jump(_) | Step::Rest => return None,
            Step::Grab => self.thumb(),
            Step::Press(at) | Step::Drag(at) => point(self.thumb().x, px(SIZE.1 * at)),
            Step::Release => self.pointer,
            Step::Leave => pointer(),
        };
        self.pointer = position;
        Some(match step {
            Step::Grab | Step::Press(_) => PlatformInput::MouseDown(MouseDownEvent {
                position,
                button,
                modifiers,
                click_count: 1,
                first_mouse: false,
            }),
            Step::Release => PlatformInput::MouseUp(MouseUpEvent {
                position,
                button,
                modifiers,
                click_count: 1,
            }),
            _ => PlatformInput::MouseMove(MouseMoveEvent {
                position,
                pressed_button: matches!(step, Step::Drag(_)).then_some(button),
                modifiers,
            }),
        })
    }

    /// Jumps to the row a fraction `at` of the way down.
    fn jump(&mut self, at: f32) {
        let ix = ((self.ids.len() - 1) as f32 * at) as usize;
        if self.chat {
            self.state.scroll_to_reveal_item(ix);
        } else {
            self.scroll.scroll_to_item(ix, ScrollStrategy::Top);
        }
    }
}

impl Render for Lists {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let painted = self.painted.clone();
        // Prepainted before the list: a frame's rows start afresh.
        let viewport = canvas(
            move |bounds, _, _| {
                let mut p = painted.borrow_mut();
                p.viewport = Some(bounds);
                p.rows.clear();
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();
        let list = if self.chat {
            list(
                self.state.clone(),
                cx.processor(|this, ix, _, _| this.message(ix).into_any_element()),
            )
            .size_full()
            .into_any_element()
        } else {
            let len = self.ids.len();
            uniform_list(
                "issues",
                len,
                cx.processor(move |this, range: Range<usize>, _, _| {
                    this.visible(range.clone());
                    range.map(|ix| this.issue(ix)).collect::<Vec<_>>()
                }),
            )
            .track_scroll(&self.scroll)
            .size_full()
            .into_any_element()
        };
        let area = div()
            .relative()
            .size_full()
            .bg(blank_color())
            .child(viewport)
            .child(list);
        if self.chat {
            area.vertical_scrollbar(&self.state)
        } else {
            area.vertical_scrollbar(&self.scroll)
        }
    }
}

fn page() -> Rgba {
    rgb(0xfafafa)
}

fn blank_color() -> Rgba {
    rgb(0xff8c1a)
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

/// One frame's input.
#[derive(Clone, Copy)]
enum Step {
    /// A scroll event of `dy` points (positive toward the top).
    Wheel(f32, TouchPhase),
    /// A jump to the row a fraction of the way down.
    Jump(f32),
    /// The mouse pressed on the scroll bar's thumb, at its middle.
    Grab,
    /// The mouse, held, moved down the scroll bar to a fraction of the
    /// window's height.
    Drag(f32),
    /// The mouse pressed in the scroll bar's track a fraction of the
    /// window's height down: a jump there.
    Press(f32),
    /// The mouse let go where it is.
    Release,
    /// The mouse moved back over the list's middle.
    Leave,
    /// No input.
    Rest,
}

/// The pushes, frame by frame, each named for the report. `toward_end` is
/// the sign of a scroll toward the list's end.
fn script(chat: bool) -> Vec<(String, Step)> {
    let toward_end = if chat { 1. } else { -1. };
    let dt = FRAME.as_secs_f32();
    let mut steps = Vec::new();
    let mut add = |name: &str, step: Step, frames: usize| {
        for _ in 0..frames {
            steps.push((name.to_owned(), step));
        }
    };
    add("open", Step::Rest, 60);
    // The scroll bar first, from the list's first page, so pages land under
    // the thumb: dragged end to end and back, in a second down to a tenth of
    // one each way, as fast as a hand goes, then held at the end the list
    // pages at (the issues' bottom, the chat's top) as pages land there and
    // lengthen the track. A scroll brings the bar up first, as on a Mac with
    // a trackpad.
    let end: [f32; 2] = if chat { [0., 0.01] } else { [1., 0.99] };
    let mut from = if chat { 1. } else { 0. };
    for (name, frames, stops) in [
        ("thumb-drag", 120, vec![1., 0., 1., 0., 0.5]),
        ("thumb-drag", 30, vec![1., 0., 1., 0., 0.5]),
        ("thumb-drag", 12, vec![1., 0., 1., 0., 0.5]),
        ("thumb-hold", 12, end.repeat(8)),
    ] {
        add(name, Step::Wheel(1., TouchPhase::Moved), 1);
        add(name, Step::Wheel(-1., TouchPhase::Moved), 1);
        add(name, Step::Grab, 1);
        for stop in stops {
            for i in 1..=frames {
                add(
                    name,
                    Step::Drag(from + (stop - from) * i as f32 / frames as f32),
                    1,
                );
            }
            from = stop;
        }
        add(name, Step::Release, 1);
        add(name, Step::Rest, 30);
    }
    // Far jumps: GPUI Component's scroll bar jumps to a click in its track.
    for at in [0.95, 0.05, 0.6, 0.01, 0.99, 0.3] {
        add("thumb-jump", Step::Wheel(1., TouchPhase::Moved), 1);
        add("thumb-jump", Step::Wheel(-1., TouchPhase::Moved), 1);
        add("thumb-jump", Step::Press(at), 1);
        add("thumb-jump", Step::Release, 1);
        add("thumb-jump", Step::Rest, 30);
    }
    add("thumb-jump", Step::Leave, 1);
    for v in [4_000., 8_000., 16_000., 32_000.] {
        let name = format!("trackpad-{}k", v as u32 / 1000);
        for sign in [1., 1., 1., -1.] {
            let v = sign * toward_end * v;
            add(&name, Step::Wheel(0., TouchPhase::Started), 1);
            add(&name, Step::Wheel(v * dt, TouchPhase::Moved), 8);
            add(&name, Step::Wheel(0., TouchPhase::Ended), 1);
            // Momentum, decaying as AppKit's does.
            let mut d = v * dt;
            while d.abs() > 0.5 {
                add(&name, Step::Wheel(d, TouchPhase::Moved), 1);
                d *= 0.975;
            }
            add(&name, Step::Rest, 12);
        }
    }
    for v in [6_000., 12_000.] {
        let name = format!("drag-{}k", v as u32 / 1000);
        for sign in [1., -1.] {
            add(&name, Step::Wheel(0., TouchPhase::Started), 1);
            add(
                &name,
                Step::Wheel(sign * toward_end * v * dt, TouchPhase::Moved),
                180,
            );
            add(&name, Step::Wheel(0., TouchPhase::Ended), 1);
            add(&name, Step::Rest, 12);
        }
    }
    for at in [0.9, 0.1, 0.6, 0.0, 1.0, 0.3] {
        add("jump", Step::Jump(at), 1);
        add("jump", Step::Rest, 30);
    }
    steps
}

/// Frames with blank space, and its points, by push.
#[derive(Default)]
struct Tally {
    frames: usize,
    blanks: Vec<(String, usize, f32)>,
}

impl Tally {
    fn count(&mut self, name: &str, gap: f32) {
        self.frames += 1;
        if gap <= 0.5 {
            return;
        }
        match self.blanks.iter_mut().find(|b| b.0 == name) {
            Some(b) => {
                b.1 += 1;
                b.2 += gap;
            }
            None => self.blanks.push((name.to_owned(), 1, gap)),
        }
    }

    fn report(&self, list: &str, rows: usize) -> String {
        let blank: usize = self.blanks.iter().map(|b| b.1).sum();
        let by: Vec<String> = self
            .blanks
            .iter()
            .map(|(d, n, pts)| format!("{d}:{n}/{pts:.0}pt"))
            .collect();
        format!(
            "BLANKING {list} frames={} blank={blank} blanks=[{}] rows={rows}",
            self.frames,
            by.join(",")
        )
    }
}

/// Where the pointer scrolls: over the list.
fn pointer() -> Point<Pixels> {
    point(px(SIZE.0 / 2.), px(SIZE.1 / 2.))
}

fn scroll_event(dy: f32, phase: TouchPhase) -> ScrollEvent {
    ScrollEvent {
        position: pointer(),
        delta: ScrollDelta::Pixels(point(px(0.), px(dy))),
        modifiers: Modifiers::default(),
        touch_phase: phase,
        source: ScrollSource::Pointer,
    }
}

/// Runs the script in a window, a step a frame, and prints the tally.
fn drive(view: Entity<Lists>, window: &mut Window, cx: &mut App) {
    let steps = script(view.read(cx).chat);
    let start = Instant::now();
    window.dispatch_event(
        PlatformInput::MouseMove(MouseMoveEvent {
            position: pointer(),
            pressed_button: None,
            modifiers: Modifiers::default(),
        }),
        cx,
    );
    next(view, steps, 0, Tally::default(), start, window);
}

fn next(
    view: Entity<Lists>,
    steps: Vec<(String, Step)>,
    i: usize,
    mut tally: Tally,
    start: Instant,
    window: &Window,
) {
    window.on_next_frame(move |window, cx| {
        // What the frame before this one painted.
        if i > 0 {
            tally.count(&steps[i - 1].0, view.read(cx).blank());
        }
        let Some((_, step)) = steps.get(i).cloned() else {
            let lists = view.read(cx);
            let list = if lists.chat { "chat" } else { "issues" };
            println!("{}", tally.report(list, lists.ids.len()));
            cx.quit();
            return;
        };
        if let Some(input) = view.update(cx, |lists, _| lists.input(step)) {
            window.dispatch_event(input, cx);
        }
        view.update(cx, |lists, cx| {
            if let Step::Jump(at) = step {
                lists.jump(at);
            }
            lists.tick(start.elapsed(), cx);
            cx.notify();
        });
        next(view, steps, i + 1, tally, start, window);
    });
}

fn main() {
    let chat = std::env::var("BLANKING").as_deref() == Ok("chat");
    gpui_kit::application().run(move |cx| {
        let bounds = Bounds::centered(None, size(px(SIZE.0), px(SIZE.1)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..Default::default()
        };
        cx.open_window(options, |window, cx| {
            let view = cx.new(|_| Lists::new(chat));
            let driven = view.clone();
            window.on_next_frame(move |window, cx| drive(driven, window, cx));
            view
        })
        .expect("the window opens");
        cx.activate(true);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The script headless, a frame a step, on a clock that moves a frame
    /// a step. The scroll bar's drags must reach both ends, held at the end
    /// it must page, and its jumps must land where clicked.
    fn run(chat: bool) -> (Tally, usize) {
        let mut app = TestApp::new();
        let mut window = app.open_window_with_options(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(SIZE.0), px(SIZE.1)),
                ))),
                ..Default::default()
            },
            |_, _| Lists::new(chat),
        );
        window.draw();
        window.simulate_mouse_move(pointer());
        let mut tally = Tally::default();
        let (mut least, mut most, mut held) = (1f32, 0f32, (0, 0));
        for (i, (name, step)) in script(chat).into_iter().enumerate() {
            match window.update(|lists, _, _| lists.input(step)) {
                Some(PlatformInput::Scroll(e)) => window.simulate_event(e),
                Some(PlatformInput::MouseDown(e)) => window.simulate_event(e),
                Some(PlatformInput::MouseMove(e)) => window.simulate_event(e),
                Some(PlatformInput::MouseUp(e)) => window.simulate_event(e),
                Some(_) => unreachable!("the script's input"),
                None => {}
            }
            let now = FRAME * i as u32;
            window.update(|lists, _, cx| {
                if let Step::Jump(at) = step {
                    lists.jump(at);
                }
                lists.tick(now, cx);
                cx.notify();
            });
            window.draw();
            tally.count(&name, window.read(|lists, _| lists.blank()));
            let (at, rows, first) =
                window.read(|lists, _| (lists.scrolled(), lists.ids.len(), lists.ids[0]));
            match (name.as_str(), step) {
                ("thumb-drag", Step::Drag(_)) => (least, most) = (least.min(at), most.max(at)),
                ("thumb-drag", Step::Release) => {
                    assert!(
                        least < 0.02 && most > 0.98,
                        "a thumb drag showed {least}..{most}"
                    );
                    (least, most) = (1., 0.);
                }
                ("thumb-hold", Step::Grab) => held = (rows, first),
                ("thumb-hold", Step::Release) => assert!(
                    rows >= held.0 + 3 * PAGE || rows == ISSUES || first == 0,
                    "held at the end, paged from {held:?} to {:?}",
                    (rows, first)
                ),
                (_, Step::Press(to)) => assert!(
                    (at - to).abs() < 0.2,
                    "a jump to {to} of the track showed {at}"
                ),
                _ => {}
            }
        }
        let rows = window.read(|lists, _| lists.ids.len());
        (tally, rows)
    }

    #[core::prelude::v1::test]
    fn issues_cover_the_viewport_every_frame() {
        let (tally, rows) = run(false);
        println!("{}", tally.report("issues", rows));
        assert!(rows > 1000, "paged to {rows} rows");
        assert!(tally.blanks.is_empty(), "{}", tally.report("issues", rows));
    }

    #[core::prelude::v1::test]
    fn a_chat_covers_the_viewport_every_frame() {
        let (tally, rows) = run(true);
        println!("{}", tally.report("chat", rows));
        assert!(rows > 500, "paged back to {rows} rows");
        assert!(tally.blanks.is_empty(), "{}", tally.report("chat", rows));
    }
}
