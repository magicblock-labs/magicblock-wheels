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

Start your release branch from `master`.

- **Rust** — `release/rust-v<version>`

  - Set `workspace.package.version` in the root `Cargo.toml`. In `rust/wheels/Cargo.toml`,
    pin the `wheels-macros` dependency to that exact version (`=<version>`).
  - Run `cargo check --workspace` and commit both manifests and the updated `Cargo.lock`.
  - The release dry run builds both packaged crates.
  - If an upload stops between crates, rerun the failed job for the same tag.
    It checks crates.io and publishes the missing crate.

- **npm** — `release/npm-v<version>`

  - In `ts/`, run `npm version <version> --no-git-tag-version`.
  - Commit the updated `package.json` and `package-lock.json`.
  - The release dry run installs the tarball in a fresh project, compiles a
    TypeScript consumer, and checks that it can encode and decode a record.

- **Publish either release**

  - Push the branch and open a PR. Merge it once CI verifies the version and the dry run passes.
  - Tag the merged commit. Use the branch name with the `release/` prefix removed.
    For example, to publish the npm release candidate, run

    ```sh
    git tag -a npm-v0.4.0-rc.1 <merge-commit> -m 'npm 0.4.0-rc.1'
    git push origin refs/tags/npm-v0.4.0-rc.1
    ```

  - Pushing the tag publishes the package. GitHub Release notes are optional.

## Before the first release

- Set the project-approved license in the Rust manifests and npm package before
  releasing each ecosystem. Include the corresponding license text in its packages.
- Create the `crates-io` and `npm` GitHub environments. Limit deployment to
  `rust-v*` and `npm-v*` tags respectively. Use a GitHub tag ruleset to restrict
  creation to release maintainers and block updates and deletions. Release
  branches run their checks without registry credentials.
- For the first Rust release, set the repository secret `CRATES_TOKEN`
  to a crates.io token allowed to create both crates. After the release, configure
  both crates to trust this repository's `publish-rust.yml` workflow and `crates-io`
  environment, then remove and revoke the bootstrap token.
- For the first npm release, set the repository secret `NPM_TOKEN`
  to a short-lived granular token authorized to create a public package in
  `@magicblock-labs` and publish from CI. Once the package exists, configure npm
  to trust `publish-npm.yml` and the `npm` environment, allow direct publishing,
  and remove and revoke the bootstrap token.
