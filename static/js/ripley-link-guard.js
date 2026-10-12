// Ripley Link Guard
// Replaces any <a href="..."> element with a styled <span> that shows the
// original link text + a "was link" badge. Catches both already-present
// links (on DOMContentLoaded) and links added later (MutationObserver).
// Also intercepts clicks so clicks on links just don't navigate anywhere.
//
// This used to be an inline <script> in every static HTML page, but the
// CSP rollout (script-src 'self') blocked inline scripts, so we moved it
// to an external file that every page loads first.
(function() {
  function markLinks(root) {
    (root || document).querySelectorAll('a[href]').forEach(function(a) {
      var href = a.getAttribute('href') || '';
      var text = (a.textContent || href || 'link').trim();
      var span = document.createElement('span');
      span.className = (a.className ? a.className + ' ' : '') + 'ripley-was-link was-link';
      span.textContent = text;
      span.title = href ? ('Original link removed: ' + href) : 'Original link removed';
      span.dataset.originalHref = href;
      a.replaceWith(span);
    });
  }
  function run() { markLinks(document); }
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', run);
  else run();
  new MutationObserver(function(mutations) {
    mutations.forEach(function(m) {
      m.addedNodes.forEach(function(n) { if (n && n.nodeType === 1) markLinks(n); });
    });
  }).observe(document.documentElement, { childList: true, subtree: true });
  document.addEventListener('click', function(e) {
    var a = e.target && e.target.closest && e.target.closest('a[href]');
    if (a) { e.preventDefault(); e.stopPropagation(); }
  }, true);
})();
