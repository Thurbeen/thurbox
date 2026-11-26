export default {
  eleventyComputed: {
    // Eleventy's pretty permalinks would turn page.html into page/index.html.
    permalink: (data) => {
      if (data.repoDoc) return data.repoDoc.url;
      return data.page.filePathStem === '/sitemap'
        ? '/sitemap.xml'
        : `${data.page.filePathStem}.html`;
    },
  },
};
