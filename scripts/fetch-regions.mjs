#!/usr/bin/env node
import { writeFileSync, mkdirSync, existsSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const REGIONS_URL =
  "https://raw.githubusercontent.com/Xiechengqi/portr-rs/refs/heads/master/regions";

const __dirname = dirname(fileURLToPath(import.meta.url));
const OUTPUT_PATH = resolve(__dirname, "../src/config/shareRegions.ts");

function parseRegions(text) {
  return text
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean)
    .map((line) => {
      const idx = line.indexOf(":");
      if (idx < 0) return null;
      return { region: line.slice(0, idx), baseUrl: line.slice(idx + 1) };
    })
    .filter(Boolean);
}

function render(regions) {
  const body = regions
    .map(
      (r) =>
        `  { region: ${JSON.stringify(r.region)}, baseUrl: ${JSON.stringify(r.baseUrl)} },`,
    )
    .join("\n");
  return `// AUTO-GENERATED at build time by scripts/fetch-regions.mjs. Do not edit manually.
// Source: ${REGIONS_URL}

export interface ShareRegion {
  region: string;
  baseUrl: string;
}

export const SHARE_REGIONS: ShareRegion[] = [
${body}
];
`;
}

async function main() {
  let regions = [];
  try {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), 15000);
    const res = await fetch(REGIONS_URL, { signal: controller.signal });
    clearTimeout(timer);
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    const text = await res.text();
    regions = parseRegions(text);
    if (regions.length === 0) throw new Error("empty regions file");
    console.log(`[fetch-regions] fetched ${regions.length} regions`);
  } catch (err) {
    console.warn(`[fetch-regions] fetch failed: ${err?.message || err}`);
    if (existsSync(OUTPUT_PATH)) {
      console.warn("[fetch-regions] keeping existing snapshot");
      return;
    }
    console.warn("[fetch-regions] writing empty snapshot");
  }

  mkdirSync(dirname(OUTPUT_PATH), { recursive: true });
  writeFileSync(OUTPUT_PATH, render(regions), "utf8");
  console.log(`[fetch-regions] wrote ${OUTPUT_PATH}`);
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
