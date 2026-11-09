#!/usr/bin/env node
// Prepares Telly's own copy of Hachidori (as Gromit Reader does for its own).
// SPDX-License-Identifier: GPL-3.0-or-later
//
//   npm run prepare:hachidori
//       Downloads the Hachidori release pinned below and checks its SHA-256.
//   npm run prepare:hachidori -- --from <folder or .zip>
//       Uses a local build instead, such as ../hachidori/extension.
//
// What it does:
//   1. Unpacks Hachidori's Chrome build into src-tauri/extensions/hachidori/.
//      WebView2 loads every folder in extensions/ as an unpacked extension.
//   2. Adds Telly's fixed public key (src-tauri/hachidori-key.json, made on the
//      first run) to manifest.json. The key decides the extension's ID, and the
//      ID decides where Hachidori keeps its dictionaries, settings and link, so
//      they survive updates.
//   3. Turns on Hachidori's overlay mode, its contract for apps that embed it
//      (docs/overlay-mode.md in Hachidori): no first-run tab, no hosting, and
//      outside links handed to the app. Linked browsers see it as "Telly",
//      not the name overlay mode gives it (GSM's).
//   4. Copies in src-tauri/hachidori-addon/telly/, our engine host page.
//   5. Writes src-tauri/hachidori.json, which tells the app the ID and version.

import crypto from "node:crypto";
import fs from "node:fs/promises";
import path from "node:path";
import process from "node:process";
import { parseArgs } from "node:util";
import zlib from "node:zlib";

// The release to use. To update: change the version and asset, then put the
// chrome.zip line from the release's SHA256SUMS.txt in sha256.
const RELEASE = {
  version: "0.5.0",
  url: "https://github.com/bee-san/hachidori/releases/download/0.5.0/hachidori-0.5.0-bf1a78f8298b-chrome.zip",
  sha256: "8a4d683bfcbbec5370023a674275f8f4c66c4b4f35d2cdd2e3b51940296aa75c",
};

const root = path.resolve(import.meta.dirname, "..");
const tauriDir = path.join(root, "src-tauri");
const extensionsDir = path.join(tauriDir, "extensions");
const targetDir = path.join(extensionsDir, "hachidori");
const addonDir = path.join(tauriDir, "hachidori-addon", "telly");
const keyFile = path.join(tauriDir, "hachidori-key.json");
const infoFile = path.join(tauriDir, "hachidori.json");
const cacheDir = path.join(root, "node_modules", ".cache", "telly-hachidori");

const OVERLAY_OFF = "export const OVERLAY_MODE = false;";
const OVERLAY_ON = "export const OVERLAY_MODE = true;";
const SHARING_FILE = "background-sharing.js";
const GSM_NAME = '"GameSentenceMiner overlay"';
const TELLY_NAME = '"Telly"';

function fail(message) {
  console.error(`prepare-hachidori: ${message}`);
  process.exit(1);
}

function sha256(bytes) {
  return crypto.createHash("sha256").update(bytes).digest("hex");
}

// ---- zip ------------------------------------------------------------------

/** Reads every file in a zip archive. Supports stored and deflated entries, which is all Hachidori's release uses. */
export function readZip(buffer) {
  const EOCD = 0x06054b50;
  const CENTRAL = 0x02014b50;
  const LOCAL = 0x04034b50;
  let end = -1;
  for (let i = buffer.length - 22; i >= Math.max(0, buffer.length - 22 - 0xffff); i--) {
    if (buffer.readUInt32LE(i) === EOCD) {
      end = i;
      break;
    }
  }
  if (end < 0) throw new Error("not a zip archive");
  const count = buffer.readUInt16LE(end + 10);
  let offset = buffer.readUInt32LE(end + 16);
  if (count === 0xffff || offset === 0xffffffff) throw new Error("ZIP64 archives are not supported");

  const files = new Map();
  for (let n = 0; n < count; n++) {
    if (buffer.readUInt32LE(offset) !== CENTRAL) throw new Error("damaged zip central directory");
    const flags = buffer.readUInt16LE(offset + 8);
    const method = buffer.readUInt16LE(offset + 10);
    const crc = buffer.readUInt32LE(offset + 16);
    const compressedSize = buffer.readUInt32LE(offset + 20);
    const size = buffer.readUInt32LE(offset + 24);
    const nameLength = buffer.readUInt16LE(offset + 28);
    const extraLength = buffer.readUInt16LE(offset + 30);
    const commentLength = buffer.readUInt16LE(offset + 32);
    const localOffset = buffer.readUInt32LE(offset + 42);
    const name = buffer.toString("utf8", offset + 46, offset + 46 + nameLength);
    offset += 46 + nameLength + extraLength + commentLength;

    if (name.endsWith("/")) continue;
    if (flags & 1) throw new Error(`${name} is encrypted`);
    const segments = name.split("/");
    if (name.startsWith("/") || name.includes("\\") || segments.some((s) => s === ".." || s === "" || s.includes(":"))) {
      throw new Error(`unsafe path in zip: ${name}`);
    }
    if (buffer.readUInt32LE(localOffset) !== LOCAL) throw new Error(`damaged zip entry: ${name}`);
    const dataStart =
      localOffset + 30 + buffer.readUInt16LE(localOffset + 26) + buffer.readUInt16LE(localOffset + 28);
    const raw = buffer.subarray(dataStart, dataStart + compressedSize);
    let data;
    if (method === 0) data = Buffer.from(raw);
    else if (method === 8) data = zlib.inflateRawSync(raw);
    else throw new Error(`${name} uses unsupported compression method ${method}`);
    if (data.length !== size) throw new Error(`${name} unpacked to the wrong size`);
    if (typeof zlib.crc32 === "function" && zlib.crc32(data) !== crc) throw new Error(`${name} failed its CRC check`);
    files.set(name, data);
  }
  return files;
}

// ---- extension key and ID --------------------------------------------------

/** Chrome's extension ID for a manifest key: the SHA-256 of the key's DER bytes, first 32 hex digits, written with a–p. */
export function extensionId(keyBase64) {
  const hex = sha256(Buffer.from(keyBase64, "base64")).slice(0, 32);
  return [...hex].map((c) => String.fromCharCode(97 + parseInt(c, 16))).join("");
}

async function loadOrMakeKey() {
  try {
    const saved = JSON.parse(await fs.readFile(keyFile, "utf8"));
    if (typeof saved.key !== "string" || !saved.key) fail(`${keyFile} has no key`);
    return saved.key;
  } catch (error) {
    if (error?.code !== "ENOENT") throw error;
  }
  // Only the public half goes in the manifest, and nothing needs the private
  // half (it would only sign .crx packages), so it is not kept.
  const { publicKey } = crypto.generateKeyPairSync("rsa", { modulusLength: 2048 });
  const key = publicKey.export({ type: "spki", format: "der" }).toString("base64");
  const saved = {
    about:
      "The public key in Telly's copy of Hachidori's manifest.json. It fixes the extension's ID, which is where Hachidori keeps its data, so never change it.",
    id: extensionId(key),
    key,
  };
  await fs.writeFile(keyFile, JSON.stringify(saved, null, 2) + "\n");
  console.log(`Made a new extension key: ${keyFile} (commit it)`);
  return key;
}

// ---- getting Hachidori -----------------------------------------------------

async function download() {
  const name = path.basename(new URL(RELEASE.url).pathname);
  const cached = path.join(cacheDir, name);
  try {
    const bytes = await fs.readFile(cached);
    if (sha256(bytes) === RELEASE.sha256) return bytes;
  } catch {
    // Not downloaded yet.
  }
  console.log(`Downloading ${RELEASE.url}`);
  const response = await fetch(RELEASE.url);
  if (!response.ok) fail(`download failed: HTTP ${response.status}`);
  const bytes = Buffer.from(await response.arrayBuffer());
  const actual = sha256(bytes);
  if (actual !== RELEASE.sha256) fail(`${name} has SHA-256 ${actual}, expected ${RELEASE.sha256}. Not using it.`);
  await fs.mkdir(cacheDir, { recursive: true });
  await fs.writeFile(cached, bytes);
  return bytes;
}

async function readFolder(dir, prefix = "", files = new Map()) {
  for (const entry of await fs.readdir(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    const name = prefix + entry.name;
    if (entry.isDirectory()) await readFolder(full, name + "/", files);
    else if (entry.isFile()) files.set(name, await fs.readFile(full));
  }
  return files;
}

async function readSource(from) {
  if (!from) {
    return { files: readZip(await download()), source: { kind: "release", url: RELEASE.url, sha256: RELEASE.sha256 } };
  }
  const full = path.resolve(from);
  const stat = await fs.stat(full).catch(() => fail(`${full} does not exist`));
  if (stat.isDirectory()) return { files: await readFolder(full), source: { kind: "folder", path: full } };
  const bytes = await fs.readFile(full);
  return { files: readZip(bytes), source: { kind: "zip", path: full, sha256: sha256(bytes) } };
}

// ---- main ------------------------------------------------------------------

async function main() {
  const { values } = parseArgs({ options: { from: { type: "string" } } });
  const { files, source } = await readSource(values.from);

  const manifestBytes = files.get("manifest.json");
  if (!manifestBytes) fail("there is no manifest.json at the top of that Hachidori build");
  const manifest = JSON.parse(manifestBytes.toString("utf8"));
  if (manifest.name !== "Hachidori") fail(`expected Hachidori, found "${manifest.name}"`);
  if (manifest.background?.page) fail("that is the Firefox build; Telly needs the Chrome build");

  const key = await loadOrMakeKey();
  manifest.key = key;
  files.set("manifest.json", Buffer.from(JSON.stringify(manifest, null, 2) + "\n"));

  const overlay = files.get("overlay-mode.js")?.toString("utf8");
  if (overlay === undefined) fail("overlay-mode.js is missing; this Hachidori is too old for Telly");
  if (!overlay.includes(OVERLAY_ON)) {
    if (!overlay.includes(OVERLAY_OFF)) fail(`overlay-mode.js no longer contains "${OVERLAY_OFF}"`);
    files.set("overlay-mode.js", Buffer.from(overlay.replace(OVERLAY_OFF, OVERLAY_ON)));
  }
  // In overlay mode Hachidori introduces itself to the browser it links to by
  // GSM's name; here it's Telly.
  const sharing = files.get(SHARING_FILE)?.toString("utf8");
  if (sharing?.includes(GSM_NAME)) files.set(SHARING_FILE, Buffer.from(sharing.replaceAll(GSM_NAME, TELLY_NAME)));
  else console.warn(`${SHARING_FILE} no longer names the overlay "${JSON.parse(GSM_NAME)}"; linked browsers may see another name`);

  for (const [name, bytes] of await readFolder(addonDir)) files.set(`telly/${name}`, bytes);

  // Build the new copy in the cache, then swap it in, so the app never sees a half-written extension.
  const staging = path.join(cacheDir, `staging-${process.pid}`);
  await fs.rm(staging, { recursive: true, force: true });
  for (const [name, bytes] of files) {
    const out = path.join(staging, ...name.split("/"));
    await fs.mkdir(path.dirname(out), { recursive: true });
    await fs.writeFile(out, bytes);
  }
  await fs.mkdir(extensionsDir, { recursive: true });
  // Move the old copy aside first (outside extensions/, where WebView2 would
  // load it too). On Windows this fails while Telly has it open, and
  // then nothing has been touched yet.
  const old = path.join(tauriDir, `.hachidori-old-${process.pid}`);
  try {
    await fs.rename(targetDir, old);
  } catch (error) {
    if (error?.code !== "ENOENT") {
      fail(`couldn't replace ${path.relative(root, targetDir)} (${error?.code}). Close Telly and try again.`);
    }
  }
  try {
    await fs.rename(staging, targetDir);
  } catch (error) {
    if (error?.code !== "EXDEV") throw error;
    // The cache is on another drive: copy instead.
    await fs.cp(staging, targetDir, { recursive: true });
    await fs.rm(staging, { recursive: true, force: true });
  }
  await fs.rm(old, { recursive: true, force: true });

  const info = { id: extensionId(key), version: manifest.version, source };
  await fs.writeFile(infoFile, JSON.stringify(info, null, 2) + "\n");
  console.log(`Hachidori ${info.version} is ready in ${path.relative(root, targetDir)} (extension ID ${info.id}).`);
}

if (import.meta.main ?? process.argv[1] === import.meta.filename) {
  main().catch((error) => fail(error?.stack ?? String(error)));
}
