(function () {
  'use strict';
  var form = document.getElementById('plugin-filters');
  if (!form) return;
  var cards = Array.prototype.slice.call(document.querySelectorAll('[data-plugin]'));
  var search = document.getElementById('plugin-search');
  var kind = document.getElementById('plugin-kind');
  var badge = document.getElementById('plugin-badge');
  var count = document.getElementById('plugin-count');
  var empty = document.getElementById('plugin-empty');

  function filter() {
    var query = search.value.trim().toLowerCase();
    var visible = 0;
    cards.forEach(function (card) {
      var matches =
        (!query || card.textContent.toLowerCase().indexOf(query) !== -1) &&
        (!kind.value || card.getAttribute('data-kind') === kind.value) &&
        (!badge.value || card.getAttribute('data-badge') === badge.value);
      card.hidden = !matches;
      if (matches) visible += 1;
    });
    count.textContent = visible + ' of ' + cards.length + ' plugins';
    empty.hidden = visible !== 0;
  }

  form.hidden = false;
  form.addEventListener('submit', function (event) {
    event.preventDefault();
  });
  form.addEventListener('input', filter);
  form.addEventListener('change', filter);
  form.addEventListener('reset', function () {
    setTimeout(filter, 0);
  });
  filter();
})();
