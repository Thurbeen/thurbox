(function () {
  'use strict';
  var form = document.getElementById('plugin-filters');
  if (!form) return;
  var grid = document.getElementById('plugin-grid');
  var cards = Array.prototype.slice.call(document.querySelectorAll('[data-plugin]'));
  var search = document.getElementById('plugin-search');
  var sort = document.getElementById('plugin-sort');
  var badge = document.getElementById('plugin-badge');
  var count = document.getElementById('plugin-count');
  var empty = document.getElementById('plugin-empty');
  var noun = count.getAttribute('data-noun') || 'items';

  function attr(card, name) {
    return card.getAttribute('data-' + name) || '';
  }

  function newestFirst(name) {
    return function (a, b) {
      return attr(b, name).localeCompare(attr(a, name));
    };
  }

  var orders = {
    popular: function () {
      return 0;
    },
    updated: newestFirst('pushed'),
    newest: newestFirst('created'),
    name: function (a, b) {
      return attr(a, 'name').localeCompare(attr(b, 'name'));
    },
  };

  function arrange() {
    var order = orders[sort.value] || orders.popular;
    cards
      .map(function (card, index) {
        return { card: card, index: index };
      })
      .sort(function (a, b) {
        return order(a.card, b.card) || a.index - b.index;
      })
      .forEach(function (entry) {
        grid.appendChild(entry.card);
      });
  }

  function filter() {
    var query = search.value.trim().toLowerCase();
    var visible = 0;
    cards.forEach(function (card) {
      var matches =
        (!query || card.textContent.toLowerCase().indexOf(query) !== -1) &&
        (!badge.value || attr(card, 'badge') === badge.value);
      card.hidden = !matches;
      if (matches) visible += 1;
    });
    count.textContent = visible + ' of ' + cards.length + ' ' + noun;
    empty.hidden = visible !== 0;
  }

  function update() {
    arrange();
    filter();
  }

  form.hidden = false;
  form.addEventListener('submit', function (event) {
    event.preventDefault();
  });
  form.addEventListener('input', update);
  form.addEventListener('change', update);
  form.addEventListener('reset', function () {
    setTimeout(update, 0);
  });
  update();
})();
