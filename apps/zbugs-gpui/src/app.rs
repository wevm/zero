//! The window: the issue list with its filters on the left, the selected
//! issue on the right, as zbugs' list and issue pages.

#[cfg(feature = "e2e")]
pub mod e2e;

use crate::emoji_picker::{EmojiPicker, OnPick as OnPickEmoji};
use crate::live::Live;
use crate::picker::{self, Choice};
use crate::session::{Session, app_dir, now_ms, save_setting};
use futures::StreamExt;
use futures::channel::mpsc;
use gpui_kit::base::actions::Confirm;
use gpui_kit::component::button::*;
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::text::TextView;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::ops::Range;
use std::rc::Rc;
use zbugs_model::args::*;
use zbugs_model::mutators::{
    ADD_COMMENT, ADD_EMOJI_TO_COMMENT, ADD_EMOJI_TO_ISSUE, ADD_LABEL, CREATE_AND_ADD_LABEL,
    CREATE_ISSUE, DELETE_ISSUE, DELETE_ISSUES, EDIT_COMMENT, MutateRequest, REMOVE_COMMENT,
    REMOVE_EMOJI, REMOVE_LABEL, SET_USER_PREF, SET_VIEW_STATE, UPDATE_ISSUE, UPDATE_NOTIFICATION,
};
use zbugs_model::queries::{
    ALL_PROJECTS, COMMENTS_PAGE, ISSUE_DETAIL, ISSUE_LIST_COUNT, ISSUE_LIST_V2, LABELS, USER,
    USER_PREF, USERS_FOR_PROJECT,
};
use zbugs_model::schema::{Comment, Emoji, Issue, IssueVisibility, Label, Project, User, UserPref};
use zbugs_model::{AuthData, ZERO_PROJECT_NAME, make_id};
use zero_client::client::{ConnectionState, MutationOutcome};
use zero_client::ttl::Ttl;

/// Rows the list asks for at a time, as zbugs' list pages.
const PAGE: i64 = 100;
const ROW_HEIGHT: f32 = 52.;
/// Comments `issueDetail` loads, and how many more "Show older comments"
/// adds.
const COMMENT_PAGE: i64 = 50;
/// The label picker's choice that makes a new label.
const NEW_LABEL: &str = "\u{0}new-label";

actions!(
    zbugs,
    [
        SelectNext,
        SelectPrevious,
        FocusSearch,
        ToggleChecked,
        StopChecking
    ]
);

/// j/k and the arrow keys move the selection while the list has focus, and
/// `/` goes to the search field, as on the web app's list. j/k also work
/// on the issue, as on the web app's issue page, unless a text field or
/// menu has the keys. Picking issues to delete, space checks the selected
/// one and escape stops. ⌘Enter (Ctrl+Enter elsewhere) in a text field
/// posts the comment or saves the dialog, as the web app's composers.
pub fn bind_keys(cx: &mut App) {
    const DETAIL: &str = "IssueDetail && !Input && !PopupMenu";
    let submit = if cx.platform_info().is_mac() {
        "cmd-enter"
    } else {
        "ctrl-enter"
    };
    cx.bind_keys([
        KeyBinding::new(submit, Confirm { secondary: true }, Some("Input")),
        KeyBinding::new("/", FocusSearch, Some("IssueList")),
        KeyBinding::new("j", SelectNext, Some("IssueList")),
        KeyBinding::new("down", SelectNext, Some("IssueList")),
        KeyBinding::new("k", SelectPrevious, Some("IssueList")),
        KeyBinding::new("up", SelectPrevious, Some("IssueList")),
        KeyBinding::new("space", ToggleChecked, Some("IssueList")),
        KeyBinding::new("escape", StopChecking, Some("IssueList")),
        KeyBinding::new("j", SelectNext, Some(DETAIL)),
        KeyBinding::new("k", SelectPrevious, Some(DETAIL)),
    ]);
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    Open,
    Closed,
    All,
}

/// What the list shows, as zbugs' list context.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
struct Filters {
    status: Status,
    text: String,
    /// Logins.
    creator: Option<String>,
    assignee: Option<String>,
    /// Label names; an issue needs all of them.
    labels: Vec<String>,
    /// By created rather than modified, as the web app's sort toggle.
    #[serde(default)]
    by_created: bool,
    /// Oldest first.
    #[serde(default)]
    ascending: bool,
    /// The project's name, as the web app's project picker.
    #[serde(default = "zero_project")]
    project: String,
}

fn zero_project() -> String {
    ZERO_PROJECT_NAME.into()
}

/// The users and labels the pickers offer: the project's.
type ProjectLives = (
    Live<Vec<User>>,
    Live<Vec<User>>,
    Live<Vec<User>>,
    Live<Vec<Label>>,
);

/// What the window shows, kept across launches as zbugs keeps it in the
/// URL.
#[derive(Serialize, Deserialize)]
struct Saved {
    filters: Filters,
    limit: i64,
    selected: Option<String>,
}

impl Saved {
    fn path() -> std::path::PathBuf {
        app_dir().join("window.json")
    }

    fn load() -> Option<Saved> {
        serde_json::from_slice(&std::fs::read(Self::path()).ok()?).ok()
    }
}

/// The reactions the add-reaction menu offers, with their annotations.
const REACTIONS: [(&str, &str); 6] = [
    ("👍", "thumbs up"),
    ("❤️", "red heart"),
    ("🎉", "party popper"),
    ("😄", "grinning face with smiling eyes"),
    ("😕", "confused face"),
    ("👀", "eyes"),
];

/// The user's skin tone for reactions, as zbugs' emoji picker keeps it:
/// 0 for none, 1 to 5 for U+1F3FB to U+1F3FF.
const SKIN_TONE_PREF: &str = "emojiSkinTone";
const SKIN_TONES: [&str; 6] = [
    "No skin tone",
    "Light",
    "Medium-light",
    "Medium",
    "Medium-dark",
    "Dark",
];

/// `emoji` in skin tone `tone`, as zbugs' `setSkinTone`, when it takes
/// one: 👍, or an emoji someone already reacted with in a tone (`toned`).
fn with_skin_tone(emoji: &str, tone: u32, toned: bool) -> String {
    let base = normalize_emoji(emoji);
    match char::from_u32(0x1F3FA + tone) {
        Some(modifier) if tone > 0 && (toned || base == "👍") => format!("{base}{modifier}"),
        _ => base,
    }
}

/// When `ms` was, as zbugs' `RelativeTime` shows it: "just now" to "1
/// day ago", then the date and time, with the year from past years.
fn relative_time(ms: f64, now: f64) -> String {
    use chrono::{Datelike, Local, TimeZone};
    const MINUTE: f64 = 60_000.;
    const HOUR: f64 = 60. * MINUTE;
    let (Some(at), Some(today)) = (
        Local.timestamp_millis_opt(ms as i64).single(),
        Local.timestamp_millis_opt(now as i64).single(),
    ) else {
        return String::new();
    };
    let delta = now - ms;
    if at.year() < today.year() {
        return at.format("%Y/%m/%d, %-I:%M %p").to_string();
    }
    if delta > 48. * HOUR {
        return at.format("%b %-d, %-I:%M %p").to_string();
    }
    match delta {
        d if d < MINUTE => "just now".into(),
        d if d < 2. * MINUTE => "1 minute ago".into(),
        d if d < HOUR => format!("{} minutes ago", (d / MINUTE) as i64),
        d if d < 2. * HOUR => "1 hour ago".into(),
        d if d < 24. * HOUR => format!("{} hours ago", (d / HOUR) as i64),
        _ => "1 day ago".into(),
    }
}

/// The comments and reactions on the shown issue so far, to tell others'
/// new ones, as zbugs' toasts.
struct Seen {
    issue_id: String,
    comments: BTreeSet<String>,
    emoji: BTreeSet<String>,
}

/// `user`'s avatar, round, as zbugs' `AvatarImage`; GitHub's at 64
/// pixels, as `avatarURLWithSize` asks for.
fn avatar(user: &User, size: Pixels) -> impl IntoElement {
    img(avatar_url(user))
        .flex_none()
        .size(size)
        .rounded_full()
        .with_fallback(move || div().size(size).into_any_element())
}

fn avatar_url(user: &User) -> String {
    match http_client::Url::parse(&user.avatar) {
        Ok(mut url) if url.host_str() == Some("avatars.githubusercontent.com") => {
            let query = url
                .query_pairs()
                .filter(|(key, _)| key != "size")
                .map(|(key, value)| (key.into_owned(), value.into_owned()))
                .collect::<Vec<_>>();
            url.query_pairs_mut()
                .clear()
                .extend_pairs(query)
                .append_pair("size", "64");
            url.to_string()
        }
        _ => user.avatar.clone(),
    }
}

/// `text` as markdown, with its images. TextView measures a block once, so
/// a block whose image arrives later keeps the empty height it had; keyed
/// by how many of its images have loaded, it's laid out again as each one
/// does. `use_asset` starts the loads and redraws the window as they end.
fn markdown(id: String, text: &str, window: &mut Window, cx: &mut App) -> TextView {
    let loaded = image_urls(text)
        .filter(|url| {
            let url = Resource::Uri(SharedUri::from(url.to_owned()));
            window.use_asset::<ImgResourceLoader>(&url, cx).is_some()
        })
        .count();
    TextView::markdown(
        SharedString::from(format!("{id}-{loaded}")),
        text.to_owned(),
    )
}

/// The URLs of the images in markdown `text`: `![alt](url "title")`.
fn image_urls(text: &str) -> impl Iterator<Item = &str> {
    text.split("![").skip(1).filter_map(|rest| {
        let (_, after) = rest.split_once("](")?;
        let url = after.split([')', ' ']).next()?;
        url.starts_with("http").then_some(url)
    })
}

/// What a reaction is on.
#[derive(Clone, Copy)]
enum Subject {
    Issue,
    Comment,
}

/// An emoji without its skin tone, so reactions that differ only in tone
/// count together, as zbugs' `normalizeEmoji`.
fn normalize_emoji(emoji: &str) -> String {
    emoji
        .chars()
        .filter(|c| !('\u{1F3FB}'..='\u{1F3FF}').contains(c))
        .collect()
}

type OnPick = Rc<dyn Fn(&mut ZbugsApp, Option<String>, &mut Window, &mut Context<ZbugsApp>)>;

pub struct ZbugsApp {
    session: Session,
    filters: Filters,
    limit: i64,
    list: Live<Vec<Issue>>,
    count: Live<i64>,
    projects: Live<Vec<Project>>,
    creators: Live<Vec<User>>,
    assignees: Live<Vec<User>>,
    crew: Live<Vec<User>>,
    labels: Live<Vec<Label>>,
    /// The logged-in user.
    me: Option<Live<Option<User>>>,
    /// Their skin tone for reactions (`SKIN_TONE_PREF`).
    skin_tone: Option<Live<Option<UserPref>>>,
    selected: Option<String>,
    detail: Option<Live<Option<Issue>>>,
    /// How many of the selected issue's comments to show, newest first.
    comments_limit: i64,
    /// Past the comments `issueDetail` brings: `commentsPage`, newest
    /// first, up to `comments_limit`.
    comment_page: Option<Live<Vec<Comment>>>,
    list_focus: FocusHandle,
    detail_focus: FocusHandle,
    /// The issues checked to delete together, while picking them (the web
    /// app's select mode).
    checked: Option<BTreeSet<String>>,
    /// Scroll to the selected issue once the list has it: after a launch
    /// restores the selection.
    reveal_selected: bool,
    /// The first rows on screen and where they were, to keep them on
    /// screen when rows above them come and go.
    anchor: Vec<(String, usize)>,
    search: Entity<InputState>,
    comment: Entity<TextareaState>,
    scroll: UniformListScrollHandle,
    connection: Rc<RefCell<ConnectionState>>,
    error: Option<String>,
    /// Marks the selected issue viewed after a second (`mark_viewed`).
    view_timer: Option<(String, Task<()>)>,
    /// The window's title, as zbugs' page titles (`window_title`).
    title: String,
    seen: Option<Seen>,
    _unsubscribe: Box<dyn Fn()>,
    _tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

/// Whether the issue changed since the user last viewed it. Its
/// `view_state` is the user's own.
fn is_unread(issue: &Issue) -> bool {
    let viewed = issue
        .view_state
        .iter()
        .flatten()
        .map(|v| v.viewed)
        .fold(0.0, f64::max);
    issue.modified > viewed
}

fn ttl() -> Ttl {
    Ttl::minutes(5.0)
}

impl ZbugsApp {
    pub fn new(session: Session, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let saved = Saved::load();
        let filters = saved.as_ref().map_or_else(
            || Filters {
                status: Status::Open,
                text: String::new(),
                creator: None,
                assignee: None,
                labels: Vec::new(),
                by_created: false,
                ascending: false,
                project: zero_project(),
            },
            |s| s.filters.clone(),
        );
        let limit = saved.as_ref().map_or(PAGE, |s| s.limit.max(PAGE));
        let search = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Search issues")
                .default_value(filters.text.clone())
        });
        let comment = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 10)
                .placeholder("Add a comment…")
        });
        let subscriptions = vec![
            cx.subscribe_in(&search, window, |this, state, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = state.read(cx).value().to_string();
                    this.refilter(|f| f.text = text, cx);
                }
            }),
            // On every change, drawn or not: a window that isn't drawing
            // still marks the issue it shows viewed.
            cx.observe_self(|this, cx| this.mark_viewed(cx)),
        ];

        // The connection badge.
        let connection = Rc::new(RefCell::new(session.zero.connection().state()));
        let (wake, mut woken) = mpsc::unbounded::<()>();
        let c = connection.clone();
        let unsubscribe = session.zero.connection().subscribe(move |state| {
            *c.borrow_mut() = state.clone();
            let _ = wake.unbounded_send(());
        });
        let watch = cx.spawn(async move |this, cx| {
            while woken.next().await.is_some() {
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    return;
                }
            }
        });

        let (list, count) = Self::queries(&session, &filters, limit, cx);
        let auth = session.auth.as_ref();
        let projects = Live::new(&session.zero, ALL_PROJECTS.call(&(), auth), &ttl(), cx);
        let (creators, assignees, crew, labels) =
            Self::project_lives(&session, &filters.project, cx);
        let me = session
            .auth
            .as_ref()
            .map(|a| Live::new(&session.zero, USER.call(&a.sub, Some(a)), &ttl(), cx));
        let skin_tone = session.auth.as_ref().map(|a| {
            let q = USER_PREF.call(&SKIN_TONE_PREF.to_owned(), Some(a));
            Live::new(&session.zero, q, &ttl(), cx)
        });
        let mut app = ZbugsApp {
            session,
            filters,
            limit,
            list,
            count,
            projects,
            creators,
            assignees,
            crew,
            labels,
            me,
            skin_tone,
            selected: None,
            detail: None,
            comments_limit: COMMENT_PAGE,
            comment_page: None,
            list_focus: cx.focus_handle(),
            detail_focus: cx.focus_handle(),
            checked: None,
            reveal_selected: false,
            anchor: Vec::new(),
            search,
            comment,
            scroll: UniformListScrollHandle::new(),
            connection,
            error: None,
            view_timer: None,
            title: String::new(),
            seen: None,
            _unsubscribe: unsubscribe,
            _tasks: vec![watch],
            _subscriptions: subscriptions,
        };
        if let Some(id) = saved.and_then(|s| s.selected) {
            app.select(id, cx);
            app.reveal_selected = true;
        }
        // Keeps the relative times ("5 minutes ago") current.
        app._tasks.push(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(30))
                    .await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        }));
        app
    }

    /// Starts over on another login or other servers.
    fn replace_session(&mut self, session: Session, window: &mut Window, cx: &mut Context<Self>) {
        *self = ZbugsApp::new(session, window, cx);
        cx.notify();
    }

    /// Logs in with a zbugs token, or logs out with `None`. Returns whether
    /// it went ahead; a token that isn't one doesn't.
    fn log_in(
        &mut self,
        token: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if let Some(token) = &token
            && AuthData::from_jwt(token, now_ms() / 1000.0).is_none()
        {
            self.error = Some("That isn't a zbugs token, or it has expired.".into());
            cx.notify();
            return false;
        }
        crate::token::save(token.as_deref());
        let (cache_url, api_url) = (self.session.cache_url.clone(), self.session.api_url.clone());
        self.replace_session(Session::new(cache_url, api_url, token, cx), window, cx);
        true
    }

    fn set_servers(
        &mut self,
        cache_url: String,
        api_url: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if cache_url.is_empty() || api_url.is_empty() {
            self.error = Some("Both servers need a URL.".into());
            cx.notify();
            return false;
        }
        save_setting("cache-url", Some(&cache_url));
        save_setting("api-url", Some(&api_url));
        let token = self.session.token.clone();
        self.replace_session(Session::new(cache_url, api_url, token, cx), window, cx);
        true
    }

    /// A dialog of text fields; Save passes their values to `save`, which
    /// returns whether the dialog can close.
    fn open_form(
        &mut self,
        title: &'static str,
        fields: Vec<(&'static str, String, bool)>,
        save: impl Fn(&mut ZbugsApp, Vec<String>, &mut Window, &mut Context<ZbugsApp>) -> bool + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let labels: Vec<&'static str> = fields.iter().map(|f| f.0).collect();
        let inputs: Vec<Entity<InputState>> = fields
            .into_iter()
            .map(|(placeholder, value, masked)| {
                cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder(placeholder)
                        .masked(masked)
                        .default_value(value)
                })
            })
            .collect();
        let first = inputs.first().cloned();
        let app = cx.entity().downgrade();
        let save = Rc::new(save);
        window.open_dialog(cx, move |dialog, _, cx| {
            let (app, inputs2, save) = (app.clone(), inputs.clone(), save.clone());
            let submit = Rc::new(move |window: &mut Window, cx: &mut App| {
                let values = inputs2
                    .iter()
                    .map(|i| i.read(cx).value().trim().to_owned())
                    .collect();
                let saved = app
                    .update(cx, |this, cx| save(this, values, window, cx))
                    .unwrap_or(false);
                if saved {
                    window.close_dialog(cx);
                }
            });
            let on_ok = submit.clone();
            dialog
                .w(px(480.))
                .title(title)
                .on_ok(move |_, window, cx| {
                    on_ok(window, cx);
                    false
                })
                .child(v_flex().gap_3().children(inputs.iter().zip(&labels).map(
                    |(input, label)| {
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(*label),
                            )
                            .child(Input::new(input))
                    },
                )))
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("form-cancel")
                                .outline()
                                .label("Cancel")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("form-save")
                                .primary()
                                .label("Save")
                                .on_click(move |_, window, cx| submit(window, cx)),
                        ),
                )
        });
        if let Some(first) = first {
            window.defer(cx, move |window, cx| {
                first.update(cx, |state, cx| state.focus(window, cx));
            });
        }
    }

    /// Opens zbugs' GitHub login in the browser, and logs in with the token
    /// it comes back with (`login::github`).
    fn log_in_with_github(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (url, token) = match crate::login::github(&self.session.api_url) {
            Ok(login) => login,
            Err(e) => {
                self.error = Some(format!("Couldn't start the GitHub login: {e}"));
                cx.notify();
                return;
            }
        };
        cx.open_url(&url);
        cx.spawn_in(window, async move |this, cx| {
            let result = token
                .await
                .unwrap_or_else(|_| Err("The login stopped.".into()));
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(token) => {
                    this.log_in(Some(token), window, cx);
                    cx.activate(true);
                }
                Err(e) => {
                    this.error = Some(e);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Asks for a label's name, then creates it in the issue's project and
    /// adds it to the issue (`label.createAndAddToIssue`, crew only).
    fn open_new_label(&mut self, issue_id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.open_form(
            "New label",
            vec![("Label name", String::new(), false)],
            move |this, values, _, cx| {
                let name = values.into_iter().next().unwrap_or_default();
                this.create_label(issue_id.clone(), name, cx)
            },
            window,
            cx,
        );
    }

    /// Creates label `name` and adds it to the issue. Returns whether it
    /// went ahead; an empty name, or one the project has, doesn't.
    fn create_label(&mut self, issue_id: String, name: String, cx: &mut Context<Self>) -> bool {
        if name.is_empty() {
            self.error = Some("A label needs a name.".into());
            cx.notify();
            return false;
        }
        let exists = self
            .labels
            .snapshot()
            .value
            .iter()
            .flatten()
            .any(|l| l.name.eq_ignore_ascii_case(&name));
        if exists {
            self.error = Some(format!("There's already a label called {name}."));
            cx.notify();
            return false;
        }
        let args = CreateAndAddLabelArgs {
            issue_id,
            label_id: make_id(),
            label_name: name,
            project_id: None,
        };
        self.mutate(CREATE_AND_ADD_LABEL.with(&args), cx);
        true
    }

    fn open_login(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_form(
            "Log in with a token",
            vec![("zbugs token (a JWT)", String::new(), true)],
            |this, values, window, cx| {
                let token = values.into_iter().next().filter(|t| !t.is_empty());
                token.is_some_and(|t| this.log_in(Some(t), window, cx))
            },
            window,
            cx,
        );
    }

    fn open_servers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_form(
            "Servers",
            vec![
                ("zero-cache URL", self.session.cache_url.clone(), false),
                ("zbugs API URL", self.session.api_url.clone(), false),
            ],
            |this, values, window, cx| {
                let mut values = values.into_iter();
                let cache_url = values.next().unwrap_or_default();
                let api_url = values.next().unwrap_or_default();
                this.set_servers(cache_url, api_url, window, cx)
            },
            window,
            cx,
        );
    }

    fn render_account(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let login = self
            .me
            .as_ref()
            .and_then(|me| me.snapshot().value.clone().flatten())
            .map(|u| u.login);
        let logged_in = self.session.auth.is_some();
        let app = cx.entity().downgrade();
        Button::new("account")
            .small()
            .ghost()
            .label(match (&login, logged_in) {
                (Some(login), _) => login.clone(),
                (None, true) => "Account".into(),
                (None, false) => "Log in".into(),
            })
            .dropdown_menu(move |menu: PopupMenu, _, _| {
                let (a, b, c) = (app.clone(), app.clone(), app.clone());
                let menu = if logged_in {
                    menu.item(
                        PopupMenuItem::new("Log out").on_click(move |_, window, cx| {
                            let _ = a.update(cx, |this, cx| this.log_in(None, window, cx));
                        }),
                    )
                } else {
                    menu.item(PopupMenuItem::new("Log in with GitHub").on_click(
                        move |_, window, cx| {
                            let _ = c.update(cx, |this, cx| this.log_in_with_github(window, cx));
                        },
                    ))
                    .item(
                        PopupMenuItem::new("Log in with a token…").on_click(
                            move |_, window, cx| {
                                let _ = a.update(cx, |this, cx| this.open_login(window, cx));
                            },
                        ),
                    )
                };
                menu.separator()
                    .item(
                        PopupMenuItem::new("Servers…").on_click(move |_, window, cx| {
                            let _ = b.update(cx, |this, cx| this.open_servers(window, cx));
                        }),
                    )
            })
    }

    /// Keeps the rows on screen where they are when rows above them come,
    /// go, or move, as zbugs' list does: shifts the scroll by the move most
    /// of the anchored rows made, so one row that moved itself (the issue
    /// just edited) doesn't drag the list. At the top, the list stays at
    /// the top.
    fn keep_scroll_anchor(&mut self) {
        let snapshot = self.list.snapshot();
        let rows = snapshot.value.as_deref().unwrap_or_default();
        let handle = self.scroll.0.borrow().base_handle.clone();
        let mut offset = handle.offset();
        if offset.y < px(0.) {
            let mut shifts: Vec<isize> = self
                .anchor
                .iter()
                .filter_map(|(id, was)| {
                    let at = rows.iter().position(|r| &r.id == id)?;
                    Some(at as isize - *was as isize)
                })
                .collect();
            shifts.sort_unstable();
            let shift = shifts
                .chunk_by(|a, b| a == b)
                .max_by_key(|run| run.len())
                .map_or(0, |run| run[0]);
            if shift != 0 {
                offset.y -= px(shift as f32 * ROW_HEIGHT);
                handle.set_offset(offset);
            }
        }
        let top = (-offset.y.as_f32() / ROW_HEIGHT).max(0.) as usize;
        self.anchor = rows
            .iter()
            .enumerate()
            .skip(top)
            .take(8)
            .map(|(ix, r)| (r.id.clone(), ix))
            .collect();
    }

    /// Keeps what the window shows for the next launch.
    fn save(&self) {
        let saved = Saved {
            filters: self.filters.clone(),
            limit: self.limit,
            selected: self.selected.clone(),
        };
        if let Ok(json) = serde_json::to_vec(&saved) {
            let _ = std::fs::write(Saved::path(), json);
        }
    }

    /// The creators, assignees, crew, and labels of `project`.
    fn project_lives(session: &Session, project: &str, cx: &mut Context<Self>) -> ProjectLives {
        let auth = session.auth.as_ref();
        let mut users = |filter| {
            let args = UsersForProjectArgs {
                disabled: false,
                login: None,
                project_name: project.into(),
                filter: Some(filter),
            };
            Live::new(
                &session.zero,
                USERS_FOR_PROJECT.call(&args, auth),
                &ttl(),
                cx,
            )
        };
        let creators = users(UsersFilter::Creators);
        let assignees = users(UsersFilter::Assignees);
        let crew = users(UsersFilter::Crew);
        let args = ProjectArgs {
            project_name: project.into(),
        };
        let labels = Live::new(&session.zero, LABELS.call(&args, auth), &ttl(), cx);
        (creators, assignees, crew, labels)
    }

    /// Switches to another project's issues: a new list, as the web app's
    /// project picker goes to, without the user and label filters or a
    /// selection.
    fn set_project(&mut self, name: String, cx: &mut Context<Self>) {
        if name == self.filters.project {
            return;
        }
        (self.creators, self.assignees, self.crew, self.labels) =
            Self::project_lives(&self.session, &name, cx);
        self.selected = None;
        self.detail = None;
        self.comment_page = None;
        self.refilter(
            |f| {
                f.project = name;
                f.creator = None;
                f.assignee = None;
                f.labels.clear();
            },
            cx,
        );
    }

    fn list_context(f: &Filters) -> ListContextParams {
        ListContextParams {
            open: match f.status {
                Status::Open => Some(true),
                Status::Closed => Some(false),
                Status::All => None,
            },
            project_name: Some(f.project.clone()),
            assignee: f.assignee.clone(),
            creator: f.creator.clone(),
            labels: (!f.labels.is_empty()).then(|| f.labels.clone()),
            text_filter: (!f.text.is_empty()).then(|| f.text.clone()),
            sort_field: if f.by_created {
                SortField::Created
            } else {
                SortField::Modified
            },
            sort_direction: if f.ascending {
                SortDirection::Asc
            } else {
                SortDirection::Desc
            },
        }
    }

    fn queries(
        session: &Session,
        filters: &Filters,
        limit: i64,
        cx: &mut Context<Self>,
    ) -> (Live<Vec<Issue>>, Live<i64>) {
        let auth = session.auth.as_ref();
        let list_context = Self::list_context(filters);
        let list = ISSUE_LIST_V2.call(
            &IssueListV2Args {
                list_context: list_context.clone(),
                limit: Some(limit),
                start: None,
                dir: PageDirection::Forward,
                inclusive: None,
            },
            auth,
        );
        let count = ISSUE_LIST_COUNT.call(&IssueListCountArgs { list_context }, auth);
        (
            Live::new(&session.zero, list, &ttl(), cx),
            Live::new(&session.zero, count, &ttl(), cx),
        )
    }

    /// Changes the filters; when they changed, the list starts over at its
    /// top.
    fn refilter(&mut self, change: impl FnOnce(&mut Filters), cx: &mut Context<Self>) {
        let mut filters = self.filters.clone();
        change(&mut filters);
        if filters == self.filters {
            return;
        }
        self.filters = filters;
        self.limit = PAGE;
        self.anchor.clear();
        self.save();
        let (list, count) = Self::queries(&self.session, &self.filters, self.limit, cx);
        self.list = list;
        self.count = count;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    /// Near the end of what's loaded: ask for the next page. The rows
    /// loaded so far stay where they are, so the list doesn't move.
    fn grow(&mut self, cx: &mut Context<Self>) {
        let loaded = self.list.snapshot().value.as_ref().map_or(0, Vec::len) as i64;
        if loaded < self.limit {
            return;
        }
        self.limit += PAGE;
        self.save();
        let (list, _) = Self::queries(&self.session, &self.filters, self.limit, cx);
        self.list = list;
        cx.notify();
    }

    fn toggle_label_filter(&mut self, name: Option<String>, cx: &mut Context<Self>) {
        self.refilter(
            |f| match name {
                None => f.labels.clear(),
                Some(name) => {
                    if let Some(ix) = f.labels.iter().position(|l| *l == name) {
                        f.labels.remove(ix);
                    } else {
                        f.labels.push(name);
                    }
                }
            },
            cx,
        );
    }

    fn select(&mut self, id: String, cx: &mut Context<Self>) {
        if self.selected.as_deref() == Some(id.as_str()) {
            return;
        }
        let q = ISSUE_DETAIL.call(
            &IssueDetailArgs {
                id_field: IdField::Id,
                id: IssueKey::String(id.clone()),
            },
            self.session.auth.as_ref(),
        );
        self.detail = Some(Live::new(&self.session.zero, q, &ttl(), cx));
        self.selected = Some(id);
        self.comments_limit = COMMENT_PAGE;
        self.comment_page = None;
        self.save();
        cx.notify();
    }

    /// Selects the issue `delta` rows from the selected one, or the first.
    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let next = {
            let snapshot = self.list.snapshot();
            let rows = snapshot.value.as_deref().unwrap_or_default();
            if rows.is_empty() {
                return;
            }
            let at = self
                .selected
                .as_ref()
                .and_then(|id| rows.iter().position(|r| &r.id == id));
            let ix = match at {
                Some(ix) => (ix as isize + delta).clamp(0, rows.len() as isize - 1) as usize,
                None => 0,
            };
            (ix, rows[ix].id.clone())
        };
        self.select(next.1, cx);
        self.scroll.scroll_to_item(next.0, ScrollStrategy::Nearest);
    }

    fn show_older_comments(&mut self, cx: &mut Context<Self>) {
        let Some(issue_id) = self.selected.clone() else {
            return;
        };
        self.comments_limit += COMMENT_PAGE;
        let args = CommentsPageArgs {
            issue_id,
            limit: self.comments_limit,
            start: None,
            dir: PageDirection::Backward,
            inclusive: false,
        };
        let q = COMMENTS_PAGE.call(&args, self.session.auth.as_ref());
        self.comment_page = Some(Live::new(&self.session.zero, q, &ttl(), cx));
        cx.notify();
    }

    /// Runs a mutation and shows why it failed, on the client or the
    /// server.
    fn mutate(&mut self, request: MutateRequest, cx: &mut Context<Self>) {
        let result = request.run(&self.session.zero);
        if let Some(Ok(MutationOutcome::AppError { message, .. })) = result.client.result() {
            self.error = Some(message);
            cx.notify();
            return;
        }
        let server = result.server;
        cx.spawn(async move |this, cx| {
            if let Ok(MutationOutcome::AppError { message, .. }) = server.await {
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(message);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Whether the user can edit or delete what `creator_id` made: their
    /// own, or anything for the crew, as zbugs' mutators allow.
    fn can_change(&self, creator_id: &str) -> bool {
        self.session
            .auth
            .as_ref()
            .is_some_and(|a| a.is_crew() || a.sub == creator_id)
    }

    /// Opens a dialog to edit `comment`'s Markdown.
    fn open_comment_editor(
        &mut self,
        comment: Comment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let body = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(8)
                .default_value(comment.body.clone())
        });
        let app = cx.entity().downgrade();
        let (b0, id) = (body.clone(), comment.id);
        window.open_dialog(cx, move |dialog, _, _| {
            let (app, b, id) = (app.clone(), b0.clone(), id.clone());
            let save = Rc::new(move |window: &mut Window, cx: &mut App| {
                let body = b.read(cx).value().trim().to_owned();
                let saved = app
                    .update(cx, |this, cx| this.edit_comment(id.clone(), body, cx))
                    .unwrap_or(false);
                if saved {
                    window.close_dialog(cx);
                }
            });
            let on_ok = save.clone();
            dialog
                .w(px(640.))
                .title("Edit comment")
                .on_ok(move |_, window, cx| {
                    on_ok(window, cx);
                    false
                })
                .child(Textarea::new(&b0))
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("comment-cancel")
                                .outline()
                                .label("Cancel")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("comment-save")
                                .primary()
                                .label("Save")
                                .on_click(move |_, window, cx| save(window, cx)),
                        ),
                )
        });
        window.defer(cx, move |window, cx| {
            body.update(cx, |state, cx| state.focus(window, cx));
        });
    }

    /// Saves comment `id`'s new body. Returns whether it went ahead; an
    /// empty body doesn't.
    fn edit_comment(&mut self, id: String, body: String, cx: &mut Context<Self>) -> bool {
        if body.is_empty() {
            self.error = Some("A comment can't be empty.".into());
            cx.notify();
            return false;
        }
        self.mutate(EDIT_COMMENT.with(&EditCommentArgs { id, body }), cx);
        true
    }

    fn delete_comment(&mut self, id: &String, cx: &mut Context<Self>) {
        self.mutate(REMOVE_COMMENT.with(id), cx);
    }

    /// Asks before a delete, which `delete` does.
    fn confirm_delete(
        &mut self,
        title: impl Into<SharedString>,
        delete: impl Fn(&mut ZbugsApp, &mut Context<ZbugsApp>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let app = cx.entity().downgrade();
        let delete = Rc::new(delete);
        let title: SharedString = title.into();
        window.open_dialog(cx, move |dialog, _, _| {
            let (app, delete) = (app.clone(), delete.clone());
            dialog
                .w(px(420.))
                .title(title.clone())
                .child("It's gone for everyone, and can't be undone.")
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("delete-cancel")
                                .outline()
                                .label("Cancel")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("delete-confirm")
                                .danger()
                                .label("Delete")
                                .on_click(move |_, window, cx| {
                                    let _ = app.update(cx, |this, cx| delete(this, cx));
                                    window.close_dialog(cx);
                                }),
                        ),
                )
        });
    }

    /// Deletes issue `id`, and leaves it if it's selected, as the web app
    /// goes back to the list.
    fn delete_issue(&mut self, id: &String, cx: &mut Context<Self>) {
        self.mutate(DELETE_ISSUE.with(id), cx);
        self.leave_deleted(std::slice::from_ref(id), cx);
    }

    /// Starts or stops checking issues to delete together.
    fn toggle_checking(&mut self, cx: &mut Context<Self>) {
        self.checked = match self.checked {
            Some(_) => None,
            None => Some(BTreeSet::new()),
        };
        cx.notify();
    }

    /// Checks or unchecks `issue` while picking issues to delete, if the
    /// user can delete it.
    fn toggle_checked(&mut self, issue: &Issue, cx: &mut Context<Self>) {
        let can = self.can_change(&issue.creator_id);
        if let Some(checked) = self.checked.as_mut().filter(|_| can)
            && !checked.remove(&issue.id)
        {
            checked.insert(issue.id.clone());
        }
        cx.notify();
    }

    /// Checks or unchecks the selected issue, for space on the list.
    fn toggle_selected_checked(&mut self, cx: &mut Context<Self>) {
        let issue = self.list.snapshot().value.as_ref().and_then(|rows| {
            rows.iter()
                .find(|r| Some(&r.id) == self.selected.as_ref())
                .cloned()
        });
        if let Some(issue) = issue {
            self.toggle_checked(&issue, cx);
        }
    }

    /// Deletes the checked issues with `issue.deleteMany`, and stops
    /// picking.
    fn delete_checked(&mut self, cx: &mut Context<Self>) {
        let Some(checked) = self.checked.take() else {
            return;
        };
        let ids = checked.into_iter().collect::<Vec<_>>();
        self.mutate(DELETE_ISSUES.with(&ids), cx);
        self.leave_deleted(&ids, cx);
    }

    /// Leaves the selected issue if it's one of `ids`.
    fn leave_deleted(&mut self, ids: &[String], cx: &mut Context<Self>) {
        if self.selected.as_ref().is_some_and(|id| ids.contains(id)) {
            self.selected = None;
            self.detail = None;
            self.comment_page = None;
            self.save();
        }
        cx.notify();
    }

    /// Makes an issue internal (crew only) or public, as the crew can on
    /// the web app.
    fn set_visibility(&mut self, issue_id: String, internal: bool, cx: &mut Context<Self>) {
        let args = UpdateIssueArgs {
            visibility: Some(if internal {
                IssueVisibility::Internal
            } else {
                IssueVisibility::Public
            }),
            ..UpdateIssueArgs::new(issue_id, now_ms())
        };
        self.mutate(UPDATE_ISSUE.with(&args), cx);
    }

    fn set_subscribed(&mut self, issue_id: String, subscribe: bool, cx: &mut Context<Self>) {
        let args = NotificationUpdateArgs {
            issue_id,
            subscribed: if subscribe {
                NotificationType::Subscribe
            } else {
                NotificationType::Unsubscribe
            },
            created: now_ms(),
        };
        self.mutate(UPDATE_NOTIFICATION.with(&args), cx);
    }

    /// Tells the user when someone else comments on or reacts to the
    /// issue they're looking at, as zbugs' toasts do. What the issue
    /// brought once the server confirmed it is already seen.
    fn notice_activity(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(detail) = self.detail.as_ref().filter(|d| d.is_synced()) else {
            return;
        };
        let snapshot = detail.snapshot();
        let Some(issue) = snapshot.value.as_ref().and_then(Option::as_ref) else {
            return;
        };
        let comments = issue.comments.as_deref().unwrap_or_default();
        let emoji = issue.emoji.as_deref().unwrap_or_default();
        let seen = self.seen.as_ref().filter(|s| s.issue_id == issue.id);
        if let Some(seen) = seen {
            let me = self.session.auth.as_ref().map(|a| a.sub.as_str());
            let others = |id: &str| Some(id) != me;
            let new_comments = comments
                .iter()
                .filter(|c| !seen.comments.contains(&c.id) && others(&c.creator_id))
                .filter_map(|c| c.creator.as_ref())
                .map(|u| format!("{} posted a new comment", u.login));
            let new_emoji = emoji
                .iter()
                .filter(|e| !seen.emoji.contains(&e.id) && others(&e.creator_id))
                .filter_map(|e| {
                    let u = e.creator.as_ref()?;
                    Some(format!("{} reacted on this issue: {}", u.login, e.value))
                });
            for message in new_comments.chain(new_emoji).collect::<Vec<_>>() {
                window.push_notification(Notification::info(message), cx);
            }
            let unchanged = comments.len() == seen.comments.len()
                && emoji.len() == seen.emoji.len()
                && comments.iter().all(|c| seen.comments.contains(&c.id))
                && emoji.iter().all(|e| seen.emoji.contains(&e.id));
            if unchanged {
                return;
            }
        }
        self.seen = Some(Seen {
            issue_id: issue.id.clone(),
            comments: comments.iter().map(|c| c.id.clone()).collect(),
            emoji: emoji.iter().map(|e| e.id.clone()).collect(),
        });
    }

    /// "Zero Bugs → " and the shown issue's title, or the list's, as
    /// zbugs' issue and list pages title themselves.
    fn window_title(&self) -> String {
        let issue = self
            .detail
            .as_ref()
            .and_then(|d| d.snapshot().value.clone().flatten());
        let f = &self.filters;
        let shown = match issue {
            Some(issue) => issue.title,
            None if f.creator.is_some()
                || f.assignee.is_some()
                || !f.labels.is_empty()
                || !f.text.is_empty() =>
            {
                "Filtered Issues".into()
            }
            None => match f.status {
                Status::Open => "Open Issues".into(),
                Status::Closed => "Closed Issues".into(),
                Status::All => "All Issues".into(),
            },
        };
        format!("Zero Bugs → {shown}")
    }

    /// Marks the selected issue viewed once it has been on screen for a
    /// second, if it changed since the user last viewed it, as the web
    /// app's issue page does. The list shows the others as unread.
    fn mark_viewed(&mut self, cx: &mut Context<Self>) {
        let unread = self
            .session
            .auth
            .as_ref()
            .and(self.detail.as_ref())
            .and_then(|d| {
                let snapshot = d.snapshot();
                let issue = snapshot.value.as_ref()?.as_ref()?;
                is_unread(issue).then(|| issue.id.clone())
            });
        let Some(id) = unread else {
            self.view_timer = None;
            return;
        };
        if self.view_timer.as_ref().is_some_and(|(at, _)| *at == id) {
            return;
        }
        let at = id.clone();
        let timer = cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(1))
                .await;
            let _ = this.update(cx, |this, cx| {
                // Done: a change the server confirms after this one, such
                // as its own `modified` for a close, starts another.
                this.view_timer = None;
                let modified = this
                    .detail
                    .as_ref()
                    .and_then(|d| d.snapshot().value.clone().flatten())
                    .filter(|issue| issue.id == id)
                    .map(|issue| issue.modified);
                if let Some(modified) = modified {
                    // Viewed no earlier than the change it saw, whatever
                    // the server's clock says.
                    let args = ViewStateArgs {
                        issue_id: id,
                        viewed: now_ms().max(modified),
                    };
                    this.mutate(SET_VIEW_STATE.with(&args), cx);
                }
                this.mark_viewed(cx);
            });
        });
        self.view_timer = Some((at, timer));
    }

    fn toggle_open(&mut self, issue: &Issue, cx: &mut Context<Self>) {
        let args = UpdateIssueArgs {
            open: Some(!issue.open),
            ..UpdateIssueArgs::new(issue.id.clone(), now_ms())
        };
        self.mutate(UPDATE_ISSUE.with(&args), cx);
    }

    fn set_assignee(&mut self, issue_id: String, user_id: Option<String>, cx: &mut Context<Self>) {
        let args = UpdateIssueArgs {
            assignee_id: Some(user_id),
            ..UpdateIssueArgs::new(issue_id, now_ms())
        };
        self.mutate(UPDATE_ISSUE.with(&args), cx);
    }

    /// Adds the label to the issue, or takes it off when it has it.
    fn toggle_label(&mut self, issue: &Issue, label_id: String, cx: &mut Context<Self>) {
        let has = issue.labels.iter().flatten().any(|l| l.id == label_id);
        let issue_id = issue.id.clone();
        let request = if has {
            REMOVE_LABEL.with(&RemoveLabelArgs { issue_id, label_id })
        } else {
            ADD_LABEL.with(&IssueLabelArgs {
                issue_id,
                label_id,
                project_id: None,
            })
        };
        self.mutate(request, cx);
    }

    /// The user's skin tone for reactions, 0 for none.
    fn skin_tone(&self) -> u32 {
        let pref = self
            .skin_tone
            .as_ref()
            .and_then(|live| live.snapshot().value.clone().flatten());
        pref.and_then(|p| p.value.parse().ok())
            .filter(|tone| *tone < SKIN_TONES.len() as u32)
            .unwrap_or(0)
    }

    fn set_skin_tone(&mut self, tone: u32, cx: &mut Context<Self>) {
        let args = UserPrefArgs {
            key: SKIN_TONE_PREF.to_owned(),
            value: tone.to_string(),
        };
        self.mutate(SET_USER_PREF.with(&args), cx);
    }

    /// Takes back the user's reaction with `unicode`, or adds it.
    fn toggle_emoji(
        &mut self,
        subject: Subject,
        subject_id: &str,
        emoji: &[Emoji],
        unicode: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(me) = self.session.auth.as_ref().map(|a| a.sub.as_str()) else {
            return;
        };
        let key = normalize_emoji(unicode);
        if let Some(mine) = emoji
            .iter()
            .find(|e| e.creator_id == me && normalize_emoji(&e.value) == key)
        {
            let id = mine.id.clone();
            self.mutate(REMOVE_EMOJI.with(&id), cx);
            return;
        }
        let annotation = REACTIONS
            .iter()
            .find(|(u, _)| normalize_emoji(u) == key)
            .map(|(_, a)| a.to_string())
            .or_else(|| {
                emoji
                    .iter()
                    .find(|e| normalize_emoji(&e.value) == key)
                    .map(|e| e.annotation.clone())
            })
            .or_else(|| emojis::get(&key).map(|e| e.name().to_owned()))
            .unwrap_or_default();
        let args = AddEmojiArgs {
            id: make_id(),
            unicode: unicode.to_owned(),
            annotation,
            subject_id: subject_id.to_owned(),
            created: now_ms(),
        };
        let add = match subject {
            Subject::Issue => ADD_EMOJI_TO_ISSUE,
            Subject::Comment => ADD_EMOJI_TO_COMMENT,
        };
        self.mutate(add.with(&args), cx);
    }

    /// Opens the picker with every emoji, to react with any of them.
    fn open_emoji_picker(
        &mut self,
        subject: Subject,
        subject_id: &str,
        emoji: &Rc<Vec<Emoji>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (app, all, id) = (
            cx.entity().downgrade(),
            emoji.clone(),
            subject_id.to_owned(),
        );
        let on_pick: OnPickEmoji = Rc::new(move |unicode, _, window, cx| {
            window.close_dialog(cx);
            let _ = app.update(cx, |this, cx| {
                this.toggle_emoji(subject, &id, &all, unicode, cx)
            });
        });
        let picker = cx.new(|cx| EmojiPicker::new(self.skin_tone(), on_pick, window, cx));
        let query = picker.read(cx).query.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .w(px(400.))
                .title("Add reaction")
                .child(picker.clone())
        });
        window.defer(cx, move |window, cx| {
            query.update(cx, |state, cx| state.focus(window, cx));
        });
    }

    fn post_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(issue_id) = self
            .selected
            .clone()
            .filter(|_| self.session.auth.is_some())
        else {
            return;
        };
        let body = self.comment.read(cx).value().trim().to_owned();
        if body.is_empty() {
            return;
        }
        let args = AddCommentArgs {
            id: make_id(),
            issue_id,
            body,
            created: now_ms(),
        };
        self.mutate(ADD_COMMENT.with(&args), cx);
        self.comment
            .update(cx, |state, cx| state.set_value("", window, cx));
    }

    /// The id of the project new issues go in.
    fn project_id(&self) -> Option<String> {
        let name = self.filters.project.to_lowercase();
        self.projects
            .snapshot()
            .value
            .iter()
            .flatten()
            .find(|p| p.lower_case_name == name)
            .map(|p| p.id.clone())
    }

    /// Opens the new issue dialog, or the edit dialog of `issue`.
    fn open_editor(&mut self, issue: Option<Issue>, window: &mut Window, cx: &mut Context<Self>) {
        let title = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Title")
                .default_value(issue.as_ref().map_or(String::new(), |i| i.title.clone()))
        });
        let description = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(10)
                .placeholder("Description (Markdown)")
                .default_value(
                    issue
                        .as_ref()
                        .map_or(String::new(), |i| i.description.clone()),
                )
        });
        let app = cx.entity().downgrade();
        let id = issue.map(|i| i.id);
        let (t0, d0) = (title.clone(), description.clone());
        window.open_dialog(cx, move |dialog, _, _| {
            let editing = id.is_some();
            let (app, t, d, i) = (app.clone(), t0.clone(), d0.clone(), id.clone());
            let save = move |window: &mut Window, cx: &mut App| {
                let title = t.read(cx).value().trim().to_owned();
                let description = d.read(cx).value().to_string();
                let saved = app
                    .update(cx, |this, cx| {
                        this.save_issue(i.clone(), title, description, cx)
                    })
                    .unwrap_or(false);
                if saved {
                    window.close_dialog(cx);
                }
            };
            let save = Rc::new(save);
            let on_ok = save.clone();
            dialog
                .w(px(640.))
                .title(if editing { "Edit issue" } else { "New issue" })
                .on_ok(move |_, window, cx| {
                    on_ok(window, cx);
                    false
                })
                .child(
                    v_flex()
                        .gap_3()
                        .child(Input::new(&t0))
                        .child(Textarea::new(&d0)),
                )
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("editor-cancel")
                                .outline()
                                .label("Cancel")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("editor-save")
                                .primary()
                                .label(if editing { "Save" } else { "Create issue" })
                                .on_click(move |_, window, cx| save(window, cx)),
                        ),
                )
        });
        // Once the dialog is up: opening it moves the focus into it.
        let focus = title.clone();
        window.defer(cx, move |window, cx| {
            focus.update(cx, |state, cx| state.focus(window, cx));
        });
    }

    /// Creates an issue, or saves the title and description of `id`.
    /// Returns whether it went ahead; a missing title doesn't.
    fn save_issue(
        &mut self,
        id: Option<String>,
        title: String,
        description: String,
        cx: &mut Context<Self>,
    ) -> bool {
        if title.is_empty() {
            self.error = Some("An issue needs a title.".into());
            cx.notify();
            return false;
        }
        let now = now_ms();
        match id {
            Some(id) => {
                let args = UpdateIssueArgs {
                    title: Some(title),
                    description: Some(description),
                    ..UpdateIssueArgs::new(id, now)
                };
                self.mutate(UPDATE_ISSUE.with(&args), cx);
            }
            None => {
                let Some(project_id) = self.project_id() else {
                    self.error = Some(format!(
                        "The {} project hasn't synced yet.",
                        self.filters.project
                    ));
                    cx.notify();
                    return false;
                };
                let id = make_id();
                let args = CreateIssueArgs {
                    id: id.clone(),
                    title,
                    description: Some(description),
                    created: now,
                    modified: now,
                    project_id,
                };
                self.mutate(CREATE_ISSUE.with(&args), cx);
                self.select(id, cx);
            }
        }
        true
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.filters.project.to_lowercase();
        let mut projects = self.projects.snapshot().value.clone().unwrap_or_default();
        projects.sort_by(|a, b| a.lower_case_name.cmp(&b.lower_case_name));
        let project = self.picker(
            Button::new("project")
                .label(self.filters.project.clone())
                .tooltip("Project"),
            projects
                .into_iter()
                .map(|p| Choice {
                    avatar: None,
                    checked: p.lower_case_name == current,
                    label: p.name.clone(),
                    value: Some(p.name),
                })
                .collect(),
            Rc::new(|this, name, _, cx| {
                if let Some(name) = name {
                    this.set_project(name, cx);
                }
            }),
            cx,
        );
        let status_button = |id: &'static str, label: &'static str, status: Status| {
            Button::new(id)
                .label(label)
                .small()
                .ghost()
                .selected(self.filters.status == status)
                .on_click(
                    cx.listener(move |this, _, _, cx| this.refilter(|f| f.status = status, cx)),
                )
        };
        let count = {
            let s = self.count.snapshot();
            match (s.value, self.list.snapshot().value.as_ref()) {
                (Some(n), _) if self.count.is_synced() || n > 0 => format!("{n}"),
                (_, Some(rows)) if !rows.is_empty() => format!("{}+", rows.len()),
                _ => String::new(),
            }
        };
        v_flex()
            .gap_2()
            .p_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .gap_1()
                    .child(project)
                    .child(status_button("status-open", "Open", Status::Open))
                    .child(status_button("status-closed", "Closed", Status::Closed))
                    .child(status_button("status-all", "All", Status::All))
                    .child(div().flex_1())
                    .child(
                        div()
                            .id("issue-count")
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(count),
                    )
                    .child(self.render_connection(cx))
                    .child(self.render_account(cx)),
            )
            .child(self.render_filters(cx))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.search).small().cleanable(true)),
                    )
                    .child(
                        Button::new("new-issue")
                            .small()
                            .primary()
                            .label("New issue")
                            .disabled(self.session.auth.is_none())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_editor(None, window, cx)
                            })),
                    ),
            )
    }

    /// `button` opens a menu of `choices`; picking one calls `on_pick` with
    /// its value.
    fn picker(
        &self,
        button: Button,
        choices: Vec<Choice>,
        on_pick: OnPick,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let app = cx.entity().downgrade();
        let choices = Rc::new(choices);
        button
            .small()
            .ghost()
            .dropdown_menu(move |menu: PopupMenu, _, _| {
                let mut menu = menu.scrollable(true).max_h(px(360.)).min_w(px(200.));
                for choice in choices.iter() {
                    let (app, value, on_pick) =
                        (app.clone(), choice.value.clone(), on_pick.clone());
                    let clears = value.is_none();
                    menu = menu.item(
                        PopupMenuItem::new(choice.label.clone())
                            .checked(choice.checked)
                            .on_click(move |_, window, cx| {
                                let _ = app.update(cx, |this, cx| {
                                    on_pick(this, value.clone(), window, cx)
                                });
                            }),
                    );
                    if clears {
                        menu = menu.separator();
                    }
                }
                menu
            })
            .into_any_element()
    }

    /// `button` opens `choices` with a search, as zbugs' comboboxes do;
    /// see `picker::searchable`.
    fn searchable(
        &self,
        id: &'static str,
        multi: bool,
        button: Button,
        choices: Vec<Choice>,
        on_pick: OnPick,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let app = cx.entity().downgrade();
        picker::searchable(
            id,
            button,
            choices,
            multi,
            Rc::new(move |value, window, cx| {
                let _ = app.update(cx, |this, cx| on_pick(this, value, window, cx));
            }),
        )
        .into_any_element()
    }

    fn user_choices(users: &Live<Vec<User>>, none: &str, current: Option<&str>) -> Vec<Choice> {
        let mut choices = vec![Choice {
            avatar: None,
            label: none.to_owned(),
            value: None,
            checked: current.is_none(),
        }];
        let mut users = users.snapshot().value.clone().unwrap_or_default();
        users.sort_by_key(|u| u.login.to_lowercase());
        choices.extend(users.into_iter().map(|u| Choice {
            avatar: Some(avatar_url(&u)),
            checked: current == Some(u.login.as_str()),
            label: u.login.clone(),
            value: Some(u.login),
        }));
        choices
    }

    fn render_filters(&self, cx: &mut Context<Self>) -> AnyElement {
        if let Some(checked) = &self.checked {
            let count = checked.len();
            return h_flex()
                .gap_1()
                .items_center()
                .child(div().px_2().text_sm().child(format!("{count} selected")))
                .child(div().flex_1())
                .child(
                    Button::new("delete-checked")
                        .small()
                        .danger()
                        .label("Delete")
                        .disabled(count == 0)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let title = match count {
                                1 => "Delete this issue?".to_owned(),
                                n => format!("Delete {n} issues?"),
                            };
                            this.confirm_delete(
                                title,
                                |this, cx| this.delete_checked(cx),
                                window,
                                cx,
                            )
                        })),
                )
                .child(
                    Button::new("stop-checking")
                        .small()
                        .ghost()
                        .label("Cancel")
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_checking(cx))),
                )
                .into_any_element();
        }
        let f = &self.filters;
        let creator = self.searchable(
            "filter-creator",
            false,
            Button::new("filter-creator")
                .label(
                    f.creator
                        .as_ref()
                        .map_or("Creator".into(), |l| format!("Creator: {l}")),
                )
                .selected(f.creator.is_some()),
            Self::user_choices(&self.creators, "Anyone", f.creator.as_deref()),
            Rc::new(|this, login, _, cx| this.refilter(|f| f.creator = login, cx)),
            cx,
        );
        let assignee = self.searchable(
            "filter-assignee",
            false,
            Button::new("filter-assignee")
                .label(
                    f.assignee
                        .as_ref()
                        .map_or("Assignee".into(), |l| format!("Assignee: {l}")),
                )
                .selected(f.assignee.is_some()),
            Self::user_choices(&self.assignees, "Anyone", f.assignee.as_deref()),
            Rc::new(|this, login, _, cx| this.refilter(|f| f.assignee = login, cx)),
            cx,
        );
        let mut names = self
            .labels
            .snapshot()
            .value
            .iter()
            .flatten()
            .map(|l| l.name.clone())
            .collect::<Vec<_>>();
        names.sort_by_key(|n| n.to_lowercase());
        let mut label_choices = vec![Choice {
            avatar: None,
            label: "Any label".into(),
            value: None,
            checked: f.labels.is_empty(),
        }];
        label_choices.extend(names.into_iter().map(|name| Choice {
            avatar: None,
            checked: f.labels.contains(&name),
            label: name.clone(),
            value: Some(name),
        }));
        let labels = self.searchable(
            "filter-labels",
            true,
            Button::new("filter-labels")
                .label(match f.labels.as_slice() {
                    [] => "Labels".into(),
                    [one] => format!("Label: {one}"),
                    many => format!("Labels: {}", many.len()),
                })
                .selected(!f.labels.is_empty()),
            label_choices,
            Rc::new(|this, name, _, cx| this.toggle_label_filter(name, cx)),
            cx,
        );
        let sort_field = Button::new("sort-field")
            .small()
            .ghost()
            .label(if f.by_created { "Created" } else { "Modified" })
            .tooltip("Sort by modified or created")
            .on_click(
                cx.listener(|this, _, _, cx| this.refilter(|f| f.by_created = !f.by_created, cx)),
            );
        let sort_direction = Button::new("sort-direction")
            .small()
            .ghost()
            .label(if f.ascending { "↑" } else { "↓" })
            .tooltip(if f.ascending {
                "Oldest first"
            } else {
                "Newest first"
            })
            .on_click(
                cx.listener(|this, _, _, cx| this.refilter(|f| f.ascending = !f.ascending, cx)),
            );
        h_flex()
            .gap_1()
            .flex_wrap()
            .child(creator)
            .child(assignee)
            .child(labels)
            .child(div().flex_1())
            .child(sort_field)
            .child(sort_direction)
            .when(self.session.auth.is_some(), |d| {
                d.child(
                    Button::new("start-checking")
                        .small()
                        .ghost()
                        .label("Select")
                        .tooltip("Select issues to delete")
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_checking(cx))),
                )
            })
            .into_any_element()
    }

    fn render_connection(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.connection.borrow().clone();
        let (color, label) = match &state {
            ConnectionState::Connected => (cx.theme().success, "Connected".to_owned()),
            ConnectionState::Connecting { .. } | ConnectionState::Initializing => {
                (cx.theme().warning, "Connecting".to_owned())
            }
            ConnectionState::NeedsAuth(_) => (cx.theme().danger, "Needs login".to_owned()),
            ConnectionState::Disconnected { reason }
            | ConnectionState::Error { reason }
            | ConnectionState::Closed { reason } => (cx.theme().danger, reason.clone()),
        };
        div()
            .id("connection")
            .size_2()
            .rounded_full()
            .bg(color)
            .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
    }

    fn render_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.list.snapshot().value.clone().unwrap_or_default();
        let len = rows.len();
        let rows = Rc::new(rows);
        if len == 0 {
            let synced = self.list.is_synced();
            return div()
                .flex_1()
                .p_4()
                .text_color(cx.theme().muted_foreground)
                .child(if synced { "No issues" } else { "Loading…" })
                .into_any_element();
        }
        let list = uniform_list(
            "issues",
            len,
            cx.processor(move |this, range: Range<usize>, window, cx| {
                if range.end + 20 >= len {
                    cx.on_next_frame(window, |this, _, cx| this.grow(cx));
                }
                range
                    .map(|ix| this.render_row(&rows[ix], cx).into_any_element())
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.scroll)
        .size_full();
        div()
            .track_focus(&self.list_focus)
            .key_context("IssueList")
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_selection(1, cx)))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.move_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                this.search.update(cx, |state, cx| state.focus(window, cx))
            }))
            .on_action(
                cx.listener(|this, _: &ToggleChecked, _, cx| this.toggle_selected_checked(cx)),
            )
            .on_action(cx.listener(|this, _: &StopChecking, _, cx| {
                if this.checked.is_some() {
                    this.toggle_checking(cx)
                }
            }))
            .w_full()
            .flex_1()
            .child(list)
            .into_any_element()
    }

    fn render_row(&self, issue: &Issue, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.selected.as_deref() == Some(issue.id.as_str());
        let unread = self.session.auth.is_some() && is_unread(issue);
        let id = issue.id.clone();
        let creator = issue
            .creator
            .as_ref()
            .map_or(String::new(), |u| u.login.clone());
        let labels = issue
            .labels
            .iter()
            .flatten()
            .map(|l| l.name.clone())
            .collect::<Vec<_>>()
            .join(", ");
        // While picking issues to delete, a click checks the issue
        // instead, if the user can delete it.
        let check = self.checked.as_ref().map(|checked| {
            (
                checked.contains(&issue.id),
                self.can_change(&issue.creator_id),
            )
        });
        let row = issue.clone();
        let content = v_flex()
            .flex_1()
            .min_w_0()
            .when(check.is_some_and(|(_, can)| !can), |d| d.opacity(0.5))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_color(cx.theme().muted_foreground)
                            .child(issue.short_id.map_or(String::new(), |n| format!("#{n}"))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(issue.title.clone()),
                    )
                    .when(!issue.open, |d| {
                        d.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("Closed"),
                        )
                    })
                    .when(unread, |d| {
                        d.child(
                            div()
                                .id(SharedString::from(format!("unread-{}", issue.id)))
                                .flex_none()
                                .size(px(7.))
                                .rounded_full()
                                .bg(cx.theme().blue)
                                .tooltip(|window, cx| {
                                    Tooltip::new("Changed since you last viewed it")
                                        .build(window, cx)
                                }),
                        )
                    }),
            )
            .child(
                h_flex()
                    .gap_2()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(creator)
                    .child(div().flex_1().min_w_0().truncate().child(labels))
                    .child(div().flex_none().child(relative_time(
                        if self.filters.by_created {
                            issue.created
                        } else {
                            issue.modified
                        },
                        now_ms(),
                    ))),
            );
        h_flex()
            .id(SharedString::from(format!("issue-{}", issue.id)))
            .w_full()
            .h(px(ROW_HEIGHT))
            .px_3()
            .gap_3()
            .overflow_hidden()
            .border_b_1()
            .border_color(cx.theme().border)
            .when(selected, |d| d.bg(cx.theme().accent))
            .hover(|d| d.bg(cx.theme().accent))
            .on_click(cx.listener(move |this, _, window, cx| {
                if this.checked.is_some() {
                    this.toggle_checked(&row, cx);
                } else {
                    this.select(id.clone(), cx);
                }
                window.focus(&this.list_focus, cx);
            }))
            .when_some(check, |d, (checked, can)| {
                d.child(
                    div()
                        .flex_none()
                        .size_4()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_sm()
                        .border_1()
                        .text_xs()
                        .when(!can, |d| d.opacity(0.4))
                        .border_color(if checked {
                            cx.theme().primary
                        } else {
                            cx.theme().border
                        })
                        .when(checked, |d| {
                            d.bg(cx.theme().primary)
                                .text_color(cx.theme().primary_foreground)
                                .child("✓")
                        }),
                )
            })
            .child(content)
    }

    /// The reactions on an issue or comment, one pill per emoji, and a
    /// menu to add one.
    fn render_reactions(
        &self,
        subject: Subject,
        subject_id: &str,
        emoji: &[Emoji],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let me = self.session.auth.as_ref().map(|a| a.sub.clone());
        let mut emoji = emoji.to_vec();
        emoji.sort_by(|a, b| a.created.total_cmp(&b.created));
        let mut groups: Vec<(String, Vec<Emoji>)> = Vec::new();
        for e in &emoji {
            let key = normalize_emoji(&e.value);
            match groups.iter_mut().find(|(k, _)| *k == key) {
                Some((_, group)) => group.push(e.clone()),
                None => groups.push((key, vec![e.clone()])),
            }
        }
        let mine = |e: &Emoji| me.as_deref() == Some(e.creator_id.as_str());
        let emoji = Rc::new(emoji);
        let tone = self.skin_tone();
        let pills = groups.into_iter().map(|(key, group)| {
            // Adds the reaction in the user's skin tone, as zbugs' pills do.
            let toned = group.iter().any(|e| e.value != key);
            let value = with_skin_tone(&key, tone, toned);
            let names = group
                .iter()
                .filter_map(|e| {
                    if mine(e) {
                        Some("you".to_owned())
                    } else {
                        e.creator.as_ref().map(|u| u.login.clone())
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            let (all, id) = (emoji.clone(), subject_id.to_owned());
            Button::new(SharedString::from(format!("emoji-{subject_id}-{key}")))
                .small()
                .outline()
                .selected(group.iter().any(mine))
                .label(format!("{} {}", group[0].value, group.len()))
                .tooltip(format!("{names} reacted with {}", group[0].annotation))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.toggle_emoji(subject, &id, &all, &value, cx)
                }))
        });
        let pills = pills.collect::<Vec<_>>();
        // The reactions in the user's skin tone, and the skin tone, as
        // zbugs' emoji picker.
        let reacted = REACTIONS
            .iter()
            .map(|(unicode, _)| {
                emoji
                    .iter()
                    .any(|e| mine(e) && normalize_emoji(&e.value) == normalize_emoji(unicode))
            })
            .collect::<Vec<_>>();
        let (all, id) = (emoji.clone(), subject_id.to_owned());
        let app = cx.entity().downgrade();
        let add = Button::new(SharedString::from(format!("react-{subject_id}")))
            .label("☺︎ +")
            .small()
            .ghost()
            .dropdown_menu(move |menu: PopupMenu, window, cx| {
                let mut menu = menu.min_w(px(200.));
                for ((unicode, annotation), checked) in REACTIONS.iter().zip(&reacted) {
                    let unicode = with_skin_tone(unicode, tone, false);
                    let (app, all, id) = (app.clone(), all.clone(), id.clone());
                    menu = menu.item(
                        PopupMenuItem::new(format!("{unicode}  {annotation}"))
                            .checked(*checked)
                            .on_click(move |_, _, cx| {
                                let _ = app.update(cx, |this, cx| {
                                    this.toggle_emoji(subject, &id, &all, &unicode, cx)
                                });
                            }),
                    );
                }
                let (picker, all, id) = (app.clone(), all.clone(), id.clone());
                menu = menu.item(PopupMenuItem::new("More reactions…").on_click(
                    move |_, window, cx| {
                        let _ = picker.update(cx, |this, cx| {
                            this.open_emoji_picker(subject, &id, &all, window, cx)
                        });
                    },
                ));
                let app = app.clone();
                menu.separator()
                    .submenu("Skin tone", window, cx, move |mut menu, _, _| {
                        for (t, name) in (0..).zip(SKIN_TONES) {
                            let app = app.clone();
                            menu = menu.item(
                                PopupMenuItem::new(format!(
                                    "{}  {name}",
                                    with_skin_tone("👍", t, false)
                                ))
                                .checked(t == tone)
                                .on_click(move |_, _, cx| {
                                    let _ = app.update(cx, |this, cx| this.set_skin_tone(t, cx));
                                }),
                            );
                        }
                        menu
                    })
            });
        h_flex()
            .gap_1()
            .flex_wrap()
            .children(pills)
            .when(self.session.auth.is_some(), |d| d.child(add))
            .into_any_element()
    }

    fn render_detail(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let empty = |text: &'static str, cx: &mut Context<Self>| {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(cx.theme().muted_foreground)
                .child(text)
                .into_any_element()
        };
        let Some(detail) = &self.detail else {
            return empty("Select an issue", cx);
        };
        let issue = detail.snapshot().value.clone().flatten();
        let Some(issue) = issue else {
            return empty(
                if detail.is_synced() {
                    "Issue not found"
                } else {
                    "Loading…"
                },
                cx,
            );
        };
        // The newest comments first; shown oldest first. The page takes
        // over once it has at least what the issue brought.
        let mut comments: Vec<Comment> = issue.comments.clone().unwrap_or_default();
        if let Some(page) = self
            .comment_page
            .as_ref()
            .and_then(|p| p.snapshot().value.clone())
            && page.len() >= comments.len()
        {
            comments = page;
        }
        let more = comments.len() as i64 >= self.comments_limit;
        comments.reverse();
        let who = |u: &Option<Box<zbugs_model::schema::User>>| {
            u.as_ref().map_or("nobody".to_owned(), |u| u.login.clone())
        };
        let toggle = issue.clone();
        let edit = issue.clone();
        let assignee_id = issue.assignee_id.clone();
        let mut assignee_choices = vec![Choice {
            avatar: None,
            label: "Unassigned".into(),
            value: None,
            checked: assignee_id.is_none(),
        }];
        let mut crew = self.crew.snapshot().value.clone().unwrap_or_default();
        crew.sort_by_key(|u| u.login.to_lowercase());
        assignee_choices.extend(crew.into_iter().map(|u| Choice {
            avatar: Some(avatar_url(&u)),
            checked: assignee_id.as_deref() == Some(u.id.as_str()),
            label: u.login,
            value: Some(u.id),
        }));
        let logged_in = self.session.auth.is_some();
        let id = issue.id.clone();
        let assignee = self.searchable(
            "detail-assignee",
            false,
            Button::new("detail-assignee")
                .label(match &issue.assignee {
                    Some(u) => format!("Assigned to {}", u.login),
                    None => "Unassigned".into(),
                })
                .disabled(!logged_in),
            assignee_choices,
            Rc::new(move |this, user_id, _, cx| this.set_assignee(id.clone(), user_id, cx)),
            cx,
        );
        let mut all_labels = self.labels.snapshot().value.clone().unwrap_or_default();
        all_labels.sort_by_key(|l| l.name.to_lowercase());
        let mut label_choices: Vec<Choice> = all_labels
            .into_iter()
            .map(|l| Choice {
                avatar: None,
                checked: issue.labels.iter().flatten().any(|has| has.id == l.id),
                label: l.name,
                value: Some(l.id),
            })
            .collect();
        // The crew can make a new label, as in the web app's label picker.
        if self.session.auth.as_ref().is_some_and(|a| a.is_crew()) {
            label_choices.push(Choice {
                avatar: None,
                label: "New label…".into(),
                value: Some(NEW_LABEL.into()),
                checked: false,
            });
        }
        let subscribed = issue
            .notification_state
            .as_ref()
            .is_some_and(|n| n.subscribed);
        let notify_id = issue.id.clone();
        let notifications = self.picker(
            Button::new("detail-notifications")
                .label(if subscribed {
                    "Subscribed"
                } else {
                    "Not subscribed"
                })
                .tooltip("Notifications")
                .disabled(!logged_in),
            vec![
                Choice {
                    avatar: None,
                    label: "Subscribed".into(),
                    value: Some("subscribe".into()),
                    checked: subscribed,
                },
                Choice {
                    avatar: None,
                    label: "Unsubscribed".into(),
                    value: Some("unsubscribe".into()),
                    checked: !subscribed,
                },
            ],
            Rc::new(move |this, value, _, cx| {
                let subscribe = value.as_deref() == Some("subscribe");
                if subscribe != subscribed {
                    this.set_subscribed(notify_id.clone(), subscribe, cx);
                }
            }),
            cx,
        );
        let crew = self.session.auth.as_ref().is_some_and(|a| a.is_crew());
        let internal = issue.visibility == IssueVisibility::Internal;
        let visibility_id = issue.id.clone();
        let visibility = crew.then(|| {
            self.picker(
                Button::new("detail-visibility")
                    .label(if internal { "Internal" } else { "Public" })
                    .tooltip("Visibility"),
                vec![
                    Choice {
                        avatar: None,
                        label: "Public".into(),
                        value: Some("public".into()),
                        checked: !internal,
                    },
                    Choice {
                        avatar: None,
                        label: "Internal".into(),
                        value: Some("internal".into()),
                        checked: internal,
                    },
                ],
                Rc::new(move |this, value, _, cx| {
                    let to_internal = value.as_deref() == Some("internal");
                    if to_internal != internal {
                        this.set_visibility(visibility_id.clone(), to_internal, cx);
                    }
                }),
                cx,
            )
        });
        let labeled = issue.clone();
        let labels = self.searchable(
            "detail-labels",
            true,
            Button::new("detail-labels")
                .label("Labels…")
                .disabled(!logged_in),
            label_choices,
            Rc::new(
                move |this, label_id, window, cx| match label_id.as_deref() {
                    Some(NEW_LABEL) => this.open_new_label(labeled.id.clone(), window, cx),
                    Some(_) => this.toggle_label(&labeled, label_id.unwrap_or_default(), cx),
                    None => {}
                },
            ),
            cx,
        );
        v_flex()
            .id("detail")
            .track_focus(&self.detail_focus)
            .key_context("IssueDetail")
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_selection(1, cx)))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.move_selection(-1, cx)))
            .size_full()
            .overflow_y_scroll()
            .p_6()
            .gap_4()
            .child(
                h_flex()
                    .gap_3()
                    .items_start()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(issue.title.clone()),
                    )
                    .child(
                        Button::new("edit-issue")
                            .small()
                            .outline()
                            .label("Edit")
                            .disabled(!logged_in)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_editor(Some(edit.clone()), window, cx)
                            })),
                    )
                    .child(
                        Button::new("toggle-open")
                            .small()
                            .label(if issue.open { "Close issue" } else { "Reopen" })
                            .disabled(!logged_in)
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.toggle_open(&toggle, cx)),
                            ),
                    )
                    .when(self.can_change(&issue.creator_id), |row| {
                        let id = issue.id.clone();
                        row.child(
                            Button::new("delete-issue")
                                .small()
                                .outline()
                                .label("Delete")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    let id = id.clone();
                                    this.confirm_delete(
                                        "Delete this issue?",
                                        move |this, cx| this.delete_issue(&id, cx),
                                        window,
                                        cx,
                                    )
                                })),
                        )
                    }),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_3()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .children(issue.short_id.map(|n| format!("#{n}")))
                    .child(if issue.open { "Open" } else { "Closed" })
                    .child(
                        h_flex()
                            .gap_1()
                            .child("by")
                            .children(issue.creator.as_deref().map(|u| avatar(u, px(18.))))
                            .child(who(&issue.creator)),
                    )
                    .child(format!(
                        "updated {}",
                        relative_time(issue.modified, now_ms())
                    ))
                    .child(assignee)
                    .child(labels)
                    .child(notifications)
                    .children(visibility)
                    .children(issue.labels.iter().flatten().map(|l| {
                        div()
                            .px_1()
                            .rounded_sm()
                            .bg(cx.theme().secondary)
                            .child(l.name.clone())
                    })),
            )
            .child(markdown(
                format!("description-{}", issue.id),
                &issue.description,
                window,
                cx,
            ))
            .child(self.render_reactions(
                Subject::Issue,
                &issue.id,
                issue.emoji.as_deref().unwrap_or_default(),
                cx,
            ))
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(format!(
                        "Comments ({}{})",
                        comments.len(),
                        if more { "+" } else { "" }
                    )),
            )
            .when(more, |d| {
                d.child(
                    Button::new("older-comments")
                        .small()
                        .ghost()
                        .label("Show older comments")
                        .on_click(cx.listener(|this, _, _, cx| this.show_older_comments(cx))),
                )
            })
            .children(comments.into_iter().map(|c| {
                let reactions = self.render_reactions(
                    Subject::Comment,
                    &c.id,
                    c.emoji.as_deref().unwrap_or_default(),
                    cx,
                );
                let (edit, remove) = (c.clone(), c.id.clone());
                v_flex()
                    .gap_1()
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .gap_1()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .children(c.creator.as_deref().map(|u| avatar(u, px(18.))))
                            .child(who(&c.creator))
                            .child(div().flex_1().child(relative_time(c.created, now_ms())))
                            .when(self.can_change(&c.creator_id), |row| {
                                row.child(
                                    Button::new(SharedString::from(format!("edit-{}", c.id)))
                                        .xsmall()
                                        .ghost()
                                        .label("Edit")
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.open_comment_editor(edit.clone(), window, cx)
                                        })),
                                )
                                .child(
                                    Button::new(SharedString::from(format!("delete-{}", c.id)))
                                        .xsmall()
                                        .ghost()
                                        .label("Delete")
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            let id = remove.clone();
                                            this.confirm_delete(
                                                "Delete this comment?",
                                                move |this, cx| this.delete_comment(&id, cx),
                                                window,
                                                cx,
                                            )
                                        })),
                                )
                            }),
                    )
                    .child(markdown(format!("comment-{}", c.id), &c.body, window, cx))
                    .child(reactions)
            }))
            .child(
                h_flex()
                    .gap_2()
                    .items_end()
                    .child(
                        div()
                            .flex_1()
                            .key_context("CommentBox")
                            .on_action(cx.listener(|this, _: &Confirm, window, cx| {
                                this.post_comment(window, cx)
                            }))
                            .child(Textarea::new(&self.comment)),
                    )
                    .child(
                        Button::new("post-comment")
                            .primary()
                            .label("Comment")
                            .tooltip(if cx.platform_info().is_mac() {
                                "⌘↩"
                            } else {
                                "Ctrl+Enter"
                            })
                            .disabled(!logged_in)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.post_comment(window, cx)),
                            ),
                    ),
            )
            .into_any_element()
    }
}

impl Render for ZbugsApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(message) = self.error.take() {
            window.push_notification(Notification::error(message), cx);
        }
        if self.reveal_selected {
            let at = self.list.snapshot().value.as_ref().and_then(|rows| {
                rows.iter()
                    .position(|r| Some(&r.id) == self.selected.as_ref())
            });
            if let Some(ix) = at {
                self.scroll.scroll_to_item(ix, ScrollStrategy::Center);
                self.reveal_selected = false;
            }
        }
        self.keep_scroll_anchor();
        self.notice_activity(window, cx);
        let title = self.window_title();
        if title != self.title {
            window.set_window_title(&title);
            self.title = title;
        }
        h_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                v_flex()
                    .flex_none()
                    .w(px(420.))
                    .h_full()
                    .border_r_1()
                    .border_color(cx.theme().border)
                    .child(self.render_toolbar(cx))
                    .child(self.render_list(cx)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_hidden()
                    .child(self.render_detail(window, cx)),
            )
    }
}
