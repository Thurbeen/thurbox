// Eleventy build for the Thurbox docs website.
//
// The site is plain static HTML/CSS/JS; Eleventy is used only to de-duplicate
// the shared chrome (head, nav, docs sidebar, footer) into layouts under
// website/_includes/. Each page keeps its hand-written content body and just
// declares front matter (layout, title, root depth, sidebar state).
//
// Page bodies are emitted verbatim (htmlTemplateEngine: false) so nothing in
// the content is ever interpreted as a template — only the .njk layouts run.

export default function (eleventyConfig) {
  // Static assets are copied through untouched. The input-dir prefix
  // ("website/") is stripped in the output, so these land at _site/css, etc.
  eleventyConfig.addPassthroughCopy('website/css');
  eleventyConfig.addPassthroughCopy('website/js');
  eleventyConfig.addPassthroughCopy('website/assets');

  return {
    dir: {
      input: 'website',
      output: '_site',
      includes: '_includes',
    },
    // Layouts are Nunjucks; page bodies are left untouched.
    htmlTemplateEngine: false,
    templateFormats: ['html'],
  };
}
