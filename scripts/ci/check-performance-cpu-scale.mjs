import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { parse } from 'node-html-parser';

const page = parse(readFileSync('_site/docs/performance.html', 'utf8'));
const table = page.querySelector('table#stand-in-comparison');
assert.ok(table, 'performance comparison table is present in the built page');

const note = page.querySelector('p#comparison-scale');
assert.ok(note, 'the comparison table has a visible scale note');
assert.equal(table.getAttribute('aria-describedby'), 'comparison-scale');
assert.equal(table.previousElementSibling, note, 'the note appears immediately before the table');
const explanation = note.textContent.replace(/\s+/g, ' ').trim();
assert.match(explanation, /CPU.*percent of one (?:logical )?core/i);
assert.match(explanation, /CPU.*PSS/i);
assert.match(explanation, /121%.*1\.21.*cores.*30%.*four.core/i);

const printingRow = table.querySelectorAll('tbody tr').find((row) =>
  row.querySelector('th')?.textContent.includes('50 idle, client attached: CPU %'),
);
assert.ok(printingRow, 'the 50-session attached-idle CPU row is present');
assert.equal(printingRow.querySelectorAll('td')[1]?.textContent.trim(), '77.8 (121)');

console.log('Performance table explains CPU/PSS and the 121% core scale.');
