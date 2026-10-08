# Releasing VAM VAR Deduper

Releases are built by GitHub Actions ([.github/workflows/release.yml](../.github/workflows/release.yml))
on a Windows runner and published on the repo's **Releases** page with:

- `VAM-VAR-Deduper-vX.Y.Z-windows-x64.zip`: the app in a zip
- `VAM-VAR-Deduper-vX.Y.Z-windows-x64.exe`: the same app as a single exe
- `SHA256SUMS.txt`: checksums for both

The app is one portable exe with the UI built in, so there is no installer.
It needs the WebView2 runtime, which Windows 10 and 11 already include.

Always release from `main`.

## Quick release: one command

From the repo root, on `main`, with your changes committed (except
`CHANGELOG.md`, which may be uncommitted because it goes into the release
commit):

```sh
npm run release -- 0.2.0     # release an exact version
npm run release -- patch     # 0.1.0 -> 0.1.1
npm run release -- minor     # 0.1.0 -> 0.2.0
npm run release -- major     # 0.1.0 -> 1.0.0
npm run release -- 0.2.0-beta.1   # anything with "-" becomes a pre-release
```

It does steps 1 and 2B below for you
([scripts/release.mjs](../scripts/release.mjs)):

1. Checks that you're on `main`, have nothing uncommitted, aren't behind
   GitHub, and that the new version is higher and not already tagged.
2. Sets the version in `src-tauri/Cargo.toml`.
3. Turns the **Unreleased** section of `CHANGELOG.md` into
   `## [X.Y.Z] - <today>` and adds a new empty Unreleased above it. If you
   didn't write anything under Unreleased, it fills in the commit messages
   since the last release. See [Release notes](#release-notes).
4. Runs `cargo check`. This updates `Cargo.lock` and makes sure the app still
   builds. If the check fails, the version and changelog changes are undone.
5. Commits `Release vX.Y.Z`, tags `vX.Y.Z`, and pushes `main` and the tag
   together. Either both reach GitHub or neither does.
6. The tag push starts the release workflow on GitHub.

`--dry-run` also prints the release notes it would use, so you can check
them first.

Options (put them after the version):

| Option | What it does |
| --- | --- |
| `--dry-run` | Run every check and show the plan, but change nothing |
| `--no-push` | Bump, commit and tag only on your PC. Push later with the command it prints |
| `--remote <name or URL>` | Push somewhere other than `origin` |

Your SSH key has a passphrase, so `git` asks for it when the command
contacts GitHub (once to check, once to push).

The `--` after `release` is how npm passes arguments on to the script.
Without it, npm keeps options like `--dry-run` for itself. To make commands
like this one, see [CUSTOM-COMMANDS.md](CUSTOM-COMMANDS.md).

The sections below are the same steps done by hand, plus what to do when
something goes wrong.

## Release notes

[CHANGELOG.md](../CHANGELOG.md) is the single source of release notes. Each
version's section is used in two places:

- **The GitHub release page.** The workflow uses the version's section as the
  release description and adds GitHub's comparison link below it.
- **The app.** Settings → About → **Release notes** shows the whole changelog,
  newest first, with the running version marked. The changelog is built into
  the exe, so each build shows the notes it shipped with. The version badge
  there also comes from `Cargo.toml`, so it updates on its own.

The file looks like this:

```markdown
## [Unreleased]

### Added
- Release notes in Settings → About

### Fixed
- VAR Packages no longer gets stuck on "Scanning…"

## [0.2.0] - 2026-10-08

- …
```

**Writing notes.** Add a line under `## [Unreleased]` when you make a change
users will notice. Write it for users ("what changed for me"), not as a commit
message. Grouping under `### Added`, `### Changed` and `### Fixed` is optional.
The app shows bullets, `**bold**`, `` `code` `` and those group headings.
Link targets are not shown, only the link text.

**Forgot to write any?** The release command fills Unreleased with the commit
messages since the last release tag. This repo's commit subjects are already
written for users, so that works as a fallback. Check them with `--dry-run`
first. Before your **first** release there is no earlier tag, so that would
list every commit ever made. For that one, write a short summary under
Unreleased instead.

**Fixing notes after a release:** edit the release on GitHub (the pencil icon
on the Releases page), and fix the same section in `CHANGELOG.md` so the app
shows the corrected text from the next build on.

## 1. Bump the version

The version lives in one place: `version = "X.Y.Z"` under `[package]` in
`src-tauri/Cargo.toml`. Tauri reads it from there, so `tauri.conf.json` and
`package.json` deliberately have no version field.

Edit that line. In `CHANGELOG.md`, rename `## [Unreleased]` to
`## [X.Y.Z] - YYYY-MM-DD` and add a new empty `## [Unreleased]` above it. Then
build once so `Cargo.lock` picks up the new version, and commit and push:

```sh
cargo check --manifest-path src-tauri/Cargo.toml
git commit -am "Release vX.Y.Z"
git push origin main
```

The release workflow builds with `--locked`. If you skip the build,
`Cargo.lock` still has the old version and the release fails at the build
step. Build, commit and push again to fix it.

## 2. Start the release (pick one)

### A. Run the workflow from GitHub (no tag needed)

1. Open the repo on GitHub, then **Actions**, then **Release**, then **Run workflow**.
2. Leave the branch on **main**.
3. Leave **Version** empty. The workflow uses the version from `Cargo.toml`.
   If you do type one, it must be the same as `Cargo.toml`, or the run stops.
   Tick **pre-release** for a test build.
4. Click **Run workflow**. The workflow creates the `vX.Y.Z` tag on the
   latest `main` commit and publishes the release.

Or start it from a terminal with the GitHub CLI:

```sh
gh workflow run release.yml --ref main                     # normal release
gh workflow run release.yml --ref main -f prerelease=true  # test build
gh run watch                                               # follow it
```

### B. Push a tag

```sh
git tag vX.Y.Z
git push origin vX.Y.Z
```

Pushing the tag starts the workflow and releases that commit. The tag has to
match the version in `Cargo.toml`. For example, tag `v0.2.0` needs
`version = "0.2.0"`.

A full build takes roughly 10 to 20 minutes, because the release profile uses LTO.
The release appears under **Releases**, with that version's section of
`CHANGELOG.md` as its notes and GitHub's comparison link below. If the
changelog has no section for the version, the workflow warns and uses only
GitHub's generated notes. You can edit the notes on GitHub afterwards.

## If a release fails or is wrong

- **The workflow failed:** open the run under **Actions** to see the error. Fix
  the problem on `main`, then start the release again. If the run got as far as
  creating the tag, delete the tag first:
  `git push origin :refs/tags/vX.Y.Z`
- **The release went out but is bad:** delete it on the Releases page, delete its
  tag as above, fix `main`, and release again. Or bump to the next version, which
  is cleaner if anyone has already downloaded the bad one.

## Releasing without GitHub Actions (fully manual)

Use this only if Actions can't be used. You need:

- Rust (stable), installed with `rustup`
- Visual Studio C++ build tools with the Windows 11 SDK
- Node.js, which runs the build-output cleanup after `npm run tauri:build`
- The GitHub CLI, signed in with `gh auth login`

Do step 1 (bump, commit, push) first. Then run this in PowerShell from the
repo root. It reads the version from `Cargo.toml`, so there is nothing to type:

```powershell
git switch main; git pull
npm run tauri:build      # = cargo build --release, then prunes old build output
$v = (Select-String -Path src-tauri/Cargo.toml -Pattern '^version = "(.*)"').Matches[0].Groups[1].Value
$name = "VAM-VAR-Deduper-v$v-windows-x64"
New-Item -ItemType Directory -Force "dist/$name" | Out-Null
Copy-Item src-tauri/target/release/vam_var_deduper_tauri.exe "dist/$name/VAM-VAR-Deduper.exe"
Compress-Archive -Force -Path "dist/$name/*" -DestinationPath "dist/$name.zip"
Copy-Item "dist/$name/VAM-VAR-Deduper.exe" "dist/$name.exe"
Get-FileHash "dist/$name.zip", "dist/$name.exe" -Algorithm SHA256 |
  ForEach-Object { "$($_.Hash.ToLower())  $(Split-Path $_.Path -Leaf)" } |
  Set-Content -Encoding ascii dist/SHA256SUMS.txt
# This version's section of CHANGELOG.md, as the release notes
(Get-Content CHANGELOG.md -Raw) -split '(?m)^## \[' |
  Where-Object { $_.StartsWith("$v]") } |
  ForEach-Object { ($_ -split "`n", 2)[1].Trim() } |
  Set-Content -Encoding utf8 dist/notes.md
gh release create "v$v" "dist/$name.zip" "dist/$name.exe" dist/SHA256SUMS.txt --target main --title "VAM VAR Deduper v$v" --notes-file dist/notes.md --generate-notes
```

Add `--prerelease` to the last line for a test build.

Close the app before building. If it is running from `target/release`, the
build can't replace the exe.

The `dist/` folder is ignored by git, so you can delete it afterwards.
