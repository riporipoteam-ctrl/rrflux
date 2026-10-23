import { createHash } from "node:crypto";
import { inflateRawSync } from "node:zlib";

const [url, ...wanted] = process.argv.slice(2);
if (!url || wanted.length === 0) {
  console.error("usage: node scripts/verify-remote-zip.mjs <url> <path>...");
  process.exit(2);
}

async function range(start, end) {
  const response = await fetch(url, { headers: { Range: `bytes=${start}-${end}` } });
  if (response.status !== 206) throw new Error(`range ${start}-${end}: HTTP ${response.status}`);
  return Buffer.from(await response.arrayBuffer());
}

const head = await fetch(url, { method: "HEAD" });
if (!head.ok) throw new Error(`HEAD: HTTP ${head.status}`);
const size = Number(head.headers.get("content-length"));
if (!Number.isSafeInteger(size)) throw new Error("missing or unsafe content length");

const tailStart = Math.max(0, size - 1024 * 1024);
const tail = await range(tailStart, size - 1);
let eocd = -1;
for (let i = tail.length - 22; i >= 0; i--) {
  if (tail.readUInt32LE(i) === 0x06054b50) { eocd = i; break; }
}
if (eocd < 0) throw new Error("ZIP end record not found");

let cdSize = tail.readUInt32LE(eocd + 12);
let cdOffset = tail.readUInt32LE(eocd + 16);
if (cdSize === 0xffffffff || cdOffset === 0xffffffff) {
  const locator = eocd - 20;
  if (locator < 0 || tail.readUInt32LE(locator) !== 0x07064b50) {
    throw new Error("ZIP64 locator not found");
  }
  const zip64Offset = Number(tail.readBigUInt64LE(locator + 8));
  const zip64 = await range(zip64Offset, zip64Offset + 55);
  if (zip64.readUInt32LE(0) !== 0x06064b50) throw new Error("ZIP64 end record not found");
  cdSize = Number(zip64.readBigUInt64LE(40));
  cdOffset = Number(zip64.readBigUInt64LE(48));
}

const central = await range(cdOffset, cdOffset + cdSize - 1);
const entries = new Map();
for (let p = 0; p + 46 <= central.length && central.readUInt32LE(p) === 0x02014b50;) {
  const method = central.readUInt16LE(p + 10);
  let compressed = central.readUInt32LE(p + 20);
  let uncompressed = central.readUInt32LE(p + 24);
  const nameLength = central.readUInt16LE(p + 28);
  const extraLength = central.readUInt16LE(p + 30);
  const commentLength = central.readUInt16LE(p + 32);
  let localOffset = central.readUInt32LE(p + 42);
  const name = central.subarray(p + 46, p + 46 + nameLength).toString("utf8").replaceAll("\\", "/");
  const extra = central.subarray(p + 46 + nameLength, p + 46 + nameLength + extraLength);
  for (let q = 0; q + 4 <= extra.length;) {
    const id = extra.readUInt16LE(q);
    const length = extra.readUInt16LE(q + 2);
    if (id === 1) {
      let z = q + 4;
      if (uncompressed === 0xffffffff) { uncompressed = Number(extra.readBigUInt64LE(z)); z += 8; }
      if (compressed === 0xffffffff) { compressed = Number(extra.readBigUInt64LE(z)); z += 8; }
      if (localOffset === 0xffffffff) localOffset = Number(extra.readBigUInt64LE(z));
    }
    q += 4 + length;
  }
  entries.set(name.toLowerCase(), { name, method, compressed, uncompressed, localOffset });
  p += 46 + nameLength + extraLength + commentLength;
}

for (const requested of wanted) {
  const entry = entries.get(requested.replaceAll("\\", "/").toLowerCase());
  if (!entry) throw new Error(`${requested}: not found`);
  const local = await range(entry.localOffset, entry.localOffset + 29);
  if (local.readUInt32LE(0) !== 0x04034b50) throw new Error(`${requested}: local header missing`);
  const dataOffset = entry.localOffset + 30 + local.readUInt16LE(26) + local.readUInt16LE(28);
  const packed = await range(dataOffset, dataOffset + entry.compressed - 1);
  const bytes = entry.method === 0 ? packed : entry.method === 8 ? inflateRawSync(packed) : null;
  if (!bytes) throw new Error(`${requested}: unsupported compression method ${entry.method}`);
  if (bytes.length !== entry.uncompressed) throw new Error(`${requested}: length mismatch`);
  console.log(`${entry.name}\t${bytes.length}\t${createHash("sha256").update(bytes).digest("hex")}`);
}
