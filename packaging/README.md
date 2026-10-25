# Packaging

Where rustune is distributed and how each channel is kept up to date.

| Channel | Files | Status |
|---------|-------|--------|
| GitHub Releases | `.github/workflows/release.yml` | published on every `v*` tag |
| install script | `install.sh` | live (reads the latest release) |
| `.deb` / `.rpm` | `[package.metadata.deb]`, `[package.metadata.generate-rpm]` in `Cargo.toml` | release assets |
| cargo-binstall | `[package.metadata.binstall]` in `Cargo.toml` | release archives |
| Nix | `flake.nix`, `flake.lock` | live (`nix run github:dzulfikar08/rustune`) |
| Homebrew | [dzulfikar08/homebrew-tap](https://github.com/dzulfikar08/homebrew-tap) | live, updates itself every 6 hours |
| Scoop | [dzulfikar08/scoop-bucket](https://github.com/dzulfikar08/scoop-bucket) | live, updates itself every 6 hours |
| AUR | `aur/rustune-bin`, `aur/rustune` | needs `AUR_SSH_PRIVATE_KEY` |
| Snap Store | `../snap/snapcraft.yaml` | needs `SNAPCRAFT_STORE_CREDENTIALS` |
| crates.io | `Cargo.toml` | needs `CARGO_REGISTRY_TOKEN` |
| winget | `winget/manifests/...` | needs one manual submission, then `WINGET_GITHUB_TOKEN` |

## What a release does

Pushing a tag `vX.Y.Z` (it must match `version` in `Cargo.toml`) runs `release.yml`:

1. Builds macOS (arm64, x86_64; signed and notarized, plus `.dmg`), Linux (x86_64, arm64) and Windows (x86_64).
2. Builds `.deb` and `.rpm` for both Linux architectures and installs them in `ubuntu:24.04`, `debian:12` and `fedora:latest`.
3. Packs `rustune-<version>-<target>.tar.gz` / `.zip` and checks them against the cargo-binstall metadata.
4. Writes `SHA256SUMS`, runs `install.sh` against the files on Linux and macOS, and builds and tests the snap.
5. Creates the GitHub release with every file.
6. Publishes to the AUR, the Snap Store, crates.io and winget, each only if its secret exists (otherwise the job logs a notice and does nothing).

Homebrew and Scoop need nothing: their repos poll the latest release every 6 hours.

`gh workflow run release.yml --ref <branch>` is a dry run: steps 1 to 4 run and every file is kept as a workflow artifact, nothing is published.

Repository secrets are added under GitHub > rustune > Settings > Secrets and variables > Actions > New repository secret, or with `gh secret set NAME -R dzulfikar08/rustune`.

## AUR

`aur/rustune-bin` installs the release binary; `aur/rustune` builds the release tag from source. The release workflow sets `pkgver`, refreshes the checksums and `.SRCINFO`, and pushes both with [github-actions-deploy-aur](https://github.com/KSXGitHub/github-actions-deploy-aur).

One-time setup:

1. Create an account at <https://aur.archlinux.org/register>.
2. Make a key just for publishing: `ssh-keygen -t ed25519 -C "rustune AUR" -f ~/.ssh/aur_rustune -N ""`.
3. Paste `~/.ssh/aur_rustune.pub` into "My Account > SSH Public Key" on the AUR.
4. Add the private key as a secret: `gh secret set AUR_SSH_PRIVATE_KEY -R dzulfikar08/rustune < ~/.ssh/aur_rustune`.

The first push creates both AUR packages (an AUR package exists as soon as something is pushed to `ssh://aur@aur.archlinux.org/<name>.git`). To publish the current version without waiting for a release, push by hand from an Arch machine or container:

```sh
git clone ssh://aur@aur.archlinux.org/rustune-bin.git && cd rustune-bin
cp ../rustune/packaging/aur/rustune-bin/PKGBUILD .
updpkgsums && makepkg --printsrcinfo > .SRCINFO
git add PKGBUILD .SRCINFO && git commit -m "Initial import" && git push
```

## Snap Store

`snap/snapcraft.yaml` builds a strictly confined snap (core24) from source for amd64 and arm64. Audio goes through PulseAudio/PipeWire (`audio-playback`), files through `home` and `removable-media`, MPRIS through the `mpris` slot.

Limitation: a strict snap cannot run programs installed on the host, so yt-dlp (online search and streaming of page URLs) and ffmpeg are not available in the snap. Bundling them would add over 100 MB and an old yt-dlp breaks quickly, so they are left out; the description tells users to pick another channel if they need them. Classic confinement would lift this but needs a manual review by the Snap Store.

One-time setup (on any machine with snapcraft: `sudo snap install snapcraft --classic`):

1. Create an Ubuntu One account at <https://snapcraft.io/account> and accept the developer agreement.
2. `snapcraft login`, then `snapcraft register rustune`.
3. Export credentials for CI: `snapcraft export-login --snaps rustune --channels stable,candidate,beta,edge --acls package_access,package_push,package_update,package_release -` and save the output with `gh secret set SNAPCRAFT_STORE_CREDENTIALS -R dzulfikar08/rustune` (paste it when prompted).
4. Optional: request auto-connection of `mpris` with the name `rustune` in the [snapcraft forum](https://forum.snapcraft.io/c/store-requests/19) if the store flags it on the first upload.

Each release then uploads both snaps to the `stable` channel. The credentials expire after a year by default (`--expires` changes that); re-export and update the secret when they do.

## crates.io

The crate ships only `src`, `Cargo.toml`, `Cargo.lock`, `README.md` and `LICENSE` (see `include` in `Cargo.toml`); the Packaging workflow runs `cargo publish --dry-run` on every change.

One-time setup:

1. Log in at <https://crates.io> with GitHub, verify your email under Account Settings.
2. Create an API token at <https://crates.io/settings/tokens> with the scopes `publish-new` and `publish-update` (crate name `rustune`).
3. Either publish the current version by hand (`cargo login`, then `cargo publish` from a clean checkout of the tag), or add the token so releases publish automatically: `gh secret set CARGO_REGISTRY_TOKEN -R dzulfikar08/rustune`.

Once the crate is live, `cargo install rustune` and `cargo binstall rustune` work (binstall downloads the release archive).

## winget

`winget/manifests/d/dzulfikar08/rustune/<version>/` holds the three manifests (version, installer with the portable exe, en-US locale) in the [winget-pkgs](https://github.com/microsoft/winget-pkgs) layout; the Packaging workflow runs `winget validate` and installs from them.

One-time setup (on Windows):

1. `winget install wingetcreate`
2. Create a classic GitHub token with the `public_repo` scope (wingetcreate forks microsoft/winget-pkgs and opens the pull request from your account).
3. Submit the first version: `wingetcreate submit --token <token> packaging\winget\manifests\d\dzulfikar08\rustune\0.3.6` (or the newest folder; bump it with `wingetcreate update dzulfikar08.rustune --version <v> --urls <exe url> --out packaging\winget` first if a newer release exists).
4. Wait for the pull request to be merged (the Microsoft bots validate it; a moderator may ask questions).
5. Add the same token so later releases are submitted automatically: `gh secret set WINGET_GITHUB_TOKEN -R dzulfikar08/rustune`.

## Homebrew and Scoop

Both live in their own repos and only use their own `GITHUB_TOKEN`:

- Homebrew: `scripts/update-formula.sh` rewrites `Formula/rustune.rb` from the latest release (adding the Linux arm64 binary once a release has it); `update.yml` runs it every 6 hours and then runs the install test.
- Scoop: the Excavator workflow runs `checkver` every 6 hours and commits the new version and hash.

To force an update right after a release: `gh workflow run update.yml -R dzulfikar08/homebrew-tap` and `gh workflow run excavator.yml -R dzulfikar08/scoop-bucket`.
