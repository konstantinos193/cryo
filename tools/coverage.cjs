// Measure which CSS properties the engine reads, and which it does not
// but a real page needs.
//
// Two numbers come out, and only the second is worth acting on:
//
//   - coverage against every property MDN documents, which is mostly a
//     measure of how long CSS's tail is;
//   - the properties that are missing *and used* by the stylesheets of
//     the pages we test against, which is a work list in priority
//     order.
//
// Run with `node tools/coverage.cjs`. Writes nothing; prints a table
// that `src/engine/cryo_impl/CONFORMANCE.md` quotes.

const fs = require("fs");
const path = require("path");
const https = require("https");

/// The pages whose stylesheets decide what counts as "used".
const SHEETS = [
  "https://vectr.bot/_next/static/chunks/2n7n_mbiqb0un.css",
  "https://hotelpoint.io/_next/static/chunks/25scnzjxba82e.css",
];

const PROPERTIES =
  "https://raw.githubusercontent.com/mdn/data/main/css/properties.json";

function get(url) {
  return new Promise((resolve, reject) => {
    https
      .get(url, { headers: { "user-agent": "cryo-coverage" } }, (response) => {
        if (response.statusCode >= 300 && response.headers.location) {
          return resolve(get(new URL(response.headers.location, url).href));
        }
        if (response.statusCode !== 200) {
          return reject(new Error(`${url} -> ${response.statusCode}`));
        }
        let body = "";
        response.on("data", (chunk) => (body += chunk));
        response.on("end", () => resolve(body));
      })
      .on("error", reject);
  });
}

/// Every Rust source the style, layout and paint stages are made of.
///
/// A property counts as supported when its name appears as a string
/// literal somewhere in them — which is how the cascade looks one up.
/// That over-counts a name mentioned only in a comment and
/// under-counts one built by `format!`, so treat it as a close
/// estimate rather than a proof.
function engineSource() {
  const roots = ["css", "layout", "paint"].map((name) =>
    path.join(__dirname, "..", "src", "engine", "cryo_impl", name)
  );
  let source = "";
  const walk = (directory) => {
    for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
      const full = path.join(directory, entry.name);
      if (entry.isDirectory()) walk(full);
      else if (entry.name.endsWith(".rs")) source += fs.readFileSync(full, "utf8");
    }
  };
  roots.forEach(walk);
  return source;
}

(async () => {
  const documented = Object.keys(JSON.parse(await get(PROPERTIES))).filter(
    // Vendor-prefixed names are nobody's conformance target.
    (name) => !name.startsWith("-")
  );
  const source = engineSource();
  const supported = documented.filter((name) => source.includes(`"${name}"`));
  const missing = new Set(documented.filter((name) => !supported.includes(name)));

  const share = Math.round((supported.length / documented.length) * 100);
  console.log(
    `properties: ${supported.length} / ${documented.length} (${share}%)\n`
  );

  // How often each missing property appears in the real stylesheets.
  const used = {};
  for (const url of SHEETS) {
    const css = await get(url);
    for (const match of css.matchAll(/([-a-zA-Z]+)\s*:/g)) {
      const name = match[1];
      if (missing.has(name)) used[name] = (used[name] || 0) + 1;
    }
  }

  const ranked = Object.entries(used).sort((a, b) => b[1] - a[1]);
  console.log(`missing and used: ${ranked.length} properties\n`);
  console.log("  uses  property");
  for (const [name, count] of ranked) {
    console.log(`  ${String(count).padStart(4)}  ${name}`);
  }
})().catch((error) => {
  console.error(error.message);
  process.exit(1);
});
