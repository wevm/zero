//! The mutators of `apps/zbugs/shared/mutators.ts`. The drift test checks
//! that each changes the same rows as its TS definition, and fails the same
//! way.

use crate::args::*;
use crate::auth::*;
use crate::schema::{
    Comment, CommentUpdate, Emoji, EmojiKey, Issue, IssueKey, IssueLabel, IssueLabelKey,
    IssueNotifications, IssueUpdate, IssueVisibility, Label, UserPref, ViewState, mutate, zql,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use zero_client::client::{Error, Mutator, MutatorResult, Transaction, Zero};
use zero_client::json::Value;
use zero_client::typed::{ColumnUpdate, RelationshipAggregates};

/// What a mutator is given besides its arguments: who is logged in.
pub type Context = Option<AuthData>;

type Body<A> = fn(&Transaction, A, Option<&AuthData>) -> Result<(), Error>;

/// A mutator with typed arguments (`defineMutator`).
pub struct MutatorDef<A> {
    pub name: &'static str,
    body: Body<A>,
}

/// A mutation to run: the mutator's name and its arguments as JSON, for
/// `Zero::mutate`.
#[derive(Clone, Debug, PartialEq)]
pub struct MutateRequest {
    pub name: &'static str,
    pub args: Value,
}

impl MutateRequest {
    /// Runs the mutation on `zero`: optimistically now, then on the server.
    pub fn run(self, zero: &Zero<Context>) -> MutatorResult {
        zero.mutate(self.name, Some(self.args))
    }
}

impl<A: Serialize> MutatorDef<A> {
    /// The mutation of this mutator with `args`.
    pub fn with(&self, args: &A) -> MutateRequest {
        MutateRequest {
            name: self.name,
            args: to_value(args),
        }
    }
}

impl<A: Serialize + DeserializeOwned + 'static> MutatorDef<A> {
    /// The client's mutator: reads the JSON arguments as `A`.
    pub fn mutator(&self) -> Mutator<Context> {
        let body = self.body;
        Mutator::new(self.name, move |tx, args, auth: &Context| {
            let args: A = from_value(args.unwrap_or(&Value::Null))
                .map_err(zero_client::client::ApplicationError::new)?;
            body(tx, args, auth.as_ref())?;
            Ok(None)
        })
    }

    /// `args` read as `A` and written back, for the drift test.
    pub fn reencode(&self, args: Option<&Value>) -> Result<Value, String> {
        let a: A = from_value(args.unwrap_or(&Value::Null))?;
        Ok(to_value(&a))
    }
}

/// Reads JSON arguments and writes them back.
pub type Reencode = Box<dyn Fn(Option<&Value>) -> Result<Value, String>>;

/// A mutator of every definition, for the client and the drift test.
pub struct AnyMutator {
    pub name: &'static str,
    pub mutator: Mutator<Context>,
    pub reencode: Reencode,
}

fn erase<A: Serialize + DeserializeOwned + 'static>(def: MutatorDef<A>) -> AnyMutator {
    let mutator = def.mutator();
    let name = def.name;
    AnyMutator {
        name,
        mutator,
        reencode: Box::new(move |args| def.reencode(args)),
    }
}

pub const CREATE_ISSUE: MutatorDef<CreateIssueArgs> = MutatorDef {
    name: "issue.create",
    body: |tx, args, auth| {
        let auth = assert_is_logged_in(auth)?;
        mutate::issue(tx).insert(&Issue {
            id: args.id.clone(),
            short_id: None,
            title: args.title,
            open: true,
            modified: args.modified,
            created: args.created,
            project_id: args.project_id,
            creator_id: auth.sub.clone(),
            assignee_id: None,
            description: args.description.unwrap_or_default(),
            visibility: IssueVisibility::Public,
            project: None,
            issue_labels: None,
            labels: None,
            comments: None,
            creator: None,
            assignee: None,
            view_state: None,
            emoji: None,
            notification_state: None,
            aggregates: RelationshipAggregates::default(),
        })?;
        // The creator follows the issue.
        update_issue_notification(
            tx,
            &auth.sub,
            &args.id,
            NotificationType::Subscribe,
            args.created,
            false,
        )
    },
};

pub const UPDATE_ISSUE: MutatorDef<UpdateIssueArgs> = MutatorDef {
    name: "issue.update",
    body: |tx, change, auth| {
        // Authorization comes first, so the error does not tell whether a
        // private issue exists.
        assert_is_creator_or_admin(tx, auth, zql::issue(), &change.id)?;
        let Some(old) = run(
            tx,
            zql::issue().where_eq(Issue::ID, change.id.clone()).one(),
        )?
        else {
            // An admin skips the creator check; the same error as for anyone
            // else.
            return Err(mutation_error(
                "Issue not found or not authorized",
                MutationErrorCode::NotAuthorized,
                Some(&change.id),
            ));
        };
        mutate::issue(tx).update(&IssueUpdate {
            title: change.title.clone(),
            open: change.open,
            modified: Some(change.modified),
            assignee_id: match &change.assignee_id {
                None => ColumnUpdate::Unchanged,
                Some(None) => ColumnUpdate::Null,
                Some(Some(a)) => ColumnUpdate::Set(a.clone()),
            },
            description: change.description.clone(),
            visibility: change.visibility,
            ..IssueUpdate::new(change.id.clone())
        })?;

        let previous = match &change.assignee_id {
            Some(new) if *new != old.assignee_id => old.assignee_id.clone(),
            _ => None,
        };
        // The assignee follows the issue; the one before stops.
        if let Some(Some(assignee)) = &change.assignee_id
            && !assignee.is_empty()
        {
            update_issue_notification(
                tx,
                assignee,
                &change.id,
                NotificationType::Subscribe,
                change.modified,
                false,
            )?;
        }
        if let Some(previous) = previous.filter(|p| !p.is_empty()) {
            update_issue_notification(
                tx,
                &previous,
                &change.id,
                NotificationType::Unsubscribe,
                change.modified,
                false,
            )?;
        }
        Ok(())
    },
};

pub const DELETE_ISSUE: MutatorDef<String> = MutatorDef {
    name: "issue.delete",
    body: |tx, id, auth| {
        assert_is_creator_or_admin(tx, auth, zql::issue(), &id)?;
        mutate::issue(tx).delete(&IssueKey { id })
    },
};

/// Deletes the issues if the user created every one of them (or is an
/// admin); otherwise none.
pub const DELETE_ISSUES: MutatorDef<Vec<String>> = MutatorDef {
    name: "issue.deleteMany",
    body: |tx, ids, auth| {
        assert_is_logged_in(auth)?;
        for id in &ids {
            assert_is_creator_or_admin(tx, auth, zql::issue(), id)?;
        }
        for id in ids {
            mutate::issue(tx).delete(&IssueKey { id })?;
        }
        Ok(())
    },
};

pub const ADD_LABEL: MutatorDef<IssueLabelArgs> = MutatorDef {
    name: "issue.addLabel",
    body: |tx, args, auth| {
        assert_is_creator_or_admin(tx, auth, zql::issue(), &args.issue_id)?;
        let project_id = issue_project_id(tx, &args.issue_id)?;
        mutate::issue_label(tx).insert(&issue_label(args.issue_id, args.label_id, project_id))
    },
};

pub const REMOVE_LABEL: MutatorDef<RemoveLabelArgs> = MutatorDef {
    name: "issue.removeLabel",
    body: |tx, args, auth| {
        assert_is_creator_or_admin(tx, auth, zql::issue(), &args.issue_id)?;
        mutate::issue_label(tx).delete(&IssueLabelKey {
            label_id: args.label_id,
            issue_id: args.issue_id,
        })
    },
};

pub const UPDATE_NOTIFICATION: MutatorDef<NotificationUpdateArgs> = MutatorDef {
    name: "notification.update",
    body: |tx, args, auth| {
        let auth = assert_is_logged_in(auth)?;
        update_issue_notification(
            tx,
            &auth.sub,
            &args.issue_id,
            args.subscribed,
            args.created,
            true,
        )
    },
};

pub const ADD_EMOJI_TO_ISSUE: MutatorDef<AddEmojiArgs> = MutatorDef {
    name: "emoji.addToIssue",
    body: |tx, args, auth| add_emoji(tx, EmojiSubject::Issue, args, auth),
};

pub const ADD_EMOJI_TO_COMMENT: MutatorDef<AddEmojiArgs> = MutatorDef {
    name: "emoji.addToComment",
    body: |tx, args, auth| add_emoji(tx, EmojiSubject::Comment, args, auth),
};

pub const REMOVE_EMOJI: MutatorDef<String> = MutatorDef {
    name: "emoji.remove",
    body: |tx, id, auth| {
        assert_is_creator_or_admin(tx, auth, zql::emoji(), &id)?;
        mutate::emoji(tx).delete(&EmojiKey { id })
    },
};

pub const ADD_COMMENT: MutatorDef<AddCommentArgs> = MutatorDef {
    name: "comment.add",
    body: |tx, args, auth| {
        let creator_id = assert_is_logged_in(auth)?.sub.clone();
        assert_user_can_see_issue(tx, &creator_id, &args.issue_id)?;
        mutate::comment(tx).insert(&Comment {
            id: args.id,
            issue_id: args.issue_id.clone(),
            created: args.created,
            body: args.body,
            creator_id: creator_id.clone(),
            creator: None,
            emoji: None,
            issue: None,
            aggregates: RelationshipAggregates::default(),
        })?;
        update_issue_notification(
            tx,
            &creator_id,
            &args.issue_id,
            NotificationType::Subscribe,
            args.created,
            false,
        )
    },
};

pub const EDIT_COMMENT: MutatorDef<EditCommentArgs> = MutatorDef {
    name: "comment.edit",
    body: |tx, args, auth| {
        assert_is_creator_or_admin(tx, auth, zql::comment(), &args.id)?;
        mutate::comment(tx).update(&CommentUpdate {
            body: Some(args.body),
            ..CommentUpdate::new(args.id)
        })
    },
};

pub const REMOVE_COMMENT: MutatorDef<String> = MutatorDef {
    name: "comment.remove",
    body: |tx, id, auth| {
        assert_is_creator_or_admin(tx, auth, zql::comment(), &id)?;
        mutate::comment(tx).delete(&crate::schema::CommentKey { id })
    },
};

pub const CREATE_LABEL: MutatorDef<CreateLabelArgs> = MutatorDef {
    name: "label.create",
    body: |tx, args, auth| {
        if !is_admin(auth)? {
            return Err(mutation_error(
                "Only admins can create labels",
                MutationErrorCode::NotAuthorized,
                Some(&args.id),
            ));
        }
        mutate::label(tx).insert(&label(args.id, args.name, args.project_id))
    },
};

pub const CREATE_AND_ADD_LABEL: MutatorDef<CreateAndAddLabelArgs> = MutatorDef {
    name: "label.createAndAddToIssue",
    body: |tx, args, auth| {
        if !is_admin(auth)? {
            return Err(mutation_error(
                "Only admins can create labels",
                MutationErrorCode::NotAuthorized,
                Some(&args.label_id),
            ));
        }
        let project_id = issue_project_id(tx, &args.issue_id)?;
        mutate::label(tx).insert(&label(
            args.label_id.clone(),
            args.label_name,
            project_id.clone(),
        ))?;
        mutate::issue_label(tx).insert(&issue_label(args.issue_id, args.label_id, project_id))
    },
};

pub const SET_VIEW_STATE: MutatorDef<ViewStateArgs> = MutatorDef {
    name: "viewState.set",
    body: |tx, args, auth| {
        let user_id = assert_is_logged_in(auth)?.sub.clone();
        mutate::view_state(tx).upsert(&ViewState {
            issue_id: args.issue_id,
            user_id,
            viewed: args.viewed,
        })
    },
};

pub const SET_USER_PREF: MutatorDef<UserPrefArgs> = MutatorDef {
    name: "userPref.set",
    body: |tx, args, auth| {
        let user_id = assert_is_logged_in(auth)?.sub.clone();
        mutate::user_pref(tx).upsert(&UserPref {
            key: args.key,
            user_id,
            value: args.value,
        })
    },
};

/// Every mutator, for the client and the drift test.
pub fn all() -> Vec<AnyMutator> {
    vec![
        erase(CREATE_ISSUE),
        erase(UPDATE_ISSUE),
        erase(DELETE_ISSUE),
        erase(DELETE_ISSUES),
        erase(ADD_LABEL),
        erase(REMOVE_LABEL),
        erase(UPDATE_NOTIFICATION),
        erase(ADD_EMOJI_TO_ISSUE),
        erase(ADD_EMOJI_TO_COMMENT),
        erase(REMOVE_EMOJI),
        erase(ADD_COMMENT),
        erase(EDIT_COMMENT),
        erase(REMOVE_COMMENT),
        erase(CREATE_LABEL),
        erase(CREATE_AND_ADD_LABEL),
        erase(SET_VIEW_STATE),
        erase(SET_USER_PREF),
    ]
}

/// The client's mutators: every one, so pending mutations replay by name.
pub fn client_mutators() -> Vec<Mutator<Context>> {
    all().into_iter().map(|m| m.mutator).collect()
}

fn label(id: String, name: String, project_id: String) -> Label {
    Label {
        id,
        name,
        project_id,
        project: None,
        aggregates: RelationshipAggregates::default(),
    }
}

fn issue_label(issue_id: String, label_id: String, project_id: String) -> IssueLabel {
    IssueLabel {
        issue_id,
        label_id,
        project_id,
        issue: None,
        label: None,
        aggregates: RelationshipAggregates::default(),
    }
}

/// The project of an issue: the issue decides it, never the caller.
fn issue_project_id(tx: &Transaction, issue_id: &str) -> Result<String, Error> {
    match run(
        tx,
        zql::issue().where_eq(Issue::ID, issue_id.to_owned()).one(),
    )? {
        Some(issue) => Ok(issue.project_id),
        None => Err(mutation_error(
            "Issue not found",
            MutationErrorCode::EntityNotFound,
            Some(issue_id),
        )),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EmojiSubject {
    Issue,
    Comment,
}

fn add_emoji(
    tx: &Transaction,
    subject: EmojiSubject,
    args: AddEmojiArgs,
    auth: Option<&AuthData>,
) -> Result<(), Error> {
    let creator_id = assert_is_logged_in(auth)?.sub.clone();
    match subject {
        EmojiSubject::Issue => assert_user_can_see_issue(tx, &creator_id, &args.subject_id)?,
        EmojiSubject::Comment => assert_user_can_see_comment(tx, &creator_id, &args.subject_id)?,
    }
    mutate::emoji(tx).insert(&Emoji {
        id: args.id,
        value: args.unicode,
        annotation: args.annotation,
        subject_id: args.subject_id.clone(),
        creator_id: creator_id.clone(),
        created: args.created,
        creator: None,
        issue: None,
        comment: None,
        aggregates: RelationshipAggregates::default(),
    })?;
    // An emoji on the issue itself follows the issue.
    if subject == EmojiSubject::Issue {
        update_issue_notification(
            tx,
            &creator_id,
            &args.subject_id,
            NotificationType::Subscribe,
            args.created,
            false,
        )?;
    }
    Ok(())
}

/// Subscribes to an issue, unless the user chose before and `force_update`
/// is false, or unsubscribes from it.
fn update_issue_notification(
    tx: &Transaction,
    user_id: &str,
    issue_id: &str,
    subscribed: NotificationType,
    created: f64,
    force_update: bool,
) -> Result<(), Error> {
    assert_user_can_see_issue(tx, user_id, issue_id)?;
    let existing = run(
        tx,
        zql::issue_notifications()
            .where_eq(IssueNotifications::USER_ID, user_id.to_owned())
            .where_eq(IssueNotifications::ISSUE_ID, issue_id.to_owned())
            .one(),
    )?;
    let row = |subscribed| IssueNotifications {
        user_id: user_id.to_owned(),
        issue_id: issue_id.to_owned(),
        subscribed,
        created,
    };
    match subscribed {
        NotificationType::Subscribe if existing.is_none() || force_update => {
            mutate::issue_notifications(tx).upsert(&row(true))
        }
        NotificationType::Unsubscribe => mutate::issue_notifications(tx).upsert(&row(false)),
        NotificationType::Subscribe => Ok(()),
    }
}
