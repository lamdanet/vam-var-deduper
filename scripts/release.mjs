// One-step release: bump the version, check it builds, commit, tag, push.
// Pushing the vX.Y.Z tag starts .github/workflows/release.yml, which builds the
// app and publishes the GitHub Release. See docs/RELEASING.md.
//
//   npm run release -- <version|patch|minor|major> [--dry-run] [--no-push] [--remote <name|url>]
//
//   npm run release -- 0.2.0          set an exact version
//   npm run release -- patch          0.1.0 -> 0.1.1   (minor: 0.2.0, major: 1.0.0)
//   npm run release -- 0.2.0-beta.1   a version with "-" is published as a pre-release
//   --dry-run   check everything and print the plan, change nothing
//   --no-push   bump, commit and tag locally only; push later yourself
//   --remote    where to push (default: origin)
//
// The version lives only in src-tauri/Cargo.toml; Tauri reads it from there.
// Release notes come from CHANGELOG.md's Unreleased section (or, if that's
// empty, the commit messages since the last release). They become the
// version's section there, which the app shows and the GitHub release uses.

import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const cargoTomlPath = path.join(root, "src-tauri", "Cargo.toml");
const cargoLockPath = path.join(root, "src-tauri", "Cargo.lock");
const changelogPath = path.join(root, "CHANGELOG.md");

const fail = (msg) => {
  console.error(`\nrelease: ${msg}`);
  process.exit(1);
};
const run = (cmd, args, opts = {}) => execFileSync(cmd, args, { cwd: root, stdio: "inherit", ...opts });
const git = (...args) => execFileSync("git", args, { cwd: root, encoding: "utf8" }).trim();
const gitOk = (...args) => {
  try {
    execFileSync("git", args, { cwd: root, stdio: "ignore" });
    return true;
  } catch {
    return false;
  }
};

// --- arguments -------------------------------------------------------------

const USAGE = "usage: npm run release -- <version|patch|minor|major> [--dry-run] [--no-push] [--remote <name|url>]";
const args = process.argv.slice(2);
let dryRun = false;
let noPush = false;
let remote = "origin";
const positional = [];
for (let i = 0; i < args.length; i++) {
  if (args[i] === "--dry-run") dryRun = true;
  else if (args[i] === "--no-push") noPush = true;
  else if (args[i] === "--remote") remote = args[++i];
  else if (args[i].startsWith("-")) fail(`unknown option ${args[i]}\n${USAGE}`);
  else positional.push(args[i]);
}
const wanted = positional[0];
if (!wanted || positional.length > 1 || !remote) fail(USAGE);

// --- versions --------------------------------------------------------------

const SEMVER = /^(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?$/;

const cargoToml = fs.readFileSync(cargoTomlPath, "utf8");
// The first top-level `version = "..."` line is [package]'s; dependency
// versions are inline tables or `name = "x"`, never at the start of a line.
const versionLine = /^version\s*=\s*"([^"]+)"/m;
const current = cargoToml.match(versionLine)?.[1];
if (!current || !SEMVER.test(current)) fail(`can't read the version from ${cargoTomlPath}`);

const parse = (v) => {
  const m = v.match(SEMVER);
  return { major: +m[1], minor: +m[2], patch: +m[3], pre: m[4] ?? "" };
};
const cur = parse(current);

let next;
if (wanted === "patch") next = cur.pre ? `${cur.major}.${cur.minor}.${cur.patch}` : `${cur.major}.${cur.minor}.${cur.patch + 1}`;
else if (wanted === "minor") next = `${cur.major}.${cur.minor + 1}.0`;
else if (wanted === "major") next = `${cur.major + 1}.0.0`;
else next = wanted.replace(/^v/, "");
if (!SEMVER.test(next)) fail(`"${wanted}" isn't a version (X.Y.Z or X.Y.Z-pre) or patch/minor/major`);

const newer = (a, b) => {
  const x = parse(a);
  const y = parse(b);
  for (const k of ["major", "minor", "patch"]) if (x[k] !== y[k]) return x[k] > y[k];
  if (x.pre === y.pre) return false;
  if (!x.pre) return true; // 1.0.0 is newer than 1.0.0-beta
  if (!y.pre) return false;
  return x.pre.localeCompare(y.pre, "en", { numeric: true }) > 0;
};
if (!newer(next, current)) fail(`${next} isn't newer than the current version ${current}`);

const tag = `v${next}`;

// --- repo checks -----------------------------------------------------------

const branch = git("rev-parse", "--abbrev-ref", "HEAD");
if (branch !== "main") fail(`release from main (you're on ${branch}): git switch main`);

// Uncommitted notes in CHANGELOG.md are fine: they go into the release commit.
const dirty = git("status", "--porcelain", "--untracked-files=no")
  .split("\n")
  .filter((l) => l && !/^.. CHANGELOG\.md$/.test(l))
  .join("\n");
if (dirty) fail(`commit or stash your changes first:\n${dirty}`);

if (gitOk("rev-parse", "-q", "--verify", `refs/tags/${tag}`)) fail(`tag ${tag} already exists locally`);

if (!noPush) {
  console.log(`Checking ${remote} ...`);
  try {
    execFileSync("git", ["fetch", "--quiet", remote, "main"], { cwd: root, stdio: ["inherit", "ignore", "inherit"] });
  } catch {
    fail(`couldn't fetch from ${remote}. Check your connection / sign-in, or use --no-push.`);
  }
  if (!gitOk("merge-base", "--is-ancestor", "FETCH_HEAD", "HEAD")) {
    fail(`main on ${remote} has commits you don't have: git pull, then try again`);
  }
  const remoteTag = execFileSync("git", ["ls-remote", "--tags", remote, `refs/tags/${tag}`], { cwd: root, encoding: "utf8" });
  if (remoteTag.trim()) fail(`tag ${tag} already exists on ${remote}`);
}

// --- release notes ---------------------------------------------------------

const changelog = fs.existsSync(changelogPath) ? fs.readFileSync(changelogPath, "utf8") : "";
const eol = changelog.includes("\r\n") ? "\r\n" : "\n";
const unreleased = changelog.match(/^## \[Unreleased\][^\n]*\n([\s\S]*?)(?=^## \[|(?![\s\S]))/m);
if (!unreleased) fail('CHANGELOG.md needs a "## [Unreleased]" heading (see docs/RELEASING.md)');

let notes = unreleased[1].trim();
let notesFrom = "CHANGELOG.md, Unreleased section";
if (!notes) {
  let lastTag = "";
  try {
    lastTag = execFileSync("git", ["describe", "--tags", "--abbrev=0", "--match", "v*"], {
      cwd: root,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    }).trim();
  } catch {}
  notes = git("log", "--no-merges", "--format=%s", lastTag ? `${lastTag}..HEAD` : "HEAD")
    .split("\n")
    .filter((s) => s && !/^Release v/.test(s))
    .map((s) => `- ${s}`)
    .join("\n");
  notesFrom = `commit messages since ${lastTag || "the first commit"} (Unreleased was empty)`;
  if (!notes) fail(`nothing to release: no commits since ${lastTag}`);
}
notes = notes.split(/\r?\n/).join(eol);

const date = new Date().toLocaleDateString("sv"); // YYYY-MM-DD, local date
const newChangelog =
  changelog.slice(0, unreleased.index) +
  `## [Unreleased]${eol}${eol}## [${next}] - ${date}${eol}${eol}${notes}${eol}${eol}` +
  changelog.slice(unreleased.index + unreleased[0].length).replace(/^(\r?\n)+/, "");

console.log(`\nRelease ${current} -> ${next}${parse(next).pre ? " (pre-release)" : ""}`);
console.log(`  1. set version = "${next}" in src-tauri/Cargo.toml`);
console.log(`  2. move the release notes into "## [${next}] - ${date}" in CHANGELOG.md`);
console.log(`  3. cargo check (updates Cargo.lock, makes sure it builds)`);
console.log(`  4. commit "Release ${tag}" and tag ${tag}`);
console.log(noPush ? `  5. (skipped: --no-push)` : `  5. push main and ${tag} to ${remote} -> GitHub builds and publishes the release`);
console.log(`\nRelease notes (from ${notesFrom}):\n`);
console.log(notes.split(/\r?\n/).map((l) => `  ${l}`).join("\n"));

if (dryRun) {
  console.log("\nDry run: nothing changed.");
  process.exit(0);
}

// --- do it -----------------------------------------------------------------

const originalLock = fs.readFileSync(cargoLockPath);
fs.writeFileSync(cargoTomlPath, cargoToml.replace(versionLine, `version = "${next}"`));
fs.writeFileSync(changelogPath, newChangelog);
try {
  run("cargo", ["check", "--manifest-path", cargoTomlPath]);
} catch {
  fs.writeFileSync(cargoTomlPath, cargoToml);
  fs.writeFileSync(cargoLockPath, originalLock);
  fs.writeFileSync(changelogPath, changelog);
  fail("cargo check failed; the version and CHANGELOG changes were undone. Fix the build and try again.");
}

run("git", ["add", "src-tauri/Cargo.toml", "src-tauri/Cargo.lock", "CHANGELOG.md"]);
run("git", ["commit", "--quiet", "-m", `Release ${tag}`]);
run("git", ["tag", "-a", tag, "-m", `Release ${tag}`]);
console.log(`\nCommitted and tagged ${tag}.`);

if (noPush) {
  console.log(`Push when ready:  git push --atomic ${remote} main ${tag}`);
  process.exit(0);
}

try {
  // --atomic: main and the tag go up together or not at all.
  run("git", ["push", "--atomic", remote, "main", tag]);
} catch {
  fail(
    `push failed; the release commit and tag are only on this PC.\n` +
      `  Retry:  git push --atomic ${remote} main ${tag}\n` +
      `  Or undo: git tag -d ${tag} && git reset --hard HEAD~1`,
  );
}

let actionsUrl = "";
try {
  const url = remote.includes(":") ? remote : git("remote", "get-url", remote);
  const m = url.match(/github\.com[:/](.+?)(?:\.git)?$/);
  if (m) actionsUrl = `https://github.com/${m[1]}/actions/workflows/release.yml`;
} catch {}
console.log(`\nPushed ${tag}. GitHub is building the release now${actionsUrl ? `:\n  ${actionsUrl}` : "."}`);
