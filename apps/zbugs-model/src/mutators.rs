//! The mutators of `apps/zbugs/shared/mutators.ts`. The drift test checks
//! that each changes the same rows as its TS definition, and fails the same
//! way.

use crate::args::*;
use crate::auth::*;
use crate::schema::{
    Comment, CommentUpdate, Emoji, EmojiKey, Issue, IssueKey, IssueLabel, IssueLabelKey,
    IssueNotifications, IssueUpdate, IssueVisibility, Label, UserPref, ViewState, mutate, zql,
};
use zero_client::client::{Error, Mutator, Transaction};
use zero_client::define::{MutatorDefinition, NamedMutator, define_mutator};
use zero_client::typed::{ColumnUpdate, RelationshipAggregates};

/// What a mutator is given besides its arguments: who is logged in.
pub type Context = Option<AuthData>;

pub const CREATE_ISSUE: MutatorDefinition<CreateIssueArgs, Context> =
    define_mutator("issue.create", |tx, args, auth| {
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
    });

pub const UPDATE_ISSUE: MutatorDefinition<UpdateIssueArgs, Context> =
    define_mutator("issue.update", |tx, change, auth| {
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
    });

pub const DELETE_ISSUE: MutatorDefinition<String, Context> =
    define_mutator("issue.delete", |tx, id, auth| {
        assert_is_creator_or_admin(tx, auth, zql::issue(), &id)?;
        mutate::issue(tx).delete(&IssueKey { id })
    });

/// Deletes the issues if the user created every one of them (or is an
/// admin); otherwise none.
pub const DELETE_ISSUES: MutatorDefinition<Vec<String>, Context> =
    define_mutator("issue.deleteMany", |tx, ids, auth| {
        assert_is_logged_in(auth)?;
        for id in &ids {
            assert_is_creator_or_admin(tx, auth, zql::issue(), id)?;
        }
        for id in ids {
            mutate::issue(tx).delete(&IssueKey { id })?;
        }
        Ok(())
    });

pub const ADD_LABEL: MutatorDefinition<IssueLabelArgs, Context> =
    define_mutator("issue.addLabel", |tx, args, auth| {
        assert_is_creator_or_admin(tx, auth, zql::issue(), &args.issue_id)?;
        let project_id = issue_project_id(tx, &args.issue_id)?;
        mutate::issue_label(tx).insert(&issue_label(args.issue_id, args.label_id, project_id))
    });

pub const REMOVE_LABEL: MutatorDefinition<RemoveLabelArgs, Context> =
    define_mutator("issue.removeLabel", |tx, args, auth| {
        assert_is_creator_or_admin(tx, auth, zql::issue(), &args.issue_id)?;
        mutate::issue_label(tx).delete(&IssueLabelKey {
            label_id: args.label_id,
            issue_id: args.issue_id,
        })
    });

pub const UPDATE_NOTIFICATION: MutatorDefinition<NotificationUpdateArgs, Context> =
    define_mutator("notification.update", |tx, args, auth| {
        let auth = assert_is_logged_in(auth)?;
        update_issue_notification(
            tx,
            &auth.sub,
            &args.issue_id,
            args.subscribed,
            args.created,
            true,
        )
    });

pub const ADD_EMOJI_TO_ISSUE: MutatorDefinition<AddEmojiArgs, Context> =
    define_mutator("emoji.addToIssue", |tx, args, auth| {
        add_emoji(tx, EmojiSubject::Issue, args, auth)
    });

pub const ADD_EMOJI_TO_COMMENT: MutatorDefinition<AddEmojiArgs, Context> =
    define_mutator("emoji.addToComment", |tx, args, auth| {
        add_emoji(tx, EmojiSubject::Comment, args, auth)
    });

pub const REMOVE_EMOJI: MutatorDefinition<String, Context> =
    define_mutator("emoji.remove", |tx, id, auth| {
        assert_is_creator_or_admin(tx, auth, zql::emoji(), &id)?;
        mutate::emoji(tx).delete(&EmojiKey { id })
    });

pub const ADD_COMMENT: MutatorDefinition<AddCommentArgs, Context> =
    define_mutator("comment.add", |tx, args, auth| {
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
    });

pub const EDIT_COMMENT: MutatorDefinition<EditCommentArgs, Context> =
    define_mutator("comment.edit", |tx, args, auth| {
        assert_is_creator_or_admin(tx, auth, zql::comment(), &args.id)?;
        mutate::comment(tx).update(&CommentUpdate {
            body: Some(args.body),
            ..CommentUpdate::new(args.id)
        })
    });

pub const REMOVE_COMMENT: MutatorDefinition<String, Context> =
    define_mutator("comment.remove", |tx, id, auth| {
        assert_is_creator_or_admin(tx, auth, zql::comment(), &id)?;
        mutate::comment(tx).delete(&crate::schema::CommentKey { id })
    });

pub const CREATE_LABEL: MutatorDefinition<CreateLabelArgs, Context> =
    define_mutator("label.create", |tx, args, auth| {
        if !is_admin(auth)? {
            return Err(mutation_error(
                "Only admins can create labels",
                MutationErrorCode::NotAuthorized,
                Some(&args.id),
            ));
        }
        mutate::label(tx).insert(&label(args.id, args.name, args.project_id))
    });

pub const CREATE_AND_ADD_LABEL: MutatorDefinition<CreateAndAddLabelArgs, Context> =
    define_mutator("label.createAndAddToIssue", |tx, args, auth| {
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
    });

pub const SET_VIEW_STATE: MutatorDefinition<ViewStateArgs, Context> =
    define_mutator("viewState.set", |tx, args, auth| {
        let user_id = assert_is_logged_in(auth)?.sub.clone();
        mutate::view_state(tx).upsert(&ViewState {
            issue_id: args.issue_id,
            user_id,
            viewed: args.viewed,
        })
    });

pub const SET_USER_PREF: MutatorDefinition<UserPrefArgs, Context> =
    define_mutator("userPref.set", |tx, args, auth| {
        let user_id = assert_is_logged_in(auth)?.sub.clone();
        mutate::user_pref(tx).upsert(&UserPref {
            key: args.key,
            user_id,
            value: args.value,
        })
    });

/// Every mutator, for the client and the drift test.
pub fn all() -> Vec<Box<dyn NamedMutator<Context>>> {
    vec![
        Box::new(CREATE_ISSUE),
        Box::new(UPDATE_ISSUE),
        Box::new(DELETE_ISSUE),
        Box::new(DELETE_ISSUES),
        Box::new(ADD_LABEL),
        Box::new(REMOVE_LABEL),
        Box::new(UPDATE_NOTIFICATION),
        Box::new(ADD_EMOJI_TO_ISSUE),
        Box::new(ADD_EMOJI_TO_COMMENT),
        Box::new(REMOVE_EMOJI),
        Box::new(ADD_COMMENT),
        Box::new(EDIT_COMMENT),
        Box::new(REMOVE_COMMENT),
        Box::new(CREATE_LABEL),
        Box::new(CREATE_AND_ADD_LABEL),
        Box::new(SET_VIEW_STATE),
        Box::new(SET_USER_PREF),
    ]
}

/// The client's mutators: every one, so pending mutations replay by name.
pub fn client_mutators() -> Vec<Mutator<Context>> {
    all().iter().map(|m| m.mutator()).collect()
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
    auth: &Context,
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
