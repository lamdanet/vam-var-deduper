// Removes what earlier builds of the app crate left behind in Cargo's target dir.
//
// Cargo names each build's output after a hash that changes from build to build
// here, and never deletes the previous one, so every rebuild leaves ~0.5-1.5 GB
// behind (test exe + pdb in deps/, plus an incremental cache). For each kind of
// build (app or tests, full build or check, per profile) this keeps only the
// newest and deletes the rest. Dependencies and build-script outputs are left alone.
//
//   node scripts/prune-target.mjs [--dry-run] [target-dir ...]
//
// With no target dir it prunes src-tauri/target and $CARGO_TARGET_DIR (if set).
//
// --from-hook: run as a Claude Code PostToolUse hook. Reads the tool call from
// stdin and only prunes after a cargo build/test/run (in that command's
// CARGO_TARGET_DIR too, if it set one).

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const cargoToml = fs.readFileSync(path.join(root, "src-tauri", "Cargo.toml"), "utf8");
const crate = cargoToml.match(/^\[package\][\s\S]*?^name\s*=\s*"([^"]+)"/m)[1].replace(/-/g, "_");

const args = process.argv.slice(2);
const dryRun = args.includes("--dry-run");
let targets = args.filter((a) => !a.startsWith("--"));
if (targets.length === 0) {
  targets = [path.join(root, "src-tauri", "target")];
  if (process.env.CARGO_TARGET_DIR) targets.push(path.resolve(process.env.CARGO_TARGET_DIR));
}

if (args.includes("--from-hook")) {
  let command = "";
  try {
    command = JSON.parse(fs.readFileSync(0, "utf8")).tool_input?.command ?? "";
  } catch {}
  if (!/\bcargo(?:\.exe)?\s+(?:\+\S+\s+)?(?:build|test|run|b|t|r)\b/.test(command)) process.exit(0);
  const custom = command.match(/CARGO_TARGET_DIR\s*=\s*["']?([^"'\s;&|]+)/);
  if (custom) targets.push(path.resolve(root, "src-tauri", custom[1]));
}

// Kinds whose outputs pile up. Build-script fingerprints are shared between
// kinds and small, so they are not touched.
const PRUNED_KINDS = new Set([`bin-${crate}`, `test-bin-${crate}`]);
const hashed = new RegExp(`^(?:lib)?${crate}-([0-9a-f]{16})(?:\\.|$)`);

let freed = 0;
let failed = 0;

const sizeOf = (p) => {
  const st = fs.statSync(p);
  if (!st.isDirectory()) return st.size;
  return fs.readdirSync(p).reduce((sum, name) => sum + sizeOf(path.join(p, name)), 0);
};

const newestMtime = (dir) =>
  Math.max(fs.statSync(dir).mtimeMs, ...fs.readdirSync(dir).map((n) => fs.statSync(path.join(dir, n)).mtimeMs));

const remove = (p) => {
  let size = 0;
  try {
    size = sizeOf(p);
    if (!dryRun) fs.rmSync(p, { recursive: true, force: true });
    freed += size;
  } catch (err) {
    // A file the running app still has open (Windows) stays; the next prune gets it.
    failed++;
    console.warn(`  could not delete ${p}: ${err.code || err.message}`);
  }
};

for (const target of targets) {
  if (!fs.existsSync(target)) continue;
  for (const profile of fs.readdirSync(target)) {
    const profileDir = path.join(target, profile);
    const fpDir = path.join(profileDir, ".fingerprint");
    if (!fs.existsSync(fpDir)) continue;

    // Group this crate's fingerprints by kind; the newest of each kind is live.
    const groups = new Map();
    for (const name of fs.readdirSync(fpDir)) {
      const m = name.match(new RegExp(`^${crate}-([0-9a-f]{16})$`));
      if (!m) continue;
      const dir = path.join(fpDir, name);
      const json = fs.readdirSync(dir).find((n) => n.endsWith(".json"));
      if (!json) continue;
      const unit = json.slice(0, -".json".length);
      if (!PRUNED_KINDS.has(unit)) continue;
      let profileId = "";
      try {
        profileId = JSON.parse(fs.readFileSync(path.join(dir, json), "utf8")).profile;
      } catch {}
      const key = `${unit}:${profileId}`;
      if (!groups.has(key)) groups.set(key, []);
      groups.get(key).push({ hash: m[1], dir, mtime: newestMtime(dir) });
    }

    const stale = new Set();
    for (const list of groups.values()) {
      list.sort((a, b) => b.mtime - a.mtime);
      for (const old of list.slice(1)) {
        stale.add(old.hash);
        remove(old.dir);
      }
    }

    const depsDir = path.join(profileDir, "deps");
    if (fs.existsSync(depsDir)) {
      for (const name of fs.readdirSync(depsDir)) {
        const m = name.match(hashed);
        if (m && stale.has(m[1])) remove(path.join(depsDir, name));
      }
    }

    // Incremental dirs aren't named by the same hash; keep as many of the
    // newest as there are live kinds. Dropping one only costs a slower rebuild.
    const incDir = path.join(profileDir, "incremental");
    if (fs.existsSync(incDir)) {
      const inc = fs
        .readdirSync(incDir)
        .filter((n) => n.startsWith(`${crate}-`))
        .map((n) => ({ dir: path.join(incDir, n), mtime: newestMtime(path.join(incDir, n)) }))
        .sort((a, b) => b.mtime - a.mtime);
      for (const old of inc.slice(Math.max(groups.size, 1))) remove(old.dir);
    }
  }
}

const gb = (freed / 1024 ** 3).toFixed(2);
console.log(`${dryRun ? "Would free" : "Freed"} ${gb} GB of old ${crate} build output` + (failed ? ` (${failed} in use, skipped)` : ""));
