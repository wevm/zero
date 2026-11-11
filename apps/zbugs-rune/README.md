# zbugs on Rune

[zbugs](../zbugs) as a native desktop app in Rust, on the Zero Rust client
(`rust/zero-client`) and the [Rune](https://github.com/anomalyco/rune) UI
framework. macOS first; it also builds and renders on Linux (CI checks
both), and Windows is next.

- The schema, queries, and mutators are [`zbugs-model`](../zbugs-model)'s,
  shared with the GPUI app.
- `src/sync.rs` runs the client on a thread of its own, on tokio, with the
  `tokio-transport`, `tls`, and `sqlite` features of `zero-client`: the
  client is single-threaded and Rune owns the main thread without an async
  executor. The UI sends it commands, and it sends back each query's rows,
  read as the generated row types, through Rune's `AppHandle::update`.
- `src/view.rs` has the screens, with `rune-kit`'s controls: the project
  and the filters (status, sort, creator, assignee, and labels, as the web
  app's list params),
  the issue list (virtual rows, paged as it scrolls, with a dot on issues
  changed since you looked, and Select to delete several; j and k step
  through it, as on the web), the issue with its Markdown, comments, and
  reactions, its assignee, visibility, notifications, and labels, and sheets
  to create, edit, and delete an issue, edit and delete comments, create
  a label, log in, and set the servers.
- `src/login.rs` logs in through GitHub in the browser: the API server
  redirects back to a listener the app opens on 127.0.0.1 for that one
  login (a loopback redirect, RFC 8252 §7.3).
- `src/settings.rs` keeps the servers and the selected issue in
  `~/Library/Application Support/zbugs-rune/settings.json`; the replica is
  SQLite, beside it in `replica/`.
- `src/token.rs` keeps the login token in the Keychain (service
  `dev.rocicorp.zbugs-rune`, account `jwt`), like zbugs-swift: the
  Credential Manager on Windows, the Secret Service on Linux. Where there's
  no store, it's a `token` file (mode 0600) beside the settings. An expired
  token is deleted at launch, and Log Out deletes it.

Rune is pre-release and pinned to a rev of its `dev` branch. It needs Rust
1.90 or later, so the app is a Cargo workspace of its own with Rune's
toolchain (`rust-toolchain.toml`), apart from `rust/`, which builds on 1.88.

## Running it

Start zbugs' backend, as for zbugs-swift (it needs Node 24, `psql`, and
Postgres with `wal_level = logical`):

```sh
ZBUGS_PG=postgresql://user:password@127.0.0.1:6434/postgres \
  node apps/zbugs-swift/server/server.ts
```

To keep its data across restarts (the database, the replica, its keys,
and so the token it prints), add `ZBUGS_KEEP=1` and a working directory.
The backend doesn't create the directory, so make it first:

```sh
mkdir -p ~/.zbugs-backend
ZBUGS_KEEP=1 ZBUGS_DIR=~/.zbugs-backend \
  ZBUGS_PG=postgresql://user:password@127.0.0.1:6434/postgres \
  node apps/zbugs-swift/server/server.ts
```

It prints a token for a crew user and `ready ws://127.0.0.1:4848
http://127.0.0.1:5173`, the servers the app uses by default. Then:

```sh
cd apps/zbugs-rune
cargo run --release
```

Log in from the sidebar with the printed token, or through GitHub when the
API server has a GitHub app (zbugs' own backend); Servers… points the app
elsewhere. `ZBUGS_TOKEN`, `ZBUGS_CACHE_URL`, and `ZBUGS_API_URL` in the
environment override the saved ones for that launch, and `ZBUGS_RUNE_DIR`
moves the settings, the replica, and the token (as a file, out of the
Keychain), so a test leaves the real login alone.

Like every Rune app, it renders headless too:
`cargo run -- --snap zbugs.png --size 1180x760` (and `--tree`, `--script`;
see Rune's README).
