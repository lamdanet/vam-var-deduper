# Contributing

Thanks for helping! Bug reports, ideas and pull requests are all welcome.

## Reporting bugs and asking for features

Use the [issue forms](https://github.com/lamdanet/vam-var-deduper/issues/new/choose).
They ask for what's needed to act on a report: the app version (shown under
Settings → About), the page you were on, steps to reproduce, and the Console
log. Questions go to
[Discussions](https://github.com/lamdanet/vam-var-deduper/discussions).

Check the [existing issues](https://github.com/lamdanet/vam-var-deduper/issues)
first. If yours is already there, add a 👍 or more detail to it.

## Making a change

1. **For anything bigger than a small fix, open an issue first,** so we can
   agree on the approach before you spend time on it.
2. **Fork the repo** and create a branch from `main`, e.g. `fix/hub-download`
   or `feat/offload-filter`.
3. **Make the change, then build and test it** (see below).
4. **Add a line to [CHANGELOG.md](CHANGELOG.md)** under `## [Unreleased]`,
   written for users ("VAR Packages no longer gets stuck on Scanning…"). Skip
   this for changes users won't notice.
5. **Open a pull request** against `main` and fill in the template. CI builds
   and tests it on Windows. A PR needs CI passing before it can be merged.
6. **PRs are squash-merged:** your commits become one commit on `main`, titled
   after the PR. So make the PR title a clear one-line summary of the change.

## Setting up

You need:

- Windows 10 or 11
- [Rust](https://rustup.rs) (stable)
- Visual Studio C++ build tools, with the **Windows 11 SDK** component
- Node.js (optional), for the helper scripts in `scripts/`

```sh
cargo run --manifest-path src-tauri/Cargo.toml    # run the app (debug build)
cargo test --manifest-path src-tauri/Cargo.toml   # run the tests
node --check ui/app.js                            # quick syntax check of the UI
```

## Where things are

| Path | What's there |
| --- | --- |
| `src-tauri/src/` | The Rust backend: scanning, database, downloads, Tauri commands. `tests.rs` holds the tests |
| `ui/` | The whole frontend, plain HTML, CSS and JS with no build step. It's built into the exe |
| `scripts/` | Helper commands (`npm run release`, `npm run prune`), see [docs/CUSTOM-COMMANDS.md](docs/CUSTOM-COMMANDS.md) |
| `docs/` | Developer docs, e.g. [RELEASING.md](docs/RELEASING.md) |

## Tips

- **Close the app before `cargo build`.** While it runs from `target/debug`,
  Windows locks the exe and the build can't replace it. `cargo check` and
  `cargo test` still work.
- **Some tests are skipped** because they need fixture folders that aren't in
  the repo. Run them with `cargo test -- --ignored` if you have those folders.
- **Large libraries matter.** Real libraries reach 80,000+ packages, so avoid
  loading whole tables on interactive paths.
- **Match the surrounding code:** naming, comment style, and how existing pages
  are built.

## License

By contributing, you agree that your contributions are licensed under the
[MIT License](LICENSE), the same as the project.
