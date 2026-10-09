import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { parse } from 'node-html-parser';

const page = parse(readFileSync('_site/docs/performance.html', 'utf8'));
const table = page.querySelector('table#comparison');
assert.ok(table, 'performance comparison table is present in the built page');

assert.match(table.querySelector('caption').textContent, /specified quantiles/i);

const note = page.querySelector('p#comparison-scale');
assert.ok(note, 'the comparison table has a visible scale note');
assert.equal(table.getAttribute('aria-describedby'), 'comparison-scale');
assert.equal(table.previousElementSibling, note, 'the note appears immediately before the table');
const explanation = note.textContent.replace(/\s+/g, ' ').trim();
assert.match(explanation, /CPU.*percent of one (?:logical )?core/i);
assert.match(explanation, /(?:CPU\s*\/\s*PSS|CPU.*then PSS)/i);

const headings = table.querySelectorAll('thead th').map((cell) => cell.textContent.trim());
assert.deepEqual(headings, [
  'Scenario',
  'Thurbox (tmux)',
  'Thurbox (RMUX)',
  'Raw tmux',
  'Raw RMUX',
  'Herdr',
]);
assert.match(explanation, /100%.*one.*core.*25%.*four.core/i);
assert.match(page.textContent, /0b0611eb/);
assert.match(page.textContent, /gap remains/i);
assert.ok(
  page.querySelector('a[href="performance-results.html"]'),
  'full repeated-run results are linked',
);
const results = parse(readFileSync('_site/docs/performance-results.html', 'utf8'));
assert.match(results.textContent, /noise band/i);
assert.match(results.textContent, /Stage-1 delta/);
assert.match(results.textContent, /confirmed transport failure/i);
assert.ok(results.querySelector('a[href="../assets/performance-v2-observations.json"]'));
const observations = JSON.parse(readFileSync('_site/assets/performance-v2-observations.json', 'utf8'));
assert.equal(observations.source_commits.current, '0b0611eb96e669c5f62bd21e912561b7b7608f0e');
assert.equal(observations.machine.cpus, 4);
assert.ok(observations.current && observations.stage1, 'both measurement snapshots are downloadable');
const growth = observations.current["('2-hour growth', '20 sessions', 'pss_mib')"];
for (const host of ['tmux', 'thurbox', 'rmux', 'thurbox-rmux', 'herdr']) {
  assert.equal(growth[host].length, 3, `${host} has three completed memory cohorts`);
}
assert.doesNotMatch(page.textContent + results.textContent, /Measurement in progress/);
assert.ok(page.querySelector('h2#hud'), 'F12 walkthrough remains available');

console.log('Five-way performance pages explain CPU/PSS, noise, baseline deltas and transport failures.');
