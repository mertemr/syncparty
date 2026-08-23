#!/usr/bin/env node
/**
 * Vendors the pinned Syncplay server into `src-tauri/syncplay-source/`.
 *
 * Only the Linux packages need this. No distribution ships a server syncparty
 * can use: `--ipv4-only` and `--interface-ipv4` arrived in Syncplay 1.7.1, and
 * the newest package in any Ubuntu LTS is 1.7.0. Without those flags the
 * server binds every interface instead of loopback, which is the one thing
 * `core::syncplay::server` refuses to do. So the package carries its own copy
 * and takes only Twisted from the distribution.
 *
 * The version and digest are read out of `server_runtime.rs` rather than
 * repeated here. One pin, one place to bump — the same rule `version.mjs`
 * applies to the application version.
 *
 *   node scripts/fetch-syncplay.mjs          # fetch into src-tauri/
 *   node scripts/fetch-syncplay.mjs --check  # print the pin and exit
 */
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { mkdtempSync, mkdirSync, rmSync, cpSync, writeFileSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const pinFile = join(root, "src-tauri/src/core/deps/server_runtime.rs");
const destination = join(root, "src-tauri/syncplay-source");

/** Everything the server imports. The rest of the archive is CI and docs. */
const KEEP = ["syncplay", "syncplayServer.py"];

function readPin() {
  const source = readFileSync(pinFile, "utf8");
  const find = (name) => {
    const match = source.match(new RegExp(`const ${name}: &str = "([^"]+)"`));
    if (!match) {
      throw new Error(`could not find ${name} in ${pinFile}`);
    }
    return match[1];
  };

  return { version: find("SYNCPLAY_VERSION"), sha256: find("SYNCPLAY_SHA256") };
}

async function main() {
  const { version, sha256 } = readPin();
  const url = `https://github.com/Syncplay/syncplay/archive/refs/tags/v${version}.tar.gz`;

  if (process.argv.includes("--check")) {
    console.log(`syncplay ${version}\n${sha256}\n${url}`);
    return;
  }

  console.log(`Fetching Syncplay ${version}`);
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`${url} returned ${response.status}`);
  }

  const archive = Buffer.from(await response.arrayBuffer());
  const actual = createHash("sha256").update(archive).digest("hex");

  // Same check the app makes at runtime. Failing here means the pin and the
  // archive have diverged, and shipping the mismatch inside a package would
  // hide that until someone tried to host.
  if (actual !== sha256) {
    throw new Error(
      `checksum mismatch for Syncplay ${version}\n  expected ${sha256}\n  actual   ${actual}`,
    );
  }

  const staging = mkdtempSync(join(tmpdir(), "syncparty-syncplay-"));

  try {
    const tarball = join(staging, "syncplay.tar.gz");
    writeFileSync(tarball, archive);
    execFileSync("tar", ["xzf", tarball, "-C", staging, "--strip-components=1"], {
      stdio: "inherit",
    });

    rmSync(destination, { recursive: true, force: true });
    mkdirSync(destination, { recursive: true });

    for (const entry of KEEP) {
      cpSync(join(staging, entry), join(destination, entry), { recursive: true });
    }
  } finally {
    rmSync(staging, { recursive: true, force: true });
  }

  console.log(`Vendored Syncplay ${version} into ${destination}`);
}

await main();
