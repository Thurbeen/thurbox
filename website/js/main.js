(function () {
  'use strict';

  // ---- Mobile nav toggle ----
  var hamburger = document.getElementById('hamburger');
  var navLinks = document.getElementById('nav-links');

  if (hamburger && navLinks) {
    hamburger.addEventListener('click', function () {
      hamburger.classList.toggle('active');
      navLinks.classList.toggle('open');
    });

    // Close menu when a link is clicked
    navLinks.querySelectorAll('a').forEach(function (link) {
      link.addEventListener('click', function () {
        hamburger.classList.remove('active');
        navLinks.classList.remove('open');
      });
    });
  }

  // ---- Sticky nav background ----
  var nav = document.getElementById('nav');
  if (nav) {
    var sentinel = document.createElement('div');
    sentinel.style.position = 'absolute';
    sentinel.style.top = '0';
    sentinel.style.height = '1px';
    sentinel.style.width = '1px';
    document.body.prepend(sentinel);

    var observer = new IntersectionObserver(
      function (entries) {
        nav.classList.toggle('scrolled', !entries[0].isIntersecting);
      },
      { threshold: 0 },
    );
    observer.observe(sentinel);
  }

  // ---- Copy-to-clipboard ----
  document.querySelectorAll('.copy-btn').forEach(function (btn) {
    btn.addEventListener('click', function () {
      var code = btn.getAttribute('data-code');
      if (!code) {
        // Fallback: get text from sibling pre
        var pre = btn.parentElement.querySelector('pre');
        if (pre) code = pre.textContent.replace(/^\$\s*/gm, '').trim();
      }
      if (!code) return;

      navigator.clipboard.writeText(code).then(function () {
        btn.textContent = 'Copied!';
        btn.classList.add('copied');
        setTimeout(function () {
          btn.textContent = 'Copy';
          btn.classList.remove('copied');
        }, 2000);
      });
    });
  });

  // ---- Smooth scroll for anchor links ----
  document.querySelectorAll('a[href^="#"]').forEach(function (link) {
    link.addEventListener('click', function (e) {
      var target = document.querySelector(link.getAttribute('href'));
      if (target) {
        e.preventDefault();
        target.scrollIntoView({ behavior: 'smooth' });
      }
    });
  });

  // ---- Active sidebar link (docs pages) ----
  var sidebarLinks = document.querySelectorAll('.docs-sidebar a[href^="#"]');
  if (sidebarLinks.length > 0) {
    var headings = [];
    sidebarLinks.forEach(function (link) {
      var id = link.getAttribute('href').slice(1);
      var heading = document.getElementById(id);
      if (heading) headings.push({ el: heading, link: link });
    });

    if (headings.length > 0) {
      var headingObserver = new IntersectionObserver(
        function (entries) {
          entries.forEach(function (entry) {
            if (entry.isIntersecting) {
              sidebarLinks.forEach(function (l) {
                l.classList.remove('active');
              });
              var match = headings.find(function (h) {
                return h.el === entry.target;
              });
              if (match) match.link.classList.add('active');
            }
          });
        },
        {
          rootMargin: '-80px 0px -70% 0px',
          threshold: 0,
        },
      );
      headings.forEach(function (h) {
        headingObserver.observe(h.el);
      });
    }
  }

  // ---- Mobile sidebar toggle (docs pages) ----
  var sidebarToggle = document.getElementById('sidebar-toggle');
  var sidebar = document.querySelector('.docs-sidebar');
  if (sidebarToggle && sidebar) {
    sidebarToggle.addEventListener('click', function () {
      sidebar.classList.toggle('open');
      sidebarToggle.classList.toggle('active');
    });

    // Close sidebar when clicking a link on mobile
    sidebar.querySelectorAll('a').forEach(function (link) {
      link.addEventListener('click', function () {
        sidebar.classList.remove('open');
        if (sidebarToggle) sidebarToggle.classList.remove('active');
      });
    });
  }
})();
