// Checks a Tauri update signature the way the helper's updater does
// (tauri-plugin-updater 2.14.1 with requireSignedVersion): minisign, Ed25519
// over the file's BLAKE2b-512 ("ED"; the legacy "Ed" signs the raw file), the
// global signature over the trusted comment, and the version the file was
// signed for. Node built-ins only; reads nothing secret.
//   node tools/verify-update.mjs <public key: base64 as in tauri.conf.json, or a tauri.conf.json path> <file> <file.sig> [version]
import { createHash, createPublicKey, verify } from "node:crypto";
import { readFileSync } from "node:fs";

const b64 = (s) => Buffer.from(String(s).trim(), "base64");
const lines = (buf) => buf.toString("utf8").split("\n").map((l) => l.replace(/\r$/, ""));

export function publicKeyOf(arg) {
  if (/\.json$/i.test(arg)) return JSON.parse(readFileSync(arg, "utf8")).plugins.updater.pubkey;
  return arg;
}

export function verifyUpdate(pubkeyB64, data, sigB64, version) {
  const pk = b64(lines(b64(pubkeyB64))[1]);
  if (pk.length !== 42 || pk.subarray(0, 2).toString() !== "Ed") throw new Error("not a minisign public key");
  const [, sigLine, trustedLine, globalLine] = lines(b64(sigB64));
  const sig = b64(sigLine);
  if (sig.length !== 74) throw new Error("not a minisign signature");
  const alg = sig.subarray(0, 2).toString();
  if (!sig.subarray(2, 10).equals(pk.subarray(2, 10))) throw new Error("signed with a different key");
  if (!trustedLine?.startsWith("trusted comment: ")) throw new Error("no trusted comment");
  const trusted = trustedLine.slice("trusted comment: ".length);
  const key = createPublicKey({ key: { kty: "OKP", crv: "Ed25519", x: pk.subarray(10).toString("base64url") }, format: "jwk" });
  const signed = alg === "ED" ? createHash("blake2b512").update(data).digest() : data;
  if (!verify(null, signed, key, sig.subarray(10))) throw new Error("the signature doesn't match the file");
  if (!verify(null, Buffer.concat([sig.subarray(10), Buffer.from(trusted, "utf8")]), key, b64(globalLine))) throw new Error("the trusted comment was changed");
  const signedVersion = /(?:^|\t)version:([^\t]+)/.exec(trusted)?.[1] ?? null;
  if (version !== undefined && signedVersion !== version) throw new Error(`signed for version ${signedVersion ?? "(none)"}, not ${version}`);
  return { alg, trusted, version: signedVersion };
}

if (import.meta.url === `file://${process.argv[1]}` || process.argv[1]?.endsWith("verify-update.mjs")) {
  const [keyArg, file, sigFile, version] = process.argv.slice(2);
  if (!keyArg || !file || !sigFile) {
    console.error("usage: node tools/verify-update.mjs <pubkey|tauri.conf.json> <file> <file.sig> [version]");
    process.exit(2);
  }
  try {
    const r = verifyUpdate(publicKeyOf(keyArg), readFileSync(file), readFileSync(sigFile, "utf8"), version);
    console.log(`ok: ${file} (${r.alg}, ${r.trusted})`);
  } catch (e) {
    console.error(`REFUSED: ${file}: ${e.message}`);
    process.exit(1);
  }
}
