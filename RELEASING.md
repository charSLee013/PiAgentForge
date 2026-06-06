# Releasing PiAgentForge

This repository publishes prebuilt GitHub Release assets for the `pi` CLI.

## Release contract

- Trigger: push a stable semantic-version tag such as `v0.1.1`
- Version rule: the tag must exactly match `Cargo.toml` `[workspace.package].version`
- Official release build:
  - package: `pi`
  - command: `cargo build --locked --release -p pi --features feat-extensions`
  - feature surface: default `feat-all` plus `feat-extensions`
- Official platforms:
  - `x86_64-unknown-linux-gnu`
  - `aarch64-apple-darwin`
  - `x86_64-pc-windows-msvc`

Each release publishes four assets:

- `pi-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz`
- `pi-vX.Y.Z-aarch64-apple-darwin.tar.gz`
- `pi-vX.Y.Z-x86_64-pc-windows-msvc.zip`
- `SHA256SUMS`

Each platform archive contains:

- `pi` or `pi.exe`
- `README.md`
- `LICENSE`

## Release steps

1. Update the workspace version in `Cargo.toml`.
2. Run the local release gate:
   - `cargo fmt --all --check`
   - `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`
   - `cargo test --locked --workspace`
3. Commit and push the version change to `master`.
4. Create and push the release tag:

   ```bash
   git tag vX.Y.Z
   git push origin vX.Y.Z
   ```

5. Wait for the `Release` workflow to finish and publish the GitHub Release.
6. Verify that all three platform archives and `SHA256SUMS` are attached.

## Failure modes

- If the tag is not a stable `vX.Y.Z`, the workflow fails before building.
- If the tag and workspace version differ, the workflow fails before building.
- If any platform build fails, no GitHub Release is published.
- If you need to retry a failed release after a tag push, delete the failed tag and release first, then recreate the tag.
