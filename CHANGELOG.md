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
- **Fix Missing**: Missing Resources, made easier to work through. The
  package you check shows with its picture, coloured tags and what the check
  found. A summary says what's easy and what needs a look (for example, 32
  of 33 missing files are already inside the package: no download, no new
  dependency) with one button to choose every exact copy. Each missing file
  says what kind it is (Morph, Script, Texture…) and where its suggested
  replacement comes from, with Use; Enter uses it and moves on. The
  suggestion says why it's the one, preferring a copy inside the package,
  then the package that has the most of your missing files. Replacement
  Sources stays beside the list: what you chose sits at its top with Clear
  and Use it for other missing files, and the candidates are tabs (In your
  folders, In the database), a click anywhere on a card picks it, and a
  chosen card lists its other copies. Run Fixes is at the top of the panel
  (a sheet at the bottom of a narrow window). You fix as many or as few as
  you like: files you leave alone stay as they are. When nothing in your
  folders has a file, the page looks up its package's download by itself:
  download it and the file works as it is, and the page checks again when
  the download finishes, keeping your choices. The database lists the
  packages that have the most of your missing files first and offers to
  download ones you don't have. Run Fixes fixes the package itself, keeping
  a backup of the original in a folder you choose (never over an earlier
  backup), then checks the package again so you see what's left; without a
  backup it asks first. The empty page lists packages you checked lately and
  the ones with missing dependencies. A package can be marked a preferred
  replacement source (thumbs up) or one to avoid (thumbs down): preferred packages are chosen
  over others wherever they have the same file, in every check, and avoided
  ones are never chosen by themselves; you can still pick one, with a
  warning. Use preferred packages switches every missing file a preferred
  package has to it in one click. VAR Packages shows them on each package, filters by them
  (Preferred source, Avoided source) and sets them from the right-click
  menu, for one package or a selection. The list works from the keyboard and with
  screen readers. Package Explorer has a Check refs button and shows what
  the last check found, with Fix references. It replaces Missing
  Resources: the sidebar, Send to and the right-click menus open Fix
  Missing
- **Package Explorer**: a new page that shows one package in pictures. It
  says in plain words what's wrong with the package, if anything (missing or
  offloaded dependencies, not on the Hub, damaged…), and its main button
  fixes the worst of it. You get its cover and pictures, tiles with its size, content, morphs and
  dependencies, and a chart of what its bytes are made of. Its scenes, looks
  and clothing show as image cards you can hide or star in VaM. A map shows
  what it needs and what needs it. Its files show as a tree of folders you
  open and close, drag around and zoom (Ctrl + wheel), as an indented list
  like Explorer's, or as a map sized by bytes, and you can show only its
  clothing, hair, morphs, textures and so on. Long dependency lists switch
  to a list you can filter and search. It also compares its files with your
  database or your folders: which files other packages have too, how much
  their copies take, and a shortcut to clean against it. A clothing or hair
  item opens a sheet of its textures. Packages whose file is gone, or that
  only the database knows, open too, with a Download button. **Open
  details** everywhere in the app opens it; you can also drop a `.var` on
  the page

### Changed
- Package Explorer replaces the VAR Details page, which is gone: everything
  it did is in Package Explorer
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
- Fixing a missing resource with a copy inside the package itself wrote a
  reference to the package by name and made it depend on itself; it now
  writes `SELF:/`, as VaM expects
- Offloaded dependencies in the details sidebar can be moved back into
  AddonPackages, one at a time or all at once
- Saved sources marked In library open your copy's details, from a button or
  the right-click menu

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

