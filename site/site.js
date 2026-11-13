// The player in the hero: the wheel's buttons, a scroll over it and the arrow keys turn its screen.
(function () {
  const screens = [
    ['home-playing', 'Home', "Reborn's Home screen"],
    ['now-playing', 'Now Playing', 'Now Playing with the cover and the progress wave'],
    ['albums', 'Cover Flow', 'Albums as Cover Flow'],
    ['album', 'An album', 'An album page with its songs'],
    ['search-results', 'Search', 'Search results'],
    ['radio-playing', 'Internet radio', 'A radio station playing'],
    ['now-playing-book', 'An audiobook', 'An audiobook playing'],
    ['quick-menu', 'Quick menu', 'The quick menu'],
    ['settings', 'Settings', 'Settings'],
  ];
  const image = document.getElementById('screen');
  const caption = document.getElementById('caption');
  const player = document.getElementById('player');
  let at = 0;
  let touched = false;

  function swap(target, source, alt) {
    const next = new Image();
    next.onload = function () {
      target.classList.add('out');
      setTimeout(function () {
        target.src = source;
        target.alt = alt;
        target.classList.remove('out');
      }, 150);
    };
    next.src = source;
  }
  function show(step) {
    at = (at + step + screens.length) % screens.length;
    swap(image, 'img/' + screens[at][0] + '.png', screens[at][2]);
    caption.textContent = screens[at][1];
  }
  function turn(step) { touched = true; show(step); }

  player.querySelector('.w-prev').addEventListener('click', function () { turn(-1); });
  player.querySelector('.w-next').addEventListener('click', function () { turn(1); });
  player.querySelector('.w-ok').addEventListener('click', function () { turn(1); });
  player.addEventListener('keydown', function (event) {
    if (event.key === 'ArrowLeft') turn(-1);
    if (event.key === 'ArrowRight') turn(1);
  });
  let last = 0;
  player.querySelector('.wheel').addEventListener('wheel', function (event) {
    event.preventDefault();
    const now = Date.now();
    if (now - last < 220) return;
    last = now;
    turn(event.deltaY > 0 ? 1 : -1);
  }, { passive: false });

  const still = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  if (!still) setInterval(function () { if (!touched && !document.hidden) show(1); }, 3800);

  // Skins: one picture, a chip for each.
  const skins = [
    ['classic', 'Classic'], ['nano-glass', 'Nano Glass'], ['walkman', 'Walkman'], ['terminal', 'Terminal'],
    ['winamp', 'Winamp'], ['paper', 'Paper'], ['big-type', 'Big Type'], ['cassette', 'Cassette'],
    ['neon', 'Neon Night'], ['zune', 'Zune'], ['bunny-bow', 'Bunny Bow'], ['luna-xp', 'Luna XP'],
    ['mini-blue', 'Mini Blue'], ['mini-gold', 'Mini Gold'], ['mini-green', 'Mini Green'],
    ['mini-pink', 'Mini Pink'], ['mini-silver', 'Mini Silver'],
  ];
  const skin = document.getElementById('skin');
  const chips = document.getElementById('chips');
  skins.forEach(function (entry, index) {
    const chip = document.createElement('button');
    chip.type = 'button';
    chip.textContent = entry[1];
    chip.setAttribute('aria-pressed', index === 0 ? 'true' : 'false');
    chip.addEventListener('click', function () {
      chips.querySelectorAll('button').forEach(function (other) { other.setAttribute('aria-pressed', 'false'); });
      chip.setAttribute('aria-pressed', 'true');
      swap(skin, 'img/skin-' + entry[0] + '.png', 'Now Playing in the ' + entry[1] + ' skin');
    });
    chips.appendChild(chip);
  });

  // What's new: the newest releases of Y2Linux, cut down to what a listener wants to know. The notes
  // on GitHub carry install steps and file lists between "github-only" markers; those are left out,
  // and each point is its name and its first sentence.
  const WANTED = ["what's new", 'fixed', 'changed'];
  function plain(text) {
    return text
      .replace(/\s*\((?:\[[^\]]*\]\([^)]*\)[,;\s]*)+\)/g, '')  // "(#12, Y2Reborn #3)" made of links
      .replace(/\[([^\]]*)\]\([^)]*\)/g, '$1')
      .replace(/[*`]/g, '')
      .replace(/\s+/g, ' ')
      .trim();
  }
  function firstSentence(text) {
    const cut = text.search(/[.!?](?:\s|$)/);
    const sentence = cut < 0 ? text : text.slice(0, cut + 1);
    return sentence.length > 220 ? sentence.slice(0, 217).replace(/\s+\S*$/, '') + '…' : sentence;
  }
  function digest(body) {
    const visible = body.split('<!-- github-only -->')[0];
    const lines = visible.split(/\r?\n/);
    const first = (lines.find(function (line) { return line.trim(); }) || '');
    const about = plain(first.replace(/^\*\*[^*]*\*\*\s*:?\s*/, ''));
    const sections = [];
    let current = null;
    lines.forEach(function (line) {
      const heading = line.match(/^##\s+(.*)$/);
      if (heading) {
        const name = heading[1].trim();
        current = WANTED.indexOf(name.toLowerCase()) < 0 ? null : { name: name, points: [] };
        if (current) sections.push(current);
        return;
      }
      const point = line.match(/^-\s+\*\*(.+?)\*\*\s*(.*)$/);
      if (!current || !point) return;
      const title = plain(point[1]).replace(/[.:]$/, '');
      const rest = plain(point[2]).replace(/^[.:]\s*/, '');
      current.points.push([title, firstSentence(rest)]);
    });
    const opening = firstSentence(about);
    return { about: opening.charAt(0).toUpperCase() + opening.slice(1), sections: sections.filter(function (s) { return s.points.length; }) };
  }
  function element(tag, text, className) {
    const node = document.createElement(tag);
    if (text) node.textContent = text;
    if (className) node.className = className;
    return node;
  }
  function showReleases(list) {
    const box = document.getElementById('releases');
    const shown = list.filter(function (r) { return r.body && r.name; }).slice(0, 5);
    if (!shown.length) return false;
    box.textContent = '';
    shown.forEach(function (release, index) {
      const parts = digest(release.body);
      const card = element('details', '', index ? 'release' : 'release newest');
      card.open = index === 0;
      const head = element('summary');
      head.appendChild(element('h3', release.name.replace(/^Y2Linux\s+/, '')));
      const date = element('time', new Date(release.published_at).toLocaleDateString('en-GB',
        { day: 'numeric', month: 'long', year: 'numeric' }));
      date.dateTime = release.published_at;
      head.appendChild(date);
      if (parts.about) head.appendChild(element('p', parts.about, 'about'));
      card.appendChild(head);
      parts.sections.forEach(function (section) {
        card.appendChild(element('h4', section.name));
        const points = element('ul');
        section.points.forEach(function (point) {
          const item = element('li');
          item.appendChild(element('strong', point[0]));
          if (point[1]) item.appendChild(document.createTextNode(' · ' + point[1]));
          points.appendChild(item);
        });
        card.appendChild(points);
      });
      const more = element('a', 'Full notes and downloads', 'more');
      more.href = release.html_url;
      card.appendChild(more);
      box.appendChild(card);
    });
    return true;
  }
  function load(url) {
    return fetch(url).then(function (answer) {
      if (!answer.ok) throw new Error(answer.status);
      return answer.json();
    });
  }
  // The copy that came with the site first, then GitHub's current list when it answers.
  load('releases.json').then(showReleases).catch(function () {}).then(function () {
    return load('https://api.github.com/repos/SchulzCode/Y2Linux/releases?per_page=8');
  }).then(showReleases).catch(function () {});

  // Motion on scroll. Everything below is decoration: with reduced motion asked for, none of it runs.
  if (still) return;
  const ease = function (t) { return t * t * (3 - 2 * t); };
  const clamp = function (t) { return Math.max(0, Math.min(1, t)); };

  // Things come in as they are scrolled to; the members of a grid one after the other.
  const groups = ['.cards .card', '.tiles .tile', '.stats .stat', '.ways .way', '.open .pillar', '.shots figure',
    '.shots .code', '.faq details', '.chips', '.skin-stage'];
  groups.forEach(function (selector) {
    const seen = new Map();
    document.querySelectorAll(selector).forEach(function (node) {
      const count = seen.get(node.parentNode) || 0;
      seen.set(node.parentNode, count + 1);
      node.classList.add('reveal');
      node.style.setProperty('--delay', Math.min(count, 8) * 0.07 + 's');
    });
  });
  document.querySelectorAll('main h2, main .eyebrow, main .lead, .copy > p, .copy > ul, .copy > .actions, .plain, .hero .actions, .hero .note')
    .forEach(function (node) { node.classList.add('reveal'); });
  const watcher = new IntersectionObserver(function (entries) {
    entries.forEach(function (entry) {
      if (!entry.isIntersecting) return;
      entry.target.classList.add('in');
      watcher.unobserve(entry.target);
      if (entry.target.classList.contains('stat')) count(entry.target.querySelector('strong'));
    });
  }, { rootMargin: '0px 0px -8% 0px', threshold: 0.08 });
  document.querySelectorAll('.reveal').forEach(function (node) { watcher.observe(node); });

  // A number counts up to its value once.
  function count(node) {
    const text = node.textContent;
    const match = text.match(/\d+/);
    if (!match) return;
    const goal = Number(match[0]);
    const start = performance.now();
    (function step(now) {
      const t = clamp((now - start) / 1100);
      node.textContent = text.replace(/\d+/, String(Math.round(goal * (1 - Math.pow(1 - t, 3)))));
      if (t < 1) requestAnimationFrame(step);
    })(start);
  }

  // The wave draws itself as it comes up the window.
  const line = document.querySelector('.wave path');
  line.setAttribute('pathLength', '1');
  line.style.strokeDasharray = '1';
  const progress = document.getElementById('progress');
  const tour = document.getElementById('tour');
  const frames = Array.prototype.slice.call(tour.querySelectorAll('.tour-screen img'));
  const words = Array.prototype.slice.call(tour.querySelectorAll('.tour-words span'));
  const stage = tour.querySelector('.tour-screen');
  const hint = tour.querySelector('.tour-hint');
  const hero = document.getElementById('player');

  function frame() {
    waiting = false;
    const high = window.innerHeight;
    const page = document.documentElement;
    progress.style.transform = 'scaleX(' + clamp(window.scrollY / (page.scrollHeight - high)) + ')';

    const wave = line.ownerSVGElement.getBoundingClientRect();
    line.style.strokeDashoffset = String(1 - ease(clamp((high - wave.top) / (high * 0.6))));

    // The player in the hero leans back a little as it leaves.
    const gone = clamp(window.scrollY / high);
    hero.style.transform = 'translateY(' + gone * -40 + 'px) scale(' + (1 - gone * 0.06) + ')';

    // The tour: its screens and words follow the scroll position.
    const box = tour.getBoundingClientRect();
    const through = clamp(-box.top / (box.height - high));
    const place = through * (frames.length - 1);
    const arrive = ease(clamp((high - box.top) / high));
    const leave = ease(clamp((box.bottom - high * 0.35) / (high * 0.65)));
    frames.forEach(function (image, index) {
      image.style.opacity = String(index === 0 ? 1 : ease(clamp((place - (index - 1)) * 1.6 - 0.3)));
    });
    words.forEach(function (word, index) {
      const far = place - index;
      word.style.opacity = String(clamp(1 - Math.abs(far) * 2.4) * arrive);
      word.style.transform = 'translateY(' + (far * -60) + 'px) scale(' + (1 - Math.abs(far) * 0.08) + ')';
    });
    stage.style.transform = 'scale(' + (0.72 + 0.28 * arrive) * (0.9 + 0.1 * leave) + ')';
    stage.style.opacity = String(leave);
    hint.style.opacity = String(clamp(1 - through * 6) * arrive);
  }
  let waiting = false;
  function ask() {
    if (waiting) return;
    waiting = true;
    requestAnimationFrame(frame);
  }
  window.addEventListener('scroll', ask, { passive: true });
  window.addEventListener('resize', ask);
  frame();
})();
