//! The searchable pickers for users and labels: a button that opens a list
//! with a search field, ranked as zbugs' `fuzzySearch` ranks its combobox.

use gpui_kit::component::button::*;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::rc::Rc;

/// One entry of a picker: `value` is what picking it passes on, `None`
/// for the entry that clears the choice.
#[derive(Clone)]
pub struct Choice {
    pub label: String,
    pub value: Option<String>,
    pub checked: bool,
    /// The user's avatar URL, for a user.
    pub avatar: Option<String>,
}

/// What picking a choice does, with its value.
pub type OnPick = Rc<dyn Fn(Option<String>, &mut Window, &mut App)>;

/// `button` opens `choices` with a search field. A `multi` picker, such as
/// labels, stays open for more until a pick clears or the user leaves.
pub fn searchable(
    id: &'static str,
    button: Button,
    choices: Vec<Choice>,
    multi: bool,
    on_pick: OnPick,
) -> impl IntoElement {
    let choices = Rc::new(choices);
    Popover::new(id)
        .trigger(button.small().ghost())
        .p_1()
        .content(move |_, window, cx| {
            let popover = cx.entity().downgrade();
            let list = window.use_keyed_state(SharedString::from(format!("{id}-list")), cx, {
                |window, cx| ChoiceList::new(window, cx)
            });
            let (choices, on_pick) = (choices.clone(), on_pick.clone());
            list.update(cx, |list, _| {
                list.choices = choices;
                list.pick = Rc::new(move |value, window, cx| {
                    let close = !multi || value.is_none();
                    on_pick(value, window, cx);
                    if close {
                        let _ = popover.update(cx, |p, cx| p.dismiss(window, cx));
                    }
                });
            });
            list
        })
}

pub struct ChoiceList {
    query: Entity<InputState>,
    choices: Rc<Vec<Choice>>,
    pick: OnPick,
    /// The highlighted entry of the shown ones, which Enter picks.
    highlighted: usize,
    scroll: ScrollHandle,
    _search: Subscription,
}

impl ChoiceList {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let search = cx.subscribe_in(&query, window, |this, _, event, window, cx| match event {
            InputEvent::Change => {
                this.highlighted = 0;
                this.scroll.scroll_to_item(0);
                cx.notify();
            }
            InputEvent::PressEnter { .. } => {
                if let Some(choice) = this.shown(cx).get(this.highlighted) {
                    (this.pick.clone())(choice.value.clone(), window, cx);
                }
            }
            _ => {}
        });
        let focus = query.clone();
        window.defer(cx, move |window, cx| {
            focus.update(cx, |state, cx| state.focus(window, cx));
        });
        Self {
            query,
            choices: Rc::default(),
            pick: Rc::new(|_, _, _| {}),
            highlighted: 0,
            scroll: ScrollHandle::new(),
            _search: search,
        }
    }

    /// The choices the search finds, best first, or all of them.
    fn shown(&self, cx: &App) -> Vec<Choice> {
        let query = self.query.read(cx).value().trim().to_lowercase();
        if query.is_empty() {
            return self.choices.to_vec();
        }
        let mut ranked = self
            .choices
            .iter()
            .map(|c| (rank(&query, &c.label), c))
            .filter(|(rank, _)| *rank > 0.)
            .collect::<Vec<_>>();
        ranked.sort_by(|(ra, a), (rb, b)| {
            rb.total_cmp(ra)
                .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
        });
        ranked.into_iter().map(|(_, c)| c.clone()).collect()
    }
}

/// How well `text` matches `query`, 0 for not at all, as zbugs'
/// `fuzzySearch`: every query character in order, worth 10 at the start,
/// 1.1 right after another match, and 1 elsewhere.
fn rank(query: &str, text: &str) -> f64 {
    let mut query = query.chars().peekable();
    let (mut rank, mut weight) = (0., 10.);
    for c in text.to_lowercase().chars() {
        let Some(&q) = query.peek() else { break };
        if q == c {
            rank += weight;
            weight = 1.1;
            query.next();
        } else {
            weight = 1.;
        }
    }
    if query.peek().is_some() { 0. } else { rank }
}

impl Render for ChoiceList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let shown = self.shown(cx);
        let highlighted = self.highlighted.min(shown.len().saturating_sub(1));
        let rows = shown.into_iter().enumerate().map(|(ix, choice)| {
            let value = choice.value.clone();
            h_flex()
                .id(ix)
                .gap_2()
                .px_2()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                // One highlight for the pointer and the arrow keys.
                .when(ix == highlighted, |d| d.bg(cx.theme().accent))
                .on_mouse_move(cx.listener(move |this, _, _, cx| {
                    if this.highlighted != ix {
                        this.highlighted = ix;
                        cx.notify();
                    }
                }))
                .child(
                    div()
                        .w_3()
                        .flex_none()
                        .child(if choice.checked { "✓" } else { "" }),
                )
                .children(
                    choice
                        .avatar
                        .map(|url| img(url).flex_none().size_4().rounded_full()),
                )
                .child(choice.label)
                .on_click(cx.listener(move |this, _, window, cx| {
                    (this.pick.clone())(value.clone(), window, cx)
                }))
        });
        let rows = rows.collect::<Vec<_>>();
        let empty = rows.is_empty();
        v_flex()
            .w(px(240.))
            .gap_1()
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                let len = this.shown(cx).len();
                let step: isize = match event.keystroke.key.as_str() {
                    "down" => 1,
                    "up" => -1,
                    _ => return,
                };
                cx.stop_propagation();
                if len > 0 {
                    let at =
                        (this.highlighted.min(len - 1) as isize + step).clamp(0, len as isize - 1);
                    this.highlighted = at as usize;
                    this.scroll.scroll_to_item(this.highlighted);
                    cx.notify();
                }
            }))
            .child(Input::new(&self.query).small())
            .child(
                v_flex()
                    .id("choices")
                    .max_h(px(320.))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .text_sm()
                    .children(rows)
                    .when(empty, |d| {
                        d.child(
                            div()
                                .p_2()
                                .text_color(cx.theme().muted_foreground)
                                .child("No matches"),
                        )
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::rank;

    #[test]
    fn ranks_as_zbugs_fuzzy_search() {
        // In order, not necessarily together; out of order doesn't match.
        assert!(rank("bg", "bug") > 0.);
        assert_eq!(rank("gb", "bug"), 0.);
        // A match at the start beats one later on.
        assert!(rank("p", "perf") > rank("p", "zero-cache perf"));
        // Consecutive characters beat scattered ones.
        assert!(rank("ze", "zero") > rank("ze", "zone-cache"));
    }
}
