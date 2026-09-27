import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { parse } from 'node-html-parser';

const page = parse(readFileSync('_site/docs/performance.html', 'utf8'));
const table = page.querySelector('table#comparison');
assert.ok(table, 'performance comparison table is present in the built page');

const note = page.querySelector('p#comparison-scale');
assert.ok(note, 'the comparison table has a visible scale note');
assert.equal(table.getAttribute('aria-describedby'), 'comparison-scale');
assert.equal(table.previousElementSibling, note, 'the note appears immediately before the table');
const explanation = note.textContent.replace(/\s+/g, ' ').trim();
assert.match(explanation, /CPU.*percent of one (?:logical )?core/i);
assert.match(explanation, /(?:CPU\s*\/\s*PSS|CPU.*then PSS)/i);
assert.match(explanation, /104%.*1\.04.*cores.*26%.*four.core/i);

const printingRow = table.querySelectorAll('tbody tr').find((row) =>
  row.querySelector('th')?.textContent.includes('50 printing, UI open'),
);
assert.ok(printingRow, 'the 50-printing UI-open row is present');
assert.equal(printingRow.querySelectorAll('td')[1]?.textContent.trim(), '104% / 53.9 MiB');

console.log('Performance table explains CPU/PSS and the 104% core scale.');
