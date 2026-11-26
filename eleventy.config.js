// Eleventy build for the Thurbox docs website.
//
// The site is plain static HTML/CSS/JS; Eleventy is used only to de-duplicate
// the shared chrome (head, nav, docs sidebar, footer) into layouts under
// website/_includes/. Each page keeps its hand-written content body and just
// declares front matter (layout, title, root depth, sidebar state).
//
// Page bodies are emitted verbatim (htmlTemplateEngine: false) so nothing in
// the content is ever interpreted as a template — only the .njk layouts run.
//
// Code blocks are the one exception: a build-time transform (`highlight-code`)
// rewrites every `<pre><code class="language-X">…</code></pre>` into a framed,
// syntax-highlighted block. Authors write plain, escaped code with a language
// class; the transform supplies the terminal chrome, language label, copy
// button, and Prism token spans. This keeps highlighting entirely at build
// time (zero client-side JS) and out of the hand-written page bodies.

import { existsSync, mkdirSync, readdirSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { RenderPlugin } from '@11ty/eleventy';
import { parse } from 'node-html-parser';
import Prism from 'prismjs';
import loadLanguages from 'prismjs/components/index.js';
import { galleryData } from './scripts/ci/mods-gallery.mjs';

loadLanguages(['bash', 'powershell', 'toml', 'json', 'rust']);

// Author-facing language tokens → the canonical Prism grammar key.
const LANG_ALIASES = {
  sh: 'bash',
  shell: 'bash',
  console: 'bash',
  bash: 'bash',
  ps: 'powershell',
  ps1: 'powershell',
  pwsh: 'powershell',
  powershell: 'powershell',
  toml: 'toml',
  json: 'json',
  rust: 'rust',
  rs: 'rust',
};

// Matches a single authored code block: an optional attribute list on <pre>
// (carrying data-title / data-wrap), then <code class="language-X">…</code>.
// The body is non-greedy and an escaped `</code>` can never appear in it, so
// this stays reliable over the controlled authoring format.
const CODE_BLOCK_RE =
  /<pre((?:\s+[a-z-]+(?:="[^"]*")?)*)\s*>\s*<code class="language-([\w-]+)">([\s\S]*?)<\/code>\s*<\/pre>/g;

// Decode HTML entities back to the raw source text Prism expects (and that the
// copy button later reads off `pre.textContent`). Reuse node-html-parser's own
// decoder by round-tripping through a throwaway element.
function decodeEntities(escaped) {
  return parse(`<x>${escaped}</x>`).querySelector('x').textContent;
}

function escapeHtml(text) {
  return text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}

// Pull data-title / data-wrap out of the captured <pre> attribute string.
function parsePreAttrs(attrs) {
  const titleMatch = attrs.match(/\bdata-title="([^"]*)"/);
  return {
    title: titleMatch ? titleMatch[1] : null,
    wrap: /\bdata-wrap\b/.test(attrs),
  };
}

function renderCodeBlock(match, attrs, rawLang, body) {
  const lang = LANG_ALIASES[rawLang.toLowerCase()];
  if (!lang || !Prism.languages[lang]) {
    return match; // unsupported language — leave the block untouched
  }

  const { title, wrap } = parsePreAttrs(attrs);
  const source = decodeEntities(body);
  const highlighted = Prism.highlight(source, Prism.languages[lang], lang);
  const label = title || lang;
  const wrapAttr = wrap ? ' data-wrap' : '';

  return `<div class="code-block"${wrapAttr}>
  <div class="code-block-header">
    <span class="code-block-lang">${escapeHtml(decodeEntities(label))}</span>
    <button class="copy-btn" type="button" aria-label="Copy code to clipboard">Copy</button>
  </div>
  <div class="code-block-body"><pre><code class="language-${lang}">${highlighted}</code></pre></div>
</div>`;
}

// ---- Build-time "on this page" rail ----
// Every docs <h2>/<h3> is already hand-authored with an `id`, so the rail can be
// derived from the rendered HTML instead of being restated in each page's front
// matter (which 19 of 22 pages used to do, and three pages simply lacked).
// Build-time keeps it consistent with the rest of the pipeline: no client-side
// JS, no first-paint layout shift from a rail that appears late.
const TOC_PLACEHOLDER = '<!--TOC-->';

// A single heading's link text, minus the trailing "#" anchor affordance.
function headingLabel(heading) {
  const clone = parse(heading.innerHTML);
  clone.querySelectorAll('.heading-anchor').forEach((a) => a.remove());
  return clone.textContent.trim().replace(/\s+/g, ' ');
}

function renderToc(root) {
  const items = [];
  root.querySelectorAll('.docs-content h2[id], .docs-content h3[id]').forEach((h) => {
    const label = headingLabel(h);
    if (label) items.push({ id: h.getAttribute('id'), label, level: h.rawTagName.toLowerCase() });
  });

  // A page with a single heading gains nothing from a rail.
  if (items.length < 2) return '';

  const lis = items
    .map(
      (i) =>
        `<li class="docs-toc__item docs-toc__item--${i.level}">` +
        `<a href="#${i.id}">${escapeHtml(i.label)}</a></li>`,
    )
    .join('\n      ');

  return `<nav class="docs-toc" aria-labelledby="docs-toc-heading">
  <h2 class="docs-toc__heading" id="docs-toc-heading">On this page</h2>
  <ul class="docs-toc__list">
      ${lis}
  </ul>
</nav>`;
}

// ---- Build-time CSS bundle ----
// The four sheets every page loads (tokens, reset, layout, components) are
// concatenated into one `core.css`, turning four render-blocking requests into
// one. They are authored separately for maintainability and must stay in this
// order — variables first (every other file reads its custom properties), then
// base, layout, components, matching the old <link> order exactly so the
// cascade is unchanged. Page-specific sheets are deliberately NOT bundled: they
// would otherwise ship landing CSS to docs pages and vice versa.
const CORE_CSS = ['variables', 'base', 'layout', 'components'];

// Comments and blank lines are ~20% of the source and carry no runtime meaning.
// Deliberately conservative: strip /* … */ and collapse runs of blank lines,
// but leave declarations untouched — a real minifier is not worth a dependency
// here, and mangling values risks the layout for a few hundred bytes.
function stripCssComments(css) {
  return css.replace(/\/\*[\s\S]*?\*\//g, '').replace(/\n{2,}/g, '\n');
}

function writeCoreBundle() {
  const parts = CORE_CSS.map((name) => {
    const src = readFileSync(`website/css/${name}.css`, 'utf8');
    return `/* ${name}.css */\n${stripCssComments(src).trim()}`;
  });
  const bundle = `/* Generated by eleventy.config.js — edit website/css/*.css, not this file. */\n${parts.join('\n')}\n`;
  mkdirSync('_site/css', { recursive: true });
  writeFileSync('_site/css/core.css', bundle);
  return bundle.length;
}

// ---- The repository's Fleet and runtime docs, rendered as pages ----
// docs/fleet and docs/runtime, and thurbox's mod-author guides, are the one
// source of these pages: each Markdown file becomes /docs/<same path>.html (a
// README becomes the directory's index.html) through the paginated
// website/repo-docs.njk. A directory is read on every build, so a new or
// renamed page there needs no change here; a single file is listed by name.
// .github/workflows/pages.yml redeploys on a change to each entry; the website
// test holds its `paths` to this list.
export const RENDERED_DOCS = [
  'docs/fleet',
  'docs/runtime',
  'docs/thurbox/mods.md',
  'docs/thurbox/kernel.md',
  'docs/thurbox/architecture.md',
  'docs/observability.md',
];

const REPO_URL = 'https://github.com/Thurbeen/thurbox';

// A repository document the site does not render, linked from one it does:
// the website page that covers it, relative to /docs/. A `path#fragment` key
// maps one section; the bare path is the fallback, since GitHub's heading ids
// mean nothing on the site's own page. A document missing here loses its link
// and keeps its text.
const DOC_ON_SITE = {
  'docs/README.md': 'architecture.html#components',
  'docs/where-sessions-run.md': 'architecture.html#places',
  'docs/thurbox/config.md#fleets--the-fleets-that-manage-sessions': 'configuration.html#fleets',
  'docs/thurbox/config.md': 'configuration.html',
};

const docUrl = (src) =>
  `/${src.replace(/(^|\/)README\.md$/, '$1index.md').replace(/\.md$/, '.html')}`;

// A site URL as a link from the page at `fromUrl`: the site links relatively,
// so it also works opened from disk.
const relativeUrl = (fromUrl, toUrl) => path.posix.relative(path.posix.dirname(fromUrl), toUrl);

// The first paragraph under the title, as plain text, for the meta description.
function summary(body) {
  const para = body
    .split(/\n\s*\n/)
    .map((block) => block.trim())
    .find((block) => block && !/^(#|\||```|!\[|[-*] )/.test(block));
  const plain = (para || '')
    .replace(/\[([^\]]*)\]\([^)]*\)/g, '$1')
    .replace(/[`*_]/g, '')
    .replace(/\s+/g, ' ')
    .trim();
  return plain.length > 200 ? `${plain.slice(0, 197).replace(/\s+\S*$/, '')}…` : plain;
}

// Every rendered Markdown file, as [directory, path below it].
const renderedFiles = () =>
  RENDERED_DOCS.flatMap((entry) =>
    entry.endsWith('.md')
      ? [[path.posix.dirname(entry), path.posix.basename(entry)]]
      : readdirSync(entry, { recursive: true })
          .filter((file) => file.endsWith('.md'))
          .sort()
          .map((file) => [entry, file]),
  );

function listRepoDocs() {
  return renderedFiles().map(([dir, file]) => {
    const src = `${dir}/${file}`;
    const url = docUrl(src);
    const source = readFileSync(src, 'utf8');
    const title = (source.match(/^# (.+)$/m) || [, file])[1].replace(/`/g, '');
    const depth = url.split('/').length - 3; // directories below /docs/
    const area = src.split('/')[2]; // resources, controllers, or a page
    return {
      src,
      url,
      title,
      description: summary(source.replace(/^# .+$/m, '')),
      body: source,
      root: Array(depth + 1)
        .fill('..')
        .join('/'),
      docsBase: '../'.repeat(depth),
      product:
        { 'docs/fleet': 'Fleet', 'docs/thurbox': 'thurbox', docs: 'Fleet' }[dir] ?? 'Runtime',
      // The sidebar entry this page sits under, if any.
      currentPage:
        {
          'docs/fleet/README.md': 'fleet-reference',
          'docs/fleet/configuration.md': 'fleet-configuration',
          'docs/runtime/herdr-users.md': 'fleet-herdr',
          'docs/fleet/resources/workspace.md': 'fleet-workspace',
          'docs/observability.md': 'observability',
          'docs/thurbox/mods.md': 'thurbox-mods',
          'docs/thurbox/kernel.md': 'thurbox-kernel',
        }[src] ?? (dir === 'docs/fleet' && file.includes('/') ? `fleet-${area}` : ''),
    };
  });
}

// Where a link in the rendered document `fromSrc` goes, or null to drop it.
function rewriteHref(href, fromSrc, renderedSet) {
  if (/^([a-z][a-z0-9+.-]*:|#|\/\/)/i.test(href)) return href;
  const [rawPath, fragment] = href.split(/#(.*)/s);
  const hash = fragment === undefined ? '' : `#${fragment}`;
  const target = path.posix.normalize(
    path.posix.join(path.posix.dirname(fromSrc), decodeURI(rawPath)),
  );
  const fromUrl = docUrl(fromSrc);
  if (renderedSet.has(target)) return relativeUrl(fromUrl, docUrl(target)) + hash;
  if (target.startsWith('docs/')) {
    const mapped = DOC_ON_SITE[target + hash] ?? DOC_ON_SITE[target];
    if (mapped) return relativeUrl(fromUrl, `/docs/${mapped}`);
    if (target.endsWith('.md')) {
      console.warn(`[11ty] ${fromSrc}: no site page for ${href}; the link is dropped`);
      return null;
    }
    return href; // a file beside the docs, copied with them (the SVGs)
  }
  // Source code and examples stay on GitHub.
  const kind = existsSync(target) && statSync(target).isDirectory() ? 'tree' : 'blob';
  return `${REPO_URL}/${kind}/main/${target}${hash}`;
}

// GitHub's heading ids (github-slugger): lower-case, drop punctuation, spaces
// to hyphens, and -1, -2… on a repeat. The docs' own `#fragment` links are
// written against these.
function githubSlug(text, seen) {
  const base = text
    .toLowerCase()
    .replace(/[^\p{L}\p{M}\p{N}\p{Pc} -]/gu, '')
    .replace(/ /g, '-');
  let slug = base;
  for (let n = 1; seen.has(slug); n++) slug = `${base}-${n}`;
  seen.add(slug);
  return slug;
}

// A markdown-it core rule: heading ids and anchors, and the links rewritten
// for the page's place on the site. The page passes { repoDoc } as the render
// environment; anything else rendered as Markdown is left alone.
function repoDocRule(state) {
  const doc = state.env && state.env.repoDoc;
  if (!doc) return;
  const renderedSet = new Set(state.env.repoDocs.map((d) => d.src));
  const seen = new Set();
  state.tokens.forEach((token, i) => {
    if (token.type === 'heading_open') {
      const inline = state.tokens[i + 1];
      const text = inline.children
        .filter((c) => c.type === 'text' || c.type === 'code_inline')
        .map((c) => c.content)
        .join('');
      const id = githubSlug(text, seen);
      token.attrSet('id', id);
      if (token.tag !== 'h1') {
        const anchor = new state.Token('html_inline', '', 0);
        anchor.content = ` <a href="#${id}" class="heading-anchor">#</a>`;
        inline.children.push(anchor);
      }
    }
    if (token.type !== 'inline' || !token.children) return;
    const kept = [];
    let dropping = false;
    for (const child of token.children) {
      if (child.type === 'link_open') {
        const href = rewriteHref(child.attrGet('href'), doc.src, renderedSet);
        if (href === null) {
          dropping = true;
          continue;
        }
        child.attrSet('href', href);
      } else if (child.type === 'link_close' && dropping) {
        dropping = false;
        continue;
      } else if (child.type === 'image') {
        child.attrSet('src', rewriteHref(child.attrGet('src'), doc.src, renderedSet) ?? '');
      }
      kept.push(child);
    }
    token.children = kept;
  });
}

export default function (eleventyConfig) {
  // Runs on every build and rebuild, so `--serve` keeps the bundle fresh.
  eleventyConfig.on('eleventy.after', () => {
    const bytes = writeCoreBundle();
    console.log(`[11ty] Wrote _site/css/core.css (${(bytes / 1024).toFixed(1)} KB)`);
  });

  // Static assets are copied through untouched. The input-dir prefix
  // ("website/") is stripped in the output, so these land at _site/css, etc.
  // Only the page-specific sheets are copied: the four core files are shipped
  // as the generated core.css bundle instead, so copying them too would be dead
  // weight in the deployed output.
  for (const sheet of ['landing', 'docs', 'tutorial', 'mods']) {
    eleventyConfig.addPassthroughCopy(`website/css/${sheet}.css`);
  }
  eleventyConfig.addPassthroughCopy('website/js');
  eleventyConfig.addPassthroughCopy('website/mods.schema.json');
  eleventyConfig.addPassthroughCopy('website/assets');
  // robots.txt (crawler discovery) + CNAME (the GitHub Pages custom domain,
  // tracked so a redeploy can't drop it) + llms.txt (curated entry point for
  // LLM-based engines and coding agents). Same prefix-stripping as above.
  eleventyConfig.addPassthroughCopy('website/robots.txt');
  eleventyConfig.addPassthroughCopy('website/CNAME');
  eleventyConfig.addPassthroughCopy('website/llms.txt');

  // The repository docs rendered as pages (see listRepoDocs), their diagrams
  // beside them, and a rebuild under --serve when one changes.
  eleventyConfig.addPlugin(RenderPlugin);
  eleventyConfig.addGlobalData('repoDocs', listRepoDocs);
  eleventyConfig.addGlobalData('gallery', () => galleryData());
  eleventyConfig.amendLibrary('md', (md) => md.core.ruler.push('repo_doc', repoDocRule));
  for (const dir of RENDERED_DOCS) {
    eleventyConfig.addPassthroughCopy(`${dir}/**/*.svg`);
    eleventyConfig.addWatchTarget(`${dir}/`);
  }

  // Build-time syntax highlighting. Runs on rendered HTML output only, so the
  // page bodies stay verbatim in source. Blocks without a `language-*` class
  // (the ASCII-art TUI mockups, video frames) never match and are untouched.
  eleventyConfig.addTransform('highlight-code', function (content, outputPath) {
    if (!outputPath || !outputPath.endsWith('.html')) return content;
    return content.replace(CODE_BLOCK_RE, renderCodeBlock);
  });

  // Registered after highlight-code: transforms run in registration order, and
  // building the rail from already-final HTML keeps the two independent. Pages
  // without the placeholder (the landing page) short-circuit before parsing.
  eleventyConfig.addTransform('docs-toc', function (content, outputPath) {
    if (!outputPath || !outputPath.endsWith('.html')) return content;
    if (!content.includes(TOC_PLACEHOLDER)) return content;
    return content.replace(TOC_PLACEHOLDER, renderToc(parse(content)));
  });

  return {
    dir: {
      input: 'website',
      output: '_site',
      includes: '_includes',
    },
    // Layouts are Nunjucks; page bodies are left untouched. `njk` is enabled
    // for generated data files (sitemap.xml); `_includes` is excluded from
    // page processing, so the shared layouts are unaffected.
    htmlTemplateEngine: false,
    templateFormats: ['html', 'njk'],
  };
}
