# zbugs in GPUI

[zbugs](../zbugs) as a native desktop app on the Zero Rust client
(`rust/zero-client`) and [GPUI](https://www.gpui.rs), through
[GPUI Kit](https://gpui-kit.com) (`gpui-kit`). macOS first.

- The model, `apps/zbugs-model`, is shared with the Rune app: the generated
  schema and the ports of zbugs' queries and mutators.
- `src/transport.rs`: the client's clock and timers on GPUI's foreground
  executor, so the client runs on the main thread with the UI, and its
  WebSockets on a tokio thread of the app's own (tokio-tungstenite, rustls).
  `zero-client` itself only gets its `sqlite` feature.
- `src/live.rs`: a typed query kept current in a GPUI view.
- `src/app.rs`: the issue list (filters, search, the synced count, paging
  as it scrolls) and the selected issue (markdown, labels, comments,
  commenting, closing and reopening).
- The local store is SQLite in `~/Library/Application Support/zbugs-gpui`,
  apart from zbugs-swift's.

## Running the app

Start zbugs' backend as for zbugs-swift (see `apps/zbugs-swift/README.md`),
which prints a token for a crew user and `ready <zero-cache> <API>`:

```sh
ZBUGS_PG=postgresql://user:password@127.0.0.1:6434/postgres \
  node apps/zbugs-swift/server/server.ts
```

Then run the app with the token:

```sh
cd apps/zbugs-gpui
ZBUGS_TOKEN=<token> cargo run
```

`ZBUGS_CACHE_URL` (default `http://127.0.0.1:4848`) and `ZBUGS_API_URL`
(default `http://127.0.0.1:5173`) point it elsewhere. Each is also read from
a file of that name (`token`, `cache-url`, `api-url`) in the app's directory.
