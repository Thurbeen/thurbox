(function () {
  'use strict';

  // Read live rather than cached: a visitor can flip the OS setting mid-visit,
  // and every caller here is in an event handler, not on the hot path.
  function prefersReducedMotion() {
    return !!window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  }

  // Only true text-entry contexts. Deliberately excludes A/BUTTON/VIDEO: those
  // are merely focusable, and treating them as typing contexts meant a couple
  // of Tab presses (focus lands on a nav link) silently killed press-to-begin.
  // Printable keys do nothing on a focused link anyway. Shared by every
  // document-level printable-key listener below (press-to-begin, god mode).
  function isTypingContext(el) {
    if (!el) return false;
    if (el.isContentEditable) return true;
    var tag = el.tagName;
    return tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT';
  }

  // ---- Mobile nav toggle ----
  var hamburger = document.getElementById('hamburger');
  var navLinks = document.getElementById('nav-links');

  if (hamburger && navLinks) {
    var setNavOpen = function (open) {
      hamburger.classList.toggle('active', open);
      navLinks.classList.toggle('open', open);
      hamburger.setAttribute('aria-expanded', open ? 'true' : 'false');
    };

    var closeNav = function (returnFocus) {
      if (!navLinks.classList.contains('open')) return;
      setNavOpen(false);
      if (returnFocus) hamburger.focus();
    };

    hamburger.addEventListener('click', function () {
      setNavOpen(!navLinks.classList.contains('open'));
    });

    // Close menu when a link is clicked
    navLinks.querySelectorAll('a').forEach(function (link) {
      link.addEventListener('click', function () {
        closeNav(false);
      });
    });

    // Escape closes and hands focus back to the toggle that opened it.
    document.addEventListener('keydown', function (e) {
      if (e.key === 'Escape') closeNav(true);
    });

    // A tap outside the open dropdown dismisses it.
    document.addEventListener('click', function (e) {
      if (!navLinks.classList.contains('open')) return;
      if (navLinks.contains(e.target) || hamburger.contains(e.target)) return;
      closeNav(false);
    });
  }

  // ---- Title-screen "press any key to begin" ----
  // The prompt is a real <button> (click / Enter / Space) plus a document
  // keydown so pressing *any* printable key really does start the tour.
  // It stays armed until it fires: an earlier version disarmed on the first
  // scroll event, which killed it for every visitor who nudged the page before
  // reaching for the keyboard (a 1px wheel tick was enough). The guards below —
  // no chords, no navigation keys, not while focus is in a field — are what
  // keep it from hijacking the keyboard, not the arming window.
  var pressStart = document.getElementById('press-start');
  if (pressStart) {
    var pressTarget = document.getElementById('demo') || document.getElementById('installation');
    var pressArmed = true;

    // Keys the browser or the user owns. Enter/Space are here because the
    // <button> already handles them natively via click — letting the document
    // handler fire too would double-scroll and hijack Enter on every link.
    var pressReserved = {
      Tab: 1,
      Escape: 1,
      Enter: 1,
      ' ': 1,
      Shift: 1,
      Control: 1,
      Alt: 1,
      Meta: 1,
      CapsLock: 1,
      ContextMenu: 1,
      PageUp: 1,
      PageDown: 1,
      Home: 1,
      End: 1,
      ArrowUp: 1,
      ArrowDown: 1,
      ArrowLeft: 1,
      ArrowRight: 1,
    };

    var onAnyKey = function (e) {
      if (!pressArmed) return;
      if (e.metaKey || e.ctrlKey || e.altKey) return; // Ctrl+F, Cmd+R, …
      // shiftKey is intentionally not checked: a capital letter is a real key.
      if (e.key && (pressReserved[e.key] || /^F\d{1,2}$/.test(e.key))) return;
      // Only printable single characters — drops Insert, Dead, media keys, …
      if (!e.key || e.key.length !== 1) return;
      if (isTypingContext(e.target) || isTypingContext(document.activeElement)) return;
      firePressStart();
    };

    var disarmPressStart = function () {
      if (!pressArmed) return;
      pressArmed = false;
      pressStart.classList.add('is-spent');
      document.removeEventListener('keydown', onAnyKey, true);
    };

    var firePressStart = function () {
      if (pressTarget) {
        pressTarget.scrollIntoView({
          behavior: prefersReducedMotion() ? 'auto' : 'smooth',
          block: 'start',
        });
      }
      disarmPressStart();
    };

    // The button itself always works, armed or not.
    pressStart.addEventListener('click', firePressStart);

    // Capture phase so no element can swallow it first; the guards make that safe.
    document.addEventListener('keydown', onAnyKey, true);
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
  // The button lives in the code-block header; the code is in the sibling
  // .code-block-body. Read the rendered <pre> text content (the Prism token
  // spans contribute only their text, so this round-trips to the source).
  document.querySelectorAll('.copy-btn').forEach(function (btn) {
    btn.addEventListener('click', function () {
      var code = btn.getAttribute('data-code');
      if (!code) {
        var block = btn.closest('.code-block') || btn.parentElement;
        var pre = block ? block.querySelector('pre') : null;
        if (pre) code = pre.textContent.replace(/^\$\s*/gm, '').trim();
      }
      if (!code) return;

      var flash = function (label, cls) {
        btn.textContent = label;
        btn.classList.add(cls);
        setTimeout(function () {
          btn.textContent = 'Copy';
          btn.classList.remove(cls);
        }, 2000);
      };

      // The Clipboard API needs a secure context, so it is absent over plain
      // http and rejects when permission is denied — report either instead of
      // failing silently with an unhandled rejection.
      if (!navigator.clipboard || !navigator.clipboard.writeText) {
        flash('Copy failed', 'copy-failed');
        return;
      }

      navigator.clipboard.writeText(code).then(
        function () {
          flash('Copied!', 'copied');
        },
        function () {
          flash('Copy failed', 'copy-failed');
        },
      );
    });
  });

  // Anchor smooth-scrolling is handled natively by `html { scroll-behavior:
  // smooth }` in base.css, which also honors prefers-reduced-motion and keeps
  // location.hash (and therefore the back button and deep links) intact — so
  // there is deliberately no JS click handler for a[href^="#"] here.

  // ---- Active sidebar link (docs pages) ----
  // Matches both in-page anchors (#id) and same-page section links
  // (page.html#id) so the section-enumerating sidebar highlights on scroll.
  // Also covers the build-time "on this page" rail, which shares the .active
  // convention with the sidebar.
  var sidebarLinks = document.querySelectorAll(
    '.docs-sidebar a[href*="#"], .docs-toc a[href*="#"]',
  );
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
              // A heading can be linked from both the sidebar and the TOC rail,
              // so activate every link pointing at it — not just the first.
              headings.forEach(function (h) {
                if (h.el === entry.target) h.link.classList.add('active');
              });
            }
          });
        },
        {
          rootMargin: '-80px 0px -70% 0px',
          threshold: 0,
        },
      );
      // Observe each heading once even when several links point at it.
      var observed = [];
      headings.forEach(function (h) {
        if (observed.indexOf(h.el) !== -1) return;
        observed.push(h.el);
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
          // Arrowing past the edge of the scroll rail must bring the tab into
          // view. No `behavior` here so it inherits scroll-behavior, which is
          // already reduced-motion aware.
          if (installTabs[next].scrollIntoView) {
            installTabs[next].scrollIntoView({ block: 'nearest', inline: 'nearest' });
          }
        }
      });
    });
  }

  // ---- Videos: lazy poster + play only when in view; honor reduced motion ----
  // A <video poster> has no loading="lazy" equivalent — the browser always
  // fetches it eagerly. So every below-the-fold video ships its poster as
  // data-poster (see index.html) and we promote it to a real poster shortly
  // before the video reaches the viewport. Only the hero keeps an eager
  // poster=, because that one *is* the above-the-fold content.
  var videos = document.querySelectorAll('video');
  var reduceMotion = prefersReducedMotion();

  var loadPoster = function (v) {
    var deferred = v.getAttribute('data-poster');
    if (!deferred) return;
    v.setAttribute('poster', deferred);
    v.removeAttribute('data-poster');
  };

  if (videos.length > 0) {
    if (reduceMotion) {
      // No autoplay: pause everything and let the user opt in via controls.
      // The poster is the only thing they'll see until they do, so load it.
      videos.forEach(function (v) {
        v.autoplay = false;
        v.removeAttribute('autoplay');
        v.setAttribute('controls', '');
        loadPoster(v);
        v.pause();
      });
    } else if ('IntersectionObserver' in window) {
      // Poster first, on a generous margin so it has arrived by the time the
      // video is actually on screen.
      var posterObserver = new IntersectionObserver(
        function (entries) {
          entries.forEach(function (entry) {
            if (!entry.isIntersecting) return;
            loadPoster(entry.target);
            posterObserver.unobserve(entry.target);
          });
        },
        { rootMargin: '400px 0px' },
      );

      var videoObserver = new IntersectionObserver(
        function (entries) {
          entries.forEach(function (entry) {
            var v = entry.target;
            if (entry.isIntersecting) {
              loadPoster(v); // covers a jump-scroll that skipped the margin
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
        if (v.getAttribute('data-poster')) posterObserver.observe(v);
        videoObserver.observe(v);
      });
    } else {
      // No IntersectionObserver: autoplay handles playback, so just make sure
      // no poster is left stranded in data-poster.
      videos.forEach(loadPoster);
    }
  }

  // ---- CRT sweep: run it only on panels that are actually on screen ----
  // The sweep is paused in CSS by default. There is no CSS way to key
  // animation-play-state off viewport position (animation-timeline: view() ties
  // the sweep to scroll position instead of a clock, and lacks Safari support),
  // so an observer adds .is-live. Pausing rather than stopping preserves the
  // animation's phase across re-entry.
  var sweepPanels = document.querySelectorAll('.terminal-frame, .tui-demo, .code-block');
  if (sweepPanels.length > 0 && 'IntersectionObserver' in window) {
    var sweepObserver = new IntersectionObserver(
      function (entries) {
        entries.forEach(function (entry) {
          entry.target.classList.toggle('is-live', entry.isIntersecting);
        });
      },
      { threshold: 0 },
    );
    sweepPanels.forEach(function (p) {
      sweepObserver.observe(p);
    });
  }

  // ---- Mobile sidebar toggle (docs pages) ----
  var sidebarToggle = document.getElementById('sidebar-toggle');
  var sidebar = document.querySelector('.docs-sidebar');
  var backdrop = document.getElementById('docs-sidebar-backdrop');
  function closeSidebar() {
    if (!sidebar) return;
    sidebar.classList.remove('open');
    if (sidebarToggle) {
      sidebarToggle.classList.remove('active');
      sidebarToggle.setAttribute('aria-expanded', 'false');
    }
    if (backdrop) backdrop.classList.remove('open');
  }

  if (sidebarToggle && sidebar) {
    sidebarToggle.addEventListener('click', function () {
      var open = sidebar.classList.toggle('open');
      sidebarToggle.classList.toggle('active', open);
      sidebarToggle.setAttribute('aria-expanded', open ? 'true' : 'false');
      if (backdrop) backdrop.classList.toggle('open', open);
      // Move focus into the drawer so keyboard and screen-reader users land
      // where the visual focus went.
      if (open) {
        var first = sidebar.querySelector('a');
        if (first) first.focus();
      }
    });

    if (backdrop) {
      backdrop.addEventListener('click', closeSidebar);
    }

    // Close sidebar when a link is clicked or the backdrop is tapped
    sidebar.querySelectorAll('a').forEach(function (link) {
      link.addEventListener('click', closeSidebar);
    });

    // Escape closes the drawer and returns focus to the toggle that opened it.
    document.addEventListener('keydown', function (e) {
      if (e.key !== 'Escape') return;
      if (!sidebar.classList.contains('open')) return;
      closeSidebar();
      sidebarToggle.focus();
    });
  }

  // ---- God mode: typing the 1993 Doom cheat plays a clip of Doom in a pane ----
  // The clue is the footer's .footer-secret line plus the HUD bar's "God Mode /
  // 5 keys" segment; the code itself is never written on the page.
  //
  // The overlay and its <video> are built on first trigger, so the several-MB
  // clip and its poster cost nothing for the overwhelming majority of visitors
  // who never type it. Regenerate the media with scripts/demo/record-doom.sh.
  //
  // Note this shares the document with press-to-begin, which fires on any
  // printable key: from the top of the landing page the leading "i" scrolls to
  // the demo section before the overlay opens. Left deliberately uncoupled —
  // suppressing that would mean breaking "press any key" for the letter i.
  var CHEAT_CODE = 'iddqd';
  var assetsDir = document.body && document.body.getAttribute('data-assets');

  if (assetsDir) {
    var cheatBuffer = '';
    var doomOverlay = null;
    var doomVideo = null;
    var doomClose = null;
    var doomReturnFocus = null;

    var closeDoom = function () {
      if (!doomOverlay || doomOverlay.hidden) return;
      doomOverlay.hidden = true;
      document.documentElement.classList.remove('doom-open');
      // Drop the source so an in-flight download stops and the decoder is
      // released; reopening re-attaches it.
      doomVideo.pause();
      doomVideo.removeAttribute('src');
      doomVideo.load();
      if (doomReturnFocus && doomReturnFocus.focus) doomReturnFocus.focus();
      doomReturnFocus = null;
    };

    // The overlay is a modal dialog, so Tab must not walk out of it into the
    // page behind. Only the close button and the video's controls are focusable.
    var trapDoomTab = function (e) {
      if (e.key !== 'Tab') return;
      var focusable = doomOverlay.querySelectorAll('button, video');
      if (focusable.length === 0) return;
      var first = focusable[0];
      var last = focusable[focusable.length - 1];
      if (e.shiftKey && document.activeElement === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    };

    var buildDoom = function () {
      var overlay = document.createElement('div');
      overlay.className = 'doom-overlay';
      overlay.hidden = true;
      overlay.setAttribute('role', 'dialog');
      overlay.setAttribute('aria-modal', 'true');
      overlay.setAttribute('aria-label', 'God mode: Doom running inside a thurbox pane');

      var frame = document.createElement('div');
      frame.className = 'doom-overlay__frame';

      var bar = document.createElement('div');
      bar.className = 'doom-overlay__bar';

      var label = document.createElement('span');
      label.className = 'doom-overlay__label';
      label.textContent = 'Degreelessness Mode On';

      doomClose = document.createElement('button');
      doomClose.type = 'button';
      doomClose.className = 'doom-overlay__close';
      doomClose.setAttribute('aria-label', 'Close');
      doomClose.textContent = 'Esc';

      bar.appendChild(label);
      bar.appendChild(doomClose);

      doomVideo = document.createElement('video');
      doomVideo.className = 'doom-overlay__video';
      // Intrinsic size reserves the aspect ratio so opening the overlay does not
      // reflow once the first frame arrives. Matches what record-doom.sh emits
      // (a 160x44 terminal rasterised at font-size 20), not the 1920x1080 the
      // VHS demos use — keep the two in sync.
      doomVideo.width = 1944;
      doomVideo.height = 1260;
      doomVideo.loop = true;
      doomVideo.muted = true;
      doomVideo.setAttribute('playsinline', '');
      doomVideo.setAttribute('controls', '');
      doomVideo.setAttribute('poster', assetsDir + '/doom-easter-egg-poster.webp');

      var caption = document.createElement('p');
      caption.className = 'doom-overlay__caption';
      caption.textContent =
        'Doom inside a thurbox pane — a pi session running the pi-doom extension.';

      frame.appendChild(bar);
      frame.appendChild(doomVideo);
      frame.appendChild(caption);
      overlay.appendChild(frame);

      doomClose.addEventListener('click', closeDoom);
      // A click on the backdrop dismisses; one inside the frame must not.
      overlay.addEventListener('click', function (e) {
        if (e.target === overlay) closeDoom();
      });
      overlay.addEventListener('keydown', trapDoomTab);

      document.body.appendChild(overlay);
      return overlay;
    };

    var openDoom = function () {
      if (!doomOverlay) doomOverlay = buildDoom();
      if (!doomOverlay.hidden) return;
      doomReturnFocus = document.activeElement;
      doomVideo.setAttribute('src', assetsDir + '/doom-easter-egg.mp4');
      doomOverlay.hidden = false;
      document.documentElement.classList.add('doom-open');
      // Reduced motion: show the poster frame and let the controls opt in,
      // rather than starting a 21-second loop unasked.
      if (!prefersReducedMotion()) {
        var played = doomVideo.play();
        if (played && played.catch) played.catch(function () {});
      }
      doomClose.focus();
    };

    // Capture phase, like press-to-begin, so nothing on the page can swallow the
    // sequence. A rolling buffer of the last few printable keys means a typo just
    // shifts the window instead of needing an explicit reset.
    document.addEventListener(
      'keydown',
      function (e) {
        if (e.key === 'Escape') {
          closeDoom();
          return;
        }
        if (e.metaKey || e.ctrlKey || e.altKey) return; // Ctrl+D is not a cheat
        if (!e.key || e.key.length !== 1) return;
        if (isTypingContext(e.target) || isTypingContext(document.activeElement)) return;
        cheatBuffer = (cheatBuffer + e.key.toLowerCase()).slice(-CHEAT_CODE.length);
        if (cheatBuffer === CHEAT_CODE) {
          cheatBuffer = '';
          openDoom();
        }
      },
      true,
    );
  }

  /* ── The panel lab ────────────────────────────────────────────────────────
   *
   * Demonstrates the one claim the page rests on: a pane is a file, and the
   * arrangement closes up around whichever files are loaded. So the rects are
   * COMPUTED from the enabled set rather than picked from a table of finished
   * layouts -- a table would have to enumerate every combination, and would let
   * a hole appear where a turned-off pane used to be, which is exactly the
   * behaviour being claimed not to happen.
   */
  (function panelLab() {
    var screen = document.getElementById('ui-lab-screen');
    if (!screen) return;

    var COLUMNS = ['sessions', 'agent', 'yours'];
    var WEIGHT = { sessions: 1, agent: 2.4, yours: 1 };
    var FOOTER = 18;

    var panes = {};
    Array.prototype.forEach.call(screen.querySelectorAll('.ui-pane'), function (el) {
      panes[el.getAttribute('data-pane')] = el;
    });

    var on = { sessions: true, agent: true, search: true, yours: false };
    var mode = 'columns';

    function place(el, x, y, w, h) {
      el.style.left = x + '%';
      el.style.top = y + '%';
      el.style.width = w + '%';
      el.style.height = h + '%';
    }

    function render() {
      var live = COLUMNS.filter(function (id) {
        return on[id];
      });
      var footer = on.search ? FOOTER : 0;
      var bodyH = 100 - footer;

      Object.keys(panes).forEach(function (id) {
        panes[id].setAttribute('data-off', on[id] ? 'false' : 'true');
      });

      if (on.search) place(panes.search, 0, bodyH, 100, footer);

      // Focus gives the whole body to the agent when it is loaded; the others
      // keep their rect so they fade rather than jump on the way back.
      if (mode === 'focus' && on.agent) {
        place(panes.agent, 0, 0, 100, bodyH);
        live
          .filter(function (id) {
            return id !== 'agent';
          })
          .forEach(function (id) {
            panes[id].setAttribute('data-off', 'true');
          });
        return;
      }

      var total = live.reduce(function (sum, id) {
        return sum + WEIGHT[id];
      }, 0);
      if (!total) return;

      var offset = 0;
      live.forEach(function (id) {
        var share = (WEIGHT[id] / total) * 100;
        if (mode === 'rows') {
          place(panes[id], 0, (offset / 100) * bodyH, 100, (share / 100) * bodyH);
        } else {
          place(panes[id], offset, 0, share, bodyH);
        }
        offset += share;
      });
    }

    var hint = document.getElementById('ui-lab-hint');
    var timer = null;
    var taken = false;
    var done = false;

    function takeOver() {
      if (taken) return;
      taken = true;
      if (timer) window.clearInterval(timer);
      timer = null;
      if (hint) hint.textContent = 'Yours now. Every pane above is one file.';
    }

    function setMode(next) {
      mode = next;
      Array.prototype.forEach.call(document.querySelectorAll('.ui-lab__btn'), function (btn) {
        btn.setAttribute(
          'aria-pressed',
          btn.getAttribute('data-arrangement') === next ? 'true' : 'false',
        );
      });
      render();
    }

    function setPane(id, enabled) {
      on[id] = enabled;
      var chip = document.querySelector('.ui-lab__chip[data-toggle="' + id + '"]');
      if (chip) chip.setAttribute('aria-pressed', enabled ? 'true' : 'false');
      render();
    }

    Array.prototype.forEach.call(document.querySelectorAll('.ui-lab__btn'), function (btn) {
      btn.addEventListener('click', function () {
        takeOver();
        setMode(btn.getAttribute('data-arrangement'));
      });
    });

    Array.prototype.forEach.call(document.querySelectorAll('.ui-lab__chip'), function (chip) {
      chip.addEventListener('click', function () {
        takeOver();
        var id = chip.getAttribute('data-toggle');
        setPane(id, chip.getAttribute('aria-pressed') !== 'true');
      });
    });

    render();

    // Autoplay so the movement is seen without being asked for. Skipped
    // entirely when motion is not wanted, and stopped for good on first input.
    var still = window.matchMedia('(prefers-reduced-motion: reduce)');
    if (still.matches) {
      if (hint) hint.textContent = 'Click a layout or a file to rearrange it.';
      return;
    }

    var SCRIPT = [
      function () {
        setPane('yours', true);
      },
      function () {
        setMode('rows');
      },
      function () {
        setPane('search', false);
      },
      function () {
        setMode('columns');
      },
      function () {
        setPane('sessions', false);
      },
      function () {
        setMode('focus');
      },
      function () {
        setMode('columns');
        setPane('sessions', true);
        setPane('search', true);
        setPane('yours', false);
      },
    ];
    var step = 0;

    function play() {
      if (taken) return;
      if (step >= SCRIPT.length) {
        // One pass, then stop for good. These transitions animate `left`/`top`/
        // `width`/`height` -- layout properties, so each frame is a reflow on the
        // main thread rather than compositor work. Looping that forever taxes the
        // whole page (and reads as restless); one pass shows the point and then
        // the section goes quiet until someone touches it.
        stopAutoplay();
        if (hint) hint.textContent = 'Your turn — click a layout or a file.';
        return;
      }
      SCRIPT[step]();
      step += 1;
    }

    function stopAutoplay() {
      if (timer) window.clearInterval(timer);
      timer = null;
      done = true;
    }

    if (!('IntersectionObserver' in window)) return;
    var watcher = new IntersectionObserver(
      function (entries) {
        entries.forEach(function (entry) {
          if (entry.isIntersecting && !taken && !done && !timer) {
            timer = window.setInterval(play, 2300);
          } else if (!entry.isIntersecting && timer) {
            // Off screen is not "taken over": stop the work, keep the offer.
            window.clearInterval(timer);
            timer = null;
          }
        });
      },
      { threshold: 0.35 },
    );
    watcher.observe(screen);
  })();
})();
