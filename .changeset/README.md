# Changesets

Changesets version `zerogen` (`packages/zerogen`), the code generator, and the
Swift client: the `zero-swift` npm package (`packages/zero-swift-cli`) and the
Swift package (`swift/`), which share one version. `zerogen` has its own.
Upstream Zero's packages are released separately and are ignored here.

To release a change, add a changeset for what it changes: `zerogen` for the
generator, `zero-swift` for the Swift package or the `zero-swift` CLI:

```sh
pnpm dlx @changesets/cli@3.0.3
```

Changesets isn't a workspace dependency, so the root `package.json` and
`pnpm-lock.yaml` stay as upstream has them and syncing upstream doesn't
conflict there.

`zero-swift` depends on `zerogen` with `workspace:*`, which `pnpm pack` turns
into the exact `zerogen` version. Changesets reads it the same way, so every
`zerogen` release also bumps `zero-swift` by a patch, which updates
`swift/VERSION` and releases the Swift package too.

On `main`, `.github/workflows/swift-release.yml` keeps a "Version Packages" pull
request open with the pending versions and changelogs. Merging it sets the
versions in `packages/zerogen/package.json`, `packages/zero-swift-cli/package.json`
and `swift/VERSION`. After approval in the `swift-release` environment, the
workflow then tags the SwiftPM mirror and publishes each npm package whose
version isn't on npm yet, `zerogen` before `zero-swift`. Before publishing, it
installs the packed packages outside the workspace (`pnpm --filter zerogen
smoke`) and checks that the packed `zero-swift` depends on exactly the
`zerogen` version being released.

npm trusted publishing has to be set up for `zerogen` (this repository,
`swift-release.yml`, the `swift-release` environment, allowed to
`npm publish`) before its first release. npm only adds a trusted publisher to a
package that exists, so its first version is published by hand; the workflow
then finds that version on npm and skips it. Until then, publishing `zerogen`
fails, and `zero-swift` isn't published either.

## Swift API compatibility

A change that breaks the Swift package's public API, so that an app building
against the last release no longer compiles, needs a `minor` changeset while
zero-swift is below 1.0, and a `major` one from 1.0. CI runs
`swift/scripts/api-breaks.sh`, which lists the breaks since the last release
and fails unless a changeset declares them.
