(function () {
  var requested = new URLSearchParams(location.search).get('fit') || 'cover';
  var allowed = ['cover', 'contain', 'fill', 'none', 'scale-down'];
  document.documentElement.dataset.videoFit = allowed.indexOf(requested) >= 0 ? requested : 'cover';
})();
