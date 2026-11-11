//! The reaction picker: every emoji by group, or the ones a search finds,
//! in the user's skin tone, as zbugs' emoji picker.

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::*;
use gpui_kit::*;
use std::rc::Rc;

const COLUMNS: usize = 9;
const CELL: f32 = 36.;

/// What the picker does with the emoji the user picks, and its name.
pub type OnPick = Rc<dyn Fn(&str, &str, &mut Window, &mut App)>;

enum Row {
    Group(&'static str),
    Emoji(Vec<&'static emojis::Emoji>),
}

pub struct EmojiPicker {
    pub query: Entity<InputState>,
    tone: Option<emojis::SkinTone>,
    rows: Rc<Vec<Row>>,
    on_pick: OnPick,
    scroll: UniformListScrollHandle,
    _search: Subscription,
}

impl EmojiPicker {
    /// `tone` as the `emojiSkinTone` pref keeps it: 0 for none, 1 to 5.
    pub fn new(tone: u32, on_pick: OnPick, window: &mut Window, cx: &mut Context<Self>) -> Self {
        use emojis::SkinTone::*;
        let tone = [Light, MediumLight, Medium, MediumDark, Dark]
            .get((tone as usize).wrapping_sub(1))
            .copied();
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search emoji"));
        let search = cx.subscribe_in(&query, window, |this, query, event, window, cx| {
            match event {
                InputEvent::Change => {
                    this.rows = Rc::new(rows(&query.read(cx).value()));
                    this.scroll.scroll_to_item(0, ScrollStrategy::Top);
                    cx.notify();
                }
                // Enter picks the first emoji the search found.
                InputEvent::PressEnter { .. } => {
                    let first = this.rows.iter().find_map(|row| match row {
                        Row::Emoji(row) => row.first().copied(),
                        Row::Group(_) => None,
                    });
                    if let Some(emoji) = first {
                        this.pick(emoji, window, cx);
                    }
                }
                _ => {}
            }
        });
        Self {
            query,
            tone,
            rows: Rc::new(rows("")),
            on_pick,
            scroll: UniformListScrollHandle::new(),
            _search: search,
        }
    }

    fn toned(&self, emoji: &'static emojis::Emoji) -> &'static emojis::Emoji {
        self.tone
            .and_then(|tone| emoji.with_skin_tone(tone))
            .unwrap_or(emoji)
    }

    fn pick(&self, emoji: &'static emojis::Emoji, window: &mut Window, cx: &mut App) {
        (self.on_pick)(self.toned(emoji).as_str(), emoji.name(), window, cx);
    }
}

/// The groups of every emoji, or, for a search, the emoji with a word in
/// their name or shortcodes that starts with each word of `query`.
fn rows(query: &str) -> Vec<Row> {
    // The emoji macOS 14 draws; Apple's font lags newer Unicode releases.
    let drawn = |e: &&emojis::Emoji| e.unicode_version() <= emojis::UnicodeVersion::new(15, 0);
    let query = query.trim().to_lowercase();
    let chunks = |found: Vec<&'static emojis::Emoji>| {
        found
            .chunks(COLUMNS)
            .map(|row| Row::Emoji(row.to_vec()))
            .collect::<Vec<_>>()
    };
    if query.is_empty() {
        return emojis::Group::iter()
            .flat_map(|group| {
                let found = group.emojis().filter(drawn).collect();
                std::iter::once(Row::Group(group_name(group))).chain(chunks(found))
            })
            .collect();
    }
    let found = emojis::iter()
        .filter(drawn)
        .filter(|e| {
            let words = e
                .name()
                .split([' ', '-', ':', ','])
                .chain(e.shortcodes().flat_map(|s| s.split(['_', '-'])))
                .map(str::to_lowercase)
                .collect::<Vec<_>>();
            query
                .split_whitespace()
                .all(|q| words.iter().any(|w| w.starts_with(q)))
        })
        .collect();
    chunks(found)
}

fn group_name(group: emojis::Group) -> &'static str {
    use emojis::Group::*;
    match group {
        SmileysAndEmotion => "Smileys and emotion",
        PeopleAndBody => "People and body",
        AnimalsAndNature => "Animals and nature",
        FoodAndDrink => "Food and drink",
        TravelAndPlaces => "Travel and places",
        Activities => "Activities",
        Objects => "Objects",
        Symbols => "Symbols",
        Flags => "Flags",
    }
}

impl Render for EmojiPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.rows.clone();
        let list = if rows.is_empty() {
            div()
                .h(px(CELL * 8.))
                .p_2()
                .text_color(cx.theme().muted_foreground)
                .child("No emoji found")
                .into_any_element()
        } else {
            uniform_list(
                "emoji",
                rows.len(),
                cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                    range
                        .map(|ix| match &rows[ix] {
                            Row::Group(name) => div()
                                .h(px(CELL))
                                .flex()
                                .items_end()
                                .pb_1()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(*name)
                                .into_any_element(),
                            Row::Emoji(row) => h_flex()
                                .h(px(CELL))
                                .children(row.iter().map(|&emoji| {
                                    let shown = this.toned(emoji);
                                    let name = SharedString::from(emoji.name());
                                    div()
                                        .id(SharedString::from(format!("emoji-{}", emoji.as_str())))
                                        .size(px(CELL))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded_md()
                                        .text_xl()
                                        .cursor_pointer()
                                        .hover(|s| s.bg(cx.theme().accent))
                                        .child(shown.as_str())
                                        .tooltip(move |window, cx| {
                                            Tooltip::new(name.clone()).build(window, cx)
                                        })
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.pick(emoji, window, cx)
                                        }))
                                }))
                                .into_any_element(),
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(&self.scroll)
            .h(px(CELL * 8.))
            .into_any_element()
        };
        v_flex()
            .gap_2()
            .w(px(CELL * COLUMNS as f32))
            .child(Input::new(&self.query))
            .child(list)
    }
}
