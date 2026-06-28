// Directory data for everything under website/.
//
// Eleventy's default "pretty permalinks" would turn installation.html into
// installation/index.html, which breaks every hand-written `.html` link in the
// site. Force flat output that mirrors each source file's path + ".html" so the
// built URLs are byte-identical to the original static site.
//
// Exception: the generated sitemap must be served as /sitemap.xml, not
// sitemap.html. The computed permalink wins over front matter, so it is
// special-cased here by source path (reading data.permalink instead would
// self-reference this computed key and silently revert pages to pretty URLs).
export default {
  eleventyComputed: {
    permalink: (data) =>
      data.page.filePathStem === '/sitemap'
        ? '/sitemap.xml'
        : `${data.page.filePathStem}.html`,
  },
};
