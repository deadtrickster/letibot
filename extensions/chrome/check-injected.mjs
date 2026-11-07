// Injected functions must be self-contained, from oracle's chrome-capture check-injected.mjs.
// `executeScript({func})` serialises the function into the page, so anything it closes over in
// background.js is a ReferenceError there.
import { readFileSync } from "node:fs";

const src = readFileSync(new URL("./background.js", import.meta.url), "utf8");

const injectedNames = [...src.matchAll(/func:\s*([A-Za-z_$][\w$]*)/g)].map((m) => m[1]);

const moduleScope = new Set(
  [...src.matchAll(/^(?:const|let|var|async function|function)\s+([A-Za-z_$][\w$]*)/gm)].map((m) => m[1]),
);

let failed = 0;
for (const name of new Set(injectedNames)) {
  const at = src.search(new RegExp(`\\n(?:async )?function ${name}\\s*\\([^)]*\\)\\s*\\{`));
  if (at < 0) {
    console.log(`FAIL  func: ${name} names no top-level function`);
    failed++;
    continue;
  }
  let i = src.indexOf("{", at), depth = 0, end = i;
  for (; end < src.length; end++) {
    if (src[end] === "{") depth++;
    else if (src[end] === "}" && --depth === 0) break;
  }
  // Comments and strings out first, or the check reads prose as identifiers.
  const body = src.slice(i, end + 1)
    .replace(/\/\*[\s\S]*?\*\//g, " ")
    .replace(/\/\/[^\n]*/g, " ")
    .replace(/`(?:\\.|[^`\\])*`/g, '""')
    .replace(/"(?:\\.|[^"\\])*"/g, '""')
    .replace(/'(?:\\.|[^'\\])*'/g, '""');
  const leaks = [...moduleScope].filter((id) =>
    id !== name && new RegExp(`\\b${id}\\b`).test(body) &&
    !new RegExp(`(?:const|let|var|function)\\s+${id}\\b`).test(body));
  if (leaks.length) {
    console.log(`FAIL  ${name}() references module scope: ${leaks.join(", ")}`);
    failed++;
  } else {
    console.log(`ok    ${name}() is self-contained`);
  }
}
process.exit(failed ? 1 : 0);
