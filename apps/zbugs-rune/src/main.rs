//! zbugs on the Zero Rust client, drawn with Rune.
//!
//! The state here is the app's: what the user chose, and the rows the sync
//! thread (`sync.rs`) last sent for each query. The views (`view.rs`) read
//! it; their handlers change it and ask the sync thread for other rows or
//! to run a mutator.

mod login;
mod settings;
mod sync;
mod token;
mod view;

use rune::{AppSpec, TextEdit, Tx};
use settings::Settings;
use sync::{Session, Setup, SyncThread};
use zbugs_model::args::{
    AddCommentArgs, AddEmojiArgs, CommentsPageArgs, CreateAndAddLabelArgs, CreateIssueArgs,
    EditCommentArgs, IdField, IssueDetailArgs, IssueKey, IssueLabelArgs, IssueListCountArgs,
    IssueListV2Args, LabelsOrderByNameArgs, ListContextParams, PageDirection, ProjectArgs,
    RemoveLabelArgs, SortDirection, SortField, UpdateIssueArgs, UsersFilter, UsersForProjectArgs,
};
use zbugs_model::mutators::{
    ADD_COMMENT, ADD_EMOJI_TO_COMMENT, ADD_EMOJI_TO_ISSUE, ADD_LABEL, CREATE_AND_ADD_LABEL,
    CREATE_ISSUE, DELETE_ISSUE, EDIT_COMMENT, REMOVE_COMMENT, REMOVE_EMOJI, REMOVE_LABEL,
    UPDATE_ISSUE,
};
use zbugs_model::schema::{Comment, Emoji, Issue, Label, Project, User};
use zbugs_model::{AuthData, Context, MutateRequest, ZERO_PROJECT_NAME, make_id, queries};
use zero_client::client::ConnectionState;
use zero_client::ivm::ResultType;
use zero_client::query::AnyQuery;

/// Rows the issue list asks for at a time: as it nears the end, it asks for
/// this many more.
const LIST_PAGE: i64 = 100;
/// Comments, likewise.
const COMMENTS_PAGE: i64 = 50;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Open,
    Closed,
    All,
}

impl Status {
    pub const ALL: [Status; 3] = [Status::Open, Status::Closed, Status::All];

    pub fn title(self) -> &'static str {
        match self {
            Status::Open => "Open",
            Status::Closed => "Closed",
            Status::All => "All",
        }
    }

    fn open(self) -> Option<bool> {
        match self {
            Status::Open => Some(true),
            Status::Closed => Some(false),
            Status::All => None,
        }
    }
}

/// The issue list's filters and sort.
#[derive(Clone, Debug, PartialEq)]
pub struct Filter {
    pub status: Status,
    pub sort_field: SortField,
    pub sort_direction: SortDirection,
    pub created_by_me: bool,
    pub assigned_to_me: bool,
}

/// The reactions the emoji menu offers, with their CLDR names, which the
/// web app shows on hover.
pub const REACTIONS: [(&str, &str); 8] = [
    ("👍", "thumbs up"),
    ("👎", "thumbs down"),
    ("😄", "grinning face with smiling eyes"),
    ("🎉", "party popper"),
    ("😕", "confused face"),
    ("❤️", "red heart"),
    ("🚀", "rocket"),
    ("👀", "eyes"),
];

/// A dropdown on the issue page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Picker {
    Assignee,
    Labels,
    /// Reactions to the issue (its ID) or to one of its comments.
    Emoji(String),
}

/// A deletion waiting for the user to confirm it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Confirm {
    DeleteIssue(String),
    DeleteComment(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sheet {
    NewIssue,
    EditIssue,
    NewLabel,
    Login,
    Servers,
}

pub struct App {
    ui: rune::AppHandle<App>,
    sync: SyncThread<App, Context>,
    /// The settings in use, and those saved (without the environment's).
    settings: Settings,
    saved: Settings,
    pub auth: Option<AuthData>,
    pub connection: ConnectionState,

    pub filter: Filter,
    pub search: TextEdit,
    pub issues: Vec<Issue>,
    pub issues_result: ResultType,
    /// How many rows the list asks for.
    limit: i64,
    pub count: Option<i64>,
    /// The logged-in user, for the "by me" filters.
    pub me: Option<User>,
    pub projects: Vec<Project>,

    pub selected: Option<String>,
    /// Scroll the list to the selected issue once it's there (a relaunch).
    reveal_selected: bool,
    pub detail: Option<Issue>,
    pub detail_result: ResultType,
    pub comments: Vec<Comment>,
    comments_limit: i64,
    pub comment: TextEdit,
    /// The comment being edited in place, and its text.
    pub editing_comment: Option<String>,
    pub comment_edit: TextEdit,
    pub confirm: Option<Confirm>,
    pub picker: Option<Picker>,
    /// Who an issue can be assigned to, by login.
    pub crew: Vec<User>,
    /// The labels of the selected issue's project, by name.
    pub labels: Vec<Label>,
    labels_project: Option<String>,

    pub sheet: Option<Sheet>,
    pub title_edit: TextEdit,
    pub description_edit: TextEdit,
    pub token_edit: TextEdit,
    pub label_edit: TextEdit,
    pub cache_edit: TextEdit,
    pub api_edit: TextEdit,
    pub sheet_error: Option<String>,
    /// Why the last mutation failed, until seen.
    pub error: Option<String>,
    /// A GitHub login waiting for the browser.
    pub github_login: Option<login::LoopbackLogin>,
}

fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_millis() as f64)
}

fn preload(auth: &Context) -> Vec<AnyQuery> {
    let project = ProjectArgs {
        project_name: ZERO_PROJECT_NAME.to_owned(),
    };
    let auth = auth.as_ref();
    vec![
        queries::ISSUE_PRELOAD_V2.call(&project, auth).into_any(),
        queries::ALL_USERS.call(&(), auth).into_any(),
        queries::ALL_LABELS.call(&(), auth).into_any(),
        queries::ALL_PROJECTS.call(&(), auth).into_any(),
    ]
}

impl App {
    fn new(ui: rune::AppHandle<App>) -> App {
        let (settings, saved) = Settings::load();
        let sync = SyncThread::start(
            ui.clone(),
            Setup {
                schema: zbugs_model::schema::schema,
                mutators: zbugs_model::client_mutators,
                preload,
                storage_key: "zbugs-rune",
                storage_dir: settings::dir().join("replica"),
            },
            |s, state| s.connection = state,
        );
        let mut app = App {
            ui,
            sync,
            auth: None,
            connection: ConnectionState::Initializing,
            filter: Filter {
                status: Status::Open,
                sort_field: SortField::Modified,
                sort_direction: SortDirection::Desc,
                created_by_me: false,
                assigned_to_me: false,
            },
            search: TextEdit::new(""),
            issues: Vec::new(),
            issues_result: ResultType::Unknown,
            limit: LIST_PAGE,
            count: None,
            me: None,
            projects: Vec::new(),
            selected: None,
            reveal_selected: false,
            detail: None,
            detail_result: ResultType::Unknown,
            comments: Vec::new(),
            comments_limit: COMMENTS_PAGE,
            comment: TextEdit::new(""),
            editing_comment: None,
            comment_edit: TextEdit::new(""),
            confirm: None,
            picker: None,
            crew: Vec::new(),
            labels: Vec::new(),
            labels_project: None,
            sheet: None,
            title_edit: TextEdit::new(""),
            description_edit: TextEdit::new(""),
            token_edit: TextEdit::new(""),
            label_edit: TextEdit::new(""),
            cache_edit: TextEdit::new(""),
            api_edit: TextEdit::new(""),
            sheet_error: None,
            error: None,
            github_login: None,
            settings,
            saved,
        };
        app.restart();
        // A relaunch opens at the issue it was showing.
        if let Some(id) = app.saved.selection.clone() {
            app.reveal_selected = true;
            app.select(id);
        }
        app
    }

    /// A new client, for the current servers and login: zbugs makes one
    /// whenever either changes.
    fn restart(&mut self) {
        // An expired token logs out.
        let now = now_ms() / 1000.0;
        self.auth = self
            .settings
            .token
            .as_deref()
            .and_then(|t| AuthData::from_jwt(t, now));
        // A saved token that has expired is deleted.
        if self
            .saved
            .token
            .as_deref()
            .is_some_and(|t| AuthData::from_jwt(t, now).is_none())
        {
            self.saved.token = None;
            token::delete();
        }
        if self.auth.is_none() {
            self.settings.token = None;
            self.filter.created_by_me = false;
            self.filter.assigned_to_me = false;
        }
        self.me = None;
        self.sync.connect(Session {
            cache_url: self.settings.cache_url.clone(),
            api_url: self.settings.api_url.clone(),
            token: self.settings.token.clone(),
            user_id: self.auth.as_ref().map(|a| a.sub.clone()),
            context: self.auth.clone(),
        });
        self.sync.watch(
            "projects",
            |auth: &Context| queries::ALL_PROJECTS.call(&(), auth.as_ref()),
            |s, rows, _, _| s.projects = rows,
        );
        let crew = UsersForProjectArgs {
            disabled: false,
            login: None,
            project_name: ZERO_PROJECT_NAME.to_owned(),
            filter: Some(UsersFilter::Crew),
        };
        self.sync.watch(
            "crew",
            move |auth: &Context| queries::USERS_FOR_PROJECT.call(&crew, auth.as_ref()),
            |s, mut rows: Vec<User>, _, _| {
                // As the web app's picker sorts them.
                rows.sort_by(|a, b| a.login.cmp(&b.login));
                s.crew = rows;
            },
        );
        if let Some(sub) = self.auth.as_ref().map(|a| a.sub.clone()) {
            self.sync.watch(
                "me",
                move |auth: &Context| queries::USER.call(&sub, auth.as_ref()),
                |s, me, _, _| s.me = me,
            );
        } else {
            self.sync.unwatch("me");
        }
        self.watch_list();
        if let Some(id) = self.selected.clone() {
            self.watch_detail(&id);
        }
    }

    fn list_context(&self) -> ListContextParams {
        let login = self.me.as_ref().map(|u| u.login.clone());
        let text = self.search.text.trim();
        ListContextParams {
            open: self.filter.status.open(),
            project_name: None,
            assignee: login.clone().filter(|_| self.filter.assigned_to_me),
            creator: login.filter(|_| self.filter.created_by_me),
            labels: None,
            text_filter: (!text.is_empty()).then(|| text.to_owned()),
            sort_field: self.filter.sort_field,
            sort_direction: self.filter.sort_direction,
        }
    }

    /// The list from its first page, for the current filters.
    pub fn list_changed(&mut self) {
        self.limit = LIST_PAGE;
        self.watch_list();
    }

    fn watch_list(&mut self) {
        let list_context = self.list_context();
        let args = IssueListV2Args {
            list_context: list_context.clone(),
            limit: Some(self.limit),
            start: None,
            dir: PageDirection::Forward,
            inclusive: None,
        };
        self.sync.watch(
            "issues",
            move |auth: &Context| queries::ISSUE_LIST_V2.call(&args, auth.as_ref()),
            apply_issues,
        );
        let count = IssueListCountArgs { list_context };
        self.sync.watch(
            "count",
            move |auth: &Context| queries::ISSUE_LIST_COUNT.call(&count, auth.as_ref()),
            |s, n, _, _| s.count = Some(n),
        );
    }

    /// The list shows rows up to `end`: ask for the next page when that's
    /// near the last one it has. Returns whether the state changed.
    pub fn list_visible(&mut self, end: usize) -> bool {
        let full = self.issues.len() as i64 >= self.limit;
        if full && end + 20 >= self.issues.len() {
            self.limit += LIST_PAGE;
            self.watch_list();
        }
        false
    }

    pub fn select(&mut self, id: String) {
        if self.selected.as_ref() == Some(&id) {
            return;
        }
        self.detail = None;
        self.detail_result = ResultType::Unknown;
        self.picker = None;
        self.editing_comment = None;
        self.comments.clear();
        self.comments_limit = COMMENTS_PAGE;
        self.comment = TextEdit::new("");
        self.watch_detail(&id);
        self.selected = Some(id.clone());
        self.saved.selection = Some(id.clone());
        self.settings.selection = Some(id);
        self.saved.save();
    }

    /// Shows no issue.
    fn deselect(&mut self) {
        self.selected = None;
        self.detail = None;
        self.detail_result = ResultType::Unknown;
        self.picker = None;
        self.editing_comment = None;
        self.comments.clear();
        self.sync.unwatch("detail");
        self.sync.unwatch("comments");
        self.saved.selection = None;
        self.settings.selection = None;
        self.saved.save();
    }

    fn watch_detail(&mut self, id: &str) {
        let args = IssueDetailArgs {
            id_field: IdField::Id,
            id: IssueKey::String(id.to_owned()),
        };
        self.sync.watch(
            "detail",
            move |auth: &Context| queries::ISSUE_DETAIL.call(&args, auth.as_ref()),
            |s, issue: Option<Issue>, result_type, _| {
                if let Some(project) = issue.as_ref().and_then(|i| i.project.as_ref()) {
                    s.watch_labels(&project.name);
                }
                s.detail = issue;
                s.detail_result = result_type;
            },
        );
        self.watch_comments(id);
    }

    /// The labels the label menu offers: the selected issue's project's.
    fn watch_labels(&mut self, project_name: &str) {
        if self.labels_project.as_deref() == Some(project_name) {
            return;
        }
        self.labels_project = Some(project_name.to_owned());
        let args = LabelsOrderByNameArgs {
            project_name: project_name.to_owned(),
            order_by: SortDirection::Asc,
        };
        self.sync.watch(
            "labels",
            move |auth: &Context| queries::LABELS_ORDER_BY_NAME.call(&args, auth.as_ref()),
            |s, rows, _, _| s.labels = rows,
        );
    }

    fn watch_comments(&mut self, id: &str) {
        // Oldest first, as the issue page shows them.
        let args = CommentsPageArgs {
            issue_id: id.to_owned(),
            limit: self.comments_limit,
            start: None,
            dir: PageDirection::Forward,
            inclusive: true,
        };
        self.sync.watch(
            "comments",
            move |auth: &Context| queries::COMMENTS_PAGE.call(&args, auth.as_ref()),
            |s, rows, _, _| s.comments = rows,
        );
    }

    /// The comments reached their end: the next page, if there is one.
    pub fn more_comments(&mut self) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        if self.comments.len() as i64 >= self.comments_limit {
            self.comments_limit += COMMENTS_PAGE;
            self.watch_comments(&id);
        }
    }

    fn mutate(&mut self, request: MutateRequest) {
        self.sync.mutate(
            move |zero| request.run(zero),
            |s, message| {
                s.error = Some(message);
            },
        );
    }

    /// Whether the user can change what `creator_id` made: the crew can
    /// change anything, others only their own.
    pub fn can_edit(&self, creator_id: &str) -> bool {
        self.auth
            .as_ref()
            .is_some_and(|a| a.is_crew() || a.sub == creator_id)
    }

    pub fn is_crew(&self) -> bool {
        self.auth.as_ref().is_some_and(AuthData::is_crew)
    }

    /// Edits the comment in place.
    pub fn edit_comment(&mut self, id: &str) {
        let Some(c) = self.comments.iter().find(|c| c.id == id) else {
            return;
        };
        self.comment_edit = TextEdit::new(c.body.clone());
        self.comment_edit.focused = true;
        self.editing_comment = Some(id.to_owned());
    }

    pub fn save_comment(&mut self) {
        let Some(id) = self.editing_comment.take() else {
            return;
        };
        let body = self.comment_edit.text.trim().to_owned();
        if !body.is_empty() {
            self.mutate(EDIT_COMMENT.with(&EditCommentArgs { id, body }));
        }
    }

    /// Runs the deletion the user confirmed.
    pub fn confirmed(&mut self) {
        match self.confirm.take() {
            Some(Confirm::DeleteIssue(id)) => {
                self.mutate(DELETE_ISSUE.with(&id));
                if self.selected.as_ref() == Some(&id) {
                    self.deselect();
                }
            }
            Some(Confirm::DeleteComment(id)) => {
                if self.editing_comment.as_ref() == Some(&id) {
                    self.editing_comment = None;
                }
                self.mutate(REMOVE_COMMENT.with(&id));
            }
            None => {}
        }
    }

    /// Creates a label in the issue's project and adds it to the issue.
    pub fn save_label(&mut self) {
        let name = self.label_edit.text.trim().to_owned();
        if name.is_empty() {
            self.sheet_error = Some("A label needs a name.".into());
            return;
        }
        if self.labels.iter().any(|l| l.name == name) {
            self.sheet_error = Some(format!("There's already a label named {name}."));
            return;
        }
        let Some(issue) = &self.detail else { return };
        self.mutate(CREATE_AND_ADD_LABEL.with(&CreateAndAddLabelArgs {
            issue_id: issue.id.clone(),
            label_id: make_id(),
            label_name: name,
            project_id: None,
        }));
        self.sheet = None;
    }

    pub fn toggle_open(&mut self) {
        let Some(issue) = &self.detail else { return };
        let args = UpdateIssueArgs {
            open: Some(!issue.open),
            ..UpdateIssueArgs::new(issue.id.clone(), now_ms())
        };
        self.mutate(UPDATE_ISSUE.with(&args));
    }

    /// Opens `picker`, or closes it when it's open.
    pub fn toggle_picker(&mut self, picker: Picker) {
        self.picker = (self.picker.as_ref() != Some(&picker)).then_some(picker);
    }

    /// Assigns the issue to the crew member with `login`, or to nobody.
    pub fn assign(&mut self, login: &str) {
        self.picker = None;
        let Some(issue) = &self.detail else { return };
        let assignee = self.crew.iter().find(|u| u.login == login);
        let args = UpdateIssueArgs {
            assignee_id: Some(assignee.map(|u| u.id.clone())),
            ..UpdateIssueArgs::new(issue.id.clone(), now_ms())
        };
        self.mutate(UPDATE_ISSUE.with(&args));
    }

    /// Adds the label to the issue, or removes it. The menu stays open for
    /// the next one.
    pub fn toggle_label(&mut self, label_id: &str) {
        let Some(issue) = &self.detail else { return };
        let has = issue.labels.iter().flatten().any(|l| l.id == label_id);
        let (issue_id, label_id) = (issue.id.clone(), label_id.to_owned());
        self.mutate(if has {
            REMOVE_LABEL.with(&RemoveLabelArgs { issue_id, label_id })
        } else {
            ADD_LABEL.with(&IssueLabelArgs {
                issue_id,
                label_id,
                project_id: None,
            })
        });
    }

    /// The reactions to the issue or to one of its comments.
    pub fn reactions(&self, subject_id: &str) -> &[Emoji] {
        let emoji = match &self.detail {
            Some(issue) if issue.id == subject_id => issue.emoji.as_deref(),
            _ => self
                .comments
                .iter()
                .find(|c| c.id == subject_id)
                .and_then(|c| c.emoji.as_deref()),
        };
        emoji.unwrap_or_default()
    }

    /// Adds the user's `value` reaction to the subject, or takes it back.
    pub fn toggle_reaction(&mut self, subject_id: &str, value: &str) {
        self.picker = None;
        let Some(sub) = self.auth.as_ref().map(|a| a.sub.clone()) else {
            return;
        };
        let mine = self
            .reactions(subject_id)
            .iter()
            .find(|e| e.creator_id == sub && e.value == value);
        if let Some(e) = mine {
            let id = e.id.clone();
            self.mutate(REMOVE_EMOJI.with(&id));
            return;
        }
        let annotation = REACTIONS
            .iter()
            .find(|(v, _)| *v == value)
            .map(|(_, a)| *a)
            .or_else(|| {
                let e = self.reactions(subject_id).iter().find(|e| e.value == value);
                e.map(|e| e.annotation.as_str())
            })
            .unwrap_or_default()
            .to_owned();
        let on_issue = self.detail.as_ref().is_some_and(|i| i.id == subject_id);
        let args = AddEmojiArgs {
            id: make_id(),
            unicode: value.to_owned(),
            annotation,
            subject_id: subject_id.to_owned(),
            created: now_ms(),
        };
        self.mutate(if on_issue {
            ADD_EMOJI_TO_ISSUE.with(&args)
        } else {
            ADD_EMOJI_TO_COMMENT.with(&args)
        });
    }

    pub fn add_comment(&mut self) {
        let (Some(id), body) = (self.selected.clone(), self.comment.text.trim().to_owned()) else {
            return;
        };
        if body.is_empty() {
            return;
        }
        self.mutate(ADD_COMMENT.with(&AddCommentArgs {
            id: make_id(),
            issue_id: id,
            body,
            created: now_ms(),
        }));
        self.comment = TextEdit::new("");
    }

    pub fn open_sheet(&mut self, sheet: Sheet) {
        self.sheet_error = None;
        match sheet {
            Sheet::NewIssue => {
                self.title_edit = TextEdit::new("");
                self.description_edit = TextEdit::new("");
            }
            Sheet::EditIssue => {
                let Some(issue) = &self.detail else { return };
                self.title_edit = TextEdit::new(issue.title.clone());
                self.description_edit = TextEdit::new(issue.description.clone());
            }
            Sheet::NewLabel => {
                self.picker = None;
                self.label_edit = TextEdit::new("");
                self.label_edit.focused = true;
            }
            Sheet::Login => self.token_edit = TextEdit::new(""),
            Sheet::Servers => {
                self.cache_edit = TextEdit::new(self.settings.cache_url.clone());
                self.api_edit = TextEdit::new(self.settings.api_url.clone());
            }
        }
        self.title_edit.focused = matches!(sheet, Sheet::NewIssue | Sheet::EditIssue);
        self.token_edit.focused = sheet == Sheet::Login;
        self.sheet = Some(sheet);
    }

    /// Saves the new or edited issue.
    pub fn save_issue(&mut self) {
        let title = self.title_edit.text.trim().to_owned();
        if title.is_empty() {
            self.sheet_error = Some("An issue needs a title.".into());
            return;
        }
        let description = self.description_edit.text.clone();
        let now = now_ms();
        if self.sheet == Some(Sheet::EditIssue) {
            let Some(issue) = &self.detail else { return };
            let args = UpdateIssueArgs {
                title: Some(title),
                description: Some(description),
                ..UpdateIssueArgs::new(issue.id.clone(), now)
            };
            self.mutate(UPDATE_ISSUE.with(&args));
        } else {
            let project = self
                .projects
                .iter()
                .find(|p| p.lower_case_name == ZERO_PROJECT_NAME.to_lowercase());
            let Some(project) = project else {
                self.sheet_error = Some("The Zero project hasn't synced yet.".into());
                return;
            };
            let id = make_id();
            self.mutate(CREATE_ISSUE.with(&CreateIssueArgs {
                id: id.clone(),
                title,
                description: Some(description),
                created: now,
                modified: now,
                project_id: project.id.clone(),
            }));
            self.select(id);
        }
        self.sheet = None;
    }

    pub fn close_sheet(&mut self) {
        self.sheet = None;
        self.github_login = None;
    }

    /// Logs in with the pasted token.
    pub fn log_in(&mut self) {
        let token = self.token_edit.text.trim().to_owned();
        self.log_in_with(token);
    }

    /// Opens the GitHub login in the browser; the API server hands the
    /// token back to a listener on this machine (`login.rs`).
    pub fn log_in_with_github(&mut self, tx: &mut Tx) {
        let ui = self.ui.clone();
        let login = login::LoopbackLogin::start(&self.settings.api_url, move |token| {
            ui.update(move |s, _| s.log_in_with(token));
        });
        match login {
            Ok(login) if tx.open_url(&login.url).is_ok() => {
                self.sheet_error = None;
                self.github_login = Some(login);
            }
            Ok(login) => {
                self.sheet_error = Some(format!(
                    "Couldn't open the browser. Open {} instead.",
                    login.url
                ));
                self.github_login = Some(login);
            }
            Err(e) => self.sheet_error = Some(format!("Couldn't wait for the login: {e}")),
        }
    }

    fn log_in_with(&mut self, token: String) {
        self.github_login = None;
        if AuthData::from_jwt(&token, now_ms() / 1000.0).is_none() {
            self.sheet_error = Some("That isn't a current zbugs token.".into());
            return;
        }
        token::save(&token);
        self.settings.token = Some(token.clone());
        self.saved.token = Some(token);
        self.sheet = None;
        self.restart();
    }

    pub fn log_out(&mut self) {
        self.settings.token = None;
        self.saved.token = None;
        token::delete();
        self.restart();
    }

    pub fn save_servers(&mut self) {
        let cache = self.cache_edit.text.trim().trim_end_matches('/').to_owned();
        let api = self.api_edit.text.trim().trim_end_matches('/').to_owned();
        if !cache.starts_with("http") || !api.starts_with("http") {
            self.sheet_error = Some("Both are http:// or https:// URLs.".into());
            return;
        }
        self.settings.cache_url = cache.clone();
        self.settings.api_url = api.clone();
        self.saved.cache_url = cache;
        self.saved.api_url = api;
        self.saved.save();
        self.sheet = None;
        self.restart();
    }

    pub fn reconnect_now(&self) {
        self.sync.reconnect_now();
    }

    pub fn cache_url(&self) -> &str {
        &self.settings.cache_url
    }
}

fn apply_issues(s: &mut App, rows: Vec<Issue>, result_type: ResultType, tx: &mut Tx) {
    s.issues = rows;
    s.issues_result = result_type;
    if s.reveal_selected
        && let Some(id) = &s.selected
        && s.issues.iter().any(|i| &i.id == id)
    {
        s.reveal_selected = false;
        tx.scroll_into_view(id.clone());
    }
}

fn on_close(s: &mut App, _: &mut Tx) {
    s.sync.close();
}

fn main() {
    let spec = AppSpec {
        min_size: Some((900.0, 520.0)),
        ..AppSpec::new("Zero Bugs", (1180.0, 760.0), placeholder, view::view)
    };
    let app = rune::App::new(spec);
    let handle = app.handle();
    app.init(move || App::new(handle)).on_close(on_close).run();
}

/// `AppSpec::init`, which `App::init` replaces: the state needs the app's
/// handle.
fn placeholder() -> App {
    unreachable!("App::init makes the state")
}
