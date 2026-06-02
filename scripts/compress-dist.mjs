#!/usr/bin/env node
import { brotliCompressSync, constants, gzipSync } from "node:zlib";
import { existsSync, readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { extname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { dirname } from "node:path";

const __dirname = dirname(fileURLToPath(import.meta.url));
const DIST_DIR = resolve(__dirname, "../dist");
const COMPRESS_EXTS = new Set([
  ".css",
  ".html",
  ".js",
  ".json",
  ".map",
  ".svg",
  ".txt",
  ".xml",
]);
const MIN_SIZE_BYTES = 1024;

function walk(dir) {
  for (const entry of readdirSync(dir)) {
    const path = join(dir, entry);
    const stat = statSync(path);
    if (stat.isDirectory()) {
      walk(path);
      continue;
    }
    if (!stat.isFile()) continue;
    if (path.endsWith(".gz") || path.endsWith(".br")) continue;
    if (!COMPRESS_EXTS.has(extname(path))) continue;
    if (stat.size < MIN_SIZE_BYTES) continue;

    const input = readFileSync(path);
    const gzip = gzipSync(input, { level: 9 });
    const brotli = brotliCompressSync(input, {
      params: {
        [constants.BROTLI_PARAM_QUALITY]: 11,
      },
    });
    writeFileSync(`${path}.gz`, gzip);
    writeFileSync(`${path}.br`, brotli);
    console.log(
      `[compress-dist] ${path.replace(`${DIST_DIR}/`, "")}: ${stat.size} -> br ${brotli.length}, gzip ${gzip.length}`,
    );
  }
}

if (!existsSync(DIST_DIR)) {
  console.warn(`[compress-dist] dist directory not found: ${DIST_DIR}`);
  process.exit(0);
}

walk(DIST_DIR);
