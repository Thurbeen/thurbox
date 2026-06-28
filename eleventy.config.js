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

import { parse } from 'node-html-parser';
import Prism from 'prismjs';
import loadLanguages from 'prismjs/components/index.js';

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
    <span class="terminal-dot red"></span>
    <span class="terminal-dot yellow"></span>
    <span class="terminal-dot green"></span>
    <span class="code-block-lang">${escapeHtml(decodeEntities(label))}</span>
    <button class="copy-btn" type="button" aria-label="Copy code to clipboard">Copy</button>
  </div>
  <div class="code-block-body"><pre><code class="language-${lang}">${highlighted}</code></pre></div>
</div>`;
}

export default function (eleventyConfig) {
  // Static assets are copied through untouched. The input-dir prefix
  // ("website/") is stripped in the output, so these land at _site/css, etc.
  eleventyConfig.addPassthroughCopy('website/css');
  eleventyConfig.addPassthroughCopy('website/js');
  eleventyConfig.addPassthroughCopy('website/assets');
  // robots.txt (crawler discovery) + CNAME (the GitHub Pages custom domain,
  // tracked so a redeploy can't drop it) + llms.txt (curated entry point for
  // LLM-based engines and coding agents). Same prefix-stripping as above.
  eleventyConfig.addPassthroughCopy('website/robots.txt');
  eleventyConfig.addPassthroughCopy('website/CNAME');
  eleventyConfig.addPassthroughCopy('website/llms.txt');

  // Build-time syntax highlighting. Runs on rendered HTML output only, so the
  // page bodies stay verbatim in source. Blocks without a `language-*` class
  // (the ASCII-art TUI mockups, video frames) never match and are untouched.
  eleventyConfig.addTransform('highlight-code', function (content, outputPath) {
    if (!outputPath || !outputPath.endsWith('.html')) return content;
    return content.replace(CODE_BLOCK_RE, renderCodeBlock);
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
