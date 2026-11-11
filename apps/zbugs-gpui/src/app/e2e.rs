//! An end-to-end run of the app against zbugs' backend, for CI: built with
//! the `e2e` feature, the app drives itself once its window is up, the way
//! a user would through its own controls' handlers, and exits 0 when every
//! step shows up, or 1 with the step that didn't.
//!
//! - `ZBUGS_E2E=online`: lists issues, creates and deletes one, creates
//!   another, unsubscribes from it, comments, edits and deletes comments,
//!   reacts, labels, assigns, filters, closes it, and waits until it's
//!   marked viewed, and for the server to confirm each mutation. CI then checks the
//!   rows in Postgres.
//! - `ZBUGS_E2E=offline`, with the backend stopped: the window reopens on
//!   the filters and issue the online run left, from the local store.
//!
//! Screenshots of each step go to `ZBUGS_E2E_SHOTS`.

use super::*;
use futures::channel::oneshot;
use std::time::{Duration, Instant};

/// The issue the online run creates, which CI looks for in Postgres.
pub const TITLE: &str = "Created by the zbugs-gpui end-to-end run";
const COMMENT: &str = "Commented by the zbugs-gpui end-to-end run";
/// The comment once edited, which CI looks for in Postgres.
const EDITED: &str = "Edited by the zbugs-gpui end-to-end run";
/// The issue the online run creates and deletes, which CI checks is gone.
const DELETED: &str = "Deleted by the zbugs-gpui end-to-end run";

type Step = Result<(), String>;

pub fn run(window: AnyWindowHandle, app: Entity<ZbugsApp>, cx: &mut App) {
    let Ok(mode) = std::env::var("ZBUGS_E2E") else {
        return;
    };
    cx.spawn(async move |cx| {
        let result = match mode.as_str() {
            "online" => online(window, &app, cx).await,
            "offline" => offline(&app, cx).await,
            other => Err(format!("ZBUGS_E2E is online or offline, not {other}")),
        };
        // The store keeps what the next run (or launch) starts from.
        app.read_with(cx, |app, _| app.session.zero.flush_storage());
        match result {
            Ok(()) => {
                eprintln!("e2e {mode}: passed");
                std::process::exit(0);
            }
            Err(error) => {
                shot("failure");
                eprintln!("e2e {mode}: {error}");
                std::process::exit(1);
            }
        }
    })
    .detach();
}

/// A screenshot of the screen, named for the step.
fn shot(name: &str) {
    let Ok(dir) = std::env::var("ZBUGS_E2E_SHOTS") else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::process::Command::new("screencapture")
        .args(["-x", &format!("{dir}/{name}.png")])
        .status();
}

/// Waits until `ready` holds for the app, for up to 30 seconds.
async fn until(
    app: &Entity<ZbugsApp>,
    cx: &mut AsyncApp,
    what: &str,
    ready: impl Fn(&ZbugsApp) -> bool,
) -> Step {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !app.read_with(cx, |app, _| ready(app)) {
        if Instant::now() > deadline {
            return Err(format!("timed out waiting for {what}"));
        }
        cx.background_executor()
            .timer(Duration::from_millis(50))
            .await;
    }
    Ok(())
}

/// Runs `body` on the app with its window, as an event handler does.
fn act<R>(
    window: AnyWindowHandle,
    app: &Entity<ZbugsApp>,
    cx: &mut AsyncApp,
    body: impl FnOnce(&mut ZbugsApp, &mut Window, &mut Context<ZbugsApp>) -> R,
) -> Result<R, String> {
    cx.update_window(window, |_, window, cx| {
        app.update(cx, |app, cx| body(app, window, cx))
    })
    .map_err(|e| e.to_string())
}

/// Waits for the server to confirm every mutation so far.
async fn settled(app: &Entity<ZbugsApp>, cx: &mut AsyncApp) -> Step {
    let (done, settled) = oneshot::channel();
    app.read_with(cx, |app, _| {
        app.session.zero.when_pending_settled(move || {
            let _ = done.send(());
        })
    });
    settled.await.map_err(|_| "the client closed".to_owned())
}

fn rows(app: &ZbugsApp) -> Vec<Issue> {
    app.list.snapshot().value.clone().unwrap_or_default()
}

fn issue(app: &ZbugsApp) -> Option<Issue> {
    app.detail.as_ref()?.snapshot().value.clone().flatten()
}

async fn online(window: AnyWindowHandle, app: &Entity<ZbugsApp>, cx: &mut AsyncApp) -> Step {
    let me = app
        .read_with(cx, |app, _| app.session.auth.clone())
        .ok_or("the e2e run needs ZBUGS_TOKEN")?;
    until(app, cx, "the open issues and their count", |app| {
        app.list.is_synced() && app.count.is_synced() && !rows(app).is_empty()
    })
    .await?;
    let open = app.read_with(cx, |app, _| app.count.snapshot().value.unwrap_or(0));
    shot("1-list");

    // An issue created and deleted again: it leaves the list and the
    // selection.
    act(window, app, cx, |app, _, cx| {
        until_ready(app.save_issue(None, DELETED.into(), String::new(), cx))
    })??;
    let doomed = app
        .read_with(cx, |app, _| app.selected.clone())
        .ok_or("creating an issue didn't select it")?;
    until(app, cx, "the issue to delete, from the server", |app| {
        issue(app).is_some_and(|i| i.title == DELETED && i.short_id.is_some())
    })
    .await?;
    act(window, app, cx, |app, _, cx| app.delete_issue(&doomed, cx))?;
    until(app, cx, "the deleted issue to leave the list", |app| {
        app.selected.is_none() && !rows(app).iter().any(|i| i.id == doomed)
    })
    .await?;
    settled(app, cx).await?;

    // A new issue, at the top of the list, numbered by the server.
    act(window, app, cx, |app, _, cx| {
        until_ready(app.save_issue(None, TITLE.into(), "Made by **e2e**.".into(), cx))
    })??;
    let id = app
        .read_with(cx, |app, _| app.selected.clone())
        .ok_or("creating an issue didn't select it")?;
    until(app, cx, "the new issue's number from the server", |app| {
        issue(app).is_some_and(|i| i.title == TITLE && i.short_id.is_some())
    })
    .await?;
    until(app, cx, "the new issue at the top of the list", |app| {
        rows(app).first().is_some_and(|i| i.id == id)
            && app.count.snapshot().value == Some(open + 1)
    })
    .await?;

    // Its creator is subscribed to it; unsubscribed here.
    until(app, cx, "the creator's subscription", |app| {
        issue(app).is_some_and(|i| i.notification_state.is_some_and(|n| n.subscribed))
    })
    .await?;
    act(window, app, cx, |app, _, cx| {
        app.set_subscribed(id.clone(), false, cx)
    })?;
    until(app, cx, "the issue unsubscribed", |app| {
        issue(app).is_some_and(|i| i.notification_state.is_some_and(|n| !n.subscribed))
    })
    .await?;

    // A comment through the comment field, and a reaction.
    act(window, app, cx, |app, window, cx| {
        app.comment
            .update(cx, |state, cx| state.set_value(COMMENT, window, cx));
        app.post_comment(window, cx);
    })?;
    until(app, cx, "the comment", |app| {
        issue(app).is_some_and(|i| i.comments.iter().flatten().any(|c| c.body == COMMENT))
    })
    .await?;

    // Another comment, deleted, and the first one edited.
    act(window, app, cx, |app, window, cx| {
        app.comment
            .update(cx, |state, cx| state.set_value("To delete", window, cx));
        app.post_comment(window, cx);
    })?;
    until(app, cx, "the second comment", |app| {
        issue(app).is_some_and(|i| i.comments.iter().flatten().count() == 2)
    })
    .await?;
    act(window, app, cx, |app, _, cx| {
        let comments = issue(app).and_then(|i| i.comments).unwrap_or_default();
        for c in comments {
            if c.body == COMMENT {
                app.edit_comment(c.id, EDITED.into(), cx);
            } else {
                app.delete_comment(&c.id, cx);
            }
        }
    })?;
    until(app, cx, "the edited comment, alone", |app| {
        issue(app).is_some_and(|i| {
            let comments = i.comments.unwrap_or_default();
            comments.len() == 1 && comments[0].body == EDITED
        })
    })
    .await?;
    act(window, app, cx, |app, _, cx| {
        let issue = issue(app).expect("the issue is shown");
        let emoji = issue.emoji.clone().unwrap_or_default();
        app.toggle_emoji(Subject::Issue, &issue.id, &emoji, "👍", cx);
    })?;
    until(app, cx, "the reaction", |app| {
        issue(app).is_some_and(|i| {
            i.emoji
                .iter()
                .flatten()
                .any(|e| e.value == "👍" && e.creator_id == me.sub)
        })
    })
    .await?;

    // The bug label, and the issue assigned to the user.
    until(app, cx, "the labels", |app| {
        app.labels
            .snapshot()
            .value
            .iter()
            .flatten()
            .any(|l| l.name == "bug")
    })
    .await?;
    act(window, app, cx, |app, _, cx| {
        let bug = app
            .labels
            .snapshot()
            .value
            .iter()
            .flatten()
            .find(|l| l.name == "bug")
            .map(|l| l.id.clone());
        let issue = issue(app).expect("the issue is shown");
        app.toggle_label(&issue, bug.expect("there's a bug label"), cx);
        app.set_assignee(issue.id.clone(), Some(me.sub.clone()), cx);
    })?;
    until(app, cx, "the label and the assignee", |app| {
        issue(app).is_some_and(|i| {
            i.labels.iter().flatten().any(|l| l.name == "bug")
                && i.assignee_id.as_deref() == Some(me.sub.as_str())
        })
    })
    .await?;
    settled(app, cx).await?;
    shot("2-edited");

    // Filtered to the user's bugs, then closed: it leaves the open list,
    // and All shows it.
    let login = app
        .read_with(cx, |app, _| {
            app.me
                .as_ref()
                .and_then(|me| me.snapshot().value.clone().flatten())
        })
        .map(|u| u.login)
        .ok_or("the user's row didn't sync")?;
    act(window, app, cx, |app, _, cx| {
        app.refilter(
            |f| {
                f.labels = vec!["bug".into()];
                f.creator = Some(login);
            },
            cx,
        )
    })?;
    until(app, cx, "the issue among the user's open bugs", |app| {
        app.list.is_synced() && rows(app).iter().any(|i| i.id == id)
    })
    .await?;
    act(window, app, cx, |app, _, cx| {
        let issue = issue(app).expect("the issue is shown");
        app.toggle_open(&issue, cx);
    })?;
    until(app, cx, "the closed issue to leave the open list", |app| {
        issue(app).is_some_and(|i| !i.open) && !rows(app).iter().any(|i| i.id == id)
    })
    .await?;
    act(window, app, cx, |app, _, cx| {
        app.refilter(|f| f.status = Status::All, cx)
    })?;
    until(app, cx, "the closed issue under All", |app| {
        rows(app).iter().any(|i| i.id == id)
    })
    .await?;
    // Shown for a second since its last change, it's marked viewed.
    until(app, cx, "the issue marked viewed", |app| {
        issue(app).is_some_and(|i| !is_unread(&i))
    })
    .await?;
    settled(app, cx).await?;
    shot("3-closed");
    Ok(())
}

/// The window the online run left, from the store, with no server.
async fn offline(app: &Entity<ZbugsApp>, cx: &mut AsyncApp) -> Step {
    until(app, cx, "the stored list and the selected issue", |app| {
        app.filters.status == Status::All
            && app.filters.labels == ["bug"]
            && issue(app).is_some_and(|i| i.title == TITLE && !i.open)
            && rows(app).iter().any(|i| i.title == TITLE)
    })
    .await?;
    shot("4-offline");
    Ok(())
}

fn until_ready(saved: bool) -> Step {
    saved
        .then_some(())
        .ok_or_else(|| "the issue didn't save".to_owned())
}
