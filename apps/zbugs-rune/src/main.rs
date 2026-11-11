//! zbugs on the Zero Rust client, drawn with Rune.
//!
//! The state here is the app's: what the user chose, and the rows the sync
//! thread (`sync.rs`) last sent for each query. The views (`view.rs`) read
//! it; their handlers change it and ask the sync thread for other rows or
//! to run a mutator.

mod images;
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
    IssueListV2Args, LabelsOrderByNameArgs, ListContextParams, NotificationType,
    NotificationUpdateArgs, PageDirection, ProjectArgs, RemoveLabelArgs, SortDirection, SortField,
    UpdateIssueArgs, UserPrefArgs, UsersFilter, UsersForProjectArgs, ViewStateArgs,
};
use zbugs_model::mutators::{
    ADD_COMMENT, ADD_EMOJI_TO_COMMENT, ADD_EMOJI_TO_ISSUE, ADD_LABEL, CREATE_AND_ADD_LABEL,
    CREATE_ISSUE, DELETE_ISSUE, DELETE_ISSUES, EDIT_COMMENT, REMOVE_COMMENT, REMOVE_EMOJI,
    REMOVE_LABEL, SET_USER_PREF, SET_VIEW_STATE, UPDATE_ISSUE, UPDATE_NOTIFICATION,
};
use zbugs_model::schema::{Comment, Emoji, Issue, IssueVisibility, Label, Project, User, UserPref};
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
    /// The project's lower-case name.
    pub project: String,
    /// Logins, as the web app's `creator` and `assignee` list params.
    pub creator: Option<String>,
    pub assignee: Option<String>,
    /// Label names: an issue must have all of them.
    pub labels: Vec<String>,
}

/// A dropdown on the issue page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Picker {
    Assignee,
    Labels,
    Visibility,
    Notifications,
    /// The sidebar's.
    Project,
    CreatorFilter,
    AssigneeFilter,
    LabelFilter,
    /// Reactions to the issue (its ID) or to one of its comments.
    Emoji(String),
}

/// A deletion waiting for the user to confirm it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Confirm {
    DeleteIssue(String),
    /// The list's checked issues.
    DeleteIssues(Vec<String>),
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
    /// Avatars, and images in issues and comments.
    pub images: images::Images,
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
    /// Who the list can be filtered by: those who created, or are assigned,
    /// issues in the project, and its labels.
    pub creators: Vec<User>,
    pub assignees: Vec<User>,
    pub project_labels: Vec<Label>,

    pub selected: Option<String>,
    /// Picking issues in the list to delete them together, and those picked.
    pub select_mode: bool,
    pub checked: Vec<String>,
    /// Scroll the list to the selected issue once it's there (a relaunch).
    reveal_selected: bool,
    pub detail: Option<Issue>,
    pub detail_result: ResultType,
    pub comments: Vec<Comment>,
    comments_limit: i64,
    pub comment: TextEdit,
    /// The emoji picker's search, and the skin tone it shows, as the web
    /// picker keeps it in the `emojiSkinTone` user pref (0 is the default).
    pub emoji_search: TextEdit,
    pub skin_tone: u8,
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

/// The user pref the web app keeps the emoji picker's skin tone in.
const SKIN_TONE_PREF: &str = "emojiSkinTone";

/// The choices of the issue page's Visibility and Notifications menus.
pub const VISIBILITY: [&str; 2] = ["Public", "Internal"];
pub const NOTIFICATIONS: [&str; 2] = ["Subscribed", "Unsubscribed"];

/// As the web app's pickers sort users.
fn by_login(rows: &mut [User]) {
    rows.sort_by(|a, b| a.login.cmp(&b.login));
}

/// When the user last looked at the issue (the query has only their row).
fn viewed(issue: &Issue) -> f64 {
    issue
        .view_state
        .iter()
        .flatten()
        .map(|v| v.viewed)
        .fold(0.0, f64::max)
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
            images: images::Images::new(ui.clone()),
            ui,
            sync,
            auth: None,
            connection: ConnectionState::Initializing,
            filter: Filter {
                status: Status::Open,
                sort_field: SortField::Modified,
                sort_direction: SortDirection::Desc,
                project: ZERO_PROJECT_NAME.to_lowercase(),
                creator: None,
                assignee: None,
                labels: Vec::new(),
            },
            search: TextEdit::new(""),
            issues: Vec::new(),
            issues_result: ResultType::Unknown,
            limit: LIST_PAGE,
            count: None,
            me: None,
            projects: Vec::new(),
            creators: Vec::new(),
            assignees: Vec::new(),
            project_labels: Vec::new(),
            selected: None,
            select_mode: false,
            checked: Vec::new(),
            reveal_selected: false,
            detail: None,
            detail_result: ResultType::Unknown,
            comments: Vec::new(),
            comments_limit: COMMENTS_PAGE,
            comment: TextEdit::new(""),
            emoji_search: TextEdit::new(""),
            skin_tone: 0,
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
        self.watch_project();
        if let Some(sub) = self.auth.as_ref().map(|a| a.sub.clone()) {
            self.sync.watch(
                "me",
                move |auth: &Context| queries::USER.call(&sub, auth.as_ref()),
                |s, me, _, _| s.me = me,
            );
            self.sync.watch(
                "skin-tone",
                |auth: &Context| queries::USER_PREF.call(&SKIN_TONE_PREF.to_owned(), auth.as_ref()),
                |s, pref: Option<UserPref>, _, _| {
                    s.skin_tone = pref.and_then(|p| p.value.parse().ok()).unwrap_or(0);
                },
            );
        } else {
            self.sync.unwatch("me");
            self.sync.unwatch("skin-tone");
            self.skin_tone = 0;
        }
        self.watch_list();
        if let Some(id) = self.selected.clone() {
            self.watch_detail(&id);
        }
    }

    /// The users and labels that depend on the project.
    fn watch_project(&mut self) {
        let users = |filter| UsersForProjectArgs {
            disabled: false,
            login: None,
            project_name: self.filter.project.clone(),
            filter: Some(filter),
        };
        let crew = users(UsersFilter::Crew);
        self.sync.watch(
            "crew",
            move |auth: &Context| queries::USERS_FOR_PROJECT.call(&crew, auth.as_ref()),
            |s, mut rows: Vec<User>, _, _| {
                by_login(&mut rows);
                s.crew = rows;
            },
        );
        let creators = users(UsersFilter::Creators);
        self.sync.watch(
            "creators",
            move |auth: &Context| queries::USERS_FOR_PROJECT.call(&creators, auth.as_ref()),
            |s, mut rows: Vec<User>, _, _| {
                by_login(&mut rows);
                s.creators = rows;
            },
        );
        let assignees = users(UsersFilter::Assignees);
        self.sync.watch(
            "assignees",
            move |auth: &Context| queries::USERS_FOR_PROJECT.call(&assignees, auth.as_ref()),
            |s, mut rows: Vec<User>, _, _| {
                by_login(&mut rows);
                s.assignees = rows;
            },
        );
        let labels = LabelsOrderByNameArgs {
            project_name: self.filter.project.clone(),
            order_by: SortDirection::Asc,
        };
        self.sync.watch(
            "project-labels",
            move |auth: &Context| queries::LABELS_ORDER_BY_NAME.call(&labels, auth.as_ref()),
            |s, rows, _, _| s.project_labels = rows,
        );
    }

    /// Shows the project's issues, with the filters that were the last
    /// project's cleared, as the web app's project picker does.
    pub fn set_project(&mut self, name: &str) {
        self.picker = None;
        let Some(p) = self.projects.iter().find(|p| p.name == name) else {
            return;
        };
        if p.lower_case_name == self.filter.project {
            return;
        }
        self.filter.project = p.lower_case_name.clone();
        self.filter.creator = None;
        self.filter.assignee = None;
        self.filter.labels.clear();
        self.watch_project();
        self.list_changed();
    }

    /// The project the list shows, once it has synced.
    pub fn project(&self) -> Option<&Project> {
        self.projects
            .iter()
            .find(|p| p.lower_case_name == self.filter.project)
    }

    /// Filters the list by creator (`Some(login)`) or not.
    pub fn set_creator(&mut self, login: Option<String>) {
        self.picker = None;
        self.filter.creator = login;
        self.list_changed();
    }

    pub fn set_assignee(&mut self, login: Option<String>) {
        self.picker = None;
        self.filter.assignee = login;
        self.list_changed();
    }

    /// Adds the label to the list's filter, or takes it out. The menu stays
    /// open for the next one.
    pub fn toggle_label_filter(&mut self, name: &str) {
        match self.filter.labels.iter().position(|l| l == name) {
            Some(i) => {
                self.filter.labels.remove(i);
            }
            None => self.filter.labels.push(name.to_owned()),
        }
        self.list_changed();
    }

    /// The user's login, once it has synced.
    pub fn my_login(&self) -> Option<&str> {
        self.me.as_ref().map(|u| u.login.as_str())
    }

    fn list_context(&self) -> ListContextParams {
        let text = self.search.text.trim();
        ListContextParams {
            open: self.filter.status.open(),
            project_name: Some(self.filter.project.clone()),
            assignee: self.filter.assignee.clone(),
            creator: self.filter.creator.clone(),
            labels: (!self.filter.labels.is_empty()).then(|| self.filter.labels.clone()),
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
                s.view_later();
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

    /// Starts or stops picking issues to delete, with none picked.
    pub fn toggle_select_mode(&mut self) {
        self.select_mode = !self.select_mode;
        self.checked.clear();
    }

    /// Picks the issue to delete, or unpicks it. The user can delete what
    /// they created; the crew can delete anything.
    pub fn toggle_checked(&mut self, id: &str) {
        let Some(issue) = self.issues.iter().find(|i| i.id == id) else {
            return;
        };
        if !self.can_edit(&issue.creator_id) {
            return;
        }
        match self.checked.iter().position(|c| c == id) {
            Some(i) => {
                self.checked.remove(i);
            }
            None => self.checked.push(id.to_owned()),
        }
    }

    /// Selects the issue `by` rows after (or before) the selected one in the
    /// list, as the web issue page's j and k do.
    fn step(&mut self, by: isize, tx: &mut Tx) {
        let at = self
            .selected
            .as_ref()
            .and_then(|id| self.issues.iter().position(|i| &i.id == id));
        let to = match at {
            Some(i) => i.checked_add_signed(by),
            None => Some(0),
        };
        let Some(issue) = to.and_then(|i| self.issues.get(i)) else {
            return;
        };
        let id = issue.id.clone();
        tx.scroll_into_view(id.clone());
        self.select(id);
        // Near the end of what's loaded: the next page.
        if let Some(i) = to {
            self.list_visible(i + 1);
        }
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
            Some(Confirm::DeleteIssues(ids)) => {
                if self.selected.as_ref().is_some_and(|id| ids.contains(id)) {
                    self.deselect();
                }
                self.mutate(DELETE_ISSUES.with(&ids));
                self.toggle_select_mode();
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
    /// Whether the issue changed since the user last looked at it, as the
    /// web app's list marks it.
    pub fn is_unread(&self, issue: &Issue) -> bool {
        self.auth.is_some() && issue.modified > viewed(issue)
    }

    /// Marks the selected issue viewed once the user has looked at it for a
    /// second, if it changed since they last did, as the web app does.
    fn view_later(&mut self) {
        let Some(issue) = self.detail.as_ref().filter(|i| self.is_unread(i)) else {
            return;
        };
        let (ui, id, modified) = (self.ui.clone(), issue.id.clone(), issue.modified);
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(1));
            ui.update(move |s, _| {
                let Some(issue) = s.detail.as_ref().filter(|i| i.id == id) else {
                    return;
                };
                // Not if another change came in the meantime: its own timer will.
                if issue.modified == modified && s.is_unread(issue) {
                    let args = ViewStateArgs {
                        issue_id: id,
                        viewed: now_ms(),
                    };
                    s.mutate(SET_VIEW_STATE.with(&args));
                }
            });
        });
    }

    pub fn set_visibility(&mut self, choice: &str) {
        self.picker = None;
        let Some(issue) = &self.detail else { return };
        let visibility = if choice == VISIBILITY[1] {
            IssueVisibility::Internal
        } else {
            IssueVisibility::Public
        };
        if visibility == issue.visibility {
            return;
        }
        let args = UpdateIssueArgs {
            visibility: Some(visibility),
            ..UpdateIssueArgs::new(issue.id.clone(), now_ms())
        };
        self.mutate(UPDATE_ISSUE.with(&args));
    }

    /// Whether the user gets notifications about the issue.
    pub fn is_subscribed(&self, issue: &Issue) -> bool {
        issue
            .notification_state
            .as_ref()
            .is_some_and(|n| n.subscribed)
    }

    pub fn set_subscribed(&mut self, choice: &str) {
        self.picker = None;
        let Some(issue) = &self.detail else { return };
        // Sent even when it's the current choice, as on the web: an explicit
        // unsubscribe keeps a later comment or assignment from subscribing.
        let subscribe = choice == NOTIFICATIONS[0];
        let args = NotificationUpdateArgs {
            issue_id: issue.id.clone(),
            subscribed: if subscribe {
                NotificationType::Subscribe
            } else {
                NotificationType::Unsubscribe
            },
            created: now_ms(),
        };
        self.mutate(UPDATE_NOTIFICATION.with(&args));
    }

    /// Shows emoji in this skin tone (0 to 5, light to dark after the
    /// default), and keeps it for the user, as the web picker does.
    pub fn set_skin_tone(&mut self, tone: u8) {
        self.skin_tone = tone;
        let args = UserPrefArgs {
            key: SKIN_TONE_PREF.to_owned(),
            value: tone.to_string(),
        };
        self.mutate(SET_USER_PREF.with(&args));
    }

    pub fn toggle_picker(&mut self, picker: Picker) {
        if matches!(picker, Picker::Emoji(_)) {
            self.emoji_search = TextEdit::new("");
            self.emoji_search.focused = true;
        }
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
        // Its CLDR name, as the web picker sends it.
        let annotation = emojis::get(value)
            .map(emojis::Emoji::name)
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
            let Some(project) = self.project() else {
                self.sheet_error = Some("The project hasn't synced yet.".into());
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

/// Keys no control took: j and k step through the list, and / starts a
/// search, as on the web.
fn on_key(s: &mut App, event: &rune::KeyEvent, tx: &mut Tx) -> bool {
    if s.sheet.is_some()
        || s.confirm.is_some()
        || event.mods.cmd
        || event.mods.ctrl
        || event.mods.alt
    {
        return false;
    }
    match &event.key {
        rune::Key::Text(t) if t == "j" => s.step(1, tx),
        rune::Key::Text(t) if t == "k" => s.step(-1, tx),
        rune::Key::Text(t) if t == "/" => s.search.focused = true,
        _ => return false,
    }
    true
}

fn on_close(s: &mut App, _: &mut Tx) {
    s.sync.close();
}

fn main() {
    let spec = AppSpec {
        min_size: Some((900.0, 520.0)),
        on_key: Some(on_key),
        setup: Some(|s, waker| s.images.start(waker)),
        poll: Some(|s, _, _| s.images.poll()),
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
