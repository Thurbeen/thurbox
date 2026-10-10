import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { parse } from 'node-html-parser';

// Reads the built site, so run it after `npm run build:website`.
const built = (path) => parse(readFileSync(`_site/${path}`, 'utf8'));
const text = (node) => node.textContent.replace(/\s+/g, ' ');

test('plugin catalog lists Voice with its repository, install steps and data flow', () => {
  const repo = 'https://github.com/TMSCH/thurbox-voice';
  const card = built('plugins.html')
    .querySelectorAll('[data-plugin]')
    .find((c) => c.querySelector('h2 a')?.getAttribute('href') === repo);
  assert.ok(card, 'a catalog card links to the thurbox-voice repository');
  assert.equal(card.querySelector('h2').textContent, 'thurbox-voice');
  assert.equal(card.getAttribute('data-badge'), 'community');
  assert.ok(card.querySelector(`a[href="${repo}#readme"]`), 'setup link goes to the README');
  const install = text(parse(card.querySelector('pre').innerHTML));
  assert.match(install, /git clone https:\/\/github\.com\/TMSCH\/thurbox-voice/);
  assert.match(install, /thurbox-cli plugin install git\+https:\/\/github\.com\/TMSCH\/thurbox-voice/);
  const copy = text(card);
  assert.match(copy, /transcribed on your machine/i);
  assert.match(copy, /cleanup/i, 'the optional text cleanup pass is disclosed');
  assert.doesNotMatch(copy, /model picker|hold mode|hold to talk/i);
});

test('homepage does not promote performance', () => {
  const home = built('index.html');
  assert.ok(!home.querySelector('#performance'), 'no performance section');
  assert.ok(!home.querySelector('a[href*="performance"]'), 'no performance links');
  assert.doesNotMatch(text(home), /benchmark|five-way|frames a second|% of one core|Herdr/i);
});
