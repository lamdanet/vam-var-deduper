# Releasing VAM VAR Deduper

Releases are built by GitHub Actions ([.github/workflows/release.yml](../.github/workflows/release.yml))
on a Windows runner and published on the repo's **Releases** page with:

- `VAM-VAR-Deduper-vX.Y.Z-windows-x64.zip`: the app in a zip
- `VAM-VAR-Deduper-vX.Y.Z-windows-x64.exe`: the same app as a single exe
- `SHA256SUMS.txt`: checksums for both

The app is one portable exe with the UI built in, so there is no installer.
It needs the WebView2 runtime, which Windows 10 and 11 already include.

Always release from `main`.

## 1. Bump the version

The version lives in one place: `version = "X.Y.Z"` under `[package]` in
`src-tauri/Cargo.toml`. Tauri reads it from there, so `tauri.conf.json` and
`package.json` deliberately have no version field.

Edit that line, build once so `Cargo.lock` picks up the new version, then
commit and push:

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
3. Leave **Version** empty to use the version from `Cargo.toml`, or type it (`X.Y.Z`).
   Tick **pre-release** for a test build.
4. Click **Run workflow**. The workflow creates the `vX.Y.Z` tag on the
   latest `main` commit and publishes the release.

Or start it from a terminal with the GitHub CLI:

```sh
gh workflow run release.yml --ref main            # version from Cargo.toml
gh workflow run release.yml --ref main -f version=X.Y.Z -f prerelease=true
gh run watch                                     # follow it
```

### B. Push a tag

```sh
git tag vX.Y.Z
git push origin vX.Y.Z
```

Pushing the tag starts the workflow and releases that commit.

A full build takes roughly 10 to 20 minutes, because the release profile uses LTO.
The release appears under **Releases** with notes generated from the commits
since the previous release. You can edit the notes on GitHub afterwards.

## If a release fails or is wrong

- **The workflow failed:** open the run under **Actions** to see the error. Fix
  the problem on `main`, then start the release again. If the run got as far as
  creating the tag, delete the tag first:
  `git push origin :refs/tags/vX.Y.Z`
- **The release went out but is bad:** delete it on the Releases page, delete its
  tag as above, fix `main`, and release again. Or bump to the next version, which
  is cleaner if anyone has already downloaded the bad one.

## Releasing without GitHub Actions (fully manual)

Use this only if Actions can't be used. It needs the build tools from the main
setup (Rust, plus VS C++ tools with the Windows SDK) and the GitHub CLI
signed in (`gh auth login`).

```powershell
git switch main; git pull
npm run tauri:build      # = cargo build --release, then prunes old build output
$v = "X.Y.Z"
$name = "VAM-VAR-Deduper-v$v-windows-x64"
New-Item -ItemType Directory -Force "dist/$name" | Out-Null
Copy-Item src-tauri/target/release/vam_var_deduper_tauri.exe "dist/$name/VAM-VAR-Deduper.exe"
Compress-Archive -Force -Path "dist/$name/*" -DestinationPath "dist/$name.zip"
Copy-Item "dist/$name/VAM-VAR-Deduper.exe" "dist/$name.exe"
gh release create "v$v" "dist/$name.zip" "dist/$name.exe" --target main --title "VAM VAR Deduper v$v" --generate-notes
```

Close the app before building. If it is running from `target/release`, the
build can't replace the exe.

The `dist/` folder is ignored by git, so you can delete it afterwards.
