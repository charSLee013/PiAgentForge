/**
 * Export models from the TypeScript models.generated.ts to a flat JSON array.
 *
 * Reads the TS source, parses each model entry, converts costs from
 * per-1M-tokens to per-token, and writes models.json.
 *
 * Usage: node scripts/export-models.mjs
 */

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(__dirname, "..");

// Paths
const TS_PATH = process.env.PI_MODELS_GENERATED_TS ?? "packages/ai/src/models.generated.ts";
const JSON_PATH = path.resolve(REPO_ROOT, "models.json");

// ── Helpers ─────────────────────────────────────────────────────────

/**
 * Extract a string value from a single-line TS field.
 *   fieldName: "value",
 */
function extractString(body, fieldName) {
  const re = new RegExp(`\\b${fieldName}:\\s*"([^"]*)"`);
  const m = body.match(re);
  return m ? m[1] : null;
}

/**
 * Extract a number from a single-line TS field.
 *   fieldName: 123,
 *   fieldName: 0.5,
 */
function extractNumber(body, fieldName) {
  const re = new RegExp(`\\b${fieldName}:\\s*([\\d.]+)`);
  const m = body.match(re);
  return m ? parseFloat(m[1]) : null;
}

/**
 * Extract a boolean from a single-line TS field.
 *   fieldName: true,
 *   fieldName: false,
 */
function extractBoolean(body, fieldName) {
  const re = new RegExp(`\\b${fieldName}:\\s*(true|false)`);
  const m = body.match(re);
  return m ? m[1] === "true" : null;
}

/**
 * Extract an array from a TS field.
 *   input: ["text", "image"],
 */
function extractArray(body, fieldName) {
  const re = new RegExp(`\\b${fieldName}:\\s*\\[([^\\]]*)\\]`);
  const m = body.match(re);
  if (!m) return [];
  return m[1]
    .split(",")
    .map((s) => s.trim().replace(/^"|"$/g, ""))
    .filter(Boolean);
}

/**
 * Extract a block between { } from a TS field.
 *   cost: { input: 2.5, output: 10, ... },
 *   thinkingLevelMap: {"xhigh": "max"},
 */
function extractBlock(body, fieldName) {
  const re = new RegExp(`\\b${fieldName}:\\s*(\\{)`, "m");
  const startMatch = body.match(re);
  if (!startMatch) return null;

  const startIdx = startMatch.index + startMatch[0].length - 1; // index of {
  let depth = 1;
  let endIdx = startIdx;
  while (endIdx < body.length && depth > 0) {
    endIdx++;
    const ch = body[endIdx];
    if (ch === "{") depth++;
    if (ch === "}") depth--;
  }
  return body.slice(startIdx + 1, endIdx);
}

// ── Provider → KnownProvider mapping ────────────────────────────────

/**
 * Map a TS provider string to its Rust KnownProvider serde name.
 * Returns null for providers not (yet) in the Rust enum.
 */
function mapProvider(tsProvider) {
  // Direct matches (serde rename matches the TS value)
  const DIRECT = new Set(["anthropic", "openai", "google", "mistral", "faux"]);
  if (DIRECT.has(tsProvider)) return tsProvider;

  // Mapped providers (TS value differs from serde rename)
  const MAPPED = {
    "amazon-bedrock": "bedrock",
  };
  if (MAPPED[tsProvider]) return MAPPED[tsProvider];

  return null; // not yet supported in Rust KnownProvider
}

// ── Main ────────────────────────────────────────────────────────────

function main() {
  if (!fs.existsSync(TS_PATH)) {
    console.error(`TS source not found at: ${TS_PATH}`);
    console.error(
      "Expected relative path: ../../../pi/packages/ai/src/models.generated.ts"
    );
    process.exit(1);
  }

  const content = fs.readFileSync(TS_PATH, "utf-8");
  const lines = content.split("\n");

  /** @type {Array<object>} */
  const models = [];

  let currentProvider = null;

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];

    // Detect provider key: exactly one tab prefix
    const provMatch = line.match(/^\t"([^"]+)":\s*\{$/);
    if (provMatch) {
      currentProvider = provMatch[1];
      continue;
    }

    // Detect model entry: two-tab prefix, key, ": {"
    const modelMatch = line.match(/^\t\t"([^"]+)":\s*\{$/);
    if (!modelMatch) continue;

    const modelKeyInParent = modelMatch[1];

    // Collect lines until we find the closing `} satisfies Model<` at two-tab indent
    let j = i + 1;
    while (j < lines.length && !/^\t\t} satisfies Model</.test(lines[j])) {
      j++;
    }

    // The model body lines are i+1 .. j-1
    const body = lines.slice(i + 1, j).join("\n");

    // ── Extract fields ─────────────────────────────────

    const model = {};

    // id (from body or from the parent key)
    model.id = extractString(body, "id") || modelKeyInParent;

    model.name = extractString(body, "name");

    // api
    model.api = extractString(body, "api");

    // provider (from body or from current provider state)
    const rawProvider = extractString(body, "provider") || currentProvider;

    // Map to KnownProvider; skip if not mapped
    const mappedProvider = mapProvider(rawProvider);
    if (!mappedProvider) {
      i = j;
      continue;
    }
    model.provider = mappedProvider;

    // base_url (snake_case for JSON)
    model.base_url = extractString(body, "baseUrl");

    // supports_* flags

    // supports_thinking ← reasoning
    const reasoning = extractBoolean(body, "reasoning");
    model.supports_thinking = reasoning === true;

    // supports_image_input ← "image" in input array
    const inputArr = extractArray(body, "input");
    model.supports_image_input = inputArr.includes("image");

    // supports_tools / supports_streaming — assume true for all modern models
    model.supports_tools = true;
    model.supports_streaming = true;

    // context_window ← contextWindow
    const cw = extractNumber(body, "contextWindow");
    model.context_window = cw;

    // max_tokens ← maxTokens
    const mt = extractNumber(body, "maxTokens");
    model.max_tokens = mt;

    // cost block
    const costBlock = extractBlock(body, "cost");
    if (costBlock) {
      const costInput = extractNumber(costBlock, "input");
      const costOutput = extractNumber(costBlock, "output");
      const costCacheRead = extractNumber(costBlock, "cacheRead");
      const costCacheWrite = extractNumber(costBlock, "cacheWrite");

      // Convert from $/1M tokens to $/token
      const PER_M = 1_000_000;
      model.cost_per_input_token = costInput !== null ? costInput / PER_M : null;
      model.cost_per_output_token =
        costOutput !== null ? costOutput / PER_M : null;
      model.cost_per_cache_read_token =
        costCacheRead !== null ? costCacheRead / PER_M : null;
      model.cost_per_cache_write_token =
        costCacheWrite !== null ? costCacheWrite / PER_M : null;
    } else {
      model.cost_per_input_token = null;
      model.cost_per_output_token = null;
      model.cost_per_cache_read_token = null;
      model.cost_per_cache_write_token = null;
    }

    models.push(model);

    // Advance past the "satisfies Model" line
    i = j;
  }

  // Write output
  const json = JSON.stringify(models, null, 2);
  fs.writeFileSync(JSON_PATH, json, "utf-8");
  console.log(`Exported ${models.length} models to ${JSON_PATH}`);
}

main();
