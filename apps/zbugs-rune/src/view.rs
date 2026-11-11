//! The screens: filters, the issue list, the selected issue, and the
//! sheets, as zbugs-swift lays them out on a Mac.

use std::borrow::Cow;
use std::sync::OnceLock;

use rune::{Cx, El, GrowthSpec, ScrollSpec, col, el, row, text};
use rune_kit::{
    Button, ButtonVariant, Checkbox, Dialog, Kit, Markdown, SegmentedControl, TextArea, TextInput,
};
use zbugs_model::args::{SortDirection, SortField};
use zbugs_model::schema::{Comment, Issue, IssueVisibility, Label};
use zero_client::client::ConnectionState;
use zero_client::ivm::ResultType;

use crate::{App, Sheet, Status};

/// The rows' estimated height, for the virtual list.
const ROW: f32 = 58.0;

fn kit(dark: bool) -> &'static Kit {
    static DARK: OnceLock<Kit> = OnceLock::new();
    static LIGHT: OnceLock<Kit> = OnceLock::new();
    if dark {
        DARK.get_or_init(|| Kit::neutral(true))
    } else {
        LIGHT.get_or_init(|| Kit::neutral(false))
    }
}

pub fn view(s: &App, cx: &mut Cx<App>) -> El {
    let k = kit(cx.system_dark());
    let mut layer = Vec::new();
    let divider = || el().w(1.0).h_full().fill(k.c.border_muted);
    let body = row()
        .key("columns")
        .w_full()
        .h_full()
        .fill(k.c.page)
        .child(sidebar(s, k).pt(cx.titlebar_inset()))
        .child(divider())
        .child(issue_list(s, k).pt(cx.titlebar_inset()))
        .child(divider())
        .child(detail(s, k).pt(cx.titlebar_inset()));
    sheets(s, k, &mut layer);
    rune_kit::root(body, layer)
}

fn label_text(k: &Kit, t: impl Into<String>) -> El {
    text(t).font(k.ty.small_medium).color(k.c.text_faint)
}

/// A dialog's explanation. `Dialog::description` doesn't wrap, so a long
/// one would widen the dialog past its panel.
fn dialog_text(k: &Kit, t: impl Into<String>) -> El {
    text(t).font(k.ty.ui.lh(20.0)).color(k.c.text_muted).wrap()
}

fn sidebar(s: &App, k: &Kit) -> El {
    let status = Status::ALL.iter().position(|x| *x == s.filter.status);
    let titles: Vec<&str> = Status::ALL.iter().map(|x| x.title()).collect();
    let mut filters = col()
        .w_full()
        .gap(8.0)
        .child(label_text(k, "Status"))
        .child(
            SegmentedControl::new(&titles, status)
                .on_pick(|s: &mut App, i, _| {
                    s.filter.status = Status::ALL[i];
                    s.list_changed();
                })
                .build(k)
                .w_full()
                .key("status"),
        )
        .child(label_text(k, "Sort"))
        .child(
            SegmentedControl::new(
                &["Modified", "Created"],
                Some(usize::from(s.filter.sort_field == SortField::Created)),
            )
            .on_pick(|s: &mut App, i, _| {
                s.filter.sort_field = if i == 0 {
                    SortField::Modified
                } else {
                    SortField::Created
                };
                s.list_changed();
            })
            .build(k)
            .w_full()
            .key("sort"),
        )
        .child(label_text(k, "Order"))
        .child(
            SegmentedControl::new(
                &["Newest", "Oldest"],
                Some(usize::from(s.filter.sort_direction == SortDirection::Asc)),
            )
            .on_pick(|s: &mut App, i, _| {
                s.filter.sort_direction = if i == 0 {
                    SortDirection::Desc
                } else {
                    SortDirection::Asc
                };
                s.list_changed();
            })
            .build(k)
            .w_full()
            .key("order"),
        );
    // Matched by login, so only once the user has synced.
    if s.me.is_some() {
        filters = filters
            .child(
                Checkbox::new(s.filter.created_by_me)
                    .label("Created by Me")
                    .on_toggle(|s: &mut App| {
                        s.filter.created_by_me = !s.filter.created_by_me;
                        s.list_changed();
                    })
                    .build(k)
                    .key("created-by-me"),
            )
            .child(
                Checkbox::new(s.filter.assigned_to_me)
                    .label("Assigned to Me")
                    .on_toggle(|s: &mut App| {
                        s.filter.assigned_to_me = !s.filter.assigned_to_me;
                        s.list_changed();
                    })
                    .build(k)
                    .key("assigned-to-me"),
            );
    }
    let account = match &s.auth {
        Some(auth) => col()
            .gap(6.0)
            .child(
                text(format!(
                    "{} ({})",
                    s.me.as_ref()
                        .map_or(auth.sub.as_str(), |u| u.login.as_str()),
                    auth.role.as_str()
                ))
                .font(k.ty.small)
                .color(k.c.text_muted)
                .truncate(),
            )
            .child(
                Button::new("Log Out")
                    .variant(ButtonVariant::Ghost)
                    .on_press(|s: &mut App| s.log_out())
                    .build(k)
                    .key("log-out"),
            ),
        None => col().child(
            Button::new("Log In…")
                .on_press(|s: &mut App| s.open_sheet(Sheet::Login))
                .build(k)
                .key("log-in"),
        ),
    };
    col()
        .key("sidebar")
        .w(230.0)
        .h_full()
        .px(14.0)
        .pb(14.0)
        .gap(16.0)
        .child(
            text("Zero Bugs")
                .font(k.ty.body_medium)
                .color(k.c.text_base),
        )
        .child(filters)
        .child(el().grow(1.0))
        .child(connection_badge(s, k))
        .child(account)
        .child(
            Button::new("Servers…")
                .variant(ButtonVariant::Ghost)
                .on_press(|s: &mut App| s.open_sheet(Sheet::Servers))
                .build(k)
                .key("servers"),
        )
}

fn connection_badge(s: &App, k: &Kit) -> El {
    let (color, label) = match &s.connection {
        ConnectionState::Connected => (k.c.success.icon, "Connected"),
        ConnectionState::Connecting { .. } | ConnectionState::Initializing => {
            (k.c.warning.icon, "Connecting")
        }
        ConnectionState::NeedsAuth(_) => (k.c.warning.icon, "Log In Again"),
        ConnectionState::Disconnected { .. } | ConnectionState::Closed { .. } => {
            (k.c.danger.icon, "Offline")
        }
        ConnectionState::Error { .. } => (k.c.danger.icon, "Error"),
    };
    let detail = match &s.connection {
        ConnectionState::Disconnected { reason } | ConnectionState::Error { reason } => {
            reason.clone()
        }
        _ => s.cache_url().to_owned(),
    };
    col()
        .key("connection")
        .role("button")
        .label(label)
        .gap(2.0)
        .on_press(|s: &mut App| s.reconnect_now())
        .child(
            row()
                .gap(6.0)
                .center_y()
                .child(el().size(8.0, 8.0).radius(4.0).fill(color))
                .child(text(label).font(k.ty.small_medium).color(k.c.text_base)),
        )
        .child(
            text(detail)
                .font(k.ty.small)
                .color(k.c.text_faint)
                .truncate(),
        )
}

fn issue_list(s: &App, k: &Kit) -> El {
    let title = format!("{} Issues", s.filter.status.title());
    let count = s.count.map_or(String::new(), |n| n.to_string());
    let header = col()
        .w_full()
        .px(12.0)
        .pb(10.0)
        .gap(8.0)
        .child(
            row()
                .w_full()
                .center_y()
                .gap(8.0)
                .child(
                    text(title)
                        .font(k.ty.body_medium)
                        .color(k.c.text_base)
                        .grow(1.0),
                )
                .child(
                    text(count)
                        .key("count")
                        .font(k.ty.small)
                        .color(k.c.text_muted),
                )
                .child(
                    Button::new("New Issue")
                        .disabled(s.auth.is_none())
                        .on_press(|s: &mut App| s.open_sheet(Sheet::NewIssue))
                        .build(k)
                        .key("new-issue"),
                ),
        )
        .child(
            TextInput::new(&s.search)
                .placeholder("Search issues")
                .clear_button()
                .on_edit(|s: &mut App| &mut s.search)
                .on_change(|s: &mut App, _| s.list_changed())
                .on_action(|s: &mut App, _| {
                    s.search = rune::TextEdit::new("");
                    s.list_changed();
                })
                .build(k)
                .key("search"),
        );
    let body = if s.issues.is_empty() {
        let message = if s.issues_result == ResultType::Complete {
            "No issues"
        } else {
            "Loading…"
        };
        col()
            .key("empty")
            .w_full()
            .grow(1.0)
            .center()
            .child(text(message).font(k.ty.ui).color(k.c.text_faint))
    } else {
        let kit = k.clone();
        col()
            .key("list-scroll")
            .w_full()
            .grow(1.0)
            .min_h(0.0)
            .scroll(ScrollSpec {
                stick_bottom: false,
            })
            .child(
                col()
                    .key("issues")
                    .w_full()
                    .growth(GrowthSpec::default())
                    .virtual_rows(
                        s.issues.len(),
                        ROW,
                        |s: &App, i| {
                            s.issues
                                .get(i)
                                .map_or(Cow::Borrowed(""), |x| Cow::Borrowed(x.id.as_str()))
                        },
                        move |s: &App, i| match s.issues.get(i) {
                            Some(issue) => issue_row(s, issue, &kit),
                            None => el(),
                        },
                    )
                    .on_visible_rows(|s: &mut App, range| s.list_visible(range.end)),
            )
    };
    col()
        .key("list")
        .w(380.0)
        .h_full()
        .child(header)
        .child(body)
}

fn labels(labels: &[Label], k: &Kit) -> El {
    labels.iter().fold(row().gap(4.0), |r, l| {
        r.child(
            text(l.name.clone())
                .font(k.ty.small)
                .color(k.c.text_muted)
                .px(6.0)
                .py(1.0)
                .radius(8.0)
                .fill(k.c.layer_03),
        )
    })
}

fn issue_row(s: &App, issue: &Issue, k: &Kit) -> El {
    let selected = s.selected.as_deref() == Some(issue.id.as_str());
    let id = issue.id.clone();
    let mut meta = row()
        .gap(6.0)
        .center_y()
        .child(text(short_id(issue)).font(k.ty.small).color(k.c.text_faint))
        .child(
            text(issue.creator.as_ref().map_or("", |u| u.login.as_str()))
                .font(k.ty.small)
                .color(k.c.text_muted),
        )
        .child(
            text(ago(issue.modified))
                .font(k.ty.small)
                .color(k.c.text_faint),
        );
    if let Some(l) = issue.labels.as_deref().filter(|l| !l.is_empty()) {
        meta = meta.child(labels(l, k));
    }
    let title_color = if issue.open {
        k.c.text_base
    } else {
        k.c.text_muted
    };
    let mut r = col()
        .key(issue.id.clone())
        .role("button")
        .label(issue.title.clone())
        .w_full()
        .px(12.0)
        .py(9.0)
        .gap(3.0)
        .on_press(move |s: &mut App| s.select(id.clone()))
        .child(
            text(issue.title.clone())
                .font(k.ty.ui_medium)
                .color(title_color)
                .truncate(),
        )
        .child(meta);
    if selected {
        r = r.fill(k.c.layer_02);
    }
    r
}

fn short_id(issue: &Issue) -> String {
    issue.short_id.map_or_else(String::new, |n| format!("#{n}"))
}

/// How long ago `ms` (since 1970) was, roughly.
fn ago(ms: f64) -> String {
    let secs = ((crate::now_ms() - ms) / 1000.0).max(0.0);
    match secs {
        s if s < 60.0 => "just now".into(),
        s if s < 3600.0 => format!("{}m ago", (s / 60.0) as i64),
        s if s < 86_400.0 => format!("{}h ago", (s / 3600.0) as i64),
        s if s < 86_400.0 * 60.0 => format!("{}d ago", (s / 86_400.0) as i64),
        s => format!("{}mo ago", (s / (86_400.0 * 30.0)) as i64),
    }
}

fn detail(s: &App, k: &Kit) -> El {
    let frame = col().key("detail").grow(1.0).h_full().min_w(0.0);
    let Some(issue) = &s.detail else {
        let message = match (&s.selected, s.detail_result) {
            (None, _) => "No issue selected",
            (Some(_), ResultType::Complete) => "Issue not found",
            _ => "Loading…",
        };
        return frame
            .center()
            .child(text(message).font(k.ty.ui).color(k.c.text_faint));
    };
    let mut status = row().gap(8.0).center_y().child(
        text(if issue.open { "Open" } else { "Closed" })
            .font(k.ty.small_medium)
            .color(if issue.open {
                k.c.success.fg
            } else {
                k.c.text_muted
            }),
    );
    if let Some(u) = &issue.creator {
        status = status.child(
            text(format!("by {}", u.login))
                .font(k.ty.small)
                .color(k.c.text_muted),
        );
    }
    if let Some(u) = &issue.assignee {
        status = status.child(
            text(format!("· assigned to {}", u.login))
                .font(k.ty.small)
                .color(k.c.text_muted),
        );
    }
    if issue.visibility == IssueVisibility::Internal {
        status = status.child(text("· Internal").font(k.ty.small).color(k.c.warning.fg));
    }
    let mut actions = row().gap(8.0);
    if s.can_edit(issue) {
        actions = actions
            .child(
                Button::new(if issue.open { "Close Issue" } else { "Reopen" })
                    .variant(ButtonVariant::Outline)
                    .on_press(|s: &mut App| s.toggle_open())
                    .build(k)
                    .key("toggle-open"),
            )
            .child(
                Button::new("Edit")
                    .variant(ButtonVariant::Ghost)
                    .on_press(|s: &mut App| s.open_sheet(Sheet::EditIssue))
                    .build(k)
                    .key("edit"),
            );
    }
    let mut column = col()
        .key("issue-column")
        .w_full()
        .max_w(760.0)
        .px(24.0)
        .py(16.0)
        .gap(14.0)
        .child(
            row()
                .w_full()
                .center_y()
                .gap(12.0)
                .child(text(short_id(issue)).font(k.ty.ui).color(k.c.text_faint))
                .child(el().grow(1.0))
                .child(actions),
        )
        .child(
            text(issue.title.clone())
                .key("issue-title")
                .font(k.ty.body_medium.lh(26.0))
                .color(k.c.text_base)
                .wrap()
                .selectable(true),
        )
        .child(status);
    if let Some(l) = issue.labels.as_deref().filter(|l| !l.is_empty()) {
        column = column.child(labels(l, k));
    }
    if !issue.description.is_empty() {
        column = column.child(
            Markdown::new(issue.description.clone())
                .build(k)
                .key("description"),
        );
    }
    column = column
        .child(el().w_full().h(1.0).fill(k.c.border_muted))
        .child(
            text(match s.comments.len() {
                0 => "No comments".to_owned(),
                1 => "1 comment".to_owned(),
                n => format!("{n} comments"),
            })
            .font(k.ty.small_medium)
            .color(k.c.text_faint),
        );
    for c in &s.comments {
        column = column.child(comment(c, k));
    }
    column = column.child(composer(s, k));
    frame.child(
        col()
            .key("detail-scroll")
            .w_full()
            .grow(1.0)
            .min_h(0.0)
            .scroll(ScrollSpec {
                stick_bottom: false,
            })
            .on_at_bottom(|s: &mut App, bottom| {
                if bottom {
                    s.more_comments();
                }
            })
            .child(col().w_full().items_center().child(column)),
    )
}

fn comment(c: &Comment, k: &Kit) -> El {
    col()
        .key(c.id.clone())
        .w_full()
        .padding(12.0)
        .gap(6.0)
        .radius(8.0)
        .fill(k.c.layer_01)
        .child(
            row()
                .gap(8.0)
                .child(
                    text(c.creator.as_ref().map_or("…", |u| u.login.as_str()))
                        .font(k.ty.small_medium)
                        .color(k.c.text_base),
                )
                .child(text(ago(c.created)).font(k.ty.small).color(k.c.text_faint)),
        )
        .child(Markdown::new(c.body.clone()).build(k))
}

fn composer(s: &App, k: &Kit) -> El {
    let empty = s.comment.text.trim().is_empty();
    col()
        .key("composer")
        .w_full()
        .gap(8.0)
        .items_end()
        .child(
            TextArea::new(&s.comment)
                .placeholder(if s.auth.is_some() {
                    "Add a comment"
                } else {
                    "Log in to comment"
                })
                .auto_grow(3, 8)
                .disabled(s.auth.is_none())
                .on_edit(|s: &mut App| &mut s.comment)
                .build(k)
                .w_full()
                .key("comment-field"),
        )
        .child(
            Button::new("Comment")
                .disabled(s.auth.is_none() || empty)
                .on_press(|s: &mut App| s.add_comment())
                .build(k)
                .key("comment-button"),
        )
}

fn sheets(s: &App, k: &Kit, layer: &mut Vec<El>) {
    let close = |s: &mut App| s.close_sheet();
    let error = |s: &App| {
        s.sheet_error
            .as_ref()
            .map(|e| text(e.clone()).font(k.ty.small).color(k.c.danger.fg).wrap())
    };
    let buttons = |save: &str, on_save: fn(&mut App)| {
        row()
            .w_full()
            .gap(8.0)
            .child(el().grow(1.0))
            .child(
                Button::new("Cancel")
                    .variant(ButtonVariant::Ghost)
                    .on_press(close)
                    .build(k)
                    .key("cancel"),
            )
            .child(Button::new(save).on_press(on_save).build(k).key("save"))
    };
    let form = |children: Vec<El>| {
        col()
            .w_full()
            .px(16.0)
            .pb(16.0)
            .gap(10.0)
            .children(children)
    };
    match s.sheet {
        Some(sheet @ (Sheet::NewIssue | Sheet::EditIssue)) => {
            let title = if sheet == Sheet::NewIssue {
                "New Issue"
            } else {
                "Edit Issue"
            };
            let mut fields = vec![
                TextInput::new(&s.title_edit)
                    .placeholder("Title")
                    .on_edit(|s: &mut App| &mut s.title_edit)
                    .on_submit(|s: &mut App, _| s.save_issue())
                    .build(k)
                    .key("title-field"),
                TextArea::new(&s.description_edit)
                    .placeholder("Description (Markdown)")
                    .auto_grow(6, 14)
                    .on_edit(|s: &mut App| &mut s.description_edit)
                    .build(k)
                    .key("description-field"),
            ];
            fields.extend(error(s));
            fields.push(buttons(
                if sheet == Sheet::NewIssue {
                    "Create"
                } else {
                    "Save"
                },
                App::save_issue,
            ));
            Dialog::new("issue-sheet", title)
                .width(560.0)
                .fit()
                .open(true)
                .on_close(close)
                .child(form(fields))
                .build(k, layer);
        }
        Some(Sheet::Login) => {
            let waiting = s.github_login.is_some();
            let mut fields = vec![
                dialog_text(
                    k,
                    "Through GitHub in the browser, or with a token, such as the one the backend prints.",
                ),
                Button::new(if waiting {
                    "Waiting for the browser…"
                } else {
                    "Log In with GitHub"
                })
                .disabled(waiting)
                .on_press_tx(|s: &mut App, tx| s.log_in_with_github(tx))
                .build(k)
                .w_full()
                .key("github"),
                label_text(k, "Or paste a token"),
                TextInput::new(&s.token_edit)
                    .placeholder("eyJ…")
                    .secure()
                    .on_edit(|s: &mut App| &mut s.token_edit)
                    .on_submit(|s: &mut App, _| s.log_in())
                    .build(k)
                    .key("token-field"),
            ];
            fields.extend(error(s));
            fields.push(buttons("Log In", App::log_in));
            Dialog::new("login-sheet", "Log In")
                .width(460.0)
                .fit()
                .open(true)
                .on_close(close)
                .child(form(fields))
                .build(k, layer);
        }
        Some(Sheet::Servers) => {
            let mut fields = vec![
                label_text(k, "zero-cache"),
                TextInput::new(&s.cache_edit)
                    .on_edit(|s: &mut App| &mut s.cache_edit)
                    .build(k)
                    .key("cache-field"),
                label_text(k, "API server"),
                TextInput::new(&s.api_edit)
                    .on_edit(|s: &mut App| &mut s.api_edit)
                    .on_submit(|s: &mut App, _| s.save_servers())
                    .build(k)
                    .key("api-field"),
            ];
            fields.extend(error(s));
            fields.push(buttons("Save", App::save_servers));
            Dialog::new("servers-sheet", "Servers")
                .width(460.0)
                .fit()
                .open(true)
                .on_close(close)
                .child(form(fields))
                .build(k, layer);
        }
        None => {}
    }
    if let Some(message) = &s.error {
        Dialog::new("error", "Couldn't Save")
            .width(400.0)
            .fit()
            .open(true)
            .on_close(|s: &mut App| s.error = None)
            .child(
                col()
                    .w_full()
                    .px(16.0)
                    .pb(16.0)
                    .gap(10.0)
                    .child(dialog_text(k, message.clone()))
                    .child(
                        row().w_full().child(el().grow(1.0)).child(
                            Button::new("OK")
                                .on_press(|s: &mut App| s.error = None)
                                .build(k)
                                .key("ok"),
                        ),
                    ),
            )
            .build(k, layer);
    }
}
