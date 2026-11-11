# zbugs on Rune

[zbugs](../zbugs) as a native desktop app in Rust, on the Zero Rust client
(`rust/zero-client`) and the [Rune](https://github.com/anomalyco/rune) UI
framework. macOS first; Linux and Windows are next.

- The schema, queries, and mutators are [`zbugs-model`](../zbugs-model)'s,
  shared with the GPUI app.
- `src/sync.rs` runs the client on a thread of its own, on tokio, with the
  `tokio-transport`, `tls`, and `sqlite` features of `zero-client`: the
  client is single-threaded and Rune owns the main thread without an async
  executor. The UI sends it commands, and it sends back each query's rows,
  read as the generated row types, through Rune's `AppHandle::update`.
- `src/view.rs` has the screens, with `rune-kit`'s controls: the filters,
  the issue list (virtual rows, paged as it scrolls), the issue with its
  Markdown and comments, and sheets to create and edit an issue, log in,
  and set the servers.
- `src/settings.rs` keeps the servers, the token, and the selected issue in
  `~/Library/Application Support/zbugs-rune/settings.json` (mode 0600); the
  replica is SQLite, beside it in `replica/`.

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

It prints a token for a crew user and `ready ws://127.0.0.1:4848
http://127.0.0.1:5173`, the servers the app uses by default. Then:

```sh
cd apps/zbugs-rune
cargo run --release
```

Log in from the sidebar with the printed token; Servers… points the app
elsewhere. `ZBUGS_TOKEN`, `ZBUGS_CACHE_URL`, and `ZBUGS_API_URL` in the
environment override the saved ones for that launch, and `ZBUGS_RUNE_DIR`
moves the settings and the replica.

Like every Rune app, it renders headless too:
`cargo run -- --snap zbugs.png --size 1180x760` (and `--tree`, `--script`;
see Rune's README).
