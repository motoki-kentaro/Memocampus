// Selects the native host name the extension uses (extension/host-config.js, Issue #67 Slice 3B-2).
//
//   node scripts/set-host-profile.mjs product|dev|legacyDev   rewrite the one HOST_PROFILE line
//   node scripts/set-host-profile.mjs --check product         exit 1 unless the file already says so
//   node scripts/set-host-profile.mjs product --file <path>   work on another copy (a packaging folder)
//
// It touches nothing but that one line of that one file.
import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const LINE = /^export const HOST_PROFILE = "([A-Za-z]+)";$/m;
const PROFILES = ["product", "dev", "legacyDev"];

export function profileOf(text) {
  const match = LINE.exec(text);
  if (!match) throw new Error("HOST_PROFILE line not found");
  return match[1];
}

export function withProfile(text, profile) {
  if (!PROFILES.includes(profile)) throw new Error(`profile must be one of ${PROFILES.join(", ")}`);
  profileOf(text);
  return text.replace(LINE, `export const HOST_PROFILE = "${profile}";`);
}

function main(argv) {
  const args = [...argv];
  const fileAt = args.indexOf("--file");
  const file = fileAt >= 0 ? args.splice(fileAt, 2)[1] : fileURLToPath(new URL("../extension/host-config.js", import.meta.url));
  const check = args[0] === "--check";
  const profile = check ? args[1] : args[0];
  if (!PROFILES.includes(profile) || !file) {
    console.error(`usage: set-host-profile.mjs [--check] <${PROFILES.join("|")}> [--file <host-config.js>]`);
    return 2;
  }
  const text = readFileSync(file, "utf8");
  if (check) {
    const now = profileOf(text);
    console.log(`HOST_PROFILE=${now}`);
    return now === profile ? 0 : 1;
  }
  writeFileSync(file, withProfile(text, profile));
  console.log(`HOST_PROFILE=${profile}`);
  return 0;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) process.exit(main(process.argv.slice(2)));
