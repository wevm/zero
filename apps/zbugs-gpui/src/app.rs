//! The window: the issue list with its filters on the left, the selected
//! issue on the right, as zbugs' list and issue pages.

#[cfg(feature = "e2e")]
pub mod e2e;

use crate::live::Live;
use crate::session::{Session, app_dir, now_ms, save_setting};
use futures::StreamExt;
use futures::channel::mpsc;
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
use std::ops::Range;
use std::rc::Rc;
use zbugs_model::args::*;
use zbugs_model::mutators::{
    ADD_COMMENT, ADD_EMOJI_TO_COMMENT, ADD_EMOJI_TO_ISSUE, ADD_LABEL, CREATE_ISSUE, DELETE_ISSUE,
    EDIT_COMMENT, MutateRequest, REMOVE_COMMENT, REMOVE_EMOJI, REMOVE_LABEL, SET_VIEW_STATE,
    UPDATE_ISSUE, UPDATE_NOTIFICATION,
};
use zbugs_model::queries::{
    ALL_PROJECTS, COMMENTS_PAGE, ISSUE_DETAIL, ISSUE_LIST_COUNT, ISSUE_LIST_V2, LABELS, USER,
    USERS_FOR_PROJECT,
};
use zbugs_model::schema::{Comment, Emoji, Issue, IssueVisibility, Label, Project, User};
use zbugs_model::{AuthData, ZERO_PROJECT_NAME, make_id};
use zero_client::client::{ConnectionState, MutationOutcome};
use zero_client::ttl::Ttl;

/// Rows the list asks for at a time, as zbugs' list pages.
const PAGE: i64 = 100;
const ROW_HEIGHT: f32 = 52.;
/// Comments `issueDetail` loads, and how many more "Show older comments"
/// adds.
const COMMENT_PAGE: i64 = 50;

actions!(zbugs, [SelectNext, SelectPrevious, FocusSearch]);

/// j/k and the arrow keys move the selection while the list has focus, and
/// `/` goes to the search field, as on the web app's list.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("/", FocusSearch, Some("IssueList")),
        KeyBinding::new("j", SelectNext, Some("IssueList")),
        KeyBinding::new("down", SelectNext, Some("IssueList")),
        KeyBinding::new("k", SelectPrevious, Some("IssueList")),
        KeyBinding::new("up", SelectPrevious, Some("IssueList")),
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
}

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

/// One entry of a picker menu: `value` is what picking it passes on,
/// `None` for the entry that clears the choice.
struct Choice {
    label: String,
    value: Option<String>,
    checked: bool,
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

type OnPick = Rc<dyn Fn(&mut ZbugsApp, Option<String>, &mut Context<ZbugsApp>)>;

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
    selected: Option<String>,
    detail: Option<Live<Option<Issue>>>,
    /// How many of the selected issue's comments to show, newest first.
    comments_limit: i64,
    /// Past the comments `issueDetail` brings: `commentsPage`, newest
    /// first, up to `comments_limit`.
    comment_page: Option<Live<Vec<Comment>>>,
    list_focus: FocusHandle,
    /// Scroll to the selected issue once the list has it: after a launch
    /// restores the selection.
    reveal_selected: bool,
    /// The first rows on screen and where they were, to keep them on
    /// screen when rows above them come and go.
    anchor: Vec<(String, usize)>,
    search: Entity<InputState>,
    comment: Entity<InputState>,
    scroll: UniformListScrollHandle,
    connection: Rc<RefCell<ConnectionState>>,
    error: Option<String>,
    /// Marks the selected issue viewed after a second (`mark_viewed`).
    view_timer: Option<(String, Task<()>)>,
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
            },
            |s| s.filters.clone(),
        );
        let limit = saved.as_ref().map_or(PAGE, |s| s.limit.max(PAGE));
        let search = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Search issues")
                .default_value(filters.text.clone())
        });
        let comment = cx.new(|cx| InputState::new(window, cx).placeholder("Add a comment…"));
        let subscriptions = vec![
            cx.subscribe_in(&search, window, |this, state, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = state.read(cx).value().to_string();
                    this.refilter(|f| f.text = text, cx);
                }
            }),
            cx.subscribe_in(&comment, window, |this, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.post_comment(window, cx);
                }
            }),
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
        let users = |filter, cx: &mut Context<Self>| {
            let args = UsersForProjectArgs {
                disabled: false,
                login: None,
                project_name: ZERO_PROJECT_NAME.into(),
                filter: Some(filter),
            };
            Live::new(
                &session.zero,
                USERS_FOR_PROJECT.call(&args, auth),
                &ttl(),
                cx,
            )
        };
        let creators = users(UsersFilter::Creators, cx);
        let assignees = users(UsersFilter::Assignees, cx);
        let crew = users(UsersFilter::Crew, cx);
        let labels = Live::new(
            &session.zero,
            LABELS.call(
                &ProjectArgs {
                    project_name: ZERO_PROJECT_NAME.into(),
                },
                auth,
            ),
            &ttl(),
            cx,
        );
        let me = session
            .auth
            .as_ref()
            .map(|a| Live::new(&session.zero, USER.call(&a.sub, Some(a)), &ttl(), cx));
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
            selected: None,
            detail: None,
            comments_limit: COMMENT_PAGE,
            comment_page: None,
            list_focus: cx.focus_handle(),
            reveal_selected: false,
            anchor: Vec::new(),
            search,
            comment,
            scroll: UniformListScrollHandle::new(),
            connection,
            error: None,
            view_timer: None,
            _unsubscribe: unsubscribe,
            _tasks: vec![watch],
            _subscriptions: subscriptions,
        };
        if let Some(id) = saved.and_then(|s| s.selected) {
            app.select(id, cx);
            app.reveal_selected = true;
        }
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

    fn list_context(f: &Filters) -> ListContextParams {
        ListContextParams {
            open: match f.status {
                Status::Open => Some(true),
                Status::Closed => Some(false),
                Status::All => None,
            },
            project_name: Some(ZERO_PROJECT_NAME.into()),
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
        title: &'static str,
        delete: impl Fn(&mut ZbugsApp, &mut Context<ZbugsApp>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let app = cx.entity().downgrade();
        let delete = Rc::new(delete);
        window.open_dialog(cx, move |dialog, _, _| {
            let (app, delete) = (app.clone(), delete.clone());
            dialog
                .w(px(420.))
                .title(title)
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
        if self.selected.as_ref() == Some(id) {
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
                if this.selected.as_ref() == Some(&id) {
                    let args = ViewStateArgs {
                        issue_id: id,
                        viewed: now_ms(),
                    };
                    this.mutate(SET_VIEW_STATE.with(&args), cx);
                }
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
            .find(|(u, _)| *u == unicode)
            .map(|(_, a)| a.to_string())
            .or_else(|| {
                emoji
                    .iter()
                    .find(|e| e.value == unicode)
                    .map(|e| e.annotation.clone())
            })
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
        let name = ZERO_PROJECT_NAME.to_lowercase();
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
                    self.error = Some("The Zero project hasn't synced yet.".into());
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
                            .on_click(move |_, _, cx| {
                                let _ = app.update(cx, |this, cx| on_pick(this, value.clone(), cx));
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

    fn user_choices(users: &Live<Vec<User>>, none: &str, current: Option<&str>) -> Vec<Choice> {
        let mut choices = vec![Choice {
            label: none.to_owned(),
            value: None,
            checked: current.is_none(),
        }];
        let mut users = users.snapshot().value.clone().unwrap_or_default();
        users.sort_by_key(|u| u.login.to_lowercase());
        choices.extend(users.into_iter().map(|u| Choice {
            checked: current == Some(u.login.as_str()),
            label: u.login.clone(),
            value: Some(u.login),
        }));
        choices
    }

    fn render_filters(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let f = &self.filters;
        let creator = self.picker(
            Button::new("filter-creator")
                .label(
                    f.creator
                        .as_ref()
                        .map_or("Creator".into(), |l| format!("Creator: {l}")),
                )
                .selected(f.creator.is_some()),
            Self::user_choices(&self.creators, "Anyone", f.creator.as_deref()),
            Rc::new(|this, login, cx| this.refilter(|f| f.creator = login, cx)),
            cx,
        );
        let assignee = self.picker(
            Button::new("filter-assignee")
                .label(
                    f.assignee
                        .as_ref()
                        .map_or("Assignee".into(), |l| format!("Assignee: {l}")),
                )
                .selected(f.assignee.is_some()),
            Self::user_choices(&self.assignees, "Anyone", f.assignee.as_deref()),
            Rc::new(|this, login, cx| this.refilter(|f| f.assignee = login, cx)),
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
            label: "Any label".into(),
            value: None,
            checked: f.labels.is_empty(),
        }];
        label_choices.extend(names.into_iter().map(|name| Choice {
            checked: f.labels.contains(&name),
            label: name.clone(),
            value: Some(name),
        }));
        let labels = self.picker(
            Button::new("filter-labels")
                .label(match f.labels.as_slice() {
                    [] => "Labels".into(),
                    [one] => format!("Label: {one}"),
                    many => format!("Labels: {}", many.len()),
                })
                .selected(!f.labels.is_empty()),
            label_choices,
            Rc::new(|this, name, cx| this.toggle_label_filter(name, cx)),
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
        v_flex()
            .id(SharedString::from(format!("issue-{}", issue.id)))
            .w_full()
            .h(px(ROW_HEIGHT))
            .px_3()
            .overflow_hidden()
            .justify_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .when(selected, |d| d.bg(cx.theme().accent))
            .hover(|d| d.bg(cx.theme().accent))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.select(id.clone(), cx);
                window.focus(&this.list_focus, cx);
            }))
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
                    .child(div().min_w_0().truncate().child(labels)),
            )
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
        let pills = groups.into_iter().map(|(key, group)| {
            let value = group[0].value.clone();
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
                .label(format!("{value} {}", group.len()))
                .tooltip(format!("{names} reacted with {}", group[0].annotation))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.toggle_emoji(subject, &id, &all, &value, cx)
                }))
        });
        let pills = pills.collect::<Vec<_>>();
        let choices = REACTIONS
            .iter()
            .map(|(unicode, annotation)| Choice {
                label: format!("{unicode}  {annotation}"),
                value: Some((*unicode).to_owned()),
                checked: emoji
                    .iter()
                    .any(|e| mine(e) && normalize_emoji(&e.value) == normalize_emoji(unicode)),
            })
            .collect();
        let (all, id) = (emoji.clone(), subject_id.to_owned());
        let add = self.picker(
            Button::new(SharedString::from(format!("react-{subject_id}"))).label("☺︎ +"),
            choices,
            Rc::new(move |this, unicode, cx| {
                if let Some(unicode) = unicode {
                    this.toggle_emoji(subject, &id, &all, &unicode, cx);
                }
            }),
            cx,
        );
        h_flex()
            .gap_1()
            .flex_wrap()
            .children(pills)
            .when(self.session.auth.is_some(), |d| d.child(add))
            .into_any_element()
    }

    fn render_detail(&self, cx: &mut Context<Self>) -> AnyElement {
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
            label: "Unassigned".into(),
            value: None,
            checked: assignee_id.is_none(),
        }];
        let mut crew = self.crew.snapshot().value.clone().unwrap_or_default();
        crew.sort_by_key(|u| u.login.to_lowercase());
        assignee_choices.extend(crew.into_iter().map(|u| Choice {
            checked: assignee_id.as_deref() == Some(u.id.as_str()),
            label: u.login,
            value: Some(u.id),
        }));
        let logged_in = self.session.auth.is_some();
        let id = issue.id.clone();
        let assignee = self.picker(
            Button::new("detail-assignee")
                .label(match &issue.assignee {
                    Some(u) => format!("Assigned to {}", u.login),
                    None => "Unassigned".into(),
                })
                .disabled(!logged_in),
            assignee_choices,
            Rc::new(move |this, user_id, cx| this.set_assignee(id.clone(), user_id, cx)),
            cx,
        );
        let mut all_labels = self.labels.snapshot().value.clone().unwrap_or_default();
        all_labels.sort_by_key(|l| l.name.to_lowercase());
        let label_choices = all_labels
            .into_iter()
            .map(|l| Choice {
                checked: issue.labels.iter().flatten().any(|has| has.id == l.id),
                label: l.name,
                value: Some(l.id),
            })
            .collect();
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
                    label: "Subscribed".into(),
                    value: Some("subscribe".into()),
                    checked: subscribed,
                },
                Choice {
                    label: "Unsubscribed".into(),
                    value: Some("unsubscribe".into()),
                    checked: !subscribed,
                },
            ],
            Rc::new(move |this, value, cx| {
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
                        label: "Public".into(),
                        value: Some("public".into()),
                        checked: !internal,
                    },
                    Choice {
                        label: "Internal".into(),
                        value: Some("internal".into()),
                        checked: internal,
                    },
                ],
                Rc::new(move |this, value, cx| {
                    let to_internal = value.as_deref() == Some("internal");
                    if to_internal != internal {
                        this.set_visibility(visibility_id.clone(), to_internal, cx);
                    }
                }),
                cx,
            )
        });
        let labeled = issue.clone();
        let labels = self.picker(
            Button::new("detail-labels")
                .label("Labels…")
                .disabled(!logged_in),
            label_choices,
            Rc::new(move |this, label_id, cx| {
                if let Some(label_id) = label_id {
                    this.toggle_label(&labeled, label_id, cx);
                }
            }),
            cx,
        );
        v_flex()
            .id("detail")
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
                    .child(format!("by {}", who(&issue.creator)))
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
            .child(TextView::markdown(
                SharedString::from(format!("description-{}", issue.id)),
                issue.description.clone(),
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
                            .child(div().flex_1().child(who(&c.creator)))
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
                    .child(TextView::markdown(
                        SharedString::from(format!("comment-{}", c.id)),
                        c.body,
                    ))
                    .child(reactions)
            }))
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().child(Input::new(&self.comment)))
                    .child(
                        Button::new("post-comment")
                            .primary()
                            .label("Comment")
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
        self.mark_viewed(cx);
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
                    .child(self.render_detail(cx)),
            )
    }
}
