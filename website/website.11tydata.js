// Directory data for everything under website/.
//
// Eleventy's default "pretty permalinks" would turn installation.html into
// installation/index.html, which breaks every hand-written `.html` link in the
// site. Force flat output that mirrors each source file's path + ".html" so the
// built URLs are byte-identical to the original static site.
export default {
  eleventyComputed: {
    permalink: (data) => `${data.page.filePathStem}.html`,
  },
};
