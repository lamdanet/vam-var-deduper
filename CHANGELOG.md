# Changelog

What's new in each release of VAM VAR Deduper. The app shows this under
Settings → About → Release notes, and each GitHub release uses its version's
section as its notes.

Write changes under **Unreleased** as you make them. `npm run release` turns
that section into the new version's notes. If Unreleased is still empty at
release time, it fills it with the commit messages since the last release.

## [Unreleased]

### Added
- **Extract presets**: save the people in a package's scenes, legacy looks
  and appearance presets as VaM presets: their whole appearance, clothing,
  hair or morphs. They land in `Custom/Atom/Person/<kind>/extracted`, with the
  scene's picture, ready in VaM's preset browsers. Open it from a package's
  right-click menu, the Extract presets link in its details, the button on a
  scene row, or for several selected packages at once. The presets use the
  package's own files, so keep it installed
- **Updates from the Hub**: VAR Packages knows which of your packages have a
  newer version on the Hub. The Updates filter lists them with Update All;
  a package's details and right-click menu offer Update to vN and View on
  Hub. The new version downloads beside the old one, which stays until you
  clean old versions up
- **Not on Hub**: a filter for packages whose exact version can't be
  downloaded again, and a warning when you delete or clean them up
- **Check integrity**: reads every file inside a package and checks it
  against its checksum. Damaged packages get a Damaged chip and filter, and
  Redownload replaces them with a fresh copy from the Hub. Unchanged
  packages aren't read again, and every download is checked the same way
- **Disable / Enable** a package in place with VaM's own `.var.disabled`
  marker. Dependencies nothing else needs are disabled with it, and you're
  told which packages would lose a dependency. A Disabled filter lists them
- **Installed and Orphans**: the app remembers which packages you chose and
  which came in as dependencies, across versions. The Orphans filter lists
  dependencies nothing uses any more, with Remove all orphans, and a package's
  details say what deleting it really frees, unused dependencies included
- **Hide and favorite in VaM**: hide or star single items from a package's
  content list, using VaM's own flags, so VaM's browser follows. Settings can
  hide everything in dependency packages; turning that off brings back only
  what it hid. Flags carry over to new versions of a package
- **Drop to add**: drop `.var` files or folders on the window to add them to
  your library, checked first and skipped when already there. Settings can
  make it move them instead of copying
- The first-run dialog finds a VaM folder next to the app by itself

### Changed
- Downloads resume where they stopped instead of starting over, retry by
  themselves after network trouble, and can be paused and resumed. The queue
  survives a restart, waiting for Resume. What you asked for downloads before
  dependencies, and a finished download fetches the dependencies it still
  lacks. The panel shows total speed and what's left
- **Scan Dependencies** replaces Download Dependencies, Find Dependencies
  Locally and Remove dependencies. It checks AddonPackages, the offload folder
  and any folders you add, shows where each dependency is, and lets you
  download it, move it into AddonPackages, offload it or delete it, one row at
  a time or for every ticked row. It works on several selected packages at
  once too. Folders you had saved in Find Dependencies Locally carry over
- When a dependency is in more than one folder, the copy in AddonPackages is
  the one shown

### Fixed
- Offloaded dependencies in the details sidebar can be moved back into
  AddonPackages, one at a time or all at once
- Saved sources marked In library open your copy in VAR Details, from a button
  or the right-click menu

## [0.1.1] - 2026-10-08

A maintenance release: the libraries the app is built on are updated, and the
app works the same as before.

### Changed
- Updated the libraries behind Hub access and downloads, `.var` reading and
  writing, and MEGA decryption, plus Tauri and several smaller ones. The app
  still connects through Windows' own secure connection, so the Hub keeps
  working, and new files it writes into a `.var` are byte-for-byte the same as
  before
- When a `.var` is rewritten, files copied over unchanged are now marked as
  made on Windows instead of Unix. VaM ignores this mark

### Project
- VAM VAR Deduper is now open source under the MIT license. Report bugs and
  ideas through the [issue forms](https://github.com/lamdanet/vam-var-deduper/issues/new/choose),
  and ask questions in [Discussions](https://github.com/lamdanet/vam-var-deduper/discussions)
- Every change is now built, linted and tested automatically before it's merged

## [0.1.0] - 2026-10-08

First release of VAM VAR Deduper, a Windows desktop app for cleaning up and
managing a Virt-A-Mate `.var` package library. Download the zip or the single
`.exe` below and run it. Nothing to install.

### Clean up your library
- **Clean VARs** finds resources duplicated across packages, lets you choose
  which package keeps each one, and writes a deduplicated set
- **Reclaim Space**, **Unique Resources**, **Resource List**, **Missing
  Resources** and **Internalize Resources** find and fix what your packages
  contain and reference. Missing Resources can also scan extra folders
- Folders left empty by deletes, Clean Duplicates, Organize and moves are
  removed

### VAR Packages
- Your packages as a browsable library, with details, dependencies and the
  packages that use them
- **Offload / Restore** moves packages out of AddonPackages and back, showing
  first how many packages depend on the one you're moving
- **Remove dependencies** of a package, with the ones that are safe to remove
  marked
- **Clean Duplicates** keeps the older version when the newer one is outside
  AddonPackages
- Stays fast on large libraries

### Hub
- Browse, filter and install VaM Hub resources, and keep a wishlist
- The Hub site in the app, next to the package info, with **Download All** and a
  download picker that files packages into creator folders

### Download sources
- Save download links for a package: MEGA files and folders, MediaFire,
  Pixeldrain and password-protected zips
- The **Sources** page reads pasted forum posts (including masked links and
  mirrors), finds the `.var` files and zip passwords in them, and saves the
  links
- Saved links keep a track record of whether they worked and why not, fall
  back to other links when one fails, and are never saved twice
- Open a file you already have straight in VAR Details or VAR Packages

### Downloads
- **Cancel all**, and cancelling a download that is still starting really stops it
- MEGA downloads run in 6 parallel parts

### Look and feel
- Many themes, including Crimson Blaze, Electric Cyber and Emerald Pulse
- Settings → About shows the app version and these release notes

