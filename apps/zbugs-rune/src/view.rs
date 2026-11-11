//! The screens: filters, the issue list, the selected issue, and the
//! sheets, as zbugs-swift lays them out on a Mac.

use std::borrow::Cow;
use std::sync::OnceLock;

use rune::{Cx, El, GrowthSpec, ScrollSpec, Tx, col, el, row, text};
use rune_kit::{
    Button, ButtonVariant, Checkbox, Combobox, ComboboxOption, ComboboxState, Dialog, IconButton,
    IconButtonVariant, Kit, Markdown, Menu, MenuItem, Placement, Popover, SegmentedControl, Select,
    Switch, TextArea, TextInput,
};
use zbugs_model::args::{SortDirection, SortField};
use zbugs_model::schema::{Comment, Emoji, Issue, IssueVisibility, Label, User};
use zero_client::client::ConnectionState;
use zero_client::ivm::ResultType;

use crate::{App, Confirm, NOTIFICATIONS, Picker, Sheet, Status, VISIBILITY};
use crate::{icons, images};

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

/// The titlebar's height: each column's header shares the band, as the
/// toolbar of zbugs-swift's window does.
pub const BAR: f32 = 52.0;

/// Closed issues' colour, as zbugs-swift's purple.
fn closed_color() -> rune::Color {
    rune::oklch(0.62, 0.19, 300.0)
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
        .child(sidebar(s, k, &mut layer, cx.chrome_insets().leading))
        .child(divider())
        .child(issue_list(s, k, &mut layer))
        .child(divider())
        .child(detail(s, k, &mut layer));
    sheets(s, k, &mut layer);
    rune_kit::root(body, layer)
}

/// A column's header in the titlebar's band: dragging it moves the window.
fn band(key: &str) -> El {
    row()
        .key(key)
        .w_full()
        .h(BAR)
        .px(12.0)
        .gap(6.0)
        .center_y()
        .window_drag()
}

fn hairline(k: &Kit) -> El {
    el().w_full().h(1.0).fill(k.c.border_muted)
}

/// Markdown with its images, as the web app shows them.
fn markdown(s: &App, body: &str) -> Markdown {
    let images = s.images.clone();
    Markdown::new(body.to_owned()).images(move |src| images.get(src))
}

/// The user's avatar, a circle `size` across; blank until it arrives.
fn avatar(s: &App, user: Option<&User>, size: f32, k: &Kit) -> El {
    let circle = el().w(size).h(size).radius(size / 2.0).a11y_hidden();
    let image = user.and_then(|u| s.images.get(&images::avatar_url(&u.avatar)));
    match image {
        Some(image) => circle.pixels(image.pixels()),
        None => circle.fill(k.c.layer_03),
    }
}

fn label_text(k: &Kit, t: impl Into<String>) -> El {
    text(t).font(k.ty.small_medium).color(k.c.text_faint)
}

/// The label of a menu on the issue page, before its value.
fn field_label(k: &Kit, t: &str) -> El {
    label_text(k, t)
}

/// A dialog's explanation. `Dialog::description` doesn't wrap, so a long
/// one would widen the dialog past its panel.
fn dialog_text(k: &Kit, t: impl Into<String>) -> El {
    text(t).font(k.ty.ui.lh(20.0)).color(k.c.text_muted).wrap()
}

/// The filters, as zbugs-swift's sidebar: one card of labeled rows, the
/// label on the left and its value on the right.
fn sidebar(s: &App, k: &Kit, layer: &mut Vec<El>, leading: f32) -> El {
    let status = s.filter.status.title();
    let sort = if s.filter.sort_field == SortField::Created {
        SORTS[1]
    } else {
        SORTS[0]
    };
    let order = ORDERS[usize::from(s.filter.sort_direction == SortDirection::Asc)];
    let titles: Vec<&str> = Status::ALL.iter().map(|x| x.title()).collect();
    let mut rows = vec![
        filter_row(
            k,
            "Status",
            pick(
                s,
                k,
                layer,
                ("status", Picker::Status),
                &titles,
                status,
                |s, v| {
                    if let Some(x) = Status::ALL.iter().find(|x| x.title() == v) {
                        s.filter.status = *x;
                        s.list_changed();
                    }
                },
            ),
        ),
        filter_row(
            k,
            "Sort",
            pick(s, k, layer, ("sort", Picker::Sort), &SORTS, sort, |s, v| {
                s.filter.sort_field = if v == SORTS[1] {
                    SortField::Created
                } else {
                    SortField::Modified
                };
                s.list_changed();
            }),
        ),
        filter_row(
            k,
            "Order",
            pick(
                s,
                k,
                layer,
                ("order", Picker::Order),
                &ORDERS,
                order,
                |s, v| {
                    s.filter.sort_direction = if v == ORDERS[1] {
                        SortDirection::Asc
                    } else {
                        SortDirection::Desc
                    };
                    s.list_changed();
                },
            ),
        ),
    ];
    // Shortcuts for the creator and assignee filters, by the user's login,
    // so only once it has synced, as the web app's "Created by me" links.
    if let Some(me) = s.my_login() {
        let mine = |f: &Option<String>| f.as_deref() == Some(me);
        rows.push(filter_row(
            k,
            "Created by Me",
            Switch::new(mine(&s.filter.creator))
                .label("Created by Me")
                .hide_label()
                .on_toggle(|s: &mut App| {
                    let me = s.my_login().map(str::to_owned);
                    let on = s.filter.creator != me;
                    s.set_creator(me.filter(|_| on));
                })
                .build(k)
                .key("created-by-me"),
        ));
        rows.push(filter_row(
            k,
            "Assigned to Me",
            Switch::new(mine(&s.filter.assignee))
                .label("Assigned to Me")
                .hide_label()
                .on_toggle(|s: &mut App| {
                    let me = s.my_login().map(str::to_owned);
                    let on = s.filter.assignee != me;
                    s.set_assignee(me.filter(|_| on));
                })
                .build(k)
                .key("assigned-to-me"),
        ));
    }
    rows.push(filter_row(
        k,
        "Creator",
        user_filter(s, k, layer, true, &s.creators, &s.filter.creator),
    ));
    rows.push(filter_row(
        k,
        "Assignee",
        user_filter(s, k, layer, false, &s.assignees, &s.filter.assignee),
    ));
    rows.push(filter_row(k, "Labels", label_filter(s, k, layer)));
    rows.push(filter_row(k, "Project", project_picker(s, k, layer)));
    let mut card = col()
        .key("filters")
        .w_full()
        .radius(10.0)
        .fill(k.c.layer_01)
        .border(1.0, k.c.border_muted);
    for (i, r) in rows.into_iter().enumerate() {
        if i > 0 {
            card = card.child(
                row()
                    .w_full()
                    .pl(12.0)
                    .child(el().grow(1.0).h(1.0).fill(k.c.border_muted)),
            );
        }
        card = card.child(r);
    }
    col()
        .key("sidebar")
        .w(250.0)
        .h_full()
        .child(band("sidebar-bar").pl(leading.max(12.0)))
        .child(col().w_full().px(10.0).child(card))
}

const SORTS: [&str; 2] = ["Modified", "Created"];
const ORDERS: [&str; 2] = ["Newest First", "Oldest First"];

/// One of the sidebar's rows: `label` on the left, its control on the right.
fn filter_row(k: &Kit, label: &str, control: El) -> El {
    row()
        .w_full()
        .h(36.0)
        .px(12.0)
        .gap(8.0)
        .center_y()
        .child(
            text(label)
                .font(k.ty.ui)
                .color(k.c.text_base)
                .truncate()
                .grow(1.0)
                .min_w(0.0),
        )
        .child(control)
}

/// A menu of fixed choices, borderless as a Mac's pop-up button in a form.
fn pick(
    s: &App,
    k: &Kit,
    layer: &mut Vec<El>,
    (key, picker): (&'static str, Picker),
    options: &[&str],
    current: &str,
    on_choose: fn(&mut App, &str),
) -> El {
    Select::new(key, options)
        .current(Some(current))
        .open(s.picker.as_ref() == Some(&picker))
        .on_toggle(move |s: &mut App| s.toggle_picker(picker.clone()))
        .on_close(|s: &mut App| s.picker = None)
        .on_choose(move |s: &mut App, v: &str, _| {
            s.picker = None;
            on_choose(s, v);
        })
        .build(k, layer)
}

/// The project the list shows, as a menu when there's more than one.
fn project_picker(s: &App, k: &Kit, layer: &mut Vec<El>) -> El {
    let name = s.project().map_or("Zero Bugs", |p| p.name.as_str());
    if s.projects.len() < 2 {
        return text(name)
            .key("project")
            .font(k.ty.ui)
            .color(k.c.text_muted);
    }
    let names: Vec<&str> = s.projects.iter().map(|p| p.name.as_str()).collect();
    Select::new("project", &names)
        .current(Some(name))
        .open(s.picker == Some(Picker::Project))
        .on_toggle(|s: &mut App| s.toggle_picker(Picker::Project))
        .on_close(|s: &mut App| s.picker = None)
        .on_choose(|s: &mut App, name: &str, _| s.set_project(name))
        .build(k, layer)
}

/// Filters the list by who created the issues (`creator`), or who they're
/// assigned to, searchable as the web app's user picker.
fn user_filter(
    s: &App,
    k: &Kit,
    layer: &mut Vec<El>,
    creator: bool,
    users: &[User],
    current: &Option<String>,
) -> El {
    const ANYONE: &str = "Anyone";
    let mut logins = vec![ANYONE];
    logins.extend(users.iter().map(|u| u.login.as_str()));
    // A login from before the users synced, or from the "me" shortcut.
    if let Some(c) = current.as_deref().filter(|c| !logins.contains(c)) {
        logins.push(c);
    }
    let (key, state, access): (_, _, Combo) = if creator {
        ("creator", &s.creator_combo, |s| &mut s.creator_combo)
    } else {
        ("assignee-filter", &s.assignee_filter_combo, |s| {
            &mut s.assignee_filter_combo
        })
    };
    let current = current.as_deref().unwrap_or(ANYONE);
    people(k, (key, state, access), &logins, current, 120.0)
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

/// Where a people picker keeps its state in the app.
type Combo = for<'a> fn(&'a mut App) -> &'a mut ComboboxState;

/// A searchable picker of `logins`, its trigger `width` points wide and
/// showing `current` as the other dropdowns do.
fn people(
    k: &Kit,
    (key, state, access): (&str, &ComboboxState, Combo),
    logins: &[&str],
    current: &str,
    width: f32,
) -> Combobox<App> {
    let options: Vec<ComboboxOption> = logins.iter().map(|l| ComboboxOption::new(*l, *l)).collect();
    let trigger = Select::new(format!("{key}-value"), &[] as &[&str])
        .width(width)
        .current(Some(current))
        .open(state.is_open())
        .trigger(k);
    Combobox::new(key, state, &options, access)
        .current(Some(current))
        .search_placeholder("Search people")
        .trigger(trigger)
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
            Select::new("label-filter-value", &[] as &[&str])
                .label("Labels")
                .current(Some(&title))
                .open(open)
                .on_toggle(|s: &mut App| s.toggle_picker(Picker::LabelFilter))
                .trigger(k),
        )
        .min_width(180.0)
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

/// The connection's state, as the dot and word in zbugs-swift's toolbar.
/// Pressing it reconnects now rather than at the next retry.
fn connection(s: &App, k: &Kit) -> El {
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
    row()
        .key("connection")
        .role("button")
        .label(label)
        .help(detail)
        .gap(5.0)
        .px(4.0)
        .center_y()
        .on_press(|s: &mut App| s.reconnect_now())
        .child(el().size(8.0, 8.0).radius(4.0).fill(color))
        .child(text(label).font(k.ty.small_medium).color(k.c.text_muted))
}

/// Who is logged in, logging out or in, and the servers, as zbugs-swift's
/// account menu.
fn account_menu(s: &App, k: &Kit, layer: &mut Vec<El>) -> El {
    let open = s.picker == Some(Picker::Account);
    let mut menu = Menu::new("account")
        .trigger(
            IconButton::new(&icons::PERSON)
                .variant(IconButtonVariant::Ghost)
                .label("Account")
                .help("Account")
                .expanded(open)
                .on_press(|s: &mut App| s.toggle_picker(Picker::Account))
                .build(k)
                .key("account-trigger"),
        )
        .placement(Placement::BottomEnd)
        .min_width(280.0)
        .open(open)
        .on_close(|s: &mut App| s.picker = None);
    menu = match &s.auth {
        Some(auth) => menu
            .item(
                MenuItem::new(format!(
                    "Logged in as {} ({})",
                    s.me.as_ref()
                        .map_or(auth.sub.as_str(), |u| u.login.as_str()),
                    auth.role.as_str()
                ))
                .lead(avatar(s, s.me.as_ref(), 16.0, k))
                .disabled(true),
            )
            .item(MenuItem::new("Log Out").on_select(|s: &mut App| {
                s.picker = None;
                s.log_out();
            })),
        None => menu.item(MenuItem::new("Log In…").on_select(|s: &mut App| {
            s.picker = None;
            s.open_sheet(Sheet::Login);
        })),
    };
    menu.separator()
        .item(MenuItem::new("Servers…").on_select(|s: &mut App| {
            s.picker = None;
            s.open_sheet(Sheet::Servers);
        }))
        .build(k, layer)
}

fn issue_list(s: &App, k: &Kit, layer: &mut Vec<El>) -> El {
    let count = s.count.map_or(String::new(), |n| n.to_string());
    // Picking issues to delete, as the web list's select mode.
    let header = if s.select_mode && s.auth.is_some() {
        let n = s.checked.len();
        band("list-bar")
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
        band("list-bar")
            .child(
                text(format!("{} Issues", s.filter.status.title()))
                    .key("list-title")
                    .font(k.ty.body_medium)
                    .color(k.c.text_base)
                    .truncate()
                    .grow(1.0)
                    .min_w(0.0),
            )
            .child(
                text(count)
                    .key("count")
                    .font(k.ty.small)
                    .color(k.c.text_muted),
            )
            .child(connection(s, k))
            .child(
                IconButton::new(&icons::SELECT)
                    .variant(IconButtonVariant::Ghost)
                    .help("Select Issues")
                    .disabled(s.auth.is_none())
                    .on_press(|s: &mut App| s.toggle_select_mode())
                    .build(k)
                    .key("select-issues"),
            )
            .child(
                IconButton::new(&icons::COMPOSE)
                    .variant(IconButtonVariant::Ghost)
                    .help("New Issue")
                    .disabled(s.auth.is_none())
                    .on_press(|s: &mut App| s.open_sheet(Sheet::NewIssue))
                    .build(k)
                    .key("new-issue"),
            )
            .child(account_menu(s, k, layer))
    };
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
        .child(hairline(k))
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
    // "#3005 · arv · 2 years ago", then the labels, as zbugs-swift's rows.
    let subtitle = [
        short_id(issue),
        issue
            .creator
            .as_ref()
            .map_or_else(String::new, |u| u.login.clone()),
        ago(issue.modified),
    ]
    .into_iter()
    .filter(|x| !x.is_empty())
    .collect::<Vec<_>>()
    .join(" · ");
    let mut meta = row()
        .gap(6.0)
        .center_y()
        .child(text(subtitle).font(k.ty.small).color(k.c.text_faint));
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
    let text_col = row()
        .min_w(0.0)
        .gap(8.0)
        .child(status_icon(issue.open, 14.0, k).mt(2.0))
        .child(col().min_w(0.0).grow(1.0).gap(3.0).child(title).child(meta));
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

/// An open issue's circle or a closed one's check, as zbugs-swift's.
fn status_icon(open: bool, size: f32, k: &Kit) -> El {
    if open {
        rune::icon(&icons::CIRCLE, size).color(k.c.success.icon)
    } else {
        rune::icon(&icons::CHECK_CIRCLE_FILL, size).color(closed_color())
    }
    .a11y_hidden()
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
    // The issue's actions as icons and the search, in the band as
    // zbugs-swift's toolbar has them.
    let mut bar = band("detail-bar");
    if let Some(issue) = s.detail.as_ref().filter(|i| s.can_edit(&i.creator_id)) {
        let id = issue.id.clone();
        let (icon, help) = if issue.open {
            (&icons::CHECK_CIRCLE, "Close Issue")
        } else {
            (&icons::REOPEN, "Reopen Issue")
        };
        bar = bar
            .child(
                IconButton::new(icon)
                    .variant(IconButtonVariant::Ghost)
                    .help(help)
                    .on_press(|s: &mut App| s.toggle_open())
                    .build(k)
                    .key("toggle-open"),
            )
            .child(
                IconButton::new(&icons::PENCIL)
                    .variant(IconButtonVariant::Ghost)
                    .help("Edit Issue")
                    .on_press(|s: &mut App| s.open_sheet(Sheet::EditIssue))
                    .build(k)
                    .key("edit"),
            )
            .child(
                IconButton::new(&icons::TRASH)
                    .variant(IconButtonVariant::Ghost)
                    .help("Delete Issue")
                    .on_press(move |s: &mut App| s.confirm = Some(Confirm::DeleteIssue(id.clone())))
                    .build(k)
                    .key("delete-issue"),
            );
    }
    bar = bar.child(el().grow(1.0)).child(
        TextInput::new(&s.search)
            .placeholder("Search issues")
            .width(Some(240.0))
            .clear_button()
            .on_edit(|s: &mut App| &mut s.search)
            .on_change(|s: &mut App, _| s.list_changed())
            .on_action(|s: &mut App, _| {
                s.search = rune::TextEdit::new("");
                s.list_changed();
            })
            // Escape clears the search, as on the web.
            .on_key(|s: &mut App, event, _| {
                if !matches!(event.key, rune::Key::Escape) || s.search.text.is_empty() {
                    return false;
                }
                s.search = rune::TextEdit::new("");
                s.list_changed();
                true
            })
            .build(k)
            .key("search"),
    );
    let frame = col()
        .key("detail")
        .grow(1.0)
        .h_full()
        .min_w(0.0)
        .child(bar)
        .child(hairline(k));
    let Some(issue) = &s.detail else {
        let message = match (&s.selected, s.detail_result) {
            (None, _) => "No issue selected",
            (Some(_), ResultType::Complete) => "Issue not found",
            _ => "Loading…",
        };
        return frame.child(
            col()
                .w_full()
                .grow(1.0)
                .center()
                .child(text(message).font(k.ty.ui).color(k.c.text_faint)),
        );
    };
    let mut status = row()
        .key("issue-status")
        .gap(6.0)
        .center_y()
        .child(status_icon(issue.open, 13.0, k))
        .child(
            text(if issue.open { "Open" } else { "Closed" })
                .font(k.ty.small_medium)
                .color(if issue.open {
                    k.c.success.fg
                } else {
                    closed_color()
                }),
        );
    if let Some(u) = &issue.creator {
        status = status
            .child(text("by").font(k.ty.small).color(k.c.text_muted))
            .child(avatar(s, Some(u), 16.0, k))
            .child(text(u.login.clone()).font(k.ty.small).color(k.c.text_muted));
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
    let mut column = col()
        .key("issue-column")
        .w_full()
        .max_w(760.0)
        .px(24.0)
        .py(16.0)
        .gap(12.0)
        .child(
            text(issue.title.clone())
                .key("issue-title")
                .font(k.ty.body_medium.lh(26.0))
                .color(k.c.text_base)
                .wrap()
                .selectable(true),
        )
        .child(status);
    // The pickers in one row under the title, each with its label.
    let mut pickers = row().key("issue-pickers").w_full().flex_wrap().gap(16.0);
    if editable {
        pickers = pickers.child(assignee_picker(s, issue, k, layer));
    }
    // Visibility is the crew's, as on the web.
    if editable && s.is_crew() {
        let current = match issue.visibility {
            IssueVisibility::Public => VISIBILITY[0],
            IssueVisibility::Internal => VISIBILITY[1],
        };
        pickers = pickers.child(choice(
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
        pickers = pickers.child(choice(
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
    if editable || s.auth.is_some() {
        column = column.child(pickers);
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
        column = column.child(markdown(s, &issue.description).build(k).key("description"));
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
        .child(avatar(s, c.creator.as_deref(), 20.0, k))
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
            .child(markdown(s, &c.body).build(k))
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
    let mut logins = vec![NOBODY];
    logins.extend(s.crew.iter().map(|u| u.login.as_str()));
    let current = issue.assignee.as_ref().map_or(NOBODY, |u| u.login.as_str());
    row()
        .gap(4.0)
        .center_y()
        .child(field_label(k, "Assignee"))
        .child(
            people(
                k,
                ("assignee", &s.assignee_combo, |s| &mut s.assignee_combo),
                &logins,
                current,
                120.0,
            )
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
        .gap(4.0)
        .center_y()
        .child(field_label(k, title))
        .child(
            Select::new(key, options)
                .width(120.0)
                .current(Some(current))
                .open(open)
                .on_toggle(move |s: &mut App| s.toggle_picker(picker.clone()))
                .on_close(|s: &mut App| s.picker = None)
                .on_choose(move |s: &mut App, v: &str, _| on_choose(s, v))
                .build(k, layer),
        )
}

/// The project's labels, the issue's first, filtered by what the user types
/// as the web app's label picker. A filter no label matches offers to create
/// a label with that name, and so does Enter.
fn label_picker(s: &App, issue_labels: &[Label], k: &Kit, layer: &mut Vec<El>) -> El {
    let open = s.picker == Some(Picker::Labels);
    let mut popover = Popover::new("labels")
        .trigger(
            Button::new("Labels…")
                .variant(ButtonVariant::Ghost)
                .expanded(open)
                .on_press(|s: &mut App| s.toggle_picker(Picker::Labels))
                .build(k),
        )
        .label("Labels")
        .open(open)
        .on_close(|s: &mut App| s.picker = None);
    if !open {
        return popover.build(k, layer);
    }
    let query = s.label_search.text.trim().to_lowercase();
    let on = |l: &Label| issue_labels.iter().any(|x| x.id == l.id);
    let (mine, others): (Vec<&Label>, Vec<&Label>) = s
        .labels
        .iter()
        .filter(|l| l.name.to_lowercase().contains(&query))
        .partition(|l| on(l));
    let mut options = col().key("label-options").w_full().gap(4.0);
    for l in mine.iter().chain(&others) {
        let id = l.id.clone();
        options = options.child(
            Checkbox::new(on(l))
                .label(l.name.clone())
                .on_toggle(move |s: &mut App| s.toggle_label(&id))
                .build(k)
                .key(format!("label-{}", l.name)),
        );
    }
    if !query.is_empty() && mine.is_empty() && others.is_empty() {
        options = options.child(
            Button::new(format!("Create \"{}\"", s.label_search.text.trim()))
                .variant(ButtonVariant::Ghost)
                .on_press(|s: &mut App| s.create_label())
                .build(k)
                .key("create-label"),
        );
    }
    popover = popover.child(
        col()
            .w(240.0)
            .gap(8.0)
            .child(
                TextInput::new(&s.label_search)
                    .placeholder("Filter or add label…")
                    .width(None)
                    .on_edit(|s: &mut App| &mut s.label_search)
                    .on_submit(|s: &mut App, _| s.create_label())
                    .on_key(close_on_escape)
                    .build(k)
                    .key("label-search"),
            )
            .child(
                col()
                    .key("label-scroll")
                    .w_full()
                    .max_h(240.0)
                    .scroll(ScrollSpec {
                        stick_bottom: false,
                    })
                    .child(options),
            ),
    );
    popover.build(k, layer)
}

/// The reactions to an issue or a comment, one pill per emoji with its
/// count, and a menu to add one. Pressing a pill adds or takes back the
/// user's own.
fn reactions(s: &App, subject_id: &str, k: &Kit, layer: &mut Vec<El>) -> El {
    let me = s.auth.as_ref().map(|a| a.sub.as_str());
    // In the order each emoji first came.
    let mut groups: Vec<(&str, Vec<&Emoji>)> = Vec::new();
    for e in s.reactions(subject_id) {
        match groups.iter_mut().find(|g| g.0 == e.value) {
            Some(g) => g.1.push(e),
            None => groups.push((&e.value, vec![e])),
        }
    }
    let mut r = row()
        .key(format!("reactions-{subject_id}"))
        .gap(6.0)
        .center_y();
    for (value, emoji) in groups {
        let count = emoji.len();
        let mine = emoji.iter().any(|e| Some(e.creator_id.as_str()) == me);
        let (subject, v) = (subject_id.to_owned(), value.to_owned());
        let mut pill = row()
            .key(format!("reaction-{value}"))
            .role("button")
            .label(format!("{value} {count}"))
            .help(reacted(&emoji, me))
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
    let mut popover = Popover::new(format!("add-reaction-{subject_id}"))
        .trigger(
            Button::new("+ 🙂")
                .variant(ButtonVariant::Ghost)
                .on_press(move |s: &mut App| s.toggle_picker(picker.clone()))
                .build(k)
                .key("add-reaction"),
        )
        .label("Emoji")
        .open(open)
        .on_close(|s: &mut App| s.picker = None);
    if open {
        popover = popover.child(emoji_picker(s, subject_id, k));
    }
    r.child(popover.build(k, layer))
}

/// Escape in a picker's search field closes the picker, which the field
/// would otherwise keep for itself.
fn close_on_escape(s: &mut App, event: &rune::KeyEvent, _: &mut Tx) -> bool {
    if !matches!(event.key, rune::Key::Escape) {
        return false;
    }
    s.picker = None;
    true
}

/// Who reacted with an emoji, as the web app's tooltip says it: "you, arv
/// and holden reacted with rocket", naming ten and counting the rest past
/// eleven.
fn reacted(emoji: &[&Emoji], me: Option<&str>) -> String {
    let names: Vec<&str> = emoji
        .iter()
        .filter_map(|e| {
            let creator = e.creator.as_ref()?;
            Some(if Some(e.creator_id.as_str()) == me {
                "you"
            } else {
                creator.login.as_str()
            })
        })
        .collect();
    let names = match names.as_slice() {
        [] => String::new(),
        [one] => (*one).to_owned(),
        _ if names.len() > 11 => {
            format!("{} and {} others", names[..10].join(", "), names.len() - 10)
        }
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    };
    let annotation = emoji.first().map_or("", |e| e.annotation.as_str());
    format!("{names} reacted with {annotation}")
}

/// The skin tones the picker offers, as the web picker's: the default,
/// then light to dark.
const SKIN_TONES: [emojis::SkinTone; 6] = [
    emojis::SkinTone::Default,
    emojis::SkinTone::Light,
    emojis::SkinTone::MediumLight,
    emojis::SkinTone::Medium,
    emojis::SkinTone::MediumDark,
    emojis::SkinTone::Dark,
];

/// The most a picker shows at once: the search narrows the rest.
const EMOJI_SHOWN: usize = 96;

/// The newest emoji the system fonts draw (Apple Color Emoji and Noto Color
/// Emoji in 2026): later ones would show as empty boxes.
const EMOJI_NEWEST: emojis::EmojiVersion = emojis::EmojiVersion::new(16, 0);

/// Every emoji, searchable by name and GitHub shortcode, in the user's skin
/// tone, as the web app's emoji picker.
fn emoji_picker(s: &App, subject_id: &str, k: &Kit) -> El {
    let query = s.emoji_search.text.trim().to_lowercase();
    let tone = SKIN_TONES[usize::from(s.skin_tone).min(SKIN_TONES.len() - 1)];
    let hands: Vec<String> = SKIN_TONES
        .iter()
        .map(|t| {
            emojis::get("✋")
                .and_then(|e| e.with_skin_tone(*t))
                .map_or_else(String::new, |e| e.as_str().to_owned())
        })
        .collect();
    let hands: Vec<&str> = hands.iter().map(String::as_str).collect();
    let matches = emojis::iter()
        .filter(|e| {
            e.emoji_version() <= EMOJI_NEWEST
                && (query.is_empty()
                    || e.name().contains(&query)
                    || e.shortcodes().any(|c| c.contains(&query)))
        })
        .take(EMOJI_SHOWN);
    let mut grid = row().key("emoji-grid").w_full().flex_wrap();
    for e in matches {
        let e = e.with_skin_tone(tone).unwrap_or(e);
        let (subject, value) = (subject_id.to_owned(), e.as_str());
        grid = grid.child(
            text(value)
                .key(format!("emoji-{value}"))
                .role("button")
                .label(e.name())
                .help(e.name())
                .font(k.ty.body)
                .w(32.0)
                .h(32.0)
                .center()
                .radius(6.0)
                .on_press(move |s: &mut App| s.toggle_reaction(&subject, value)),
        );
    }
    col()
        .w(288.0)
        .gap(8.0)
        .child(
            TextInput::new(&s.emoji_search)
                .placeholder("Search")
                .on_edit(|s: &mut App| &mut s.emoji_search)
                .on_key(close_on_escape)
                .build(k)
                .key("emoji-search"),
        )
        .child(
            SegmentedControl::new(&hands, Some(usize::from(s.skin_tone)))
                .on_pick(|s: &mut App, i, _| s.set_skin_tone(i as u8))
                .width(None)
                .build(k)
                .key("skin-tone"),
        )
        .child(
            col()
                .key("emoji-scroll")
                .w_full()
                .h(232.0)
                .scroll(ScrollSpec {
                    stick_bottom: false,
                })
                .child(grid),
        )
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
                .on_submit(|s: &mut App, _| s.add_comment())
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
                    .on_submit(|s: &mut App, _| s.save_issue())
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
