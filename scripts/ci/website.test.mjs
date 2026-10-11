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

test('plugin catalog lists Automations with its repository, install command and current requirements', () => {
  const repo = 'https://github.com/Thurbeen/thurbox-automations';
  const card = built('plugins.html')
    .querySelectorAll('[data-plugin]')
    .find((c) => c.querySelector('h2 a')?.getAttribute('href') === repo);
  assert.ok(card, 'a catalog card links to the thurbox-automations repository');
  assert.equal(card.getAttribute('data-badge'), 'official');
  assert.equal(
    text(parse(card.querySelector('pre').innerHTML)).trim(),
    'thurbox-cli plugin install git+https://github.com/Thurbeen/thurbox-automations',
  );
  const copy = text(card);
  assert.match(copy, /run history|recent runs/i);
  assert.match(copy, /2\.56\.5/);
  assert.match(copy, /2\.57\.0/, 'names the release that brought create and edit');
  assert.doesNotMatch(copy, /need a release with/i, 'create and edit have shipped');
  const gif = card.querySelector('img').getAttribute('src');
  assert.equal(gif, 'https://raw.githubusercontent.com/Thurbeen/thurbox-automations/main/media/demo.gif');
});

test('features page points automations at their pane', () => {
  const features = built('docs/features.html');
  const repo = 'https://github.com/Thurbeen/thurbox-automations';
  assert.ok(features.querySelector(`a[href="${repo}"]`), 'links the automations pane');
  assert.doesNotMatch(text(features), /No pane, but they still fire/);
});
