//! The screens: filters, the issue list, the selected issue, and the
//! sheets, as zbugs-swift lays them out on a Mac.

use std::borrow::Cow;
use std::sync::OnceLock;

use rune::{Cx, El, GrowthSpec, ScrollSpec, col, el, row, text};
use rune_kit::{
    Button, ButtonVariant, Checkbox, Dialog, Kit, Markdown, Menu, MenuItem, SegmentedControl,
    Select, TextArea, TextInput,
};
use zbugs_model::args::{SortDirection, SortField};
use zbugs_model::schema::{Comment, Issue, IssueVisibility, Label, User};
use zero_client::client::ConnectionState;
use zero_client::ivm::ResultType;

use crate::{App, Confirm, NOTIFICATIONS, Picker, REACTIONS, Sheet, Status, VISIBILITY};

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
        .child(sidebar(s, k, &mut layer).pt(cx.titlebar_inset()))
        .child(divider())
        .child(issue_list(s, k).pt(cx.titlebar_inset()))
        .child(divider())
        .child(detail(s, k, &mut layer).pt(cx.titlebar_inset()));
    sheets(s, k, &mut layer);
    rune_kit::root(body, layer)
}

fn label_text(k: &Kit, t: impl Into<String>) -> El {
    text(t).font(k.ty.small_medium).color(k.c.text_faint)
}

/// The label of a menu on the issue page, as wide as the others so the
/// menus line up.
fn field_label(k: &Kit, t: &str) -> El {
    label_text(k, t).w(96.0)
}

/// A dialog's explanation. `Dialog::description` doesn't wrap, so a long
/// one would widen the dialog past its panel.
fn dialog_text(k: &Kit, t: impl Into<String>) -> El {
    text(t).font(k.ty.ui.lh(20.0)).color(k.c.text_muted).wrap()
}

fn sidebar(s: &App, k: &Kit, layer: &mut Vec<El>) -> El {
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
    // Shortcuts for the creator and assignee filters, by the user's login,
    // so only once it has synced, as the web app's "Created by me" links.
    if let Some(me) = s.my_login() {
        let mine = |f: &Option<String>| f.as_deref() == Some(me);
        filters = filters
            .child(
                Checkbox::new(mine(&s.filter.creator))
                    .label("Created by Me")
                    .on_toggle(|s: &mut App| {
                        let me = s.my_login().map(str::to_owned);
                        let on = s.filter.creator != me;
                        s.set_creator(me.filter(|_| on));
                    })
                    .build(k)
                    .key("created-by-me"),
            )
            .child(
                Checkbox::new(mine(&s.filter.assignee))
                    .label("Assigned to Me")
                    .on_toggle(|s: &mut App| {
                        let me = s.my_login().map(str::to_owned);
                        let on = s.filter.assignee != me;
                        s.set_assignee(me.filter(|_| on));
                    })
                    .build(k)
                    .key("assigned-to-me"),
            );
    }
    filters = filters
        .child(label_text(k, "Creator"))
        .child(user_filter(
            s,
            k,
            layer,
            Picker::CreatorFilter,
            &s.creators,
            &s.filter.creator,
        ))
        .child(label_text(k, "Assignee"))
        .child(user_filter(
            s,
            k,
            layer,
            Picker::AssigneeFilter,
            &s.assignees,
            &s.filter.assignee,
        ))
        .child(label_text(k, "Labels"))
        .child(label_filter(s, k, layer));
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
        .child(project_picker(s, k, layer))
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

/// The project the list shows, as a menu when there's more than one.
fn project_picker(s: &App, k: &Kit, layer: &mut Vec<El>) -> El {
    let name = s.project().map_or("Zero Bugs", |p| p.name.as_str());
    if s.projects.len() < 2 {
        return text(name)
            .key("project")
            .font(k.ty.body_medium)
            .color(k.c.text_base);
    }
    let names: Vec<&str> = s.projects.iter().map(|p| p.name.as_str()).collect();
    Select::new("project", &names)
        .width(202.0)
        .current(Some(name))
        .open(s.picker == Some(Picker::Project))
        .on_toggle(|s: &mut App| s.toggle_picker(Picker::Project))
        .on_close(|s: &mut App| s.picker = None)
        .on_choose(|s: &mut App, name: &str, _| s.set_project(name))
        .build(k, layer)
}

/// Filters the list by who created the issues, or who they're assigned to.
fn user_filter(
    s: &App,
    k: &Kit,
    layer: &mut Vec<El>,
    picker: Picker,
    users: &[User],
    current: &Option<String>,
) -> El {
    const ANYONE: &str = "Anyone";
    let mut options = vec![ANYONE];
    options.extend(users.iter().map(|u| u.login.as_str()));
    // A login from before the users synced, or from the "me" shortcut.
    if let Some(c) = current.as_deref().filter(|c| !options.contains(c)) {
        options.push(c);
    }
    let creator = picker == Picker::CreatorFilter;
    let key = if creator {
        "creator"
    } else {
        "assignee-filter"
    };
    Select::new(key, &options)
        .width(202.0)
        .current(Some(current.as_deref().unwrap_or(ANYONE)))
        .open(s.picker.as_ref() == Some(&picker))
        .on_toggle(move |s: &mut App| s.toggle_picker(picker.clone()))
        .on_close(|s: &mut App| s.picker = None)
        .on_choose(move |s: &mut App, login: &str, _| {
            let login = (login != ANYONE).then(|| login.to_owned());
            if creator {
                s.set_creator(login);
            } else {
                s.set_assignee(login);
            }
        })
        .build(k, layer)
}

/// Filters the list by labels, checked when it does.
fn label_filter(s: &App, k: &Kit, layer: &mut Vec<El>) -> El {
    let open = s.picker == Some(Picker::LabelFilter);
    let title = match s.filter.labels.as_slice() {
        [] => "Any".to_owned(),
        names => names.join(", "),
    };
    let mut menu = Menu::new("label-filter")
        .trigger(
            Button::new(title)
                .variant(ButtonVariant::Outline)
                .expanded(open)
                .on_press(|s: &mut App| s.toggle_picker(Picker::LabelFilter))
                .build(k)
                .w_full(),
        )
        .min_width(202.0)
        .open(open)
        .on_close(|s: &mut App| s.picker = None);
    for l in &s.project_labels {
        let name = l.name.clone();
        menu = menu.item(
            MenuItem::new(l.name.clone())
                .checked(s.filter.labels.contains(&l.name))
                .on_select(move |s: &mut App| s.toggle_label_filter(&name)),
        );
    }
    menu.build(k, layer)
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
    let count = s.count.map_or(String::new(), |n| n.to_string());
    // Picking issues to delete, as the web list's select mode.
    let toolbar = if s.select_mode && s.auth.is_some() {
        let n = s.checked.len();
        row()
            .w_full()
            .center_y()
            .gap(8.0)
            .child(
                text(format!("{n} selected"))
                    .key("checked-count")
                    .font(k.ty.body_medium)
                    .color(k.c.text_base)
                    .grow(1.0),
            )
            .child(
                Button::new("Done")
                    .variant(ButtonVariant::Ghost)
                    .on_press(|s: &mut App| s.toggle_select_mode())
                    .build(k)
                    .key("done-selecting"),
            )
            .child(
                Button::new(if n == 0 {
                    "Delete".to_owned()
                } else {
                    format!("Delete ({n})")
                })
                .variant(ButtonVariant::Danger)
                .disabled(n == 0)
                .on_press(|s: &mut App| s.confirm = Some(Confirm::DeleteIssues(s.checked.clone())))
                .build(k)
                .key("delete-checked"),
            )
    } else {
        row()
            .w_full()
            .center_y()
            .gap(8.0)
            .child(
                text(format!("{} Issues", s.filter.status.title()))
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
                Button::new("Select")
                    .variant(ButtonVariant::Ghost)
                    .disabled(s.auth.is_none())
                    .on_press(|s: &mut App| s.toggle_select_mode())
                    .build(k)
                    .key("select-issues"),
            )
            .child(
                Button::new("New Issue")
                    .disabled(s.auth.is_none())
                    .on_press(|s: &mut App| s.open_sheet(Sheet::NewIssue))
                    .build(k)
                    .key("new-issue"),
            )
    };
    let header = col()
        .w_full()
        .px(12.0)
        .pb(10.0)
        .gap(8.0)
        .child(toolbar)
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
    let mut title = row().w_full().gap(8.0).center_y().child(
        text(issue.title.clone())
            .font(k.ty.ui_medium)
            .color(title_color)
            .truncate()
            .grow(1.0)
            .min_w(0.0),
    );
    // Changed since the user last looked, as the web app's list marks it.
    if s.is_unread(issue) {
        title = title.child(
            el().key("unread")
                .label("Unread")
                .w(6.0)
                .h(6.0)
                .radius(3.0)
                .fill(k.c.icon_accent),
        );
    }
    let text_col = col().min_w(0.0).gap(3.0).child(title).child(meta);
    if s.select_mode && s.auth.is_some() {
        // Pressing the row picks it, as the checkbox does; only what the
        // user can delete.
        let deletable = s.can_edit(&issue.creator_id);
        let checked = s.checked.contains(&issue.id);
        let toggle = id.clone();
        let mut r = row()
            .key(issue.id.clone())
            .role("button")
            .label(issue.title.clone())
            .w_full()
            .px(12.0)
            .py(9.0)
            .gap(10.0)
            .center_y()
            .on_press(move |s: &mut App| s.toggle_checked(&id))
            .child(
                Checkbox::new(checked)
                    .label(format!("Select {}", issue.title))
                    .hide_label()
                    .disabled(!deletable)
                    .on_toggle(move |s: &mut App| s.toggle_checked(&toggle))
                    .build(k)
                    .key("check"),
            )
            .child(text_col.grow(1.0));
        if !deletable {
            r = r.opacity(0.5);
        } else if checked {
            r = r.fill(k.c.layer_02);
        }
        return r;
    }
    let mut r = col()
        .key(issue.id.clone())
        .role("button")
        .label(issue.title.clone())
        .w_full()
        .px(12.0)
        .py(9.0)
        .on_press(move |s: &mut App| s.select(id.clone()))
        .child(text_col.w_full());
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

fn detail(s: &App, k: &Kit, layer: &mut Vec<El>) -> El {
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
    let editable = s.can_edit(&issue.creator_id);
    if let (Some(u), false) = (&issue.assignee, editable) {
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
    if editable {
        let id = issue.id.clone();
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
            )
            .child(
                Button::new("Delete")
                    .variant(ButtonVariant::Danger)
                    .on_press(move |s: &mut App| s.confirm = Some(Confirm::DeleteIssue(id.clone())))
                    .build(k)
                    .key("delete-issue"),
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
    if editable {
        column = column.child(assignee_picker(s, issue, k, layer));
    }
    // Visibility is the crew's, as on the web.
    if editable && s.is_crew() {
        let current = match issue.visibility {
            IssueVisibility::Public => VISIBILITY[0],
            IssueVisibility::Internal => VISIBILITY[1],
        };
        column = column.child(choice(
            k,
            layer,
            ("Visibility", "visibility"),
            &VISIBILITY,
            current,
            (s.picker == Some(Picker::Visibility), Picker::Visibility),
            |s, v| s.set_visibility(v),
        ));
    }
    if s.auth.is_some() {
        let current = NOTIFICATIONS[usize::from(!s.is_subscribed(issue))];
        column = column.child(choice(
            k,
            layer,
            ("Notifications", "notifications"),
            &NOTIFICATIONS,
            current,
            (
                s.picker == Some(Picker::Notifications),
                Picker::Notifications,
            ),
            |s, v| s.set_subscribed(v),
        ));
    }
    let issue_labels = issue.labels.as_deref().unwrap_or_default();
    // Labels are the crew's, as on the web.
    let label_menu = editable && s.is_crew();
    if !issue_labels.is_empty() || label_menu {
        let mut r = row().gap(8.0).center_y().child(labels(issue_labels, k));
        if label_menu {
            r = r.child(label_picker(s, issue_labels, k, layer));
        }
        column = column.child(r);
    }
    if !issue.description.is_empty() {
        column = column.child(
            Markdown::new(issue.description.clone())
                .build(k)
                .key("description"),
        );
    }
    column = column
        .child(reactions(s, &issue.id, k, layer))
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
        column = column.child(comment(s, c, k, layer));
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

fn comment(s: &App, c: &Comment, k: &Kit, layer: &mut Vec<El>) -> El {
    let editing = s.editing_comment.as_deref() == Some(c.id.as_str());
    let mut header = row()
        .w_full()
        .gap(8.0)
        .center_y()
        .child(
            text(c.creator.as_ref().map_or("…", |u| u.login.as_str()))
                .font(k.ty.small_medium)
                .color(k.c.text_base),
        )
        .child(text(ago(c.created)).font(k.ty.small).color(k.c.text_faint))
        .child(el().grow(1.0));
    if s.can_edit(&c.creator_id) && !editing {
        let (edit, delete) = (c.id.clone(), c.id.clone());
        header = header
            .child(
                Button::new("Edit")
                    .variant(ButtonVariant::GhostMuted)
                    .on_press(move |s: &mut App| s.edit_comment(&edit))
                    .build(k)
                    .key("edit-comment"),
            )
            .child(
                Button::new("Delete")
                    .variant(ButtonVariant::GhostMuted)
                    .on_press(move |s: &mut App| {
                        s.confirm = Some(Confirm::DeleteComment(delete.clone()))
                    })
                    .build(k)
                    .key("delete-comment"),
            );
    }
    let frame = col()
        .key(c.id.clone())
        .w_full()
        .padding(12.0)
        .gap(6.0)
        .radius(8.0)
        .fill(k.c.layer_01)
        .child(header);
    if !editing {
        return frame
            .child(Markdown::new(c.body.clone()).build(k))
            .child(reactions(s, &c.id, k, layer));
    }
    frame
        .child(
            TextArea::new(&s.comment_edit)
                .auto_grow(3, 12)
                .on_edit(|s: &mut App| &mut s.comment_edit)
                .on_submit(|s: &mut App, _| s.save_comment())
                .build(k)
                .w_full()
                .key("comment-edit-field"),
        )
        .child(
            row()
                .w_full()
                .gap(8.0)
                .child(el().grow(1.0))
                .child(
                    Button::new("Cancel")
                        .variant(ButtonVariant::Ghost)
                        .on_press(|s: &mut App| s.editing_comment = None)
                        .build(k)
                        .key("cancel-comment-edit"),
                )
                .child(
                    Button::new("Save")
                        .on_press(|s: &mut App| s.save_comment())
                        .build(k)
                        .key("save-comment-edit"),
                ),
        )
}

/// Who the issue is assigned to, for those who can change it.
fn assignee_picker(s: &App, issue: &Issue, k: &Kit, layer: &mut Vec<El>) -> El {
    const NOBODY: &str = "Nobody";
    let mut options = vec![NOBODY];
    options.extend(s.crew.iter().map(|u| u.login.as_str()));
    let current = issue.assignee.as_ref().map_or(NOBODY, |u| u.login.as_str());
    row()
        .gap(8.0)
        .center_y()
        .child(field_label(k, "Assignee"))
        .child(
            Select::new("assignee", &options)
                .width(200.0)
                .current(Some(current))
                .open(s.picker == Some(Picker::Assignee))
                .on_toggle(|s: &mut App| s.toggle_picker(Picker::Assignee))
                .on_close(|s: &mut App| s.picker = None)
                .on_choose(|s: &mut App, login: &str, _| s.assign(login))
                .build(k, layer),
        )
}

/// A labeled menu of fixed choices on the issue page.
fn choice(
    k: &Kit,
    layer: &mut Vec<El>,
    (title, key): (&str, &'static str),
    options: &[&str],
    current: &str,
    (open, picker): (bool, Picker),
    on_choose: fn(&mut App, &str),
) -> El {
    row()
        .gap(8.0)
        .center_y()
        .child(field_label(k, title))
        .child(
            Select::new(key, options)
                .width(200.0)
                .current(Some(current))
                .open(open)
                .on_toggle(move |s: &mut App| s.toggle_picker(picker.clone()))
                .on_close(|s: &mut App| s.picker = None)
                .on_choose(move |s: &mut App, v: &str, _| on_choose(s, v))
                .build(k, layer),
        )
}

/// The project's labels, checked when the issue has them.
fn label_picker(s: &App, issue_labels: &[Label], k: &Kit, layer: &mut Vec<El>) -> El {
    let open = s.picker == Some(Picker::Labels);
    let mut menu = Menu::new("labels")
        .trigger(
            Button::new("Labels…")
                .variant(ButtonVariant::Ghost)
                .expanded(open)
                .on_press(|s: &mut App| s.toggle_picker(Picker::Labels))
                .build(k),
        )
        .min_width(200.0)
        .open(open)
        .on_close(|s: &mut App| s.picker = None)
        .item(MenuItem::new("New Label…").on_select(|s: &mut App| s.open_sheet(Sheet::NewLabel)))
        .separator();
    for l in &s.labels {
        let id = l.id.clone();
        menu = menu.item(
            MenuItem::new(l.name.clone())
                .checked(issue_labels.iter().any(|x| x.id == l.id))
                .on_select(move |s: &mut App| s.toggle_label(&id)),
        );
    }
    menu.build(k, layer)
}

/// The reactions to an issue or a comment, one pill per emoji with its
/// count, and a menu to add one. Pressing a pill adds or takes back the
/// user's own.
fn reactions(s: &App, subject_id: &str, k: &Kit, layer: &mut Vec<El>) -> El {
    let me = s.auth.as_ref().map(|a| a.sub.as_str());
    // In the order each emoji first came.
    let mut groups: Vec<(&str, usize, bool)> = Vec::new();
    for e in s.reactions(subject_id) {
        let mine = Some(e.creator_id.as_str()) == me;
        match groups.iter_mut().find(|g| g.0 == e.value) {
            Some(g) => {
                g.1 += 1;
                g.2 |= mine;
            }
            None => groups.push((&e.value, 1, mine)),
        }
    }
    let mut r = row()
        .key(format!("reactions-{subject_id}"))
        .gap(6.0)
        .center_y();
    for (value, count, mine) in groups {
        let (subject, v) = (subject_id.to_owned(), value.to_owned());
        let mut pill = row()
            .key(format!("reaction-{value}"))
            .role("button")
            .label(format!("{value} {count}"))
            .gap(4.0)
            .px(8.0)
            .py(2.0)
            .radius(12.0)
            .center_y()
            .border(
                1.0,
                if mine {
                    k.c.border_focus
                } else {
                    k.c.border_muted
                },
            )
            .child(text(value.to_owned()).font(k.ty.ui))
            .child(
                text(count.to_string())
                    .font(k.ty.small_medium)
                    .color(k.c.text_muted),
            );
        if mine {
            pill = pill.fill(k.c.layer_02);
        }
        if me.is_some() {
            pill = pill.on_press(move |s: &mut App| s.toggle_reaction(&subject, &v));
        }
        r = r.child(pill);
    }
    if me.is_none() {
        return r;
    }
    let picker = Picker::Emoji(subject_id.to_owned());
    let open = s.picker.as_ref() == Some(&picker);
    let mut menu = Menu::new(format!("add-reaction-{subject_id}"))
        .trigger(
            Button::new("+ 🙂")
                .variant(ButtonVariant::Ghost)
                .expanded(open)
                .on_press(move |s: &mut App| s.toggle_picker(picker.clone()))
                .build(k)
                .key("add-reaction"),
        )
        .min_width(280.0)
        .open(open)
        .on_close(|s: &mut App| s.picker = None);
    for (value, name) in REACTIONS {
        let subject = subject_id.to_owned();
        menu = menu.item(
            MenuItem::new(format!("{value}  {name}"))
                .on_select(move |s: &mut App| s.toggle_reaction(&subject, value)),
        );
    }
    r.child(menu.build(k, layer))
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
        Some(Sheet::NewLabel) => {
            let mut fields = vec![
                TextInput::new(&s.label_edit)
                    .placeholder("Name")
                    .on_edit(|s: &mut App| &mut s.label_edit)
                    .on_submit(|s: &mut App, _| s.save_label())
                    .build(k)
                    .key("label-field"),
            ];
            fields.extend(error(s));
            fields.push(buttons("Create", App::save_label));
            Dialog::new("label-sheet", "New Label")
                .width(400.0)
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
    if let Some(confirm) = &s.confirm {
        let (title, message) = match confirm {
            Confirm::DeleteIssue(_) => (
                "Delete Issue",
                "Deleting the issue also deletes its comments. It can't be undone.",
            ),
            Confirm::DeleteIssues(ids) => (
                if ids.len() == 1 {
                    "Delete 1 Issue"
                } else {
                    "Delete Issues"
                },
                "Deleting issues also deletes their comments. It can't be undone.",
            ),
            Confirm::DeleteComment(_) => ("Delete Comment", "It can't be undone."),
        };
        Dialog::new("confirm", title)
            .width(400.0)
            .fit()
            .open(true)
            .on_close(|s: &mut App| s.confirm = None)
            .child(
                col()
                    .w_full()
                    .px(16.0)
                    .pb(16.0)
                    .gap(10.0)
                    .child(dialog_text(k, message))
                    .child(
                        row()
                            .w_full()
                            .gap(8.0)
                            .child(el().grow(1.0))
                            .child(
                                Button::new("Cancel")
                                    .variant(ButtonVariant::Ghost)
                                    .on_press(|s: &mut App| s.confirm = None)
                                    .build(k)
                                    .key("cancel"),
                            )
                            .child(
                                Button::new("Delete")
                                    .variant(ButtonVariant::Danger)
                                    .on_press(|s: &mut App| s.confirmed())
                                    .build(k)
                                    .key("confirm-delete"),
                            ),
                    ),
            )
            .build(k, layer);
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
