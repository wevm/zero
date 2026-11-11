//! The window: the issue list with its filters on the left, the selected
//! issue on the right, as zbugs' list and issue pages.

use crate::live::Live;
use crate::session::{Session, now_ms};
use futures::StreamExt;
use futures::channel::mpsc;
use gpui_kit::component::button::*;
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::text::TextView;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;
use zbugs_model::args::*;
use zbugs_model::mutators::{ADD_COMMENT, CREATE_ISSUE, MutateRequest, UPDATE_ISSUE};
use zbugs_model::queries::{ALL_PROJECTS, ISSUE_DETAIL, ISSUE_LIST_COUNT, ISSUE_LIST_V2};
use zbugs_model::schema::{Comment, Issue, Project};
use zbugs_model::{ZERO_PROJECT_NAME, make_id};
use zero_client::client::{ConnectionState, MutationOutcome};
use zero_client::ttl::Ttl;

/// Rows the list asks for at a time, as zbugs' list pages.
const PAGE: i64 = 100;
const ROW_HEIGHT: f32 = 52.;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Open,
    Closed,
    All,
}

pub struct ZbugsApp {
    session: Session,
    status: Status,
    text: String,
    limit: i64,
    list: Live<Vec<Issue>>,
    count: Live<i64>,
    projects: Live<Vec<Project>>,
    selected: Option<String>,
    detail: Option<Live<Option<Issue>>>,
    search: Entity<InputState>,
    comment: Entity<InputState>,
    scroll: UniformListScrollHandle,
    connection: Rc<RefCell<ConnectionState>>,
    error: Option<String>,
    _unsubscribe: Box<dyn Fn()>,
    _tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

fn ttl() -> Ttl {
    Ttl::minutes(5.0)
}

impl ZbugsApp {
    pub fn new(session: Session, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search issues"));
        let comment = cx.new(|cx| InputState::new(window, cx).placeholder("Add a comment…"));
        let subscriptions = vec![
            cx.subscribe_in(&search, window, |this, state, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.text = state.read(cx).value().to_string();
                    this.refilter(cx);
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

        let status = Status::Open;
        let (list, count) = Self::queries(&session, status, "", PAGE, cx);
        let projects = Live::new(
            &session.zero,
            ALL_PROJECTS.call(&(), session.auth.as_ref()),
            &ttl(),
            cx,
        );
        ZbugsApp {
            session,
            status,
            text: String::new(),
            limit: PAGE,
            list,
            count,
            projects,
            selected: None,
            detail: None,
            search,
            comment,
            scroll: UniformListScrollHandle::new(),
            connection,
            error: None,
            _unsubscribe: unsubscribe,
            _tasks: vec![watch],
            _subscriptions: subscriptions,
        }
    }

    fn list_context(status: Status, text: &str) -> ListContextParams {
        ListContextParams {
            open: match status {
                Status::Open => Some(true),
                Status::Closed => Some(false),
                Status::All => None,
            },
            project_name: Some(ZERO_PROJECT_NAME.into()),
            assignee: None,
            creator: None,
            labels: None,
            text_filter: (!text.is_empty()).then(|| text.to_owned()),
            sort_field: SortField::Modified,
            sort_direction: SortDirection::Desc,
        }
    }

    fn queries(
        session: &Session,
        status: Status,
        text: &str,
        limit: i64,
        cx: &mut Context<Self>,
    ) -> (Live<Vec<Issue>>, Live<i64>) {
        let auth = session.auth.as_ref();
        let list_context = Self::list_context(status, text);
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

    /// The filters changed: the list starts over at its top.
    fn refilter(&mut self, cx: &mut Context<Self>) {
        self.limit = PAGE;
        let (list, count) = Self::queries(&self.session, self.status, &self.text, self.limit, cx);
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
        let (list, _) = Self::queries(&self.session, self.status, &self.text, self.limit, cx);
        self.list = list;
        cx.notify();
    }

    fn set_status(&mut self, status: Status, cx: &mut Context<Self>) {
        if self.status != status {
            self.status = status;
            self.refilter(cx);
        }
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

    fn toggle_open(&mut self, issue: &Issue, cx: &mut Context<Self>) {
        let args = UpdateIssueArgs {
            open: Some(!issue.open),
            ..UpdateIssueArgs::new(issue.id.clone(), now_ms())
        };
        self.mutate(UPDATE_ISSUE.with(&args), cx);
    }

    fn post_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(issue_id) = self.selected.clone() else {
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
                .selected(self.status == status)
                .on_click(cx.listener(move |this, _, _, cx| this.set_status(status, cx)))
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
                    .child(self.render_connection(cx)),
            )
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
        uniform_list(
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
        .w_full()
        .flex_1()
        .into_any_element()
    }

    fn render_row(&self, issue: &Issue, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.selected.as_deref() == Some(issue.id.as_str());
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
            .on_click(cx.listener(move |this, _, _, cx| this.select(id.clone(), cx)))
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
        let mut comments: Vec<Comment> = issue.comments.clone().unwrap_or_default();
        // The latest 50, newest first; shown oldest first.
        comments.reverse();
        let who = |u: &Option<Box<zbugs_model::schema::User>>| {
            u.as_ref().map_or("nobody".to_owned(), |u| u.login.clone())
        };
        let toggle = issue.clone();
        let edit = issue.clone();
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
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_editor(Some(edit.clone()), window, cx)
                            })),
                    )
                    .child(
                        Button::new("toggle-open")
                            .small()
                            .label(if issue.open { "Close issue" } else { "Reopen" })
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.toggle_open(&toggle, cx)),
                            ),
                    ),
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
                    .child(format!("assigned to {}", who(&issue.assignee)))
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
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(format!("Comments ({})", comments.len())),
            )
            .children(comments.into_iter().map(|c| {
                v_flex()
                    .gap_1()
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(who(&c.creator)),
                    )
                    .child(TextView::markdown(
                        SharedString::from(format!("comment-{}", c.id)),
                        c.body,
                    ))
            }))
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().child(Input::new(&self.comment)))
                    .child(
                        Button::new("post-comment")
                            .primary()
                            .label("Comment")
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
