# Changesets

Changesets version the native clients. Upstream Zero's packages are released
separately and are ignored here.

- The Swift client: the `zero-swift` npm package (`packages/zero-swift-cli`)
  and the Swift package (`swift/`), which share one version.
- The Rust client (`rust/`): `zero-rust` (`packages/zero-rust`), which isn't
  published to npm and only carries the crates' version and changelog.

To release a change to the Swift client, add a changeset for `zero-swift`; to
release one to the Rust client, add one for `zero-rust`:

```sh
pnpm dlx @changesets/cli@3.0.3
```

Changesets isn't a workspace dependency, so the root `package.json` and
`pnpm-lock.yaml` stay as upstream has them and syncing upstream doesn't
conflict there.

On `main`, `.github/workflows/swift-release.yml` keeps a "Version Packages" pull
request open with the pending version and changelog. Merging it sets the
version in `packages/zero-swift-cli/package.json` and `swift/VERSION`, and the
workflow then publishes the npm package and tags the SwiftPM mirror after
approval in the `swift-release` environment. The same PR versions the Rust
client: it sets the version in `packages/zero-rust/package.json`,
`rust/Cargo.toml`, and `rust/Cargo.lock`, and `.github/workflows/rust-release.yml`
then publishes the mirror (`wevm/zero-rust`) and `zero-client` on crates.io
after approval in the `rust-release` environment.

A change that breaks the Swift package's public API, so that an app building
against the last release no longer compiles, needs a `minor` changeset while
zero-swift is below 1.0, and a `major` one from 1.0. CI runs
`swift/scripts/api-breaks.sh`, which lists the breaks since the last release
and fails unless a changeset declares them.

The same goes for the Rust client: a break of `zero-client`'s public API since
the last release on crates.io needs a `minor` `zero-rust` changeset below 1.0
and a `major` one from 1.0, which `cargo semver-checks` in `rust.yml` checks.
