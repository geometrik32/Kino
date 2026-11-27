(function () {
  if (window.__kinoMotionInit) return;
  window.__kinoMotionInit = true;

  var RIPPLE_SEL = [
    '.btn',
    '.play-cta-card',
    '.play-split-toggle',
    '.rel-cta-btn',
    '.status-dd-btn',
    '.chip',
    '.pill-btn',
    '.seg button',
    '.tab-pill',
    '.top-icon-btn',
    '.nav-pills button',
    '.glass-circle',
    '.poster .img',
    '.cw-thumb',
    '.ep-card',
    '.rel-card',
    '.pre-btn',
    '.round-act',
    '.sheet-close',
    '.st-item',
    '.cw-pop-item',
    '.p-glass-btn',
    '.p-play-btn',
    '.p-big',
    '.p-opt',
    '.user-card',
    '.cal-cell.active',
    '.cal-row',
    '.hist-row',
    '.dl-row',
    '.acc-card',
    '.dl-pill'
  ].join(',');

  var isAndroid = function () {
    return document.documentElement && document.documentElement.classList.contains('kino-android');
  };

  var TOUCH_SKIP_RIPPLE_SEL = '.poster .img, .cw-thumb, .ep-card, .rel-card, .cal-row, .hist-row, .dl-row, .user-card';

  var reducedMotion = function () {
    return window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  };

  // Track last pointer coordinates for radial theme View Transition.
  document.addEventListener('pointerdown', function (e) {
    window.__kinoLastPt = { x: e.clientX, y: e.clientY };

    if (e.button > 0 || reducedMotion()) return;
    var target = e.target && e.target.closest ? e.target.closest(RIPPLE_SEL) : null;
    if (!target || target.disabled || target.classList.contains('ro')) return;
    if ((e.pointerType === 'touch' || isAndroid()) && target.matches && target.matches(TOUCH_SKIP_RIPPLE_SEL)) return;

    var rect = target.getBoundingClientRect();
    if (!rect.width || !rect.height) return;

    var size = Math.max(rect.width, rect.height) * 2.25;
    var wave = document.createElement('span');
    wave.className = 'm3-ripple';
    wave.style.width = size + 'px';
    wave.style.height = size + 'px';
    wave.style.left = (e.clientX - rect.left - size / 2) + 'px';
    wave.style.top = (e.clientY - rect.top - size / 2) + 'px';
    target.appendChild(wave);

    var cleaned = false;
    var fadeOut = function () {
      if (cleaned) return;
      cleaned = true;
      wave.classList.add('out');
      setTimeout(function () {
        if (wave.parentNode) wave.parentNode.removeChild(wave);
      }, 480);
      window.removeEventListener('pointerup', fadeOut);
      window.removeEventListener('pointercancel', fadeOut);
    };
    window.addEventListener('pointerup', fadeOut, { passive: true });
    window.addEventListener('pointercancel', fadeOut, { passive: true });
    setTimeout(fadeOut, 1200);
  }, { passive: true });

  // Mobile bottom sheet drag-to-dismiss on ReleasePicker.
  var drag = null;
  document.addEventListener('pointerdown', function (e) {
    if (e.button > 0) return;
    if (!window.matchMedia || !window.matchMedia('(max-width: 720px)').matches) return;
    var handle = e.target && e.target.closest ? e.target.closest('.sheet-handle-wrap, .sheet-head') : null;
    if (!handle) return;
    if (e.target.closest('button, a, input')) return;
    var sheet = handle.closest('.sheet');
    var bg = sheet ? sheet.closest('.sheet-bg') : null;
    if (!sheet || !bg) return;
    if (handle.classList.contains('sheet-head') && sheet.scrollTop > 2) return;
    drag = {
      sheet: sheet,
      bg: bg,
      y0: e.clientY,
      dy: 0,
      moved: false,
      pid: e.pointerId
    };
    try { handle.setPointerCapture(e.pointerId); } catch (_) {}
  });

  document.addEventListener('pointermove', function (e) {
    if (!drag || e.pointerId !== drag.pid) return;
    var dy = e.clientY - drag.y0;
    if (!drag.moved) {
      if (Math.abs(dy) < 6) return;
      if (dy < 0 && drag.sheet.scrollTop > 0) { drag = null; return; }
      drag.moved = true;
      drag.sheet.classList.add('m3-dragging');
    }
    drag.dy = dy;
    var offset = dy > 0 ? dy : dy * 0.18;
    drag.sheet.style.transform = 'translateY(' + offset + 'px)';
    var ratio = Math.max(0, 1 - Math.max(dy, 0) / 420);
    drag.bg.style.backgroundColor = 'rgba(6, 11, 17, ' + (0.78 * ratio).toFixed(3) + ')';
  }, { passive: true });

  var endDrag = function (e) {
    if (!drag || (e && e.pointerId !== drag.pid)) return;
    var d = drag;
    drag = null;
    d.sheet.classList.remove('m3-dragging');
    if (!d.moved) return;
    if (d.dy > 96) {
      d.sheet.style.transition = 'transform .26s cubic-bezier(.3, 0, .8, .15)';
      d.sheet.style.transform = 'translateY(105%)';
      d.bg.style.transition = 'opacity .24s ease';
      d.bg.style.opacity = '0';
      setTimeout(function () {
        var closeBtn = d.sheet.querySelector('.sheet-close');
        if (closeBtn) closeBtn.click();
        else d.bg.click();
      }, 220);
    } else {
      d.sheet.style.transform = '';
      d.bg.style.backgroundColor = '';
    }
  };
  document.addEventListener('pointerup', endDrag, { passive: true });
  document.addEventListener('pointercancel', endDrag, { passive: true });

  // Soft fade-in for newly loaded poster / episode / cast images.
  var FADE_IMG_SEL = '.poster .img img, .ep-still img, .title-poster img, .cast-card .avatar img, .dl-poster img';
  var markImg = function (img) {
    if (!img || img.tagName !== 'IMG' || !img.matches || !img.matches(FADE_IMG_SEL)) return;
    if (img.complete && img.naturalWidth > 0) {
      img.removeAttribute('data-m3-img-wait');
      return;
    }
    img.setAttribute('data-m3-img-wait', '1');
    var done = function () { img.removeAttribute('data-m3-img-wait'); };
    img.addEventListener('load', done, { once: true });
    img.addEventListener('error', done, { once: true });
  };
  var scanImgs = function (root) {
    if (!root || !root.querySelectorAll) return;
    if (root.tagName === 'IMG') markImg(root);
    var list = root.querySelectorAll(FADE_IMG_SEL);
    for (var i = 0; i < list.length; i++) markImg(list[i]);
  };
  if (!isAndroid() && typeof MutationObserver !== 'undefined') {
    var mo = new MutationObserver(function (muts) {
      for (var i = 0; i < muts.length; i++) {
        var added = muts[i].addedNodes;
        for (var j = 0; j < added.length; j++) scanImgs(added[j]);
      }
    });
    if (document.body) mo.observe(document.body, { childList: true, subtree: true });
    else document.addEventListener('DOMContentLoaded', function () {
      mo.observe(document.body, { childList: true, subtree: true });
    }, { once: true });
  }

  // Smooth theme switch with radial View Transition wave where supported.
  window.__kinoApplyTheme = function (mode, animate) {
    var root = document.documentElement;
    var apply = function () {
      if (mode === 'dark' || mode === 'light') root.dataset.theme = mode;
      else delete root.dataset.theme;
    };
    if (!animate || reducedMotion() || typeof document.startViewTransition !== 'function') {
      apply();
      return;
    }
    var pt = window.__kinoLastPt || { x: window.innerWidth / 2, y: window.innerHeight / 2 };
    var x = pt.x, y = pt.y;
    var R = Math.hypot(Math.max(x, window.innerWidth - x), Math.max(y, window.innerHeight - y));
    root.classList.add('m3-theme-vt');
    try {
      var vt = document.startViewTransition(apply);
      vt.ready.then(function () {
        root.animate(
          { clipPath: ['circle(0px at ' + x + 'px ' + y + 'px)', 'circle(' + R + 'px at ' + x + 'px ' + y + 'px)'] },
          { duration: 620, easing: 'cubic-bezier(.2, 0, 0, 1)', pseudoElement: '::view-transition-new(root)' }
        );
      }).catch(function () {});
      vt.finished.finally(function () { root.classList.remove('m3-theme-vt'); });
    } catch (_) {
      root.classList.remove('m3-theme-vt');
      apply();
    }
  };
})();
