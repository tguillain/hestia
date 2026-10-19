// Tableau de bord hestia. Aucune dépendance, aucune étape de build.

const app = document.getElementById('app');
const toastBox = document.getElementById('toast');
const devices = new Map();
const cards = new Map();
let events = null;
let reloadTimer = null;
let toastTimer = null;
let charts = [];
const chartViews = new Set();
let chartTimer = null;
let currentUser = null;
// Newer version published on GitHub, as last seen by the Pi.
let availableUpdate = null;

const ICONS = {
  house: '<path d="M3 11 12 4l9 7v8.5a1.5 1.5 0 0 1-1.5 1.5h-15A1.5 1.5 0 0 1 3 19.5Z"/><circle cx="12" cy="14.5" r="2.5"/>',
  bulb: '<path d="M9 18h6M10 21h4M12 3a6 6 0 0 0-3.6 10.8c.6.5 1 1.3 1 2.2h5.2c0-.9.4-1.7 1-2.2A6 6 0 0 0 12 3Z"/>',
  power: '<path d="M12 3v8M6.3 6.8a8 8 0 1 0 11.4 0"/>',
  device: '<rect x="4" y="4" width="16" height="16" rx="3"/><path d="M9 12h6"/>',
  gear: '<circle cx="12" cy="12" r="3"/><path d="M12 2.5v3M12 18.5v3M2.5 12h3M18.5 12h3M5.3 5.3l2.1 2.1M16.6 16.6l2.1 2.1M5.3 18.7l2.1-2.1M16.6 7.4l2.1-2.1"/>',
};

const UNITS = {
  percent: '%',
  kelvin: 'K',
  celsius: '°C',
  millisecond: 'ms',
  revolution_per_minute: 'tr/min',
};

class HttpError extends Error {
  constructor(status, message) {
    super(message);
    this.status = status;
  }
}

async function api(method, path, body) {
  const response = await fetch(path, {
    method,
    credentials: 'same-origin',
    headers: body === undefined ? {} : { 'Content-Type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (response.status === 204) return null;
  const data = await response.json().catch(() => ({}));
  if (!response.ok) {
    if (response.status === 401 && path !== '/api/login') showLogin();
    throw new HttpError(response.status, data.message || `Erreur ${response.status}`);
  }
  return data;
}

function h(tag, props = {}, ...children) {
  const element = document.createElement(tag);
  for (const [key, value] of Object.entries(props)) {
    if (value === undefined || value === null || value === false) continue;
    if (key.startsWith('on')) element.addEventListener(key.slice(2), value);
    else if (key === 'class') element.className = value;
    else if (key === 'value') element.value = value;
    else element.setAttribute(key, value === true ? '' : String(value));
  }
  element.append(...children.flat().filter((child) => child !== null && child !== undefined && child !== false));
  return element;
}

function icon(name) {
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  svg.setAttribute('viewBox', '0 0 24 24');
  svg.setAttribute('class', 'icon');
  svg.setAttribute('fill', 'none');
  svg.setAttribute('stroke', 'currentColor');
  svg.setAttribute('stroke-width', '1.8');
  svg.setAttribute('stroke-linecap', 'round');
  svg.setAttribute('stroke-linejoin', 'round');
  svg.setAttribute('aria-hidden', 'true');
  svg.innerHTML = ICONS[name];
  return svg;
}

function toast(message) {
  toastBox.textContent = message;
  toastBox.classList.add('visible');
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => toastBox.classList.remove('visible'), 4000);
}

// ---- Devices -----------------------------------------------------------------------------

function point(device, key) {
  return device.points.find((candidate) => candidate.key === key);
}

function value(device, key) {
  return point(device, key)?.value ?? null;
}

function isLight(device) {
  const on = device.points.find((candidate) => candidate.semantic === 'on_off');
  return Boolean(on && on.access === 'read_write' && device.points.some((candidate) => candidate.semantic === 'brightness'));
}

function byRoomThenName(a, b) {
  return (a.room ?? '').localeCompare(b.room ?? '', 'fr') || a.name.localeCompare(b.name, 'fr');
}

async function send(device, key, newValue) {
  const card = cards.get(device.id);
  card?.root.classList.add('pending');
  try {
    await api('POST', `/api/devices/${encodeURIComponent(device.id)}/points/${encodeURIComponent(key)}`, { value: newValue });
  } catch (error) {
    if (error.status !== 401) toast(`${device.name} : ${error.message}`);
    scheduleReload();
  } finally {
    card?.root.classList.remove('pending');
  }
}

function formatValue(spec) {
  const current = spec.value;
  if (current === null || current === undefined || current === '') return '—';
  if (typeof current === 'boolean' && spec.semantic === 'connectivity') return current ? 'En ligne' : 'Hors ligne';
  if (typeof current === 'boolean') return current ? 'Oui' : 'Non';
  if (typeof current === 'object') return rgbToHex(current);
  if (typeof current !== 'number') return String(current);
  if (spec.unit === 'bit_per_second') return formatScaled(current, 1000, ['bit/s', 'kbit/s', 'Mbit/s', 'Gbit/s']);
  if (spec.unit === 'byte') return formatScaled(current, 1024, ['o', 'Ko', 'Mo', 'Go', 'To']);
  const unit = UNITS[spec.unit];
  const number = current.toLocaleString('fr-FR', { maximumFractionDigits: 1 });
  return unit ? `${number} ${unit}` : number;
}

function formatScaled(number, base, units) {
  let index = 0;
  while (Math.abs(number) >= base && index < units.length - 1) {
    number /= base;
    index += 1;
  }
  return `${number.toLocaleString('fr-FR', { maximumFractionDigits: 1 })} ${units[index]}`;
}

function rgbToHex({ r, g, b }) {
  return `#${[r, g, b].map((channel) => channel.toString(16).padStart(2, '0')).join('')}`;
}

function hexToRgb(hex) {
  const number = Number.parseInt(hex.slice(1), 16);
  return { r: (number >> 16) & 255, g: (number >> 8) & 255, b: number & 255 };
}

function kelvinToRgb(kelvin) {
  const t = kelvin / 100;
  const clamp = (channel) => Math.max(0, Math.min(255, Math.round(channel)));
  const r = t <= 66 ? 255 : clamp(329.698727446 * (t - 60) ** -0.1332047592);
  const g = t <= 66 ? clamp(99.4708025861 * Math.log(t) - 161.1195681661) : clamp(288.1221695283 * (t - 60) ** -0.0755148492);
  const b = t >= 66 ? 255 : t <= 19 ? 0 : clamp(138.5177312231 * Math.log(t - 10) - 305.0447927307);
  return { r, g, b };
}

// A slider being dragged must not jump back when a state event arrives.
function slider(props) {
  const input = h('input', { type: 'range', ...props });
  input.addEventListener('pointerdown', () => { input.dataset.busy = '1'; });
  input.addEventListener('pointerup', () => { delete input.dataset.busy; });
  input.addEventListener('pointercancel', () => { delete input.dataset.busy; });
  return input;
}

function setIfIdle(input, newValue) {
  if (!input.dataset.busy && document.activeElement !== input) input.value = newValue;
}

function lightCard(device) {
  const bulb = h('button', { class: 'bulb', 'aria-hidden': 'true', tabindex: '-1' }, icon('bulb'));
  const title = h('h3');
  const meta = h('p', { class: 'meta' });
  const toggle = h('button', { class: 'switch', role: 'switch' });
  const brightness = slider({ min: 1, max: 100, step: 1, 'aria-label': 'Luminosité' });
  const brightnessOut = h('output');
  const colorField = h('label', { class: 'field' }, h('span', {}, 'Couleur'));
  const color = h('input', { type: 'color', 'aria-label': 'Couleur' });
  colorField.append(color, h('span'));
  const whiteSpec = point(device, 'color_temp');
  const kelvin = slider({
    class: 'kelvin',
    min: whiteSpec?.kind.min ?? 2000,
    max: whiteSpec?.kind.max ?? 9000,
    step: 100,
    'aria-label': 'Température de blanc',
  });
  const kelvinOut = h('output');
  const whiteField = h('label', { class: 'field' }, h('span', {}, 'Blanc'), kelvin, kelvinOut);
  const root = h('article', { class: 'card light' },
    h('div', { class: 'card-head' }, bulb, h('div', { class: 'card-title' }, title, meta), toggle),
    h('label', { class: 'field' }, h('span', {}, 'Luminosité'), brightness, brightnessOut),
    whiteSpec || point(device, 'color')
      ? h('details', { class: 'more' }, h('summary', {}, 'Couleur et blanc'), h('div', { class: 'more-body' }, point(device, 'color') ? colorField : null, whiteSpec ? whiteField : null))
      : null,
  );

  let current = device;
  const toggleLight = () => send(current, 'on', value(current, 'on') !== true);
  toggle.addEventListener('click', toggleLight);
  bulb.addEventListener('click', toggleLight);
  brightness.addEventListener('input', () => { brightnessOut.textContent = `${brightness.value} %`; });
  brightness.addEventListener('change', () => send(current, 'brightness', Number(brightness.value)));
  color.addEventListener('change', () => send(current, 'color', hexToRgb(color.value)));
  kelvin.addEventListener('input', () => { kelvinOut.textContent = `${kelvin.value} K`; });
  kelvin.addEventListener('change', () => send(current, 'color_temp', Number(kelvin.value)));

  function update(next) {
    current = next;
    const on = value(next, 'on') === true;
    const level = Math.round(value(next, 'brightness') ?? 0);
    const mode = value(next, 'color_mode');
    const rgb = value(next, 'color');
    const temperature = value(next, 'color_temp');
    title.textContent = next.name;
    const place = next.room ? `${next.room} · ` : '';
    meta.textContent = place + (!next.available ? 'Injoignable' : on ? `Allumée · ${level} %` : 'Éteinte');
    root.classList.toggle('on', on && next.available);
    root.classList.toggle('offline', !next.available);
    toggle.setAttribute('aria-checked', String(on));
    toggle.setAttribute('aria-label', `${next.name} : ${on ? 'éteindre' : 'allumer'}`);
    for (const control of [toggle, brightness, color, kelvin]) control.disabled = !next.available;
    setIfIdle(brightness, Math.max(level, 1));
    brightnessOut.textContent = `${level} %`;
    // In white mode the lamp reports no colour: offer white rather than black.
    setIfIdle(color, rgb && (rgb.r || rgb.g || rgb.b) ? rgbToHex(rgb) : '#ffffff');
    if (temperature) {
      setIfIdle(kelvin, temperature);
      kelvinOut.textContent = `${temperature} K`;
    }
    colorField.classList.toggle('active', mode === 'color');
    whiteField.classList.toggle('active', mode === 'white');
    const glow = mode === 'white' && temperature ? kelvinToRgb(temperature) : rgb ?? { r: 245, g: 166, b: 35 };
    bulb.style.setProperty('--glow', `${glow.r} ${glow.g} ${glow.b}`);
  }

  update(device);
  return { root, update };
}

function genericCard(device) {
  const title = h('h3');
  const meta = h('p', { class: 'meta' });
  const rows = h('dl', { class: 'rows' });
  const expanded = new Map();
  const root = h('article', { class: 'card' },
    h('div', { class: 'card-head' }, h('span', { class: 'bulb', 'aria-hidden': 'true' }, icon('device')), h('div', { class: 'card-title' }, title, meta)),
    rows,
  );
  let current = device;

  function toggleChart(spec) {
    const view = expanded.get(spec.key);
    if (view) {
      chartViews.delete(view);
      expanded.delete(spec.key);
    } else {
      const series = [{ device: current.id, point: spec.key, label: spec.label, unit: spec.unit ?? null, semantic: spec.semantic }];
      expanded.set(spec.key, chartView({ title: '', hours: 24, series }));
    }
    update(current);
  }

  function control(next, spec) {
    if (spec.access !== 'read_write' || !next.available) {
      if (spec.kind.type === 'numeric' && spec.value !== null && spec.value !== undefined) {
        const link = h('button', { class: 'value-link', 'aria-expanded': String(expanded.has(spec.key)), title: 'Historique sur 24 h' }, formatValue(spec));
        link.addEventListener('click', () => toggleChart(spec));
        return h('dd', {}, link);
      }
      return h('dd', {}, formatValue(spec));
    }
    const kind = spec.kind;
    if (kind.type === 'binary') {
      const toggle = h('button', { class: 'switch', role: 'switch', 'aria-checked': String(spec.value === true), 'aria-label': spec.label });
      toggle.addEventListener('click', () => send(next, spec.key, spec.value !== true));
      return h('dd', {}, toggle);
    }
    if (kind.type === 'numeric') {
      const input = slider({ min: kind.min, max: kind.max, step: kind.step, value: spec.value ?? kind.min, 'aria-label': spec.label });
      input.addEventListener('change', () => send(next, spec.key, Number(input.value)));
      return h('dd', {}, input);
    }
    if (kind.type === 'enum') {
      const select = h('select', { 'aria-label': spec.label }, kind.values.map((choice) => h('option', { value: choice }, choice)));
      select.value = spec.value ?? '';
      select.addEventListener('change', () => send(next, spec.key, select.value));
      return h('dd', {}, select);
    }
    return h('dd', {}, formatValue(spec));
  }

  function update(next) {
    current = next;
    title.textContent = next.name;
    meta.textContent = (next.room ? `${next.room} · ` : '') + (next.available ? next.model ?? 'Disponible' : 'Injoignable');
    root.classList.toggle('offline', !next.available);
    rows.replaceChildren(...next.points.flatMap((spec) => {
      const row = h('div', { class: 'row' }, h('dt', {}, spec.label), control(next, spec));
      const view = expanded.get(spec.key);
      return view ? [row, view.root] : [row];
    }));
  }

  update(device);
  return { root, update };
}

// ---- Charts ------------------------------------------------------------------------------

const SVG_NS = 'http://www.w3.org/2000/svg';
const BINARY_SEMANTICS = new Set(['connectivity', 'on_off']);

function svg(tag, attrs = {}, ...children) {
  const element = document.createElementNS(SVG_NS, tag);
  for (const [key, value] of Object.entries(attrs)) element.setAttribute(key, String(value));
  element.append(...children);
  return element;
}

function timeLabel(timestamp, hours, detailed) {
  let options = { hour: '2-digit', minute: '2-digit' };
  if (hours > 48) options = detailed ? { weekday: 'short', hour: '2-digit', minute: '2-digit' } : { weekday: 'short', day: 'numeric' };
  return new Intl.DateTimeFormat('fr-FR', options).format(new Date(timestamp));
}

// Local midnights for multi-day ranges, round hours otherwise.
function timeTicks(from, to, hours) {
  const daily = hours > 48;
  const every = hours > 12 ? 6 : hours > 4 ? 2 : 1;
  const cursor = new Date(from);
  cursor.setMinutes(0, 0, 0);
  if (daily) cursor.setHours(0);
  const ticks = [];
  while (cursor.getTime() <= to) {
    const time = cursor.getTime();
    if (time >= from && (daily || cursor.getHours() % every === 0)) ticks.push(time);
    cursor.setHours(cursor.getHours() + (daily ? 24 : 1));
  }
  return ticks;
}

function lastKnown(values) {
  for (let index = values.length - 1; index >= 0; index -= 1) {
    if (values[index] !== null) return values[index];
  }
  return null;
}

function drawChart(series, hours) {
  const known = series.flatMap((one) => one.values.filter((sample) => sample !== null));
  if (!known.length) return h('p', { class: 'meta' }, 'Pas encore de données.');
  const width = 640;
  const height = 210;
  const left = 70;
  const right = 12;
  const top = 12;
  const bottom = 26;
  const plotWidth = width - left - right;
  const plotHeight = height - top - bottom;
  const { from, step } = series[0];
  const count = series[0].values.length;
  const to = from + step * count;
  const axis = { unit: series[0].unit, semantic: series[0].semantic };
  // On/off series are stored as 0/1 (averaged per slice): draw them on a two-level axis.
  const binary = series.every((one) => BINARY_SEMANTICS.has(one.semantic));
  let low = binary ? 0 : Math.min(...known);
  let high = binary ? 1 : Math.max(...known);
  if (axis.semantic !== 'temperature' && low >= 0) low = 0;
  if (high === low) high = low + 1;
  const counting = axis.semantic === 'count';
  // Whole numbers on the axis for counts: 0, 1, 2… never 0,3 players.
  if (counting) high = Math.max(4, Math.ceil(high / 4) * 4);
  const margin = (high - low) * 0.1;
  if (axis.semantic === 'temperature' || binary) low -= margin;
  if (!counting) high += margin;
  if (axis.unit === 'percent') high = Math.min(high, 100);
  const x = (time) => left + ((time - from) / (to - from)) * plotWidth;
  const y = (level) => top + (1 - (level - low) / (high - low)) * plotHeight;
  const describe = (one, sample) => {
    if (binary && sample !== null) return formatValue({ semantic: one.semantic, value: sample >= 0.5 });
    return formatValue({ unit: one.unit, semantic: one.semantic, value: sample });
  };

  const chart = svg('svg', { viewBox: `0 0 ${width} ${height}`, class: 'chart', role: 'img', 'aria-label': series.map((one) => one.label).join(', ') });
  const levels = binary ? [0, 1] : [0, 1, 2, 3, 4].map((index) => low + ((high - low) * index) / 4);
  for (const level of levels) {
    chart.append(
      svg('line', { x1: left, x2: width - right, y1: y(level), y2: y(level), class: 'grid-line' }),
      svg('text', { x: left - 8, y: y(level) + 4, class: 'axis', 'text-anchor': 'end' }, describe(series[0], level)),
    );
  }
  for (const tick of timeTicks(from, to, hours)) {
    chart.append(
      svg('line', { x1: x(tick), x2: x(tick), y1: top, y2: top + plotHeight, class: 'grid-line faint' }),
      svg('text', { x: x(tick), y: height - 8, class: 'axis', 'text-anchor': 'middle' }, timeLabel(tick, hours, false)),
    );
  }
  series.forEach((one, index) => {
    let path = '';
    let drawing = false;
    one.values.forEach((sample, position) => {
      if (sample === null) {
        drawing = false;
        return;
      }
      path += `${drawing ? 'L' : 'M'}${x(from + step * (position + 0.5)).toFixed(1)},${y(sample).toFixed(1)}`;
      drawing = true;
    });
    chart.append(svg('path', { d: path, class: `line series-${index % 4}` }));
  });
  const cursor = svg('line', { y1: top, y2: top + plotHeight, x1: -10, x2: -10, class: 'cursor' });
  chart.append(cursor);

  const stamp = h('span', { class: 'legend-time' });
  const readouts = series.map(() => h('strong'));
  const legend = h('div', { class: 'legend' },
    series.map((one, index) => h('span', { class: 'legend-item' }, h('i', { class: `swatch series-${index % 4}` }), `${one.label} `, readouts[index])),
    stamp,
  );
  const show = (position) => {
    series.forEach((one, index) => {
      const sample = position === null ? lastKnown(one.values) : one.values[position];
      readouts[index].textContent = describe(one, sample);
    });
    stamp.textContent = position === null ? '' : timeLabel(from + step * position, hours, true);
    const at = position === null ? -10 : x(from + step * (position + 0.5));
    cursor.setAttribute('x1', at);
    cursor.setAttribute('x2', at);
  };
  chart.addEventListener('pointermove', (event) => {
    const box = chart.getBoundingClientRect();
    const position = Math.floor(((((event.clientX - box.left) / box.width) * width - left) / plotWidth) * count);
    show(position >= 0 && position < count ? position : null);
  });
  chart.addEventListener('pointerleave', () => show(null));
  show(null);
  return h('div', { class: 'chart-wrap' }, chart, legend);
}

function chartView(chart) {
  const body = h('div', { class: 'chart-body' }, h('p', { class: 'meta' }, 'Chargement…'));
  const root = h('div', { class: 'chart-view' }, chart.title ? h('h3', {}, chart.title) : null, body);
  const refresh = async () => {
    if (!chart.series.length) {
      body.replaceChildren(h('p', { class: 'meta' }, 'Pas encore de données.'));
      return;
    }
    try {
      const data = await Promise.all(chart.series.map(async (one) => {
        const query = new URLSearchParams({ device: one.device, point: one.point, hours: String(chart.hours) });
        return { ...one, ...(await api('GET', `/api/history?${query}`)) };
      }));
      body.replaceChildren(drawChart(data, chart.hours));
    } catch (error) {
      if (error.status !== 401) body.replaceChildren(h('p', { class: 'meta' }, 'Historique indisponible.'));
    }
  };
  const view = { root, refresh };
  chartViews.add(view);
  refresh();
  return view;
}

function refreshChartsEveryMinute() {
  clearInterval(chartTimer);
  chartTimer = setInterval(() => {
    if (document.visibilityState === 'visible') for (const view of chartViews) view.refresh();
  }, 60_000);
}

// ---- Screens -----------------------------------------------------------------------------

function showLogin(message = '') {
  stopEvents();
  const error = h('p', { class: 'form-error', role: 'alert' }, message);
  const submit = h('button', { class: 'primary', type: 'submit' }, 'Se connecter');
  const form = h('form', { class: 'login' },
    h('label', {}, 'Identifiant', h('input', { name: 'username', autocomplete: 'username', autocapitalize: 'none', required: true })),
    h('label', {}, 'Mot de passe', h('input', { name: 'password', type: 'password', autocomplete: 'current-password', required: true })),
    error,
    submit,
  );
  form.addEventListener('submit', async (event) => {
    event.preventDefault();
    const fields = new FormData(form);
    submit.disabled = true;
    error.textContent = '';
    try {
      await api('POST', '/api/login', { username: fields.get('username'), password: fields.get('password') });
      await showDashboard();
    } catch (failure) {
      error.textContent = failure.status === 401 ? 'Identifiant ou mot de passe incorrect.' : failure.message;
      submit.disabled = false;
    }
  });
  app.removeAttribute('aria-busy');
  app.replaceChildren(h('div', { class: 'login-screen' }, h('div', { class: 'login-box' }, h('div', { class: 'brand' }, icon('house'), 'Hestia'), form)));
  form.querySelector('input').focus();
}

async function showDashboard() {
  const [session, list, configured] = await Promise.all([
    api('GET', '/api/session'),
    api('GET', '/api/devices'),
    api('GET', '/api/charts'),
  ]);
  setDevices(list);
  charts = configured;
  currentUser = session.user;
  availableUpdate = session.update ?? null;
  route();
  startEvents();
}

function setDevices(list) {
  devices.clear();
  for (const device of list) devices.set(device.id, device);
}

function renderDashboard(user) {
  cards.clear();
  chartViews.clear();
  const all = [...devices.values()].sort(byRoomThenName);
  const lights = all.filter(isLight);
  const others = all.filter((device) => !isLight(device));
  const card = (device, build) => {
    const built = build(device);
    cards.set(device.id, built);
    return built.root;
  };
  const allOff = h('button', { class: 'pill' }, icon('power'), 'Tout éteindre');
  allOff.addEventListener('click', () => {
    for (const light of devices.values()) {
      if (isLight(light) && light.available && value(light, 'on') === true) send(light, 'on', false);
    }
  });
  const sections = [];
  sections.push(h('section', { class: 'section' },
    h('div', { class: 'section-head' }, h('h2', {}, 'Lumières'), lights.length ? allOff : null),
    lights.length
      ? h('div', { class: 'grid' }, lights.map((device) => card(device, lightCard)))
      : h('p', { class: 'empty' }, 'Aucune lampe pour l’instant. Elles apparaissent dès qu’elles répondent sur le réseau.'),
  ));
  if (charts.length) {
    sections.push(h('section', { class: 'section' },
      h('div', { class: 'section-head' }, h('h2', {}, 'Graphiques')),
      h('div', { class: 'grid wide' }, charts.map((chart) => h('article', { class: 'card' }, chartView(chart).root))),
    ));
  }
  if (others.length) {
    sections.push(h('section', { class: 'section' },
      h('div', { class: 'section-head' }, h('h2', {}, 'Appareils')),
      h('div', { class: 'grid' }, others.map((device) => card(device, genericCard))),
    ));
  }
  app.removeAttribute('aria-busy');
  app.replaceChildren(
    topbar(user),
    h('div', { class: 'content' }, sections),
  );
  refreshChartsEveryMinute();
}

// ---- Settings ----------------------------------------------------------------------------

function onSettingsPage() {
  return window.location.hash === '#reglages';
}

function topbar(user) {
  const settings = onSettingsPage();
  const updateNote = availableUpdate ? `Nouvelle version ${availableUpdate} disponible` : null;
  const navigate = settings
    ? h('a', { class: 'ghost', href: '#' }, icon('house'), 'Tableau de bord')
    : h('a', { class: 'ghost', href: '#reglages', title: updateNote },
      icon('gear'), 'Réglages',
      updateNote ? h('span', { class: 'update-dot', role: 'img', 'aria-label': updateNote }) : null);
  const logout = h('button', { class: 'ghost', title: `Connecté : ${user}` }, 'Déconnexion');
  logout.addEventListener('click', async () => {
    await api('POST', '/api/logout').catch(() => {});
    showLogin();
  });
  return h('header', { class: 'topbar' },
    h('a', { class: 'brand', href: '#' }, icon('house'), 'Hestia'),
    h('span', { class: 'spacer' }),
    h('span', { id: 'status', class: 'status', title: 'Temps réel' }),
    navigate,
    logout,
  );
}

function route() {
  if (onSettingsPage()) renderSettings(currentUser);
  else renderDashboard(currentUser);
}

function settingsCard(title, description, body) {
  return h('section', { class: 'card setting' }, h('h3', {}, title), description ? h('p', { class: 'meta' }, description) : null, body);
}

function line(text, kind = '') {
  return h('p', { class: kind }, text);
}

// Runs a settings action; shows its error instead of throwing.
async function attempt(action) {
  try {
    await action();
    return true;
  } catch (error) {
    if (error.status !== 401) toast(error.message);
    return false;
  }
}

function renderSettings(user) {
  cards.clear();
  chartViews.clear();
  clearInterval(chartTimer);
  app.removeAttribute('aria-busy');
  app.replaceChildren(
    topbar(user),
    h('div', { class: 'content settings' },
      h('h2', { class: 'page-title' }, 'Réglages'),
      deviceSettings(),
      homekitSettings(),
      freeboxSettings(),
      tokenSettings(),
      accountSettings(user),
      updateSettings(),
    ),
  );
}

function homekitSettings() {
  const body = h('div', { class: 'setting-body' }, line('Chargement…', 'meta'));
  const load = () => attempt(async () => {
    const status = await api('GET', '/api/setup/homekit');
    if (!status.enabled) {
      body.replaceChildren(line('Le pont Apple Home est désactivé dans la configuration.', 'meta'));
      return;
    }
    if (status.paired) {
      body.replaceChildren(
        line(`✓ Appairé avec ${status.controllers} appareil(s) Apple, ${status.accessories} lampe(s) exposée(s).`, 'ok-line'),
        line('Pour les autres membres de la famille : invite-les depuis l’app Maison (Réglages de la maison → Inviter).', 'meta'),
      );
      return;
    }
    const refresh = h('button', { class: 'pill' }, 'J’ai terminé l’appairage');
    refresh.addEventListener('click', load);
    body.replaceChildren(h('div', { class: 'pairing' },
      h('img', { class: 'qr', src: `/api/setup/homekit/qr.svg?t=${Date.now()}`, alt: 'QR code d’appairage Apple Home', width: 200, height: 200 }),
      h('div', { class: 'pairing-steps' },
        line('Sur l’iPhone, connecté au Wi-Fi de la maison :'),
        h('ol', {},
          h('li', {}, 'Vise ce QR code avec l’appareil photo, ou dans l’app Maison : + → Ajouter un accessoire.'),
          h('li', {}, `Choisis « ${status.name} » et accepte l’accessoire non certifié.`),
          h('li', {}, 'Si un code est demandé :'),
        ),
        h('p', { class: 'setup-code' }, status.setup_code),
        refresh,
      ),
    ));
  });
  load();
  return settingsCard('Apple Home', 'Les lampes dans l’app Maison et avec Siri.', body);
}

function freeboxSettings() {
  const body = h('div', { class: 'setting-body' }, line('Chargement…', 'meta'));
  let timer = null;
  const load = () => attempt(async () => {
    clearTimeout(timer);
    const status = await api('GET', '/api/setup/freebox');
    if (!status.configured) {
      body.replaceChildren(line('Aucun pilote Freebox dans la configuration.', 'meta'));
      return;
    }
    const flow = status.flow?.state ?? 'idle';
    const parts = [line(status.reachable ? `${status.model} (API ${status.api_version})` : `Freebox injoignable : ${status.error}`, 'meta')];
    if (status.authorized) {
      parts.push(
        line('✓ Hestia est autorisé sur la Freebox.', 'ok-line'),
        line('Pour l’interrupteur Wi-Fi : Freebox OS → Paramètres de la Freebox → Gestion des accès → Applications → Hestia, coche « Modification des réglages de la Freebox ».', 'meta'),
      );
    } else if (flow === 'pending') {
      parts.push(line('Valide la demande « Hestia » sur l’écran de la Freebox, dans les 2 minutes…', 'pending-line'));
      if (body.isConnected) timer = setTimeout(load, 2000);
    } else {
      if (flow === 'failed') parts.push(line(status.flow.reason, 'form-error'));
      const authorize = h('button', { class: 'primary inline' }, 'Autoriser Hestia sur la Freebox');
      authorize.disabled = !status.reachable;
      authorize.addEventListener('click', async () => {
        authorize.disabled = true;
        await attempt(() => api('POST', '/api/setup/freebox/authorize'));
        load();
      });
      parts.push(authorize);
    }
    body.replaceChildren(...parts);
  });
  load();
  return settingsCard('Freebox', 'Débits, températures et Wi-Fi de la box.', body);
}

function copyButton(label, text) {
  const button = h('button', { class: 'pill', type: 'button' }, label);
  button.addEventListener('click', async () => {
    try {
      await navigator.clipboard.writeText(text);
      button.textContent = 'Copié ✓';
    } catch {
      toast('Copie impossible ici : sélectionne le texte et copie-le.');
    }
  });
  return button;
}

function tokenSettings() {
  const list = h('ul', { class: 'item-list' });
  const revealed = h('div');
  const name = h('input', { name: 'name', placeholder: 'claude', autocapitalize: 'none', required: true });
  const control = h('input', { type: 'checkbox' });
  const form = h('form', { class: 'inline-form' },
    h('label', { class: 'field-label' }, 'Nom du jeton', name),
    h('label', { class: 'check' }, control, 'Peut commander les appareils'),
    h('button', { class: 'primary inline', type: 'submit' }, 'Créer un jeton'),
  );
  const load = () => attempt(async () => {
    const tokens = await api('GET', '/api/setup/tokens');
    list.replaceChildren(...(tokens.length
      ? tokens.map((token) => {
        const revoke = h('button', { class: 'ghost danger' }, 'Révoquer');
        revoke.addEventListener('click', async () => {
          if (!window.confirm(`Révoquer le jeton « ${token.name} » ? L’IA qui l’utilise perdra l’accès immédiatement.`)) return;
          if (await attempt(() => api('DELETE', `/api/setup/tokens/${encodeURIComponent(token.name)}`))) load();
        });
        return h('li', {}, h('span', { class: 'item-name' }, token.name), h('span', { class: 'badge' }, token.control ? 'lecture et commandes' : 'lecture seule'), revoke);
      })
      : [h('li', { class: 'meta' }, 'Aucun jeton pour l’instant.')]));
  });
  form.addEventListener('submit', async (event) => {
    event.preventDefault();
    await attempt(async () => {
      const created = await api('POST', '/api/setup/tokens', { name: name.value, control: control.checked });
      name.value = '';
      control.checked = false;
      const command = `claude mcp add --transport http hestia ${window.location.origin}/mcp --header "Authorization: Bearer ${created.token}"`;
      revealed.replaceChildren(h('div', { class: 'reveal' },
        line(`Jeton « ${created.name} » créé. Copie-le maintenant : il ne sera plus jamais affiché.`, 'pending-line'),
        h('code', { class: 'secret' }, created.token),
        copyButton('Copier le jeton', created.token),
        line('Commande pour Claude Code, à lancer sur ton PC :', 'meta'),
        h('code', { class: 'secret' }, command),
        copyButton('Copier la commande', command),
      ));
      load();
    });
  });
  load();
  return settingsCard('Accès IA (MCP)', 'Laisse une IA comme Claude lire l’état de la maison, et la piloter si tu le permets.', h('div', { class: 'setting-body' }, list, form, revealed));
}

function accountSettings(user) {
  const list = h('ul', { class: 'item-list' });
  const load = () => attempt(async () => {
    const accounts = await api('GET', '/api/setup/users');
    list.replaceChildren(...accounts.users.map((name) => {
      if (name === accounts.current) return h('li', {}, h('span', { class: 'item-name' }, name), h('span', { class: 'badge' }, 'toi'));
      const remove = h('button', { class: 'ghost danger' }, 'Supprimer');
      remove.addEventListener('click', async () => {
        if (!window.confirm(`Supprimer le compte « ${name} » ? Ses sessions seront fermées.`)) return;
        if (await attempt(() => api('DELETE', `/api/setup/users/${encodeURIComponent(name)}`))) load();
      });
      return h('li', {}, h('span', { class: 'item-name' }, name), remove);
    }));
  });

  const newName = h('input', { name: 'name', autocapitalize: 'none', autocomplete: 'off', required: true });
  const newPassword = h('input', { name: 'password', type: 'password', autocomplete: 'new-password', minlength: 10, required: true });
  const addForm = h('form', { class: 'inline-form' },
    h('label', { class: 'field-label' }, 'Identifiant', newName),
    h('label', { class: 'field-label' }, 'Mot de passe (10 caractères min.)', newPassword),
    h('button', { class: 'primary inline', type: 'submit' }, 'Ajouter le compte'),
  );
  addForm.addEventListener('submit', async (event) => {
    event.preventDefault();
    const ok = await attempt(() => api('POST', '/api/setup/users', { name: newName.value, password: newPassword.value }));
    if (ok) {
      toast(`Compte « ${newName.value.trim().toLowerCase()} » créé.`);
      addForm.reset();
      load();
    }
  });

  const current = h('input', { type: 'password', autocomplete: 'current-password', required: true });
  const fresh = h('input', { type: 'password', autocomplete: 'new-password', minlength: 10, required: true });
  const again = h('input', { type: 'password', autocomplete: 'new-password', minlength: 10, required: true });
  const passwordForm = h('form', { class: 'inline-form' },
    h('label', { class: 'field-label' }, 'Mot de passe actuel', current),
    h('label', { class: 'field-label' }, 'Nouveau', fresh),
    h('label', { class: 'field-label' }, 'Confirmation', again),
    h('button', { class: 'primary inline', type: 'submit' }, 'Changer'),
  );
  passwordForm.addEventListener('submit', async (event) => {
    event.preventDefault();
    if (fresh.value !== again.value) {
      toast('Les deux nouveaux mots de passe sont différents.');
      return;
    }
    if (await attempt(() => api('POST', '/api/setup/password', { current: current.value, new: fresh.value }))) {
      toast('Mot de passe changé.');
      passwordForm.reset();
    }
  });

  load();
  return settingsCard('Comptes', `Les membres du foyer qui peuvent se connecter. Tu es connecté en tant que ${user}.`,
    h('div', { class: 'setting-body' }, list,
      h('h4', {}, 'Ajouter un compte'), addForm,
      h('h4', {}, 'Changer mon mot de passe'), passwordForm));
}

function deviceSettings() {
  const list = h('ul', { class: 'item-list' });
  const manual = h('ul', { class: 'item-list' });
  const progress = line('', 'pending-line');
  const search = h('button', { class: 'primary inline' }, 'Rechercher des appareils');
  const address = h('input', { name: 'address', placeholder: '192.168.1.40', inputmode: 'decimal', autocomplete: 'off', required: true });
  const addForm = h('form', { class: 'inline-form' },
    h('label', { class: 'field-label' }, 'Adresse IP d’une lampe Govee', address),
    h('button', { class: 'primary inline', type: 'submit' }, 'Ajouter'),
  );
  let known = new Set();
  let fresh = [];

  const load = () => attempt(async () => {
    const [found, govee] = await Promise.all([api('GET', '/api/devices'), api('GET', '/api/setup/govee')]);
    fresh = known.size ? found.filter((device) => !known.has(device.id)) : [];
    known = new Set(found.map((device) => device.id));
    list.replaceChildren(...(found.length
      ? found.sort(byRoomThenName).map((device) => h('li', {},
        h('span', { class: 'item-name' }, device.name),
        h('span', { class: 'meta' }, device.model ?? device.id),
        fresh.some((added) => added.id === device.id) ? h('span', { class: 'badge new' }, 'nouveau') : null,
        h('span', { class: 'badge' }, device.available ? 'disponible' : 'injoignable'),
      ))
      : [h('li', { class: 'meta' }, 'Aucun appareil pour l’instant.')]));
    manual.replaceChildren(...govee.manual.map((ip) => {
      const remove = h('button', { class: 'ghost danger' }, 'Retirer');
      remove.addEventListener('click', async () => {
        if (await attempt(() => api('DELETE', `/api/setup/govee/addresses/${encodeURIComponent(ip)}`))) load();
      });
      return h('li', {}, h('span', { class: 'item-name' }, ip), h('span', { class: 'badge' }, 'ajoutée à la main'), remove);
    }));
  });

  const lookAround = async (wait) => {
    search.disabled = true;
    progress.textContent = 'Recherche sur tout le réseau…';
    await new Promise((resolve) => setTimeout(resolve, wait));
    await load();
    progress.textContent = fresh.length
      ? `${fresh.length} nouvel appareil trouvé : ${fresh.map((device) => device.name).join(', ')}.`
      : 'Aucun nouvel appareil. Pour une lampe Govee qui n’apparaît pas : active « Contrôle LAN » dans l’app Govee, ou ajoute son adresse IP ci-dessous (Freebox OS → Périphériques réseau).';
    search.disabled = false;
  };
  search.addEventListener('click', async () => {
    if (await attempt(() => api('POST', '/api/setup/discover'))) await lookAround(8000);
  });
  addForm.addEventListener('submit', async (event) => {
    event.preventDefault();
    if (await attempt(() => api('POST', '/api/setup/govee/addresses', { address: address.value }))) {
      addForm.reset();
      await lookAround(5000);
    }
  });

  load();
  return settingsCard('Appareils', 'Les appareils trouvés sur le réseau. Une recherche interroge toutes les adresses du réseau local.',
    h('div', { class: 'setting-body' }, list, search, progress, addForm, manual));
}

function updateSettings() {
  const body = h('div', { class: 'setting-body' }, line('Chargement…', 'meta'));
  const load = () => attempt(async () => {
    const status = await api('GET', '/api/setup/update');
    const parts = [line(`Version en service : ${status.version}${status.from_update ? ' (installée depuis cette page)' : ''}`)];
    if (!status.supported) {
      parts.push(line('La mise à jour depuis l’interface ne fonctionne que sur le Raspberry Pi.', 'meta'));
    } else {
      parts.push(onlineUpdate());
      const files = h('input', { type: 'file', multiple: true, 'aria-label': 'Fichiers hestia et hestia.sig' });
      const install = h('button', { class: 'primary inline' }, 'Installer');
      const progress = line('', 'pending-line');
      install.addEventListener('click', async () => {
        const chosen = [...files.files];
        const program = chosen.find((file) => !file.name.endsWith('.sig'));
        const signature = chosen.find((file) => file.name.endsWith('.sig'));
        if (!program || !signature) {
          toast('Choisis les deux fichiers ensemble : hestia et hestia.sig.');
          return;
        }
        install.disabled = true;
        progress.textContent = 'Envoi et vérification de la signature…';
        try {
          const response = await fetch('/api/setup/update', {
            method: 'POST',
            credentials: 'same-origin',
            headers: { 'Content-Type': 'application/octet-stream', 'X-Hestia-Signature': (await signature.text()).trim() },
            body: program,
          });
          const data = await response.json().catch(() => ({}));
          if (!response.ok) throw new HttpError(response.status, data.message || `Erreur ${response.status}`);
          progress.textContent = `Version ${data.version} installée. Redémarrage…`;
          await waitForRestart();
        } catch (error) {
          toast(error.message);
          progress.textContent = '';
          install.disabled = false;
        }
      });
      parts.push(
        line('Ou installe à la main les fichiers hestia et hestia.sig préparés sur ton PC (dossier deploy).', 'meta'),
        files,
        install,
        progress,
      );
    }
    if (status.can_roll_back) {
      const rollBack = h('button', { class: 'ghost' }, 'Revenir à la version précédente');
      rollBack.addEventListener('click', async () => {
        if (!window.confirm('Revenir à la version précédente ? Hestia redémarre en quelques secondes.')) return;
        if (await attempt(() => api('POST', '/api/setup/update/rollback'))) await waitForRestart();
      });
      parts.push(rollBack);
    }
    body.replaceChildren(...parts);
  });
  load();
  return settingsCard('Mise à jour', 'Installer une nouvelle version de Hestia, signée avec ta clé de publication.', body);
}

// Asks GitHub for the latest published release, then offers to install it.
function onlineUpdate() {
  const check = h('button', { class: 'primary inline' }, 'Chercher une mise à jour');
  const result = h('div', { class: 'setting-body' });
  const lookUp = async () => {
    check.disabled = true;
    result.replaceChildren(line('Interrogation de GitHub…', 'pending-line'));
    const ok = await attempt(async () => {
      const report = await api('GET', '/api/setup/update/latest');
      availableUpdate = report.update_available ? report.latest : null;
      const published = report.published ? ` (publiée le ${new Date(report.published).toLocaleDateString('fr-FR')})` : '';
      if (!report.update_available) {
        result.replaceChildren(line(`Hestia est à jour. Dernière version publiée : ${report.latest}${published}.`, 'meta'));
        return;
      }
      const install = h('button', { class: 'primary inline' }, `Installer la version ${report.latest}`);
      const progress = line('', 'pending-line');
      install.addEventListener('click', async () => {
        install.disabled = true;
        progress.textContent = 'Téléchargement et vérification de la signature…';
        if (await attempt(async () => {
          const done = await api('POST', '/api/setup/update/latest');
          progress.textContent = `Version ${done.version} installée. Redémarrage…`;
        })) {
          await waitForRestart();
        } else {
          progress.textContent = '';
          install.disabled = false;
        }
      });
      result.replaceChildren(line(`Nouvelle version disponible : ${report.latest}${published}.`), install, progress);
    });
    if (!ok) result.replaceChildren();
    check.disabled = false;
  };
  check.addEventListener('click', lookUp);
  // The Pi already saw a new version (dot on « Réglages »): show it without a click.
  if (availableUpdate) lookUp();
  return h('div', { class: 'setting-body' }, check, result);
}

// The service stops after an update and systemd starts the new program: wait, then reload.
async function waitForRestart() {
  stopEvents();
  await new Promise((resolve) => setTimeout(resolve, 2500));
  for (let tries = 0; tries < 60; tries += 1) {
    try {
      const response = await fetch('/api/session', { credentials: 'same-origin' });
      if (response.ok) {
        window.location.reload();
        return;
      }
    } catch {
      // Still restarting.
    }
    await new Promise((resolve) => setTimeout(resolve, 1000));
  }
  toast('Hestia ne répond pas encore : recharge la page dans un moment.');
}

// ---- Live updates ------------------------------------------------------------------------

function setLive(live) {
  const dot = document.getElementById('status');
  if (!dot) return;
  dot.classList.toggle('live', live);
  dot.title = live ? 'Temps réel : connecté' : 'Temps réel : reconnexion…';
}

function startEvents() {
  stopEvents();
  let opened = false;
  events = new EventSource('/api/events');
  events.addEventListener('open', () => {
    if (opened) scheduleReload();
    opened = true;
    setLive(true);
  });
  events.addEventListener('error', () => {
    setLive(false);
    if (events?.readyState === EventSource.CLOSED) {
      // Closed for good (e.g. session expired): check, then retry.
      api('GET', '/api/session').then(() => setTimeout(startEvents, 3000)).catch(() => {});
    }
  });
  events.addEventListener('resync', scheduleReload);
  events.addEventListener('message', (message) => apply(JSON.parse(message.data)));
}

function stopEvents() {
  clearInterval(chartTimer);
  events?.close();
  events = null;
}

function apply(event) {
  const device = devices.get(event.device);
  if (event.type === 'device_updated' || !device) {
    scheduleReload();
    return;
  }
  if (event.type === 'state') {
    const spec = point(device, event.point);
    if (!spec) return;
    spec.value = event.value;
    spec.at = event.at;
  } else if (event.type === 'availability') {
    device.available = event.available;
  } else {
    return;
  }
  cards.get(device.id)?.update(device);
}

function scheduleReload() {
  clearTimeout(reloadTimer);
  reloadTimer = setTimeout(reload, 300);
}

async function reload() {
  if (onSettingsPage()) return;
  try {
    const list = await api('GET', '/api/devices');
    const sameDevices = list.length === devices.size && list.every((device) => devices.has(device.id) && isLight(device) === isLight(devices.get(device.id)));
    setDevices(list);
    if (sameDevices && cards.size === devices.size) {
      for (const device of devices.values()) cards.get(device.id)?.update(device);
    } else {
      const [session, configured] = await Promise.all([api('GET', '/api/session'), api('GET', '/api/charts')]);
      charts = configured;
      currentUser = session.user;
      availableUpdate = session.update ?? null;
      renderDashboard(currentUser);
    }
  } catch (error) {
    if (error.status !== 401) setLive(false);
  }
}

window.addEventListener('hashchange', () => {
  if (currentUser) route();
});

document.addEventListener('visibilitychange', () => {
  if (document.visibilityState === 'visible' && events) scheduleReload();
});

// ---- Boot ----------------------------------------------------------------------------

showDashboard().catch((error) => {
  if (error.status === 401) return;
  const retry = h('button', { class: 'pill' }, 'Réessayer');
  retry.addEventListener('click', () => window.location.reload());
  app.removeAttribute('aria-busy');
  app.replaceChildren(h('div', { class: 'login-screen' }, h('div', { class: 'login-box' }, h('p', { class: 'empty' }, 'Le serveur ne répond pas.'), retry)));
});
