//! The queries of `apps/zbugs/shared/queries.ts`. The drift test checks
//! that each builds the same AST as its TS definition.
//!
//! Each is a [`QueryDefinition`]: the client builds its AST for local
//! results and sends its name and arguments, which zero-cache asks the API
//! server to run with the TS definition.

use crate::args::*;
use crate::mutators::Context;
use crate::schema::{
    Comment, Emoji, Issue, IssueLabel, Label, Project, User, UserPref, UserRole, ViewState, zql,
};
use zero_client::define::{NamedQuery, QueryDefinition, define_query};
use zero_client::json::Value;
use zero_client::like::escape_like;
use zero_client::typed::{Condition, Direction, ExistsOptions, Query, SimpleOperator, ZeroRow};

pub const ZERO_PROJECT_NAME: &str = "Zero";

/// The string when JS would take it as true: neither `None` nor empty.
fn truthy(s: Option<&str>) -> Option<&str> {
    s.filter(|s| !s.is_empty())
}

fn scalar() -> ExistsOptions {
    ExistsOptions {
        scalar: Some(true),
        ..ExistsOptions::default()
    }
}

/// Matches no row.
fn always_false<R: ZeroRow, T>(q: Query<R, T>) -> Query<R, T> {
    q.where_expr(|e| e.or(Vec::new()))
}

/// `where(column, value)` by the column's name, as JS writes it.
fn where_named<R: ZeroRow, T>(q: Query<R, T>, column: &str, value: Value) -> Query<R, T> {
    Query::from_any(q.into_any().where_(column, SimpleOperator::Eq, value))
}

fn apply_issue_permissions<T>(q: Query<Issue, T>, role: Option<UserRole>) -> Query<Issue, T> {
    if role == Some(UserRole::Crew) {
        q
    } else {
        q.where_eq(Issue::VISIBILITY, crate::schema::IssueVisibility::Public)
    }
}

/// Narrows a comment query to comments whose issue `role` may see. Comments
/// and emoji are fetched by id, so each of those queries applies the
/// issue's visibility itself.
fn apply_comment_permissions<T>(q: Query<Comment, T>, role: Option<UserRole>) -> Query<Comment, T> {
    if role == Some(UserRole::Crew) {
        q
    } else {
        q.where_exists(Comment::ISSUE, ExistsOptions::default(), move |i| {
            apply_issue_permissions(i, role)
        })
    }
}

fn in_project<R: ZeroRow, T>(
    q: Query<R, T>,
    relationship: zero_client::typed::Relationship<R, Project>,
    project_name: &str,
) -> Query<R, T> {
    let lower = project_name.to_lowercase();
    q.where_exists(relationship, scalar(), move |p| {
        p.where_eq(Project::LOWER_CASE_NAME, lower)
    })
}

fn labels_query(project_name: &str, order_by: Option<SortDirection>) -> Query<Label> {
    let q = in_project(zql::label(), Label::PROJECT, project_name);
    match order_by {
        Some(d) => q.order_by(Label::NAME, d.ast()),
        None => q,
    }
}

fn role(auth: &Context) -> Option<UserRole> {
    auth.as_ref().map(|a| a.role)
}

/// The user's own rows of a per-user relationship, or none when logged
/// out.
fn own<R: ZeroRow, T>(
    q: Query<R, T>,
    auth: &Context,
    user_id: zero_client::typed::Column<R, String>,
) -> Query<R, T> {
    match truthy(auth.as_ref().map(|a| a.sub.as_str())) {
        Some(sub) => q.where_eq(user_id, sub.to_owned()),
        None => always_false(q),
    }
}

fn emoji_with_creator(q: Query<Emoji>) -> Query<Emoji> {
    q.related(Emoji::CREATOR, |u| u)
}

/// The crew, without the bots.
fn crew(q: Query<User>) -> Query<User> {
    q.where_expr(|e| {
        e.and(vec![
            User::ROLE.eq(UserRole::Crew),
            e.not(User::LOGIN.like("rocibot%")),
        ])
    })
}

pub const ALL_LABELS: QueryDefinition<(), Context, Label> =
    define_query("allLabels", |_, _| zql::label());

pub const ALL_USERS: QueryDefinition<(), Context, User> =
    define_query("allUsers", |_, _| zql::user());

pub const ALL_PROJECTS: QueryDefinition<(), Context, Project> =
    define_query("allProjects", |_, _| zql::project());

pub const USER: QueryDefinition<String, Context, User, Option<User>> =
    define_query("user", |id, _| {
        zql::user().where_eq(User::ID, id.clone()).one()
    });

pub const CREW_USER: QueryDefinition<String, Context, User, Option<User>> =
    define_query("crewUser", |id, _| {
        zql::user()
            .where_eq(User::ID, id.clone())
            .where_eq(User::ROLE, UserRole::Crew)
            .one()
    });

pub const LABELS: QueryDefinition<ProjectArgs, Context, Label> =
    define_query("labels", |a, _| labels_query(&a.project_name, None));

pub const LABELS_ORDER_BY_NAME: QueryDefinition<LabelsOrderByNameArgs, Context, Label> =
    define_query("labelsOrderByName", |a, _| {
        labels_query(&a.project_name, Some(a.order_by))
    });

pub const ISSUE_PRELOAD_V2: QueryDefinition<ProjectArgs, Context, Issue> =
    define_query("issuePreloadV2", |a, auth| {
        apply_issue_permissions(
            in_project(zql::issue(), Issue::PROJECT, &a.project_name)
                .related(Issue::LABELS, |l| l)
                .related(Issue::VIEW_STATE, |v| own(v, auth, ViewState::USER_ID))
                .related(Issue::CREATOR, |u| u)
                .related(Issue::ASSIGNEE, |u| u)
                .related(Issue::EMOJI, emoji_with_creator)
                .related(Issue::COMMENTS, |c| {
                    c.related(Comment::CREATOR, |u| u)
                        .related(Comment::EMOJI, emoji_with_creator)
                        .limit(10)
                        .order_by(Comment::CREATED, Direction::Desc)
                })
                .order_by(Issue::MODIFIED, Direction::Desc)
                .order_by(Issue::ID, Direction::Desc)
                .limit(1000),
            role(auth),
        )
    });

pub const USER_PREF: QueryDefinition<String, Context, UserPref, Option<UserPref>> =
    define_query("userPref", |key, auth| {
        // `where('userID', auth?.sub)`: `= null` when logged out.
        let sub = auth
            .as_ref()
            .map_or(Value::Null, |a| Value::from(a.sub.as_str()));
        where_named(
            zql::user_pref().where_eq(UserPref::KEY, key.clone()),
            "userID",
            sub,
        )
        .one()
    });

pub const USERS_FOR_PROJECT: QueryDefinition<UsersForProjectArgs, Context, User> =
    define_query("usersForProject", |a, _| {
        let q = zql::user();
        if let (true, Some(login)) = (a.disabled, truthy(a.login.as_deref())) {
            return q.where_eq(User::LOGIN, login.to_owned());
        }
        let project = a.project_name.clone();
        match a.filter {
            Some(UsersFilter::Crew) => crew(q),
            Some(UsersFilter::Creators) => {
                q.where_exists(User::CREATED_ISSUES, ExistsOptions::default(), move |i| {
                    in_project(i, Issue::PROJECT, &project)
                })
            }
            Some(UsersFilter::Assignees) => {
                q.where_exists(User::ASSIGNED_ISSUES, ExistsOptions::default(), move |i| {
                    in_project(i, Issue::PROJECT, &project)
                })
            }
            None => q,
        }
    });

/// A page of an issue's comments, ordered by (created, id); a backward page
/// flips the order. `start` is the cursor row.
pub const COMMENTS_PAGE: QueryDefinition<CommentsPageArgs, Context, Comment> =
    define_query("commentsPage", |a, auth| {
        let dir = match a.dir {
            PageDirection::Forward => Direction::Asc,
            PageDirection::Backward => Direction::Desc,
        };
        let mut q = apply_comment_permissions(
            zql::comment()
                .related(Comment::CREATOR, |u| u)
                .related(Comment::EMOJI, emoji_with_creator)
                .where_eq(Comment::ISSUE_ID, a.issue_id.clone()),
            role(auth),
        )
        .order_by(Comment::CREATED, dir)
        .order_by(Comment::ID, dir);
        if let Some(start) = &a.start {
            q = q.start_values(
                vec![
                    (Comment::ID.into(), Value::from(start.id.as_str())),
                    (Comment::CREATED.into(), Value::from(start.created)),
                ],
                a.inclusive,
            );
        }
        q.limit(a.limit)
    });

pub const COMMENT: QueryDefinition<String, Context, Comment, Option<Comment>> =
    define_query("comment", |id, auth| {
        apply_comment_permissions(
            zql::comment()
                .related(Comment::CREATOR, |u| u)
                .related(Comment::EMOJI, emoji_with_creator)
                .where_eq(Comment::ID, id.clone()),
            role(auth),
        )
        .one()
    });

pub const ISSUE_DETAIL: QueryDefinition<IssueDetailArgs, Context, Issue, Option<Issue>> =
    define_query("issueDetail", |a, auth| {
        apply_issue_permissions(
            where_named(zql::issue(), a.id_field.column(), a.id.json())
                .related(Issue::PROJECT, |p| p)
                .related(Issue::EMOJI, emoji_with_creator)
                .related(Issue::CREATOR, |u| u)
                .related(Issue::ASSIGNEE, |u| u)
                .related(Issue::LABELS, |l| l)
                .related(Issue::NOTIFICATION_STATE, |n| {
                    own(n, auth, crate::schema::IssueNotifications::USER_ID)
                })
                .related(Issue::VIEW_STATE, |v| {
                    own(v, auth, ViewState::USER_ID).one()
                })
                // The most recent comments, so the comments list renders
                // locally on navigation.
                .related(Issue::COMMENTS, |c| {
                    c.related(Comment::CREATOR, |u| u)
                        .related(Comment::EMOJI, emoji_with_creator)
                        .limit(50)
                        .order_by(Comment::CREATED, Direction::Desc)
                        .order_by(Comment::ID, Direction::Desc)
                })
                .one(),
            role(auth),
        )
    });

pub const ISSUE_LIST_V2: QueryDefinition<IssueListV2Args, Context, Issue> =
    define_query("issueListV2", |a, auth| {
        build_list_query(ListQueryArgs {
            list_context: Some(&a.list_context),
            user_id: auth.as_ref().map(|a| a.sub.as_str()),
            role: role(auth),
            limit: a.limit,
            start: a.start.as_ref(),
            dir: a.dir,
            inclusive: a.inclusive.unwrap_or(false),
        })
    });

/// The number of issues in the list: the same filters and permissions as
/// `issueListV2`, reduced to a `count()`.
pub const ISSUE_LIST_COUNT: QueryDefinition<IssueListCountArgs, Context, Issue, i64> =
    define_query("issueListCount", |a, auth| {
        filter_list_query(zql::issue(), &a.list_context, role(auth)).count()
    });

/// One issue, for the order to start a list at.
pub const LIST_ISSUE_BY_ID: QueryDefinition<ListIssueByIdArgs, Context, Issue, Option<Issue>> =
    define_query("listIssueByID", |a, auth| {
        let q = build_list_query(ListQueryArgs {
            list_context: Some(&a.list_context),
            role: role(auth),
            ..ListQueryArgs::default()
        });
        where_named(q, a.id_field.column(), a.id_value.json()).one()
    });

pub const EMOJI_CHANGE: QueryDefinition<String, Context, Emoji> =
    define_query("emojiChange", |subject_id, auth| {
        let role = role(auth);
        let mut q = zql::emoji().where_eq(Emoji::SUBJECT_ID, subject_id.clone());
        if role != Some(UserRole::Crew) {
            // The subject is an issue or a comment.
            q = q.where_expr(|e| {
                e.or(vec![
                    e.exists(Emoji::ISSUE, ExistsOptions::default(), |i| {
                        apply_issue_permissions(i, role)
                    }),
                    e.exists(Emoji::COMMENT, ExistsOptions::default(), |c| {
                        apply_comment_permissions(c, role)
                    }),
                ])
            });
        }
        q.related(Emoji::CREATOR, |u| u.one())
    });

/// Deprecated in zbugs.
pub const PREV_NEXT: QueryDefinition<PrevNextArgs, Context, Issue, Option<Issue>> =
    define_query("prevNext", |a, auth| {
        build_list_query(ListQueryArgs {
            list_context: a.list_context.as_ref(),
            role: role(auth),
            start: a.issue.as_ref(),
            dir: match a.dir {
                PrevNextDirection::Next => PageDirection::Forward,
                PrevNextDirection::Prev => PageDirection::Backward,
            },
            ..ListQueryArgs::default()
        })
        .one()
    });

/// Deprecated in zbugs.
pub const ISSUE_LIST: QueryDefinition<IssueListArgs, Context, Issue> =
    define_query("issueList", |a, auth| {
        build_list_query(ListQueryArgs {
            list_context: Some(&a.list_context),
            user_id: auth.as_ref().map(|a| a.sub.as_str()),
            role: role(auth),
            limit: Some(a.limit),
            ..ListQueryArgs::default()
        })
    });

/// Deprecated in zbugs.
pub const USER_PICKER: QueryDefinition<UserPickerArgs, Context, User> =
    define_query("userPicker", |a, _| {
        let q = zql::user();
        if let (true, Some(login)) = (a.disabled, truthy(a.login.as_deref())) {
            return q.where_eq(User::LOGIN, login.to_owned());
        }
        match a.filter {
            Some(UserPickerFilter::Crew) => crew(q),
            Some(UserPickerFilter::Creators) => {
                q.where_exists(User::CREATED_ISSUES, ExistsOptions::default(), |i| i)
            }
            None => q,
        }
    });

/// Every query, for the drift test.
pub fn all() -> Vec<Box<dyn NamedQuery<Context>>> {
    vec![
        Box::new(ALL_LABELS),
        Box::new(ALL_USERS),
        Box::new(ALL_PROJECTS),
        Box::new(USER),
        Box::new(CREW_USER),
        Box::new(LABELS),
        Box::new(LABELS_ORDER_BY_NAME),
        Box::new(ISSUE_PRELOAD_V2),
        Box::new(USER_PREF),
        Box::new(USERS_FOR_PROJECT),
        Box::new(COMMENTS_PAGE),
        Box::new(COMMENT),
        Box::new(ISSUE_DETAIL),
        Box::new(ISSUE_LIST_V2),
        Box::new(ISSUE_LIST_COUNT),
        Box::new(LIST_ISSUE_BY_ID),
        Box::new(EMOJI_CHANGE),
        Box::new(PREV_NEXT),
        Box::new(ISSUE_LIST),
        Box::new(USER_PICKER),
    ]
}

#[derive(Default)]
struct ListQueryArgs<'a> {
    list_context: Option<&'a ListContextParams>,
    user_id: Option<&'a str>,
    role: Option<UserRole>,
    limit: Option<i64>,
    start: Option<&'a IssueRowSort>,
    dir: PageDirection,
    inclusive: bool,
}

fn build_list_query(args: ListQueryArgs<'_>) -> Query<Issue> {
    let user_id = truthy(args.user_id).map(str::to_owned);
    let q = zql::issue()
        .related(Issue::VIEW_STATE, move |v| {
            match user_id {
                Some(id) => v.where_eq(ViewState::USER_ID, id),
                None => always_false(v),
            }
            .one()
        })
        .related(Issue::LABELS, |l| l)
        // The issue page cannot render until the creator is present; loading
        // it here lets a click from the list render locally.
        .related(Issue::CREATOR, |u| u);

    let Some(list_context) = args.list_context else {
        return always_false(q);
    };

    let dir = match args.dir {
        PageDirection::Forward => list_context.sort_direction,
        PageDirection::Backward => list_context.sort_direction.flipped(),
    }
    .ast();
    let sort = match list_context.sort_field {
        SortField::Modified => Issue::MODIFIED,
        SortField::Created => Issue::CREATED,
    };
    let mut q = q.order_by(sort, dir).order_by(Issue::ID, dir);

    if let Some(start) = args.start {
        q = q.start_values(
            vec![
                (Issue::ID.into(), Value::from(start.id.as_str())),
                (Issue::CREATED.into(), Value::from(start.created)),
                (Issue::MODIFIED.into(), Value::from(start.modified)),
            ],
            args.inclusive,
        );
    }
    if let Some(limit) = args.limit
        && limit != 0
    {
        q = q.limit(limit);
    }

    filter_list_query(q, list_context, args.role)
}

/// Narrows `q` to the issues `list_context` selects that `role` may see
/// (`filterListQuery`): shared by the list and its count.
fn filter_list_query<T>(
    q: Query<Issue, T>,
    list_context: &ListContextParams,
    role: Option<UserRole>,
) -> Query<Issue, T> {
    let project_name = list_context
        .project_name
        .as_deref()
        .unwrap_or(ZERO_PROJECT_NAME)
        .to_lowercase();
    let q = in_project(q, Issue::PROJECT, &project_name);

    let text = truthy(list_context.text_filter.as_deref()).map(|t| format!("%{}%", escape_like(t)));
    let q = q.where_expr(|e| {
        let mut conditions: Vec<Condition> = Vec::new();
        if let Some(open) = list_context.open {
            conditions.push(Issue::OPEN.eq(open));
        }
        if let Some(text) = &text {
            conditions.push(e.or(vec![
                Issue::TITLE.ilike(text),
                Issue::DESCRIPTION.ilike(text),
                e.exists(Issue::COMMENTS, ExistsOptions::default(), |c| {
                    c.where_(Comment::BODY, SimpleOperator::Ilike, text.clone())
                }),
            ]));
        }
        for label in list_context.labels.iter().flatten() {
            // `label`'s unique key is (projectID, name); the scalar `project`
            // gate pins projectID before the `name` gate runs.
            let (label, project_name) = (label.clone(), project_name.clone());
            conditions.push(e.exists(
                Issue::ISSUE_LABELS,
                ExistsOptions::default(),
                move |il: Query<IssueLabel>| {
                    il.where_exists(IssueLabel::LABEL, scalar(), move |l| {
                        in_project(
                            l.where_eq(Label::NAME, label),
                            Label::PROJECT,
                            &project_name,
                        )
                    })
                },
            ));
        }
        e.and(conditions)
    });

    let mut q = q;
    if let Some(creator) = truthy(list_context.creator.as_deref()) {
        let creator = creator.to_owned();
        q = q.where_exists(Issue::CREATOR, scalar(), move |u| {
            u.where_eq(User::LOGIN, creator)
        });
    }
    if let Some(assignee) = truthy(list_context.assignee.as_deref()) {
        let assignee = assignee.to_owned();
        q = q.where_exists(Issue::ASSIGNEE, scalar(), move |u| {
            u.where_eq(User::LOGIN, assignee)
        });
    }
    apply_issue_permissions(q, role)
}
