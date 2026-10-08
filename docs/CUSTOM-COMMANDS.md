# Making your own project commands

This repo's one-word commands, like `npm run release` and `npm run prune`, are
small Node scripts in [scripts/](../scripts/) that are registered in
[package.json](../package.json). This guide shows how to add your own, and
when another approach fits better.

## How `npm run <name>` works

`package.json` has a `"scripts"` section. Each entry maps a name to a shell
command:

```json
"scripts": {
  "tauri:dev": "cargo run --manifest-path src-tauri/Cargo.toml",
  "prune": "node scripts/prune-target.mjs",
  "release": "node scripts/release.mjs"
}
```

- `npm run release` runs `node scripts/release.mjs` from the repo root, from
  any subfolder.
- `npm run release -- 0.2.0 --dry-run` passes everything after `--` to the
  script. Without the `--`, npm keeps options like `--dry-run` for itself.
- `npm run` with no name lists every command.
- **Automatic before/after steps:** a script named `pre<name>` or `post<name>`
  runs automatically before or after `<name>`. That is how `npm run tauri:build`
  cleans up old build output: `posttauri:build` runs the prune script after it.
  A post step only runs if the main command succeeded.

You only need Node.js. The scripts use Node's built-in modules, so there is
nothing to `npm install`.

## Add a command in 3 steps

### 1. Write the script

Create `scripts/<name>.mjs`. This template has the same building blocks as
`release.mjs`:

```js
// What this command does, in one or two lines.
//   npm run <name> -- <arg> [--dry-run]

import { execFileSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

// Always work from the repo root, wherever the command was started.
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

const fail = (msg) => {
  console.error(`\n<name>: ${msg}`);
  process.exit(1); // non-zero = failed, so npm and post-steps know to stop
};

// Run a program and show its output live. Arguments go in an array, so
// spaces and quotes in them need no escaping.
const run = (cmd, args) => execFileSync(cmd, args, { cwd: root, stdio: "inherit" });

// Run a program and return its output as text, e.g. to check git state.
const output = (cmd, args) => execFileSync(cmd, args, { cwd: root, encoding: "utf8" }).trim();

// 1. Read arguments
const args = process.argv.slice(2);
const dryRun = args.includes("--dry-run");
const [target] = args.filter((a) => !a.startsWith("-"));
if (!target) fail("usage: npm run <name> -- <arg> [--dry-run]");

// 2. Check everything before changing anything
if (output("git", ["rev-parse", "--abbrev-ref", "HEAD"]) !== "main") fail("run this on main");

// 3. Say what will happen; stop here on --dry-run
console.log(`Will do X with ${target}`);
if (dryRun) process.exit(0);

// 4. Do it. If a step fails, undo what you changed before exiting.
try {
  run("cargo", ["check", "--manifest-path", "src-tauri/Cargo.toml"]);
} catch {
  fail("cargo check failed");
}
```

### 2. Register it in package.json

```json
"scripts": {
  "release": "node scripts/release.mjs",
  "<name>": "node scripts/<name>.mjs"
}
```

The name can contain `:` to group related commands, like `tauri:dev`.

### 3. Try it

```sh
npm run <name> -- something --dry-run
```

Commit the script and the `package.json` change together.

## Habits that make a command safe

These are the rules `release.mjs` follows. They matter most for commands that
change files, commit, or push:

- **Check first, change second.** Find every reason to stop (wrong branch,
  uncommitted work, a tag that already exists) before touching anything, so a
  failed run leaves nothing half-done.
- **Offer `--dry-run`.** Show exactly what would happen. It's the easiest way to
  test the command itself.
- **Undo on failure.** If a later step fails, restore what earlier steps
  changed. For example, `release.mjs` puts `Cargo.toml` and `Cargo.lock` back
  when `cargo check` fails.
- **Make the last step all-or-nothing when you can.** `git push --atomic`
  pushes the branch and the tag together, or neither.
- **On failure, print the next command to run** (retry or undo), not just
  "failed".
- **Pass arguments as an array** (`execFileSync("git", ["commit", "-m", msg])`),
  never as one string through a shell. Then file names and messages with
  spaces or quotes just work, on Windows too.
- **Exit non-zero on failure** (`process.exit(1)`) so anything chained after
  the command stops too.

## Other ways to make commands

| Approach | Good for | Example |
| --- | --- | --- |
| **npm script + Node file** (this repo) | Multi-step tasks: git + cargo + file edits | `npm run release -- patch` |
| **Cargo alias** in `.cargo/config.toml` | A shortcut for a single cargo command | `[alias]` `t = "test --manifest-path src-tauri/Cargo.toml"` then `cargo t` |
| **`cargo xtask`** | Rust-only projects that want the tooling in Rust too | A small extra crate, run with `cargo xtask release 0.2.0` |
| **Ready-made cargo tools** | The standard tasks they cover, with no code of your own | `cargo install cargo-edit`, then `cargo set-version 0.2.0`. Or `cargo install cargo-release`, then `cargo release minor --execute` |
| **PowerShell function** in your `$PROFILE` | Personal shortcuts that don't belong in the repo | `function rel { npm run release -- $args }` |

Notes on these:

- **Cargo aliases** can only expand to other cargo commands. They can't run
  git or a script.
- **`cargo xtask`** is the Rust community's usual answer to "project
  commands". It needs a cargo workspace, and the tool code compiles like any
  other crate. Since this repo already uses Node for its scripts, the npm
  approach is simpler here.
- **`cargo-release`** does what `npm run release` does, and more: publishing to
  crates.io and workspaces. It is configured in `Cargo.toml` or
  `release.toml`. The in-repo script was chosen because this app has one
  crate, is never published to crates.io, and the script's checks (on `main`,
  not behind GitHub, build passes) are visible and easy to change.
