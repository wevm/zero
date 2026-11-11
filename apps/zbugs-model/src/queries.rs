//! The queries of `apps/zbugs/shared/queries.ts`. The drift test checks
//! that each builds the same AST as its TS definition.
//!
//! Each is a [`NamedQuery`]: the client builds its AST for local results
//! and sends its name and arguments, which zero-cache asks the API server
//! to run with the TS definition.

use crate::args::*;
use crate::auth::AuthData;
use crate::schema::{
    Comment, Emoji, Issue, IssueLabel, Label, Project, User, UserPref, UserRole, ViewState, zql,
};
use zero_client::json::Value;
use zero_client::like::escape_like;
use zero_client::typed::{Condition, Direction, ExistsOptions, Query, SimpleOperator, ZeroRow};

pub const ZERO_PROJECT_NAME: &str = "Zero";

/// A query the server runs by name (`defineQuery`): `A` is its argument,
/// `()` for none.
pub struct NamedQuery<A, R, T = Vec<R>> {
    pub name: &'static str,
    build: fn(&A, Option<&AuthData>) -> Query<R, T>,
    encode: fn(&A) -> Vec<Value>,
}

impl<A, R, T> NamedQuery<A, R, T> {
    /// The query for `args`, as the logged-in user (`auth`) sees it.
    pub fn call(&self, args: &A, auth: Option<&AuthData>) -> Query<R, T> {
        let q = (self.build)(args, auth);
        Query::from_any(q.into_any().name_and_args(self.name, (self.encode)(args)))
    }
}

fn no_args(_: &()) -> Vec<Value> {
    Vec::new()
}

fn one_arg<A: serde::Serialize>(args: &A) -> Vec<Value> {
    vec![to_value(args)]
}

/// Builds a query from JSON arguments, with the arguments as encoded.
pub type BuildAny =
    Box<dyn Fn(&[Value], Option<&AuthData>) -> Result<(Query<()>, Vec<Value>), String>>;

/// A query of every query's definition, for the drift test: from the JSON
/// arguments, the query and the arguments as this side encodes them.
pub struct AnyNamedQuery {
    pub name: &'static str,
    pub build: BuildAny,
}

fn erase<A: serde::de::DeserializeOwned + 'static, R: 'static, T: 'static>(
    q: NamedQuery<A, R, T>,
    decode: fn(&[Value]) -> Result<A, String>,
) -> AnyNamedQuery {
    AnyNamedQuery {
        name: q.name,
        build: Box::new(move |args, auth| {
            let a = decode(args)?;
            let built = q.call(&a, auth);
            Ok((Query::from_any(built.into_any()), (q.encode)(&a)))
        }),
    }
}

fn decode_none(args: &[Value]) -> Result<(), String> {
    if args.is_empty() {
        Ok(())
    } else {
        Err(format!("expected no arguments, got {}", args.len()))
    }
}

fn decode_one<A: serde::de::DeserializeOwned>(args: &[Value]) -> Result<A, String> {
    match args {
        [a] => from_value(a),
        _ => Err(format!("expected one argument, got {}", args.len())),
    }
}

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

fn role(auth: Option<&AuthData>) -> Option<UserRole> {
    auth.map(|a| a.role)
}

/// The user's own rows of a per-user relationship, or none when logged
/// out.
fn own<R: ZeroRow, T>(
    q: Query<R, T>,
    auth: Option<&AuthData>,
    user_id: zero_client::typed::Column<R, String>,
) -> Query<R, T> {
    match truthy(auth.map(|a| a.sub.as_str())) {
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

pub const ALL_LABELS: NamedQuery<(), Label> = NamedQuery {
    name: "allLabels",
    build: |_, _| zql::label(),
    encode: no_args,
};

pub const ALL_USERS: NamedQuery<(), User> = NamedQuery {
    name: "allUsers",
    build: |_, _| zql::user(),
    encode: no_args,
};

pub const ALL_PROJECTS: NamedQuery<(), Project> = NamedQuery {
    name: "allProjects",
    build: |_, _| zql::project(),
    encode: no_args,
};

pub const USER: NamedQuery<String, User, Option<User>> = NamedQuery {
    name: "user",
    build: |id, _| zql::user().where_eq(User::ID, id.clone()).one(),
    encode: one_arg,
};

pub const CREW_USER: NamedQuery<String, User, Option<User>> = NamedQuery {
    name: "crewUser",
    build: |id, _| {
        zql::user()
            .where_eq(User::ID, id.clone())
            .where_eq(User::ROLE, UserRole::Crew)
            .one()
    },
    encode: one_arg,
};

pub const LABELS: NamedQuery<ProjectArgs, Label> = NamedQuery {
    name: "labels",
    build: |a, _| labels_query(&a.project_name, None),
    encode: one_arg,
};

pub const LABELS_ORDER_BY_NAME: NamedQuery<LabelsOrderByNameArgs, Label> = NamedQuery {
    name: "labelsOrderByName",
    build: |a, _| labels_query(&a.project_name, Some(a.order_by)),
    encode: one_arg,
};

pub const ISSUE_PRELOAD_V2: NamedQuery<ProjectArgs, Issue> = NamedQuery {
    name: "issuePreloadV2",
    build: |a, auth| {
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
    },
    encode: one_arg,
};

pub const USER_PREF: NamedQuery<String, UserPref, Option<UserPref>> = NamedQuery {
    name: "userPref",
    build: |key, auth| {
        // `where('userID', auth?.sub)`: `= null` when logged out.
        let sub = auth.map_or(Value::Null, |a| Value::from(a.sub.as_str()));
        where_named(
            zql::user_pref().where_eq(UserPref::KEY, key.clone()),
            "userID",
            sub,
        )
        .one()
    },
    encode: one_arg,
};

pub const USERS_FOR_PROJECT: NamedQuery<UsersForProjectArgs, User> = NamedQuery {
    name: "usersForProject",
    build: |a, _| {
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
    },
    encode: one_arg,
};

/// A page of an issue's comments, ordered by (created, id); a backward page
/// flips the order. `start` is the cursor row.
pub const COMMENTS_PAGE: NamedQuery<CommentsPageArgs, Comment> = NamedQuery {
    name: "commentsPage",
    build: |a, auth| {
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
    },
    encode: one_arg,
};

pub const COMMENT: NamedQuery<String, Comment, Option<Comment>> = NamedQuery {
    name: "comment",
    build: |id, auth| {
        apply_comment_permissions(
            zql::comment()
                .related(Comment::CREATOR, |u| u)
                .related(Comment::EMOJI, emoji_with_creator)
                .where_eq(Comment::ID, id.clone()),
            role(auth),
        )
        .one()
    },
    encode: one_arg,
};

pub const ISSUE_DETAIL: NamedQuery<IssueDetailArgs, Issue, Option<Issue>> = NamedQuery {
    name: "issueDetail",
    build: |a, auth| {
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
    },
    encode: one_arg,
};

pub const ISSUE_LIST_V2: NamedQuery<IssueListV2Args, Issue> = NamedQuery {
    name: "issueListV2",
    build: |a, auth| {
        build_list_query(ListQueryArgs {
            list_context: Some(&a.list_context),
            user_id: auth.map(|a| a.sub.as_str()),
            role: role(auth),
            limit: a.limit,
            start: a.start.as_ref(),
            dir: a.dir,
            inclusive: a.inclusive.unwrap_or(false),
        })
    },
    encode: one_arg,
};

/// The number of issues in the list: the same filters and permissions as
/// `issueListV2`, reduced to a `count()`.
pub const ISSUE_LIST_COUNT: NamedQuery<IssueListCountArgs, Issue, i64> = NamedQuery {
    name: "issueListCount",
    build: |a, auth| filter_list_query(zql::issue(), &a.list_context, role(auth)).count(),
    encode: one_arg,
};

/// One issue, for the order to start a list at.
pub const LIST_ISSUE_BY_ID: NamedQuery<ListIssueByIdArgs, Issue, Option<Issue>> = NamedQuery {
    name: "listIssueByID",
    build: |a, auth| {
        let q = build_list_query(ListQueryArgs {
            list_context: Some(&a.list_context),
            role: role(auth),
            ..ListQueryArgs::default()
        });
        where_named(q, a.id_field.column(), a.id_value.json()).one()
    },
    encode: one_arg,
};

pub const EMOJI_CHANGE: NamedQuery<String, Emoji> = NamedQuery {
    name: "emojiChange",
    build: |subject_id, auth| {
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
    },
    encode: one_arg,
};

/// Deprecated in zbugs.
pub const PREV_NEXT: NamedQuery<PrevNextArgs, Issue, Option<Issue>> = NamedQuery {
    name: "prevNext",
    build: |a, auth| {
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
    },
    encode: one_arg,
};

/// Deprecated in zbugs.
pub const ISSUE_LIST: NamedQuery<IssueListArgs, Issue> = NamedQuery {
    name: "issueList",
    build: |a, auth| {
        build_list_query(ListQueryArgs {
            list_context: Some(&a.list_context),
            user_id: auth.map(|a| a.sub.as_str()),
            role: role(auth),
            limit: Some(a.limit),
            ..ListQueryArgs::default()
        })
    },
    encode: one_arg,
};

/// Deprecated in zbugs.
pub const USER_PICKER: NamedQuery<UserPickerArgs, User> = NamedQuery {
    name: "userPicker",
    build: |a, _| {
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
    },
    encode: one_arg,
};

/// Every query, for the drift test.
pub fn all() -> Vec<AnyNamedQuery> {
    vec![
        erase(ALL_LABELS, decode_none),
        erase(ALL_USERS, decode_none),
        erase(ALL_PROJECTS, decode_none),
        erase(USER, decode_one),
        erase(CREW_USER, decode_one),
        erase(LABELS, decode_one),
        erase(LABELS_ORDER_BY_NAME, decode_one),
        erase(ISSUE_PRELOAD_V2, decode_one),
        erase(USER_PREF, decode_one),
        erase(USERS_FOR_PROJECT, decode_one),
        erase(COMMENTS_PAGE, decode_one),
        erase(COMMENT, decode_one),
        erase(ISSUE_DETAIL, decode_one),
        erase(ISSUE_LIST_V2, decode_one),
        erase(ISSUE_LIST_COUNT, decode_one),
        erase(LIST_ISSUE_BY_ID, decode_one),
        erase(EMOJI_CHANGE, decode_one),
        erase(PREV_NEXT, decode_one),
        erase(ISSUE_LIST, decode_one),
        erase(USER_PICKER, decode_one),
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
