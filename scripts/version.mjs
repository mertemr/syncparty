#!/usr/bin/env node

// The application version is declared in more than one file because three
// different toolchains need to read it, and a release fails outright when they
// disagree. Editing them by hand is how v0.5.0 through v0.5.3 were all tagged
// against a mismatched set. This script is the only supported way to move the
// number, and the same check it performs runs on every pull request.
//
//   node scripts/version.mjs check [--tag v0.5.0]
//   node scripts/version.mjs set 0.5.0
//
// `src-tauri/tauri.conf.json` is deliberately absent from the list below: it
// points at `../package.json` instead of repeating the number, and `check`
// asserts that it still does.

import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

const PACKAGE_JSON = "package.json";
const TAURI_CONF = "src-tauri/tauri.conf.json";
const CARGO_TOML = "src-tauri/Cargo.toml";
const CARGO_LOCK = "src-tauri/Cargo.lock";

// Where tauri.conf.json has to point for the number to reach the bundle.
const TAURI_VERSION_SOURCE = "../package.json";

const SEMVER = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/;

const read = (file) => readFileSync(join(root, file), "utf8");
const write = (file, text) => writeFileSync(join(root, file), text);

// Each declaration is a pattern with the version as its second capture group,
// so one helper can both read and rewrite it.
const DECLARATIONS = [
  {
    file: PACKAGE_JSON,
    // The only top level "version" key; dependency entries are plain strings.
    pattern: /^(\s*"version":\s*")([^"]+)(")/m,
  },
  {
    file: CARGO_TOML,
    // Anchored to [package] so a dependency's `version = "..."` cannot match.
    pattern: /^(\[package\][\s\S]*?\nversion = ")([^"]+)(")/m,
  },
  {
    file: CARGO_LOCK,
    // Cargo rewrites this entry on the next build, but a stale lock file would
    // otherwise reach the release build as an uncommitted change.
    pattern: /^(\[\[package\]\]\r?\nname = "syncparty"\r?\nversion = ")([^"]+)(")/m,
  },
];

function declaredVersions() {
  return DECLARATIONS.map(({ file, pattern }) => {
    const match = read(file).match(pattern);
    if (!match) {
      fail(`Could not find a version declaration in ${file}.`);
    }
    return { file, version: match[2] };
  });
}

function fail(message) {
  console.error(`error: ${message}`);
  process.exit(1);
}

function check(tag) {
  const declared = declaredVersions();
  const [{ version }] = declared;
  const disagreeing = declared.filter((entry) => entry.version !== version);

  if (disagreeing.length > 0) {
    const listed = declared.map((e) => `  ${e.file}: ${e.version}`).join("\n");
    fail(`Version declarations disagree:\n${listed}`);
  }

  const tauriSource = JSON.parse(read(TAURI_CONF)).version;
  if (tauriSource !== TAURI_VERSION_SOURCE) {
    fail(
      `${TAURI_CONF} declares version ${JSON.stringify(tauriSource)}; it must ` +
        `stay ${JSON.stringify(TAURI_VERSION_SOURCE)} so the bundle takes its ` +
        `version from ${PACKAGE_JSON}.`,
    );
  }

  if (tag !== undefined && tag !== `v${version}`) {
    fail(`Tag ${tag} does not match the application version v${version}.`);
  }

  console.log(`Version ${version} is consistent across every declaration.`);
}

function set(version) {
  if (!SEMVER.test(version)) {
    fail(`${version} is not a semver version number, e.g. 0.5.0.`);
  }

  for (const { file, pattern } of DECLARATIONS) {
    const text = read(file);
    const updated = text.replace(pattern, `$1${version}$3`);
    if (updated === text && !pattern.test(text)) {
      fail(`Could not find a version declaration in ${file}.`);
    }
    write(file, updated);
  }

  console.log(`Set version ${version} in:`);
  for (const { file } of DECLARATIONS) console.log(`  ${file}`);
  console.log(
    `\nCommit these together, merge the pull request, then tag v${version}.`,
  );
}

const [command, ...rest] = process.argv.slice(2);

switch (command) {
  case "check": {
    const tagFlag = rest.indexOf("--tag");
    check(tagFlag === -1 ? undefined : rest[tagFlag + 1]);
    break;
  }
  case "set":
    if (rest.length !== 1) fail("Usage: node scripts/version.mjs set <version>");
    set(rest[0]);
    break;
  default:
    fail(
      "Usage:\n" +
        "  node scripts/version.mjs check [--tag v0.5.0]\n" +
        "  node scripts/version.mjs set 0.5.0",
    );
}
