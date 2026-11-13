# Rocicorp Monorepo

This is the mono repo for [Rocicorp](https://rocicorp.dev/)'s two main products, Zero and Replicache.

## About this repository

rocicorp/mono is published from Rocicorp's internal repository. Each commit
here comes from an internal commit, which the `Mono-Internal-Commit:` line at
the end of the commit message records; internal commits that only touch
private files aren't published. Releases of Zero are built and
published from this repository.

Test suites are included for [zero-client](./packages/zero-client),
[replicache](./packages/replicache), [zero-events](./packages/zero-events),
[zero-react](./packages/zero-react), [zero-solid](./packages/zero-solid),
[shared](./packages/shared) and [go](./go). The test harnesses (helpers, fixtures, vitest
configs and the Postgres test setup) are included for every package.

Tests for zero-cache, zql (including IVM), zqlite and some other packages are
not included in this repository. These tests were previously published under
the Apache 2.0 license, and those previously published copies remain open
source under that license. They were removed from the published repository on
September 30, 2026.

The history of this repository was rewritten on 2026-09-30 to remove the files
that are no longer published. If you cloned or forked it before then, clone it
again.

## Contributing

Pull requests are welcome. We don't merge them here: a maintainer ports your
change into the internal repository, and it comes back here in a commit that
lists you as a co-author and closes your pull request. See [CONTRIBUTING.md](./CONTRIBUTING.md).

## Zero

Web site and docs: https://zero.rocicorp.dev/.

### Code

- [packages/zero-client](./packages/zero-client): The main client library. It uses replicache under the hood.
- [packages/zero-cache](./packages/zero-cache): The server side code.
- [packages/zql](./packages/zql): The IVM (incremental view maintenance) engine as well as the query language/API.
- [@rocicorp/zero-docs](https://github.com/rocicorp/zero-docs): The docs for zero is currently in a separate repo.
- [apps/zbugs](./apps/zbugs): The bug tracker app.

## Replicache

Web site and docs https://replicache.dev/.

### Code

- [packages/replicache](./packages/replicache): The replicache client library.
- [packages/replicache-doc](./packages/replicache-doc): The Replicache documentation is in this monorepo.

## Older Projects

### Reflect

Reflect is no longer under development. The code lives in the [rocicorp/reflect-archive](https://github.com/rocicorp/reflect-archive) repo.

<!-- comment -->

## This fork

[wevm/zero](https://github.com/wevm/zero) is a fork of rocicorp/mono.
Everything above is upstream's README, unchanged, and two parts of it don't
apply here: this fork does include tests for zql (including IVM), zqlite and
other packages, and it merges pull requests itself.

### What it adds

- [swift](./swift): Zero for Swift, a native client for iOS and macOS,
  released through [wevm/zero-swift](https://github.com/wevm/zero-swift).
- [rust](./rust): Zero for Rust, a native Rust client (the `zero-client`
  crate).
- [packages/zero-swift-cli](./packages/zero-swift-cli) and
  [packages/zero-rust-cli](./packages/zero-rust-cli): the `zero-swift` and
  `zero-rust` CLIs, which generate Swift and Rust code from a Zero app's
  TypeScript schema.
- [packages/zero-conformance](./packages/zero-conformance): conformance
  vectors recorded from the JS client, which the Swift and Rust clients
  replay.
- zbugs as native apps: [apps/zbugs-swift](./apps/zbugs-swift) on the Swift
  client, and [apps/zbugs-rune](./apps/zbugs-rune) and
  [apps/zbugs-gpui](./apps/zbugs-gpui) on the Rust client.
- Tests restored from upstream, at their original paths.
  [tools/restored-tests](./tools/restored-tests) lists them and how they
  run.

### How it differs from upstream

- **Tests:** rocicorp/mono stopped publishing the tests of zql, zqlite,
  zero-cache and some other packages on 2026-09-30. This fork restores the
  client-side ones from upstream's last published copy, under Apache 2.0:
  zql (including IVM), zqlite, zero-protocol, zero-schema, zero-types and
  zql-integration-tests. The tests of zero-cache and the other server-side
  packages aren't restored. See
  [tools/restored-tests](./tools/restored-tests/README.md).
- **Pull requests** are reviewed and merged here, not ported to an internal
  repository.
- **Upstream sync:** rocicorp/mono's `main` is merged in weekly, in a
  `Sync rocicorp/mono main` pull request that lands with a merge commit, so
  upstream's history stays intact.
