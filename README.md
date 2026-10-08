# VAM VAR Deduper

A Windows desktop app for cleaning up and managing a Virt-A-Mate (VaM) `.var`
package library.
It finds resources duplicated across packages, manages dependencies, offloads
packages you don't use, and finds and saves download links for missing ones.

## Download

Get the latest version from the
[**Releases**](https://github.com/lamdanet/vam-var-deduper/releases/latest)
page. Download the `.zip` or the single `.exe` and run `VAM-VAR-Deduper.exe`.
There is nothing to install.

- **Requirements:** Windows 10 or 11 (64-bit). The WebView2 runtime is needed
  too, but Windows 10 and 11 already include it.
- **"Windows protected your PC":** the exe isn't code-signed, so SmartScreen
  may warn you the first time. Click **More info**, then **Run anyway**. To
  check that your download wasn't altered, compare its SHA-256 hash with
  `SHA256SUMS.txt` on the release page:
  `Get-FileHash .\VAM-VAR-Deduper-*.exe`
- **What's new:** see [CHANGELOG.md](CHANGELOG.md), or open
  Settings → About → Release notes in the app.

## What it does

- **Clean VARs:** find resources duplicated across packages, choose which
  package keeps each one, and write a deduplicated set
- **VAR Packages:** your library at a glance, with details, dependencies,
  dependents, Offload / Restore and dependency removal
- **Hub:** browse, filter and install VaM Hub resources, and keep a wishlist
- **Sources:** paste a forum post and the app finds the `.var` files, mirrors and
  zip passwords in it, then saves the links (MEGA, MediaFire, Pixeldrain, zips)
- **Reclaim Space, Unique Resources, Resource List, Missing Resources,
  Internalize Resources:** find and fix what your packages contain and reference

Removing packages sends them to the Recycle Bin, and Clean VARs can back up the
files it changes. Still, back up your library before large cleanups.

## Feedback and help

- **Found a bug or have an idea?**
  [Open an issue](https://github.com/lamdanet/vam-var-deduper/issues/new/choose).
- **A question?** Ask in
  [Discussions](https://github.com/lamdanet/vam-var-deduper/discussions).
- **A security problem?** See [SECURITY.md](SECURITY.md). Please don't open a
  public issue for it.

## Building from source

You need [rustup](https://rustup.rs). It installs the Rust version pinned in
`rust-toolchain.toml` automatically. You also need the Visual Studio C++ build
tools with the Windows 11 SDK. Node.js is optional; it's used for the helper
scripts.

```sh
git clone https://github.com/lamdanet/vam-var-deduper.git
cd vam-var-deduper
cargo run --manifest-path src-tauri/Cargo.toml     # or: npm run tauri:dev
```

The UI is plain HTML, CSS and JS in [ui/](ui/), built into the exe. There is
no frontend build step.

- **Contributing:** [CONTRIBUTING.md](CONTRIBUTING.md)
- **Making a release:** [docs/RELEASING.md](docs/RELEASING.md)

## License

[MIT](LICENSE)
