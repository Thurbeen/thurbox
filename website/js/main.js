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
  // Matches both in-page anchors (#id) and same-page section links
  // (page.html#id) so the section-enumerating sidebar highlights on scroll.
  var sidebarLinks = document.querySelectorAll('.docs-sidebar a[href*="#"]');
  if (sidebarLinks.length > 0) {
    var headings = [];
    sidebarLinks.forEach(function (link) {
      var id = link.getAttribute('href').split('#')[1];
      var heading = id ? document.getElementById(id) : null;
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

  // ---- Active top-nav link (landing page scroll-spy) ----
  var navSpyLinks = [];
  document.querySelectorAll('.nav-links a[href*="#"]').forEach(function (link) {
    var hash = link.getAttribute('href').split('#')[1];
    var section = hash ? document.getElementById(hash) : null;
    if (section) navSpyLinks.push({ el: section, link: link });
  });

  if (navSpyLinks.length > 0 && 'IntersectionObserver' in window) {
    var navObserver = new IntersectionObserver(
      function (entries) {
        entries.forEach(function (entry) {
          if (entry.isIntersecting) {
            navSpyLinks.forEach(function (s) {
              s.link.classList.remove('active');
              s.link.removeAttribute('aria-current');
            });
            var match = navSpyLinks.find(function (s) {
              return s.el === entry.target;
            });
            if (match) {
              match.link.classList.add('active');
              match.link.setAttribute('aria-current', 'true');
            }
          }
        });
      },
      { rootMargin: '-80px 0px -70% 0px', threshold: 0 },
    );
    navSpyLinks.forEach(function (s) {
      navObserver.observe(s.el);
    });
  }

  // ---- Back to top button ----
  var backToTop = document.getElementById('back-to-top');
  if (backToTop) {
    window.addEventListener(
      'scroll',
      function () {
        backToTop.classList.toggle('visible', window.scrollY > 600);
      },
      { passive: true },
    );
    backToTop.addEventListener('click', function () {
      window.scrollTo({ top: 0, behavior: 'smooth' });
    });
  }

  // ---- Install method tabs (ARIA tabs pattern) ----
  var installTablist = document.querySelector('.install-tablist');
  if (installTablist) {
    var installTabs = Array.prototype.slice.call(installTablist.querySelectorAll('[role="tab"]'));
    var selectInstallTab = function (tab) {
      installTabs.forEach(function (t) {
        var isSelected = t === tab;
        t.setAttribute('aria-selected', isSelected ? 'true' : 'false');
        t.tabIndex = isSelected ? 0 : -1;
        var panel = document.getElementById(t.getAttribute('aria-controls'));
        if (panel) panel.hidden = !isSelected;
      });
    };
    installTabs.forEach(function (tab, i) {
      tab.addEventListener('click', function () {
        selectInstallTab(tab);
      });
      tab.addEventListener('keydown', function (e) {
        var next = null;
        if (e.key === 'ArrowRight' || e.key === 'ArrowDown') next = (i + 1) % installTabs.length;
        else if (e.key === 'ArrowLeft' || e.key === 'ArrowUp')
          next = (i - 1 + installTabs.length) % installTabs.length;
        else if (e.key === 'Home') next = 0;
        else if (e.key === 'End') next = installTabs.length - 1;
        if (next !== null) {
          e.preventDefault();
          selectInstallTab(installTabs[next]);
          installTabs[next].focus();
        }
      });
    });
  }

  // ---- Videos: play only when in view; honor reduced motion ----
  var videos = document.querySelectorAll('video');
  var reduceMotion =
    window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches;

  if (videos.length > 0) {
    if (reduceMotion) {
      // No autoplay: pause everything and let the user opt in via controls.
      videos.forEach(function (v) {
        v.autoplay = false;
        v.removeAttribute('autoplay');
        v.setAttribute('controls', '');
        v.pause();
      });
    } else if ('IntersectionObserver' in window) {
      var videoObserver = new IntersectionObserver(
        function (entries) {
          entries.forEach(function (entry) {
            var v = entry.target;
            if (entry.isIntersecting) {
              var played = v.play();
              if (played && played.catch) played.catch(function () {});
            } else {
              v.pause();
            }
          });
        },
        { threshold: 0.25 },
      );
      videos.forEach(function (v) {
        videoObserver.observe(v);
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
