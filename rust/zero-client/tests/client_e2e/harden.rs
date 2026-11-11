//! Chaos against the real zero-cache, as Swift's `HardenE2ETests`:
//! connections cut, stalled, and refused by a [`ChaosProxy`], data changed
//! in Postgres behind the client's back, mutations replayed across outages
//! and relaunches. The oracle is convergence: afterwards the client holds
//! what Postgres holds, every mutation applied exactly once.
//!
//! Swift also checks the outcomes its `addMutationOutcomeListener` hears;
//! that listener is Swift's own (JS has none), so here exactly once rests
//! on Postgres: its rows and the client's last mutation ID.

use super::chaos::{
    Chaos, LogCollector, complete, connected, difference, issues, last_mutation_id, open_issue_ids,
    psql, run, settled, sink, wait_for,
};
use super::proxy::{Accept, ChaosProxy};
use super::{MutationOutcomes, SUCCESS, close, create, row, rows, unique};
use std::collections::BTreeSet;
use std::rc::Rc;
use std::time::{Duration, Instant};
use zero_client::json::Value;
use zero_client::ttl::Ttl;

const LIMIT: Duration = Duration::from_secs(120);

fn secs(s: u64) -> Duration {
    Duration::from_secs(s)
}

async fn sleep_ms(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
}

/// Waits for `proxy` to have accepted `n` connections.
async fn accepted(proxy: &ChaosProxy, n: usize) {
    wait_for(&format!("{n} connections"), secs(30), || {
        proxy.accepted() >= n
    })
    .await;
}

/// Waits up to `timeout` for `proxy` to have at most `n` open connections.
async fn at_most_open(proxy: &ChaosProxy, n: usize, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while proxy.open_connections() > n && Instant::now() < deadline {
        sleep_ms(20).await;
    }
}

fn set(ids: impl IntoIterator<Item = String>) -> BTreeSet<String> {
    ids.into_iter().collect()
}

// Servers that don't answer

/// A server that accepts the TCP connection and never answers the
/// upgrade. The connect timeout (10 s, `CONNECT_TIMEOUT_MS`) gives up on it
/// and closes the socket; then the client reconnects once the server
/// answers, and no socket is left behind.
#[tokio::test]
async fn hung_upgrade_times_out_closes_the_socket_and_recovers() {
    let Some(chaos) = Chaos::new().await else {
        return;
    };
    run(LIMIT, async {
        let proxy = ChaosProxy::new(&chaos.upstream());
        proxy.set(|k| k.accept = Accept::Blackhole);
        let zero = chaos.zero(Some(&proxy.cache_url()), &unique("chaos-key"), true, |_| {});
        accepted(&proxy, 1).await;
        proxy.set(|k| k.accept = Accept::Pass);
        // 10 s for the timeout, 5 s of backoff.
        wait_for("the client to connect", secs(25), || connected(&zero)).await;
        assert_eq!(proxy.accepted(), 2);
        // The hung socket was closed when its attempt timed out.
        at_most_open(&proxy, 1, secs(2)).await;
        assert_eq!(proxy.open_connections(), 1, "the timed-out socket leaked");
        zero.close();
    })
    .await;
}

/// A server that completes the WebSocket upgrade and never sends
/// `connected`: the ack timeout closes the socket, and the client connects
/// once the server behaves.
#[tokio::test]
async fn open_socket_without_connected_times_out_and_recovers() {
    let Some(chaos) = Chaos::new().await else {
        return;
    };
    run(LIMIT, async {
        let proxy = ChaosProxy::new(&chaos.upstream());
        proxy.set(|k| k.accept = Accept::SilentAfterUpgrade);
        let zero = chaos.zero(Some(&proxy.cache_url()), &unique("chaos-key"), true, |_| {});
        let open = chaos.open(&zero);
        accepted(&proxy, 1).await;
        proxy.set(|k| k.accept = Accept::Pass);
        assert!(!connected(&zero));
        wait_for("the client to connect", secs(25), || connected(&zero)).await;
        wait_for("the open issues", secs(30), || complete(&open)).await;
        at_most_open(&proxy, 1, secs(2)).await;
        assert_eq!(proxy.open_connections(), 1);
        zero.close();
    })
    .await;
}

// Cuts and slow reads

/// The connection is cut partway through a large initial poke, several
/// times. The partial poke must not be applied, and the client must
/// converge on Postgres once a connection survives.
#[tokio::test]
async fn cuts_mid_initial_poke_converge() {
    let Some(chaos) = Chaos::new().await else {
        return;
    };
    run(LIMIT, async {
        chaos.seed(3000, 300).await;
        let expected = open_issue_ids(&chaos.prefix);
        assert_eq!(expected.len(), 3000);
        let proxy = ChaosProxy::new(&chaos.upstream());
        proxy.set(|k| k.cut_after_downstream_bytes = Some(150_000));
        let zero = chaos.zero(
            Some(&proxy.cache_url()),
            &unique("chaos-key"),
            false,
            |_| {},
        );
        let open = chaos.open(&zero);
        accepted(&proxy, 3).await;
        // Never a partial result: either nothing of the seed, or all of it.
        let got = chaos.ids(&open);
        assert!(
            got.is_empty() || got == expected,
            "{} of {} rows",
            got.len(),
            expected.len()
        );
        assert!(!complete(&open));
        proxy.set(|k| k.cut_after_downstream_bytes = None);
        wait_for("the issues after the cuts", secs(40), || {
            complete(&open) && chaos.ids(&open) == expected
        })
        .await;
        zero.close();
    })
    .await;
}

/// A slow reader: the client takes a ~3 MB initial poke at 600 KB/s. Each
/// message the server sends resets the ping timer, so the client must not
/// give up on the connection while the poke trickles in.
#[tokio::test]
async fn slow_reader_large_poke_completes_on_one_connection() {
    let Some(chaos) = Chaos::new().await else {
        return;
    };
    run(LIMIT, async {
        chaos.seed(3000, 1000).await;
        let expected = open_issue_ids(&chaos.prefix);
        let proxy = ChaosProxy::new(&chaos.upstream());
        proxy.set(|k| k.downstream_rate = Some(600_000));
        let zero = chaos.zero(
            Some(&proxy.cache_url()),
            &unique("chaos-key"),
            false,
            |_| {},
        );
        let start = Instant::now();
        let open = chaos.open(&zero);
        wait_for("the throttled poke", secs(50), || complete(&open)).await;
        println!(
            "[chaos] throttled poke of {} rows in {:?}",
            expected.len(),
            start.elapsed()
        );
        let got = chaos.ids(&open);
        assert!(got == expected, "{}", difference(&got, &expected));
        assert_eq!(
            proxy.accepted(),
            1,
            "the client reconnected during the slow poke"
        );
        zero.close();
    })
    .await;
}

// Mutations across outages

/// Mutations made while zero-cache is unreachable are replayed in order
/// once it is back, each applied exactly once: Postgres holds what the
/// client predicted, and the server's last mutation ID for the client is
/// the number of mutations.
#[tokio::test]
async fn offline_mutations_replay_in_order_exactly_once() {
    let Some(chaos) = Chaos::new().await else {
        return;
    };
    run(LIMIT, async {
        let proxy = ChaosProxy::new(&chaos.upstream());
        let zero = chaos.zero(Some(&proxy.cache_url()), &unique("chaos-key"), true, |_| {});
        let open = chaos.open(&zero);
        wait_for("the open issues", secs(30), || complete(&open)).await;

        proxy.set(|k| k.accept = Accept::Refuse);
        proxy.cut_all();
        wait_for("the cut", secs(30), || !connected(&zero)).await;

        let mut results: Vec<MutationOutcomes> = Vec::new();
        let mut expected_open = BTreeSet::new();
        for n in 0..30 {
            let id = format!("{}{n}", chaos.prefix);
            results.push(create(&zero, &id, &format!("Offline {n}"), None));
            expected_open.insert(id.clone());
            if n % 3 == 0 {
                // Only applies after the create before it.
                results.push(close(&zero, &id));
                expected_open.remove(&id);
            }
        }
        assert_eq!(chaos.ids(&open), expected_open);
        tokio::time::sleep(secs(1)).await;
        proxy.set(|k| k.accept = Accept::Pass);

        for (i, r) in results.iter().enumerate() {
            let outcome = settled(&r.1, secs(30)).await;
            assert_eq!(outcome, Some(SUCCESS), "mutation {}", i + 1);
        }
        assert_eq!(open_issue_ids(&chaos.prefix), expected_open);
        assert_eq!(issues(&chaos.prefix).len(), 30);
        assert_eq!(
            last_mutation_id(&zero.client_id()),
            Some(results.len() as u64)
        );
        wait_for("the client to settle", secs(30), || {
            chaos.ids(&open) == expected_open
        })
        .await;

        let fresh = chaos.direct();
        let fresh_open = chaos.open(&fresh);
        wait_for("a fresh client", secs(30), || complete(&fresh_open)).await;
        assert_eq!(chaos.ids(&fresh_open), expected_open);
        fresh.close();
        zero.close();
    })
    .await;
}

/// Mutations made while the connection is cut over and over: some are
/// pushed and lost, some pushed twice, some confirmed only by the
/// reconnect's last mutation ID. Each is applied exactly once.
#[tokio::test]
async fn mutations_racing_connection_cuts_apply_exactly_once() {
    let Some(chaos) = Chaos::new().await else {
        return;
    };
    run(LIMIT, async {
        let proxy = ChaosProxy::new(&chaos.upstream());
        let zero = chaos.zero(Some(&proxy.cache_url()), &unique("chaos-key"), true, |_| {});
        let open = chaos.open(&zero);
        wait_for("the open issues", secs(30), || complete(&open)).await;
        let mut results = Vec::new();
        let mut expected = BTreeSet::new();
        for n in 0..60 {
            let id = format!("{}{n}", chaos.prefix);
            results.push(create(&zero, &id, &format!("Racing {n}"), None));
            expected.insert(id);
            if n % 7 == 3 {
                proxy.cut_all();
            }
            sleep_ms(30).await;
        }
        for (i, r) in results.iter().enumerate() {
            let outcome = settled(&r.1, secs(40)).await;
            assert_eq!(outcome, Some(SUCCESS), "mutation {}", i + 1);
        }
        assert_eq!(open_issue_ids(&chaos.prefix), expected);
        assert_eq!(
            last_mutation_id(&zero.client_id()),
            Some(results.len() as u64)
        );
        wait_for("the client to settle", secs(30), || {
            chaos.ids(&open) == expected
        })
        .await;
        println!(
            "[chaos] {} connections for 60 racing mutations",
            proxy.accepted()
        );
        zero.close();
    })
    .await;
}

/// The server applies a mutation but its response is lost: the server
/// stalls on the way back, the ping times out, and the connection is cut.
/// The reconnect's last mutation ID confirms the mutation, which must not
/// be applied again.
#[tokio::test]
async fn lost_push_response_confirmed_by_reconnect() {
    let Some(chaos) = Chaos::new().await else {
        return;
    };
    run(LIMIT, async {
        let proxy = ChaosProxy::new(&chaos.upstream());
        let zero = chaos.zero(Some(&proxy.cache_url()), &unique("chaos-key"), true, |o| {
            o.ping_timeout_ms = 1000.0
        });
        let open = chaos.open(&zero);
        wait_for("the open issues", secs(30), || complete(&open)).await;
        proxy.set(|k| k.downstream_rate = Some(0));
        let id = format!("{}lost-response", chaos.prefix);
        let created = create(&zero, &id, "Lost response", None);
        wait_for("the server to apply it", secs(30), || {
            open_issue_ids(&chaos.prefix) == set([id.clone()])
        })
        .await;
        // The ping fails, and the client reconnects once the server is heard.
        wait_for("the ping timeout", secs(30), || !connected(&zero)).await;
        proxy.set(|k| k.downstream_rate = None);
        assert_eq!(settled(&created.1, secs(20)).await, Some(SUCCESS));
        wait_for("the client to connect", secs(30), || connected(&zero)).await;
        assert_eq!(last_mutation_id(&zero.client_id()), Some(1));
        wait_for("the confirmed row", secs(30), || {
            chaos.ids(&open).contains(&id)
        })
        .await;
        // Still syncing.
        let next = close(&zero, &id);
        assert_eq!(settled(&next.1, secs(10)).await, Some(SUCCESS));
        wait_for("the close to sync", secs(30), || {
            !chaos.ids(&open).contains(&id)
        })
        .await;
        zero.close();
    })
    .await;
}

/// A push that never reaches the server: what the client sends is held,
/// then the connection is cut. The reconnect pushes it again.
#[tokio::test]
async fn push_lost_on_the_way_is_sent_again() {
    let Some(chaos) = Chaos::new().await else {
        return;
    };
    run(LIMIT, async {
        let proxy = ChaosProxy::new(&chaos.upstream());
        let zero = chaos.zero(Some(&proxy.cache_url()), &unique("chaos-key"), true, |_| {});
        let open = chaos.open(&zero);
        wait_for("the open issues", secs(30), || complete(&open)).await;
        proxy.set(|k| k.stall_upstream = true);
        let issue_ids: Vec<String> = (0..5)
            .map(|n| format!("{}held-{n}", chaos.prefix))
            .collect();
        let results: Vec<_> = issue_ids
            .iter()
            .map(|id| create(&zero, id, "Held", None))
            .collect();
        sleep_ms(300).await;
        assert!(open_issue_ids(&chaos.prefix).is_empty());
        proxy.cut_all();
        proxy.set(|k| k.stall_upstream = false);
        for r in &results {
            assert_eq!(settled(&r.1, secs(20)).await, Some(SUCCESS));
        }
        assert_eq!(open_issue_ids(&chaos.prefix), set(issue_ids));
        assert_eq!(last_mutation_id(&zero.client_id()), Some(5));
        zero.close();
    })
    .await;
}

/// The server stops sending while the socket stays open: the ping times
/// out (`ping_timeout_ms`), the client drops the connection and opens a new
/// one, and catches up on what changed in Postgres meanwhile.
#[tokio::test]
async fn stalled_server_is_detected_by_ping_and_caught_up() {
    let Some(chaos) = Chaos::new().await else {
        return;
    };
    run(LIMIT, async {
        let proxy = ChaosProxy::new(&chaos.upstream());
        let zero = chaos.zero(Some(&proxy.cache_url()), &unique("chaos-key"), true, |o| {
            o.ping_timeout_ms = 1000.0
        });
        let open = chaos.open(&zero);
        wait_for("the open issues", secs(30), || complete(&open)).await;
        proxy.set(|k| k.downstream_rate = Some(0));
        let id = format!("{}while-stalled", chaos.prefix);
        psql(&format!(
            "INSERT INTO issues (id, title, description, closed, owner_id, \"createdAt\")
             VALUES ('{id}', 'Behind the back', '', false, 'u3', 7)"
        ));
        // Ping after 1 s of silence, give up after 1 s more.
        wait_for("the ping timeout", secs(5), || !connected(&zero)).await;
        // The stalled socket is closed. The proxy sees that a moment after
        // the client gives up, by when the client may have opened the next
        // one.
        let stalled_closed = || {
            let open = proxy.open_connections();
            open == 0 || (open == 1 && proxy.accepted() >= 2)
        };
        let deadline = Instant::now() + secs(5);
        while !stalled_closed() && Instant::now() < deadline {
            sleep_ms(20).await;
        }
        assert!(stalled_closed(), "the stalled socket was left open");
        proxy.set(|k| k.downstream_rate = None);
        wait_for("the row written behind the client's back", secs(30), || {
            row(&rows(&open.data()), &id)
                .and_then(|r| r.get("owner"))
                .and_then(|o| o.get("name"))
                .and_then(Value::as_str)
                == Some("Çağrı")
        })
        .await;
        assert!(proxy.accepted() >= 2);
        zero.close();
    })
    .await;
}

// Postgres behind the client's back

/// Inserts, updates, and deletes made with psql arrive, including a
/// related row with a large JSON value.
#[tokio::test]
async fn postgres_writes_arrive_including_large_json() {
    let Some(chaos) = Chaos::new().await else {
        return;
    };
    let user = format!("{}user", chaos.prefix);
    run(LIMIT, async {
        let zero = chaos.direct();
        let open = chaos.open(&zero);
        wait_for("the open issues", secs(30), || complete(&open)).await;
        let id = format!("{}psql", chaos.prefix);
        // ~2 MB of JSON: 20,000 contacts.
        psql(&format!(
            "INSERT INTO users (id, name, metadata) SELECT '{user}', 'Big',
               jsonb_build_object('registrar', 'google', 'email', 'big@example.com',
                 'altContacts', jsonb_agg(n || ':' || repeat('é😀', 30) ORDER BY n))
             FROM generate_series(1, 20000) AS n;
             INSERT INTO issues (id, title, description, closed, owner_id, \"createdAt\")
             VALUES ('{id}', 'From psql', '', false, '{user}', 8);"
        ));
        let metadata = || {
            row(&rows(&open.data()), &id)
                .and_then(|r| r.get("owner"))
                .and_then(|o| o.get("metadata"))
                .cloned()
        };
        let start = Instant::now();
        wait_for("the issue and its big owner", secs(30), || {
            metadata().is_some()
        })
        .await;
        println!("[chaos] 2 MB JSON row arrived in {:?}", start.elapsed());
        let metadata = metadata().unwrap();
        let contacts = metadata.get("altContacts").unwrap().as_array().unwrap();
        assert_eq!(contacts.len(), 20000);
        assert_eq!(
            contacts.last().and_then(Value::as_str),
            Some(format!("20000:{}", "é😀".repeat(30)).as_str())
        );
        psql(&format!(
            "UPDATE issues SET title = 'Renamed in psql' WHERE id = '{id}'"
        ));
        wait_for("the update", secs(30), || {
            row(&rows(&open.data()), &id)
                .and_then(|r| r.get("title"))
                .and_then(Value::as_str)
                == Some("Renamed in psql")
        })
        .await;
        psql(&format!("DELETE FROM issues WHERE id = '{id}'"));
        wait_for("the delete", secs(30), || !chaos.ids(&open).contains(&id)).await;
        zero.close();
    })
    .await;
    psql(&format!("DELETE FROM issues WHERE owner_id = '{user}'"));
    psql(&format!("DELETE FROM users WHERE id = '{user}'"));
}

/// Two clients of one user create the same issue at once. The server's
/// insert is `ON CONFLICT DO NOTHING` (`makeServerTableCRUD` in
/// `packages/zero-server/src/custom.ts`), so both succeed and the first
/// wins; both clients must drop their optimistic row for Postgres's.
#[tokio::test]
async fn two_clients_racing_on_one_row_converge() {
    let Some(chaos) = Chaos::new().await else {
        return;
    };
    run(LIMIT, async {
        let a = chaos.direct();
        let b = chaos.direct();
        let open_a = chaos.open(&a);
        let open_b = chaos.open(&b);
        wait_for("both clients", secs(30), || {
            complete(&open_a) && complete(&open_b)
        })
        .await;
        let title_in = |open: &zero_client::client::MaterializedView, id: &str| {
            row(&rows(&open.data()), id)
                .and_then(|r| r.get("title"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        for round in 0..10 {
            let id = format!("{}race-{round}", chaos.prefix);
            let ra = create(&a, &id, "A", None);
            let rb = create(&b, &id, "B", None);
            assert_eq!(
                settled(&ra.1, secs(10)).await,
                Some(SUCCESS),
                "round {round}"
            );
            assert_eq!(
                settled(&rb.1, secs(10)).await,
                Some(SUCCESS),
                "round {round}"
            );
            let title = psql(&format!("SELECT title FROM issues WHERE id = '{id}'"))
                .first()
                .cloned();
            assert!(title.is_some(), "round {round}");
            wait_for(&format!("round {round} to converge"), secs(30), || {
                title_in(&open_a, &id) == title && title_in(&open_b, &id) == title
            })
            .await;
        }
        a.close();
        b.close();
    })
    .await;
}

// Queries

/// Hundreds of queries materialized and destroyed in a burst. The server
/// ends up with the queries still open active, the client keeps syncing,
/// and every destroyed query is let go.
#[tokio::test]
async fn query_churn_settles_on_the_server() {
    let Some(chaos) = Chaos::new().await else {
        return;
    };
    run(LIMIT, async {
        let zero = chaos.direct();
        let open = chaos.open(&zero);
        wait_for("the open issues", secs(30), || complete(&open)).await;
        let mut kept = Vec::new();
        for n in 0..300 {
            let q = zero
                .materialize(
                    &chaos.e.by_id(&format!("{}churn-{n}", chaos.prefix)),
                    &Ttl::None,
                )
                .unwrap();
            if n % 50 == 0 {
                kept.push(q);
            } else {
                q.destroy();
            }
            if n % 20 == 0 {
                sleep_ms(1).await;
            }
        }
        wait_for("the kept queries", secs(30), || kept.iter().all(complete)).await;
        let id = format!("{}churn-100", chaos.prefix);
        let created = create(&zero, &id, "Churned", None);
        assert_eq!(settled(&created.1, secs(10)).await, Some(SUCCESS));
        wait_for("a kept query to see its row", secs(30), || {
            rows(&kept[2].data())
                .first()
                .and_then(|r| r.get("title"))
                .and_then(Value::as_str)
                == Some("Churned")
        })
        .await;
        // `issues.open` and the kept `issues.byID`s.
        let client = zero.client_id();
        let want = (1 + kept.len()).to_string();
        wait_for("the server's desired queries", secs(15), || {
            psql(&format!(
                "SELECT count(*) FROM \"zero_0/cvr\".desires
                 WHERE \"clientID\" = '{client}' AND NOT coalesce(deleted, false)
                   AND \"inactivatedAt\" IS NULL"
            ))
            .first()
                == Some(&want)
        })
        .await;
        zero.close();
    })
    .await;
}

// Storage

/// Clients opened on one SQLite store, each making a mutation and closed
/// at once, before it could push. The last one pushes them all, each
/// applied once.
#[tokio::test]
async fn rapid_close_and_reopen_on_sqlite_loses_no_mutation() {
    let Some(chaos) = Chaos::new().await else {
        return;
    };
    run(LIMIT, async {
        let key = unique("chaos-key");
        let mut client_ids = Vec::new();
        for n in 0..15 {
            let zero = chaos.zero(None, &key, true, |_| {});
            client_ids.push(zero.client_id());
            create(
                &zero,
                &format!("{}{n}", chaos.prefix),
                &format!("Reopened {n}"),
                None,
            );
            zero.close();
        }
        let last = chaos.zero(None, &key, true, |_| {});
        let open = chaos.open(&last);
        let expected = set((0..15).map(|n| format!("{}{n}", chaos.prefix)));
        wait_for("every mutation in Postgres", secs(30), || {
            open_issue_ids(&chaos.prefix) == expected
        })
        .await;
        for id in &client_ids {
            assert_eq!(last_mutation_id(id), Some(1), "{id}");
        }
        wait_for("the last client to see them", secs(30), || {
            chaos.ids(&open) == expected
        })
        .await;
        last.close();
    })
    .await;
}

/// A relaunch while the server is down and mutations are pending: the new
/// client shows them from its store, and pushes them once the server is
/// back.
#[tokio::test]
async fn relaunch_with_pending_mutations_while_server_is_down() {
    let Some(chaos) = Chaos::new().await else {
        return;
    };
    run(LIMIT, async {
        let proxy = ChaosProxy::new(&chaos.upstream());
        proxy.set(|k| k.accept = Accept::Refuse);
        let key = unique("chaos-key");
        let first = chaos.zero(Some(&proxy.cache_url()), &key, true, |_| {});
        let issue_ids: Vec<String> = (0..5)
            .map(|n| format!("{}pending-{n}", chaos.prefix))
            .collect();
        for id in &issue_ids {
            create(&first, id, "Pending", None);
        }
        close(&first, &issue_ids[0]);
        let first_id = first.client_id();
        first.close();
        drop(first);

        let second = chaos.zero(Some(&proxy.cache_url()), &key, true, |_| {});
        let open = chaos.open(&second);
        let expected = set(issue_ids[1..].iter().cloned());
        wait_for("the pending rows from the store", secs(30), || {
            chaos.ids(&open) == expected
        })
        .await;
        proxy.set(|k| k.accept = Accept::Pass);
        wait_for("the mutations in Postgres", secs(30), || {
            open_issue_ids(&chaos.prefix) == expected
        })
        .await;
        assert_eq!(issues(&chaos.prefix).len(), 5);
        assert_eq!(last_mutation_id(&first_id), Some(6));
        wait_for("the confirmed rows", secs(30), || {
            complete(&open) && chaos.ids(&open) == expected
        })
        .await;
        second.close();
    })
    .await;
}

/// Clients opened and closed in a burst, some mid-handshake: every socket
/// is closed, including one closed just as its 101 arrives.
#[tokio::test]
async fn rapid_open_close_leaves_no_sockets() {
    let Some(chaos) = Chaos::new().await else {
        return;
    };
    run(LIMIT, async {
        let proxy = ChaosProxy::new(&chaos.upstream());
        for n in 0..20 {
            let zero = chaos.zero(
                Some(&proxy.cache_url()),
                &unique("chaos-key"),
                false,
                |_| {},
            );
            let _open = chaos.open(&zero);
            sleep_ms(n * 5).await;
            zero.close();
        }
        let deadline = Instant::now() + secs(5);
        while proxy.open_connections() > 0 && Instant::now() < deadline {
            sleep_ms(50).await;
        }
        assert_eq!(
            proxy.open_connections(),
            0,
            "left open: {}",
            proxy.open_connection_report().join("; ")
        );
    })
    .await;
}

/// A mutation too large for zero-cache (over its 10 MB message limit): the
/// server closes with 1009. The client must not loop resending it, and
/// must sync again: with no `on_client_state_not_found`, it replaces
/// itself on the same store.
#[tokio::test]
async fn oversized_mutation_does_not_wedge_the_client() {
    let Some(chaos) = Chaos::new().await else {
        return;
    };
    run(LIMIT, async {
        let logs = Rc::new(LogCollector::default());
        let zero = chaos.zero(None, &unique("chaos-key"), true, |o| {
            o.log_sink = Some(sink(&logs));
        });
        let open = chaos.open(&zero);
        wait_for("the open issues", secs(30), || complete(&open)).await;
        let first_id = zero.client_id();
        let big = create(
            &zero,
            &format!("{}big", chaos.prefix),
            &"x".repeat(11 << 20),
            None,
        );
        settled(&big.1, secs(15)).await;
        tokio::time::sleep(secs(2)).await;
        let id = format!("{}after-big", chaos.prefix);
        let after = create(&zero, &id, "After", None);
        let outcome = settled(&after.1, secs(20)).await;
        println!("[chaos] after the oversized mutation: {outcome:?}");
        wait_for("a mutation after the oversized one", secs(20), || {
            open_issue_ids(&chaos.prefix).contains(&id)
        })
        .await;
        // The client replaced itself, as JS reloads, and the view made
        // before follows the new one.
        assert_ne!(zero.client_id(), first_id);
        wait_for("the view to see the mutation after", secs(20), || {
            chaos.ids(&open).contains(&id)
        })
        .await;
        println!("[chaos] 1009 closes: {}", logs.count("code=1009"));
        assert!(logs.count("code=1009") <= 1);
        assert!(!open_issue_ids(&chaos.prefix).contains(&format!("{}big", chaos.prefix)));
        zero.close();
    })
    .await;
}
