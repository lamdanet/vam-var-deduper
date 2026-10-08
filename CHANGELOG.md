# Changelog

What's new in each release of VAM VAR Deduper. The app shows this under
Settings → About → Release notes, and each GitHub release uses its version's
section as its notes.

Write changes under **Unreleased** as you make them. `npm run release` turns
that section into the new version's notes. If Unreleased is still empty at
release time, it fills it with the commit messages since the last release.

## [Unreleased]

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

