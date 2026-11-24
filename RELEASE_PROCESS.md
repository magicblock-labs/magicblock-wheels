# Package releases

Rust and npm have independent versions. Each release has its own PR and Git tag.
To release both, follow both rows. The tags can point to the same commit on `master`.

| Release branch | Git tag | Publishes |
| --- | --- | --- |
| `release/rust-v0.4.0` | `rust-v0.4.0` | `wheels-macros` and `wheels` at `0.4.0` |
| `release/npm-v0.2.1` | `npm-v0.2.1` | `@magicblock-labs/wheels@0.2.1` under `latest` |
| `release/rust-v0.4.0-alpha.1` | `rust-v0.4.0-alpha.1` | Both crates at `0.4.0-alpha.1` |
| `release/npm-v0.4.0-rc.1` | `npm-v0.4.0-rc.1` | `@magicblock-labs/wheels@0.4.0-rc.1` under `rc` |

Use the same version string in the manifest, branch, and tag. Both ecosystems
accept `-alpha.N`, `-beta.N`, and `-rc.N`. npm publishes each prerelease under
its channel, so a normal install continues to select the stable version.

CI reserves `release/` branches and all Git tags for these formats. A PR that
bumps an existing package version must use its matching release branch. Tag the
merged commit once. CI rejects a push that moves an existing release tag.

## Prepare and publish

1. Branch from `master`. For Rust, update `workspace.package.version` in the root
   manifest and the exact `wheels-macros` version in `rust/wheels/Cargo.toml`.
   Run `cargo check --workspace` and include the updated `Cargo.lock`. For npm,
   run `npm version <version> --no-git-tag-version` in `ts/` and include both JSON files.
2. Push the branch and open a PR. The corresponding workflow checks the version
   and dry-runs publication. Cargo builds both packaged crates. npm installs the
   tarball in a fresh consumer project, compiles its TypeScript, and runs a round trip.
3. Merge the PR. At the merged commit, create the tag by dropping `release/`
   from the branch name. For example:

   ```sh
   git tag -a npm-v0.4.0-rc.1 <merge-commit> -m 'npm 0.4.0-rc.1'
   git push origin refs/tags/npm-v0.4.0-rc.1
   ```

   Pushing the tag publishes the package. GitHub Release notes are optional.
4. If the Rust upload stops between crates, rerun the failed workflow job for the
   same tag. It looks up both exact versions on crates.io and publishes the missing crate.

## Before the first release

- Set the project-approved license in the Rust manifests and npm package before
  releasing each ecosystem. Include the corresponding license text in its packages.
- Create the `crates-io` and `npm` GitHub environments. Limit deployment to
  `rust-v*` and `npm-v*` tags respectively. Use a GitHub tag ruleset to restrict
  creation to release maintainers and block updates and deletions. Release
  branches run their checks without registry credentials.
- For the first Rust release, set the environment secret `CRATES_IO_BOOTSTRAP_TOKEN`
  to a crates.io token allowed to create both crates. After the release, configure
  both crates to trust this repository's `publish-rust.yml` workflow and `crates-io`
  environment, then remove and revoke the bootstrap token.
- For the first npm release, set `NPM_BOOTSTRAP_TOKEN` in the `npm` environment
  to a short-lived granular token authorized to create a public package in
  `@magicblock-labs` and publish from CI. Once the package exists, configure npm
  to trust `publish-npm.yml` and the `npm` environment, allow direct publishing,
  and remove and revoke the bootstrap token.
