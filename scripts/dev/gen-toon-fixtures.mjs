import { encode } from '@toon-format/toon';
import fs from 'node:fs';
import path from 'node:path';

// serde_json's Map is a BTreeMap: keys come out sorted. Sort the reference
// encoder's input the same way so the two encoders see one key order and the
// diff is about encoding, not ordering.
const sortDeep = (v) => {
  if (Array.isArray(v)) return v.map(sortDeep);
  if (v && typeof v === 'object') {
    const out = {};
    for (const k of Object.keys(v).sort()) out[k] = sortDeep(v[k]);
    return out;
  }
  return v;
};

const dir = process.argv[2];
const cases = [];
for (const f of fs.readdirSync(dir).filter((f) => f.endsWith('.json'))) {
  const fx = JSON.parse(fs.readFileSync(path.join(dir, f), 'utf8'));
  for (const t of fx.tests) {
    const input = sortDeep(t.input);
    const opts = t.options ?? {};
    let expected;
    try { expected = encode(input, opts); } catch (e) { continue; }
    cases.push({
      name: `${f}:${t.name}`,
      section: t.specSection,
      input,
      expected,
      delimiter: opts.delimiter ?? ',',
      indent: opts.indentSize ?? 2,
    });
  }
}
fs.writeFileSync(process.argv[3], cases.map((c) => JSON.stringify(c)).join('\n'));
console.log(`${cases.length} cases`);
