// The Automatisations page: scenes, the house's location, and automations drawn as graphs
// (n8n style): nodes linked by wires on a canvas, a side panel to set the selected node.

const NODE_W = 210;
const NODE_H = 78;
const GRID = 10;
const DAY_LABELS = ['Lun', 'Mar', 'Mer', 'Jeu', 'Ven', 'Sam', 'Dim'];

const STEPS = {
  when_state: { group: 'trigger', title: 'Quand un appareil change' },
  when_threshold: { group: 'trigger', title: 'Quand une valeur passe un seuil' },
  at_time: { group: 'trigger', title: 'À une heure' },
  at_sun: { group: 'trigger', title: 'Lever ou coucher du soleil' },
  every: { group: 'trigger', title: 'Toutes les N minutes' },
  presence: { group: 'trigger', title: 'Arrivée ou départ' },
  on_start: { group: 'trigger', title: 'Au démarrage de Hestia' },
  manual: { group: 'trigger', title: 'À la main' },
  if: { group: 'logic', title: 'Si…' },
  wait: { group: 'logic', title: 'Attendre' },
  wait_for: { group: 'logic', title: 'Attendre que…' },
  set: { group: 'action', title: 'Régler un appareil' },
  toggle: { group: 'action', title: 'Basculer' },
  scene: { group: 'action', title: 'Lancer une scène' },
  lights_off: { group: 'action', title: 'Éteindre toutes les lumières' },
};
const GROUPS = { trigger: 'Déclencheurs', logic: 'Logique', action: 'Actions' };
const PORT_LABELS = { yes: 'oui', no: 'non', ok: 'ok', timeout: 'délai' };
const STATUS_PORT = { done: 'out', simulated: 'out', yes: 'yes', no: 'no', ok: 'ok', timeout: 'timeout' };
const STATUS_LABELS = {
  done: 'fait', failed: 'échec', simulated: 'simulé', yes: 'oui', no: 'non', ok: 'ok', timeout: 'délai dépassé',
};
const RUN_LABELS = { running: 'en cours', done: 'réussie', failed: 'en échec', cancelled: 'interrompue' };

export function automationsPage(ui, path) {
  const id = decodeURIComponent(path ?? '');
  if (id) return editorPage(ui, id === 'nouvelle' ? null : id);
  return listPage(ui);
}

// ---- Shared ---------------------------------------------------------------------------

function ports(step) {
  if (step.type === 'if') return ['yes', 'no'];
  if (step.type === 'wait_for') return ['ok', 'timeout'];
  return ['out'];
}

function isTrigger(step) {
  return STEPS[step.type]?.group === 'trigger';
}

function describeDays(days) {
  if (!days?.length || days.length === 7) return 'tous les jours';
  const sorted = [...days].sort();
  if (sorted.join() === '1,2,3,4,5') return 'en semaine';
  if (sorted.join() === '6,7') return 'le week-end';
  return sorted.map((day) => DAY_LABELS[day - 1]).join(', ');
}

function duration(seconds) {
  if (seconds % 3600 === 0) return `${seconds / 3600} h`;
  if (seconds % 60 === 0) return `${seconds / 60} min`;
  return `${seconds} s`;
}

function pointName(ui, deviceId, key) {
  const device = ui.devices.get(deviceId);
  if (!device) return `${deviceId} · ${key}`;
  const spec = ui.point(device, key);
  return `${device.name} · ${spec?.label ?? key}`;
}

function showValue(ui, deviceId, key, value) {
  const device = ui.devices.get(deviceId);
  const spec = device ? ui.point(device, key) : null;
  return ui.formatValue({ ...(spec ?? {}), value });
}

function describeRule(ui, rule) {
  switch (rule.kind) {
    case 'state': {
      const ops = { eq: '=', ne: '≠', gt: '>', ge: '≥', lt: '<', le: '≤' };
      return `${pointName(ui, rule.device, rule.point)} ${ops[rule.op ?? 'eq']} ${showValue(ui, rule.device, rule.point, rule.value)}`;
    }
    case 'time': {
      const window = [rule.after ? `après ${rule.after}` : '', rule.before ? `avant ${rule.before}` : ''].filter(Boolean).join(' et ');
      return `${window || 'toute la journée'}, ${describeDays(rule.days)}`;
    }
    case 'sun': return rule.is === 'day' ? 'il fait jour' : 'il fait nuit';
    case 'presence': return rule.is === 'someone' ? 'quelqu’un est là' : 'personne n’est là';
    default: return rule.kind;
  }
}

function describeStep(ui, step, scenes) {
  switch (step.type) {
    case 'when_state': {
      const parts = [pointName(ui, step.device, step.point)];
      if (step.from !== undefined && step.from !== null) parts.push(`de ${showValue(ui, step.device, step.point, step.from)}`);
      if (step.to !== undefined && step.to !== null) parts.push(`à ${showValue(ui, step.device, step.point, step.to)}`);
      if (step.for_s) parts.push(`pendant ${duration(step.for_s)}`);
      return parts.join(' ');
    }
    case 'when_threshold': {
      const limits = [step.above != null ? `> ${step.above}` : '', step.below != null ? `< ${step.below}` : ''].filter(Boolean).join(' ou ');
      return `${pointName(ui, step.device, step.point)} ${limits}${step.for_s ? ` pendant ${duration(step.for_s)}` : ''}`;
    }
    case 'at_time': return `${step.at}, ${describeDays(step.days)}`;
    case 'at_sun': {
      const offset = step.offset_min ? ` ${step.offset_min > 0 ? '+' : '−'}${Math.abs(step.offset_min)} min` : '';
      return `${step.event === 'sunrise' ? 'Lever' : 'Coucher'} du soleil${offset}, ${describeDays(step.days)}`;
    }
    case 'every': return `Toutes les ${step.minutes} min`;
    case 'presence': return step.event === 'first_arrival' ? 'Quelqu’un arrive (maison vide)' : 'Le dernier part';
    case 'on_start': return 'Quand Hestia démarre';
    case 'manual': return 'Bouton « Lancer »';
    case 'if': return step.rules.map((rule) => describeRule(ui, rule)).join(step.all === false ? ' ou ' : ' et ') || 'aucune règle';
    case 'wait': return duration(step.seconds);
    case 'wait_for': return `${describeRule(ui, step.rule)} (au plus ${duration(step.timeout_s)})`;
    case 'set': return `${pointName(ui, step.device, step.point)} → ${showValue(ui, step.device, step.point, step.value)}`;
    case 'toggle': return pointName(ui, step.device, step.point);
    case 'scene': return `« ${scenes.find((scene) => scene.id === step.scene)?.name ?? '?'} »`;
    case 'lights_off': return 'Toutes les lampes allumées';
    default: return step.type;
  }
}

function sentence(ui, automation, scenes) {
  const triggers = automation.graph.nodes.filter(isTrigger).map((node) => describeStep(ui, node, scenes));
  const actions = automation.graph.nodes.filter((node) => !isTrigger(node)).length;
  return `${triggers.join(' · ') || 'Aucun déclencheur'} → ${actions} étape${actions > 1 ? 's' : ''}`;
}

function ago(ms) {
  const minutes = Math.round((Date.now() - ms) / 60000);
  if (minutes < 1) return 'à l’instant';
  if (minutes < 60) return `il y a ${minutes} min`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `il y a ${hours} h`;
  return new Date(ms).toLocaleDateString('fr-FR', { day: 'numeric', month: 'short' });
}

// Devices with points matching `filter`, for the pickers.
function choices(ui, filter) {
  return [...ui.devices.values()].sort(ui.byRoomThenName).flatMap((device) => device.points
    .filter((spec) => filter(spec, device))
    .map((spec) => ({ device, spec })));
}

function defaultValue(spec) {
  if (!spec) return true;
  switch (spec.kind.type) {
    case 'binary': return true;
    case 'numeric': return spec.value ?? spec.kind.min;
    case 'enum': return spec.kind.values[0] ?? '';
    case 'color': return { r: 255, g: 180, b: 80 };
    default: return '';
  }
}

function newStep(ui, type, scenes) {
  const first = (filter) => choices(ui, filter)[0];
  const writable = (spec) => spec.access === 'read_write';
  switch (type) {
    case 'when_state': {
      const pick = first((spec) => spec.kind.type === 'binary') ?? first(() => true);
      return { type, device: pick?.device.id ?? '', point: pick?.spec.key ?? '', to: pick ? defaultValue(pick.spec) : true, for_s: 0 };
    }
    case 'when_threshold': {
      const pick = first((spec) => spec.kind.type === 'numeric');
      return { type, device: pick?.device.id ?? '', point: pick?.spec.key ?? '', above: pick?.spec.value ?? 0, for_s: 0 };
    }
    case 'at_time': return { type, at: '07:00', days: [] };
    case 'at_sun': return { type, event: 'sunset', offset_min: 0, days: [] };
    case 'every': return { type, minutes: 15 };
    case 'presence': return { type, event: 'last_departure' };
    case 'if': return { type, all: true, rules: [{ kind: 'presence', is: 'someone' }] };
    case 'wait': return { type, seconds: 60 };
    case 'wait_for': return { type, rule: { kind: 'presence', is: 'nobody' }, timeout_s: 600 };
    case 'set': {
      const pick = first((spec) => writable(spec) && spec.semantic === 'on_off') ?? first(writable);
      return { type, device: pick?.device.id ?? '', point: pick?.spec.key ?? '', value: pick ? defaultValue(pick.spec) : true };
    }
    case 'toggle': {
      const pick = first((spec) => writable(spec) && spec.kind.type === 'binary');
      return { type, device: pick?.device.id ?? '', point: pick?.spec.key ?? '' };
    }
    case 'scene': return { type, scene: scenes[0]?.id ?? '' };
    default: return { type };
  }
}

// ---- List ------------------------------------------------------------------------------

function listPage(ui) {
  const { h, api, attempt, line, toast } = ui;
  const body = h('div', { class: 'content settings' }, h('h2', { class: 'page-title' }, 'Automatisations'), line('Chargement…', 'meta'));
  const load = () => attempt(async () => {
    const status = await api('GET', '/api/automations');
    const { document: doc } = status;
    const lastRun = new Map();
    for (const run of status.runs) if (!lastRun.has(run.automation) && !run.dry) lastRun.set(run.automation, run);

    const save = async (next) => {
      try {
        await api('PUT', '/api/automations', next);
        return true;
      } catch (error) {
        if (error.status !== 401) toast(error.message);
        return false;
      }
    };

    const create = h('a', { class: 'primary inline', href: '#automatisations/nouvelle' }, '+ Nouvelle automatisation');
    const list = h('ul', { class: 'item-list automation-list' }, ...(doc.automations.length
      ? doc.automations.map((automation) => {
        const toggle = h('input', { type: 'checkbox', checked: automation.enabled, 'aria-label': 'Active' });
        toggle.addEventListener('change', async () => {
          const automations = doc.automations.map((other) => (other.id === automation.id ? { ...other, enabled: toggle.checked, note: null } : other));
          if (await save({ ...doc, automations })) load();
          else toggle.checked = !toggle.checked;
        });
        const run = h('button', { class: 'pill' }, ui.icon('play'), 'Lancer');
        run.addEventListener('click', async () => {
          if (await attempt(() => api('POST', `/api/automations/${encodeURIComponent(automation.id)}/run`))) {
            toast(`« ${automation.name} » lancée.`);
            setTimeout(load, 1500);
          }
        });
        const last = lastRun.get(automation.id);
        return h('li', { class: 'automation-item' },
          h('label', { class: 'check' }, toggle),
          h('div', { class: 'automation-text' },
            h('a', { class: 'item-name', href: `#automatisations/${encodeURIComponent(automation.id)}` }, automation.name),
            h('span', { class: 'meta' }, sentence(ui, automation, doc.scenes)),
            automation.note ? h('span', { class: 'form-error' }, automation.note) : null,
            last ? h('span', { class: `run-badge run-${last.status}` }, `Dernière exécution ${ago(last.started)} : ${RUN_LABELS[last.status]}`) : null),
          run);
      })
      : [h('li', { class: 'meta' }, 'Aucune automatisation pour l’instant.')]));

    body.replaceChildren(
      h('h2', { class: 'page-title' }, 'Automatisations'),
      ui.settingsCard('Automatisations', 'Des graphes « quand… si… alors… » qui s’exécutent tout seuls. Une case décochée met l’automatisation en pause.',
        h('div', { class: 'setting-body' }, list, create)),
      scenesCard(ui, doc, load, save),
      locationCard(ui, doc, status.sun, load, save),
    );
  });
  load();
  return body;
}

function scenesCard(ui, doc, reload, save) {
  const { h, api, attempt, toast, line } = ui;
  const list = h('ul', { class: 'item-list' }, ...(doc.scenes.length
    ? doc.scenes.map((scene) => {
      const run = h('button', { class: 'pill' }, 'Lancer');
      run.addEventListener('click', () => attempt(() => api('POST', `/api/scenes/${encodeURIComponent(scene.id)}/run`)));
      const remove = h('button', { class: 'ghost danger' }, 'Supprimer');
      remove.addEventListener('click', async () => {
        if (!confirm(`Supprimer la scène « ${scene.name} » ?`)) return;
        if (await save({ ...doc, scenes: doc.scenes.filter((other) => other.id !== scene.id) })) reload();
      });
      const lamps = [...new Set(scene.actions.map((action) => ui.devices.get(action.device)?.name ?? action.device))].join(', ');
      return h('li', {}, h('span', { class: 'item-name' }, scene.name), h('span', { class: 'meta' }, lamps), run, remove);
    })
    : [h('li', { class: 'meta' }, 'Aucune scène pour l’instant.')]));
  const lamps = [...ui.devices.values()].filter(ui.isLight).sort(ui.byRoomThenName);
  const name = h('input', { name: 'name', placeholder: 'Soirée', required: true });
  const picks = lamps.map((lamp) => ({ lamp, box: h('input', { type: 'checkbox', checked: ui.value(lamp, 'on') === true }) }));
  const form = h('form', { class: 'inline-form' },
    h('label', { class: 'field-label' }, 'Nom de la scène', name),
    h('div', { class: 'days' }, ...picks.map(({ lamp, box }) => h('label', { class: 'check' }, box, lamp.name))),
    h('button', { class: 'primary inline', type: 'submit' }, 'Enregistrer l’état actuel'));
  form.addEventListener('submit', async (event) => {
    event.preventDefault();
    const devices = picks.filter(({ box }) => box.checked).map(({ lamp }) => lamp.id);
    if (await attempt(() => api('POST', '/api/automations/scenes', { name: name.value, devices }))) {
      toast('Scène enregistrée.');
      reload();
    }
  });
  return ui.settingsCard('Scènes', 'Règle les lampes comme tu le veux, coche-les, puis enregistre : la scène retient leur allumage, leur luminosité et leur couleur. Chaque scène a un bouton sur le tableau de bord.',
    h('div', { class: 'setting-body' }, list, lamps.length ? form : line('Aucune lampe disponible.', 'meta')));
}

function locationCard(ui, doc, sun, reload, save) {
  const { h, line, toast } = ui;
  const latitude = h('input', { name: 'latitude', inputmode: 'decimal', placeholder: '48.85', value: doc.location?.latitude ?? '' });
  const longitude = h('input', { name: 'longitude', inputmode: 'decimal', placeholder: '2.35', value: doc.location?.longitude ?? '' });
  const here = h('button', { class: 'pill', type: 'button' }, 'Ma position');
  here.addEventListener('click', () => {
    if (!navigator.geolocation) {
      toast('Position indisponible sur ce navigateur.');
      return;
    }
    navigator.geolocation.getCurrentPosition(
      (position) => {
        latitude.value = position.coords.latitude.toFixed(4);
        longitude.value = position.coords.longitude.toFixed(4);
      },
      () => toast('Position refusée ou indisponible (elle demande une connexion https).'),
    );
  });
  const form = h('form', { class: 'inline-form' },
    h('label', { class: 'field-label' }, 'Latitude', latitude),
    h('label', { class: 'field-label' }, 'Longitude', longitude),
    here,
    h('button', { class: 'primary inline', type: 'submit' }, 'Enregistrer'));
  form.addEventListener('submit', async (event) => {
    event.preventDefault();
    const number = (input) => Number(input.value.replace(',', '.'));
    const location = latitude.value && longitude.value ? { latitude: number(latitude), longitude: number(longitude) } : null;
    if (await save({ ...doc, location })) reload();
  });
  return ui.settingsCard('Position de la maison', 'Pour le lever et le coucher du soleil, et les règles « il fait jour / nuit ».',
    h('div', { class: 'setting-body' },
      line(sun ? `Aujourd’hui : lever ${sun.sunrise}, coucher ${sun.sunset}.` : 'Position pas encore indiquée.', 'meta'),
      form));
}

// ---- Editor ----------------------------------------------------------------------------

function editorPage(ui, id) {
  const { h, api, toast } = ui;
  const root = h('div', { class: 'editor-page' }, h('p', { class: 'meta' }, 'Chargement…'));
  (async () => {
    let status;
    try {
      status = await api('GET', '/api/automations');
    } catch (error) {
      if (error.status !== 401) toast(error.message);
      return;
    }
    const doc = status.document;
    const saved = id ? doc.automations.find((automation) => automation.id === id) : null;
    if (id && !saved) {
      root.replaceChildren(h('p', { class: 'empty' }, 'Cette automatisation n’existe plus. ', h('a', { href: '#automatisations' }, 'Retour à la liste')));
      return;
    }
    const automation = structuredClone(saved ?? {
      id: '',
      name: 'Nouvelle automatisation',
      enabled: true,
      mode: 'restart',
      graph: { nodes: [{ id: 't', ...newStep(ui, 'manual', doc.scenes), x: 40, y: 80 }], edges: [] },
    });
    const runs = status.runs.filter((run) => run.automation === automation.id);
    buildEditor(ui, root, doc, automation, runs);
  })();
  return root;
}

function buildEditor(ui, root, doc, automation, initialRuns) {
  const { h, api, toast, icon } = ui;
  const graph = automation.graph;
  const state = { selected: null, view: { x: 40, y: 70, k: 1 }, statuses: new Map(), dirty: false };

  // -- Toolbar
  const name = h('input', { class: 'editor-name', value: automation.name, 'aria-label': 'Nom' });
  name.addEventListener('input', () => { automation.name = name.value; state.dirty = true; });
  const enabled = h('input', { type: 'checkbox', checked: automation.enabled });
  enabled.addEventListener('change', () => { automation.enabled = enabled.checked; state.dirty = true; });
  const mode = h('select', { 'aria-label': 'Si elle est déjà en cours' },
    h('option', { value: 'restart' }, 'Redéclenchée : recommence'),
    h('option', { value: 'single' }, 'Redéclenchée : ignore'),
    h('option', { value: 'queued' }, 'Redéclenchée : à la suite'));
  mode.value = automation.mode ?? 'restart';
  mode.addEventListener('change', () => { automation.mode = mode.value; state.dirty = true; });
  const back = h('a', { class: 'ghost', href: '#automatisations' }, '← Liste');
  back.addEventListener('click', (event) => {
    if (state.dirty && !confirm('Quitter sans enregistrer les changements ?')) event.preventDefault();
  });
  const testButton = h('button', { class: 'pill' }, 'Tester');
  const runButton = h('button', { class: 'pill' }, icon('play'), 'Lancer');
  const saveButton = h('button', { class: 'primary inline' }, 'Enregistrer');
  const deleteButton = h('button', { class: 'ghost danger' }, 'Supprimer');
  const toolbar = h('div', { class: 'editor-toolbar' },
    back, name, h('label', { class: 'check' }, enabled, 'Active'), mode, testButton, runButton, saveButton, deleteButton);

  // -- Canvas
  const svg = document_createSvg('svg', { class: 'edges' });
  const world = h('div', { class: 'world' }, svg);
  const canvas = h('div', { class: 'canvas', tabindex: '0' }, world);
  const addButton = h('button', { class: 'pill' }, '+ Ajouter une étape');
  const tidyButton = h('button', { class: 'pill' }, 'Ranger');
  const zoomOut = h('button', { class: 'pill', 'aria-label': 'Dézoomer' }, '−');
  const zoomIn = h('button', { class: 'pill', 'aria-label': 'Zoomer' }, '+');
  const canvasTools = h('div', { class: 'canvas-tools' }, addButton, tidyButton, zoomOut, zoomIn);
  const palette = h('div', { class: 'palette', hidden: true });
  const stage = h('div', { class: 'stage' }, canvas, canvasTools, palette);
  const inspector = h('aside', { class: 'inspector' });
  const history = h('section', { class: 'card setting run-history' });

  root.replaceChildren(toolbar, h('div', { class: 'editor-body' }, stage, inspector), history);

  const markDirty = () => { state.dirty = true; };
  const nodeById = (nodeId) => graph.nodes.find((node) => node.id === nodeId);
  const nodeElements = new Map();

  function applyView() {
    world.style.transform = `translate(${state.view.x}px, ${state.view.y}px) scale(${state.view.k})`;
  }

  function portPosition(node, port) {
    const list = ports(node);
    const index = Math.max(0, list.indexOf(port));
    return { x: node.x + NODE_W, y: node.y + (NODE_H * (index + 1)) / (list.length + 1) };
  }

  function inputPosition(node) {
    return { x: node.x, y: node.y + NODE_H / 2 };
  }

  function curve(from, to) {
    const dx = Math.max(40, Math.abs(to.x - from.x) / 2);
    return `M ${from.x} ${from.y} C ${from.x + dx} ${from.y}, ${to.x - dx} ${to.y}, ${to.x} ${to.y}`;
  }

  function takenEdge(edge) {
    const status = state.statuses.get(edge.from);
    return status && STATUS_PORT[status] === edge.port && state.statuses.has(edge.to);
  }

  function drawEdges() {
    svg.replaceChildren();
    graph.edges.forEach((edge, index) => {
      const from = nodeById(edge.from);
      const to = nodeById(edge.to);
      if (!from || !to) return;
      const d = curve(portPosition(from, edge.port), inputPosition(to));
      const selected = state.selected?.edge === index;
      const classes = ['edge', `port-${edge.port}`, selected ? 'selected' : '', takenEdge(edge) ? 'taken' : ''].filter(Boolean).join(' ');
      const hit = document_createSvg('path', { d, class: 'edge-hit' });
      hit.addEventListener('pointerdown', (event) => {
        event.stopPropagation();
        select({ edge: index });
      });
      svg.append(document_createSvg('path', { d, class: classes }), hit);
    });
  }

  function drawNodes() {
    for (const element of nodeElements.values()) element.remove();
    nodeElements.clear();
    for (const node of graph.nodes) {
      const meta = STEPS[node.type] ?? { group: 'action', title: node.type };
      const status = state.statuses.get(node.id);
      const element = h('div', {
        class: ['node', `group-${meta.group}`, state.selected?.node === node.id ? 'selected' : '', status ? `status-${status}` : ''].filter(Boolean).join(' '),
        'data-node': node.id,
      },
      h('div', { class: 'node-title' }, meta.title),
      h('div', { class: 'node-summary' }, describeStep(ui, node, doc.scenes)),
      status ? h('span', { class: 'node-status' }, STATUS_LABELS[status] ?? status) : null);
      element.style.left = `${node.x}px`;
      element.style.top = `${node.y}px`;
      if (!isTrigger(node)) element.append(h('span', { class: 'port in' }));
      ports(node).forEach((port, index, list) => {
        const handle = h('span', { class: `port out port-${port}`, 'data-port': port, title: PORT_LABELS[port] ?? '' },
          PORT_LABELS[port] ? h('span', { class: 'port-label' }, PORT_LABELS[port]) : null);
        handle.style.top = `${(NODE_H * (index + 1)) / (list.length + 1)}px`;
        handle.addEventListener('pointerdown', (event) => startLink(event, node, port));
        element.append(handle);
      });
      element.addEventListener('pointerdown', (event) => startDrag(event, node));
      world.append(element);
      nodeElements.set(node.id, element);
    }
  }

  function draw() {
    drawEdges();
    drawNodes();
  }

  function select(selection) {
    state.selected = selection;
    draw();
    drawInspector();
  }

  // -- Pointer handling: drag nodes, draw wires, pan and pinch.
  const pointers = new Map();
  let gesture = null;

  function toWorld(event) {
    const box = canvas.getBoundingClientRect();
    return { x: (event.clientX - box.left - state.view.x) / state.view.k, y: (event.clientY - box.top - state.view.y) / state.view.k };
  }

  function startDrag(event, node) {
    if (event.target.closest('.port')) return;
    event.stopPropagation();
    canvas.setPointerCapture(event.pointerId);
    const at = toWorld(event);
    gesture = { kind: 'drag', node, dx: at.x - node.x, dy: at.y - node.y, moved: false };
  }

  function startLink(event, node, port) {
    event.stopPropagation();
    canvas.setPointerCapture(event.pointerId);
    const temp = document_createSvg('path', { class: 'edge drawing' });
    svg.append(temp);
    gesture = { kind: 'link', node, port, temp };
  }

  canvas.addEventListener('pointerdown', (event) => {
    pointers.set(event.pointerId, { x: event.clientX, y: event.clientY });
    if (gesture?.kind === 'drag' || gesture?.kind === 'link') return;
    canvas.setPointerCapture(event.pointerId);
    if (pointers.size === 2) {
      const [a, b] = [...pointers.values()];
      gesture = { kind: 'pinch', distance: Math.hypot(a.x - b.x, a.y - b.y), k: state.view.k, center: { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 } };
    } else {
      gesture = { kind: 'pan', x: event.clientX, y: event.clientY, view: { ...state.view }, moved: false };
    }
  });

  canvas.addEventListener('pointermove', (event) => {
    if (pointers.has(event.pointerId)) pointers.set(event.pointerId, { x: event.clientX, y: event.clientY });
    if (!gesture) return;
    if (gesture.kind === 'drag') {
      const at = toWorld(event);
      const x = Math.round((at.x - gesture.dx) / GRID) * GRID;
      const y = Math.round((at.y - gesture.dy) / GRID) * GRID;
      if (x !== gesture.node.x || y !== gesture.node.y) {
        gesture.node.x = x;
        gesture.node.y = y;
        gesture.moved = true;
        const element = nodeElements.get(gesture.node.id);
        element.style.left = `${x}px`;
        element.style.top = `${y}px`;
        drawEdges();
      }
    } else if (gesture.kind === 'link') {
      gesture.temp.setAttribute('d', curve(portPosition(gesture.node, gesture.port), toWorld(event)));
    } else if (gesture.kind === 'pan') {
      state.view.x = gesture.view.x + event.clientX - gesture.x;
      state.view.y = gesture.view.y + event.clientY - gesture.y;
      gesture.moved ||= Math.abs(event.clientX - gesture.x) + Math.abs(event.clientY - gesture.y) > 4;
      applyView();
    } else if (gesture.kind === 'pinch' && pointers.size === 2) {
      const [a, b] = [...pointers.values()];
      const distance = Math.hypot(a.x - b.x, a.y - b.y);
      zoomAt(gesture.center, (gesture.k * distance) / gesture.distance);
    }
  });

  const endPointer = (event) => {
    pointers.delete(event.pointerId);
    if (!gesture) return;
    const done = gesture;
    if (done.kind === 'pinch' && pointers.size > 0) return;
    gesture = null;
    if (done.kind === 'drag') {
      if (done.moved) markDirty();
      select({ node: done.node.id });
    } else if (done.kind === 'link') {
      done.temp.remove();
      const target = document.elementFromPoint(event.clientX, event.clientY)?.closest('.node');
      if (target) connect(done.node.id, done.port, target.dataset.node);
      else {
        const at = toWorld(event);
        openPalette({ x: at.x, y: at.y - NODE_H / 2 }, { from: done.node.id, port: done.port });
      }
    } else if (done.kind === 'pan' && !done.moved) {
      select(null);
    }
  };
  canvas.addEventListener('pointerup', endPointer);
  canvas.addEventListener('pointercancel', endPointer);

  function zoomAt(client, k) {
    const box = canvas.getBoundingClientRect();
    const next = Math.min(1.8, Math.max(0.35, k));
    const px = client.x - box.left;
    const py = client.y - box.top;
    state.view.x = px - ((px - state.view.x) * next) / state.view.k;
    state.view.y = py - ((py - state.view.y) * next) / state.view.k;
    state.view.k = next;
    applyView();
  }

  canvas.addEventListener('wheel', (event) => {
    event.preventDefault();
    zoomAt({ x: event.clientX, y: event.clientY }, state.view.k * (event.deltaY < 0 ? 1.1 : 1 / 1.1));
  }, { passive: false });

  const zoomCenter = (factor) => {
    const box = canvas.getBoundingClientRect();
    zoomAt({ x: box.left + box.width / 2, y: box.top + box.height / 2 }, state.view.k * factor);
  };
  zoomIn.addEventListener('click', () => zoomCenter(1.2));
  zoomOut.addEventListener('click', () => zoomCenter(1 / 1.2));

  canvas.addEventListener('keydown', (event) => {
    if ((event.key === 'Delete' || event.key === 'Backspace') && state.selected && !event.target.closest('input, select, textarea')) {
      event.preventDefault();
      removeSelected();
    }
  });

  // -- Graph editing
  function reaches(from, to) {
    const seen = new Set();
    const stack = [from];
    while (stack.length) {
      const current = stack.pop();
      if (current === to) return true;
      if (seen.has(current)) continue;
      seen.add(current);
      for (const edge of graph.edges) if (edge.from === current) stack.push(edge.to);
    }
    return false;
  }

  function connect(from, port, to) {
    const target = nodeById(to);
    if (!target || from === to) return;
    if (isTrigger(target)) {
      toast('Un déclencheur commence l’automatisation : rien ne peut y mener.');
      return;
    }
    if (graph.edges.some((edge) => edge.from === from && edge.port === port && edge.to === to)) return;
    if (reaches(to, from)) {
      toast('Ce lien formerait une boucle.');
      return;
    }
    graph.edges.push({ from, port, to });
    markDirty();
    draw();
  }

  function nextId() {
    let index = graph.nodes.length + 1;
    while (graph.nodes.some((node) => node.id === `n${index}`)) index += 1;
    return `n${index}`;
  }

  // The nearest spot below `at` where the new node covers no other.
  function freeSpot(at) {
    const snap = (value) => Math.round(value / GRID) * GRID;
    const spot = { x: snap(at.x), y: snap(at.y) };
    const overlaps = () => graph.nodes.some((node) => Math.abs(node.x - spot.x) < NODE_W + 20 && Math.abs(node.y - spot.y) < NODE_H + 20);
    while (overlaps()) spot.y += NODE_H + 30;
    return spot;
  }

  function addNode(type, at, link) {
    const node = { id: nextId(), ...newStep(ui, type, doc.scenes), ...freeSpot(at) };
    graph.nodes.push(node);
    if (link && !isTrigger(node)) graph.edges.push({ from: link.from, port: link.port, to: node.id });
    markDirty();
    select({ node: node.id });
  }

  function removeSelected() {
    if (!state.selected) return;
    if (state.selected.node) {
      const nodeId = state.selected.node;
      graph.nodes = graph.nodes.filter((node) => node.id !== nodeId);
      graph.edges = graph.edges.filter((edge) => edge.from !== nodeId && edge.to !== nodeId);
    } else if (state.selected.edge !== undefined) {
      graph.edges.splice(state.selected.edge, 1);
    }
    markDirty();
    select(null);
  }

  function openPalette(at, link) {
    palette.hidden = false;
    const offered = (group) => Object.entries(STEPS)
      .filter(([type, meta]) => meta.group === group && !(link && meta.group === 'trigger') && !(type === 'scene' && !doc.scenes.length));
    palette.replaceChildren(...Object.entries(GROUPS)
      .filter(([group]) => offered(group).length)
      .map(([group, title]) => h('div', { class: 'palette-group' },
        h('h4', {}, title),
        ...offered(group).map(([type, meta]) => {
          const choice = h('button', { class: `palette-item group-${group}` }, meta.title);
          choice.addEventListener('click', () => {
            palette.hidden = true;
            addNode(type, at, link);
          });
          return choice;
        }))),
    h('button', { class: 'ghost palette-close' }, 'Fermer'));
    palette.querySelector('.palette-close').addEventListener('click', () => { palette.hidden = true; });
  }

  addButton.addEventListener('click', () => {
    const box = canvas.getBoundingClientRect();
    const center = { x: (box.width / 2 - state.view.x) / state.view.k - NODE_W / 2, y: (box.height / 2 - state.view.y) / state.view.k - NODE_H / 2 };
    const selected = state.selected?.node ? nodeById(state.selected.node) : null;
    const link = selected ? { from: selected.id, port: ports(selected)[0] } : null;
    openPalette(selected ? { x: selected.x + NODE_W + 80, y: selected.y } : center, link);
  });

  // Columns by depth from the triggers, rows in wire order.
  function tidy() {
    const depth = new Map();
    const visit = (nodeId, level) => {
      if ((depth.get(nodeId) ?? -1) >= level) return;
      depth.set(nodeId, level);
      for (const edge of graph.edges) if (edge.from === nodeId) visit(edge.to, level + 1);
    };
    graph.nodes.filter(isTrigger).forEach((node) => visit(node.id, 0));
    graph.nodes.forEach((node) => { if (!depth.has(node.id)) depth.set(node.id, 0); });
    const rows = new Map();
    for (const node of graph.nodes) {
      const level = depth.get(node.id);
      const row = rows.get(level) ?? 0;
      rows.set(level, row + 1);
      node.x = level * (NODE_W + 90);
      node.y = row * (NODE_H + 50);
    }
    state.view = { x: 40, y: 70, k: 1 };
    applyView();
    markDirty();
    draw();
  }
  tidyButton.addEventListener('click', tidy);

  // -- Inspector
  function field(label, input) {
    return h('label', { class: 'field-label' }, label, input);
  }

  function numberInput(value, onChange, attrs = {}) {
    const input = h('input', { type: 'number', value: value ?? '', ...attrs });
    input.addEventListener('input', () => onChange(input.value === '' ? null : Number(input.value)));
    return input;
  }

  function daysInput(days, onChange) {
    const boxes = DAY_LABELS.map((label, index) => ({ day: index + 1, box: h('input', { type: 'checkbox', checked: !days?.length || days.includes(index + 1) }), label }));
    for (const { box } of boxes) {
      box.addEventListener('change', () => {
        const chosen = boxes.filter((entry) => entry.box.checked).map((entry) => entry.day);
        onChange(chosen.length === 7 ? [] : chosen);
      });
    }
    return h('div', { class: 'days' }, ...boxes.map(({ box, label }) => h('label', { class: 'check' }, box, label)));
  }

  function durationInput(seconds, onChange) {
    const units = [['s', 1], ['min', 60], ['h', 3600]];
    const [, factor] = [...units].reverse().find(([, size]) => seconds && seconds % size === 0) ?? units[0];
    const amount = h('input', { type: 'number', min: '0', value: seconds ? seconds / factor : 0 });
    const unit = h('select', {}, ...units.map(([label, size]) => h('option', { value: String(size) }, label)));
    unit.value = String(factor);
    const update = () => onChange(Math.max(0, Math.round(Number(amount.value || 0) * Number(unit.value))));
    amount.addEventListener('input', update);
    unit.addEventListener('change', update);
    return h('div', { class: 'duration' }, amount, unit);
  }

  function valueInput(spec, value, onChange) {
    const kind = spec?.kind?.type ?? 'text';
    if (kind === 'binary') {
      const select = h('select', {}, h('option', { value: 'true' }, ui.formatValue({ ...spec, value: true })), h('option', { value: 'false' }, ui.formatValue({ ...spec, value: false })));
      select.value = String(value !== false);
      select.addEventListener('change', () => onChange(select.value === 'true'));
      return select;
    }
    if (kind === 'numeric') return numberInput(value, (number) => onChange(number ?? 0), { min: spec.kind.min, max: spec.kind.max, step: spec.kind.step });
    if (kind === 'enum') {
      const select = h('select', {}, ...spec.kind.values.map((option) => h('option', { value: option }, option)));
      select.value = value ?? spec.kind.values[0];
      select.addEventListener('change', () => onChange(select.value));
      return select;
    }
    if (kind === 'color') {
      const hex = typeof value === 'object' && value ? `#${[value.r, value.g, value.b].map((channel) => channel.toString(16).padStart(2, '0')).join('')}` : '#ffb450';
      const input = h('input', { type: 'color', value: hex });
      input.addEventListener('input', () => {
        const number = parseInt(input.value.slice(1), 16);
        onChange({ r: (number >> 16) & 255, g: (number >> 8) & 255, b: number & 255 });
      });
      return input;
    }
    const input = h('input', { value: value ?? '' });
    input.addEventListener('input', () => onChange(input.value));
    return input;
  }

  // Device and point of a step or rule, then its value fields when `withValue`.
  function pointPicker(target, filter, onPick) {
    const options = choices(ui, filter);
    const select = h('select', {}, ...options.map(({ device, spec }, index) => h('option', { value: String(index) }, `${device.name} · ${spec.label}`)));
    const current = options.findIndex(({ device, spec }) => device.id === target.device && spec.key === target.point);
    if (current >= 0) select.value = String(current);
    else if (target.device) select.prepend(h('option', { value: '', selected: true }, `${target.device} · ${target.point} (absent)`));
    select.addEventListener('change', () => {
      const pick = options[Number(select.value)];
      if (!pick) return;
      target.device = pick.device.id;
      target.point = pick.spec.key;
      onPick(pick.spec);
    });
    return select;
  }

  function specOf(target) {
    const device = ui.devices.get(target.device);
    return device ? ui.point(device, target.point) : null;
  }

  function ruleEditor(rule, onChange) {
    const box = h('div', { class: 'rule' });
    const render = () => {
      const kind = h('select', {},
        h('option', { value: 'state' }, 'Valeur d’un appareil'),
        h('option', { value: 'time' }, 'Plage horaire'),
        h('option', { value: 'sun' }, 'Jour ou nuit'),
        h('option', { value: 'presence' }, 'Présence'));
      kind.value = rule.kind;
      kind.addEventListener('change', () => {
        const fresh = { state: { kind: 'state', op: 'eq' }, time: { kind: 'time', after: '22:00', before: '07:00', days: [] }, sun: { kind: 'sun', is: 'night' }, presence: { kind: 'presence', is: 'someone' } }[kind.value];
        for (const key of Object.keys(rule)) delete rule[key];
        Object.assign(rule, fresh);
        if (rule.kind === 'state') {
          const pick = choices(ui, () => true)[0];
          rule.device = pick?.device.id ?? '';
          rule.point = pick?.spec.key ?? '';
          rule.value = pick ? defaultValue(pick.spec) : true;
        }
        onChange();
        render();
      });
      const parts = [kind];
      if (rule.kind === 'state') {
        const op = h('select', {}, ...[['eq', '='], ['ne', '≠'], ['gt', '>'], ['ge', '≥'], ['lt', '<'], ['le', '≤']].map(([value, label]) => h('option', { value }, label)));
        op.value = rule.op ?? 'eq';
        op.addEventListener('change', () => { rule.op = op.value; onChange(); });
        parts.push(pointPicker(rule, () => true, (spec) => { rule.value = defaultValue(spec); onChange(); render(); }), op,
          valueInput(specOf(rule), rule.value, (value) => { rule.value = value; onChange(); }));
      } else if (rule.kind === 'time') {
        const after = h('input', { type: 'time', value: rule.after ?? '' });
        const before = h('input', { type: 'time', value: rule.before ?? '' });
        after.addEventListener('input', () => { rule.after = after.value || null; onChange(); });
        before.addEventListener('input', () => { rule.before = before.value || null; onChange(); });
        parts.push(field('Après', after), field('Avant', before), daysInput(rule.days, (days) => { rule.days = days; onChange(); }));
      } else if (rule.kind === 'sun') {
        const is = h('select', {}, h('option', { value: 'day' }, 'Il fait jour'), h('option', { value: 'night' }, 'Il fait nuit'));
        is.value = rule.is;
        is.addEventListener('change', () => { rule.is = is.value; onChange(); });
        parts.push(is);
      } else if (rule.kind === 'presence') {
        const is = h('select', {}, h('option', { value: 'someone' }, 'Quelqu’un est là'), h('option', { value: 'nobody' }, 'Personne n’est là'));
        is.value = rule.is;
        is.addEventListener('change', () => { rule.is = is.value; onChange(); });
        parts.push(is);
      }
      box.replaceChildren(...parts);
    };
    render();
    return box;
  }

  function drawInspector() {
    const changed = () => { markDirty(); draw(); };
    if (state.selected?.edge !== undefined) {
      const remove = h('button', { class: 'ghost danger' }, 'Supprimer ce lien');
      remove.addEventListener('click', removeSelected);
      inspector.replaceChildren(h('h3', {}, 'Lien'), h('p', { class: 'meta' }, 'Touche Suppr pour l’effacer aussi.'), remove);
      return;
    }
    const node = state.selected?.node ? nodeById(state.selected.node) : null;
    if (!node) {
      inspector.replaceChildren(
        h('h3', {}, 'Mode d’emploi'),
        h('ul', { class: 'meta help' },
          h('li', {}, 'Commence par un déclencheur, puis ajoute des étapes.'),
          h('li', {}, 'Tire un fil depuis le rond à droite d’une étape vers la suivante ; relâché dans le vide, il propose une nouvelle étape.'),
          h('li', {}, 'Une condition « Si… » a deux sorties : oui et non.'),
          h('li', {}, 'Glisse le fond pour te déplacer, molette ou pincement pour zoomer.'),
          h('li', {}, '« Tester » suit le graphe sans rien commander.')));
      return;
    }
    const meta = STEPS[node.type];
    const fields = [];
    const writable = (spec) => spec.access === 'read_write';
    switch (node.type) {
      case 'when_state': {
        const any = h('input', { type: 'checkbox', checked: node.to === undefined || node.to === null });
        const toBox = h('div');
        const fromBox = h('div');
        const anyFrom = h('input', { type: 'checkbox', checked: node.from === undefined || node.from === null });
        const renderValues = () => {
          toBox.replaceChildren(any.checked ? h('span', { class: 'meta' }, 'n’importe quelle valeur') : valueInput(specOf(node), node.to, (value) => { node.to = value; changed(); }));
          fromBox.replaceChildren(anyFrom.checked ? h('span', { class: 'meta' }, 'n’importe quelle valeur') : valueInput(specOf(node), node.from, (value) => { node.from = value; changed(); }));
        };
        any.addEventListener('change', () => { node.to = any.checked ? null : defaultValue(specOf(node)); changed(); renderValues(); });
        anyFrom.addEventListener('change', () => { node.from = anyFrom.checked ? null : defaultValue(specOf(node)); changed(); renderValues(); });
        renderValues();
        fields.push(field('Appareil', pointPicker(node, () => true, () => { node.to = null; node.from = null; any.checked = true; anyFrom.checked = true; changed(); renderValues(); })),
          h('label', { class: 'check' }, any, 'Toute nouvelle valeur'), field('Devient', toBox),
          h('label', { class: 'check' }, anyFrom, 'Depuis n’importe quelle valeur'), field('Était', fromBox),
          field('Pendant au moins', durationInput(node.for_s ?? 0, (seconds) => { node.for_s = seconds; changed(); })));
        break;
      }
      case 'when_threshold':
        fields.push(field('Appareil', pointPicker(node, (spec) => spec.kind.type === 'numeric', changed)),
          field('Au-dessus de', numberInput(node.above, (value) => { node.above = value; changed(); }, { step: 'any' })),
          field('En dessous de', numberInput(node.below, (value) => { node.below = value; changed(); }, { step: 'any' })),
          field('Pendant au moins', durationInput(node.for_s ?? 0, (seconds) => { node.for_s = seconds; changed(); })));
        break;
      case 'at_time': {
        const at = h('input', { type: 'time', value: node.at });
        at.addEventListener('input', () => { node.at = at.value; changed(); });
        fields.push(field('Heure', at), daysInput(node.days, (days) => { node.days = days; changed(); }));
        break;
      }
      case 'at_sun': {
        const event = h('select', {}, h('option', { value: 'sunrise' }, 'Lever du soleil'), h('option', { value: 'sunset' }, 'Coucher du soleil'));
        event.value = node.event;
        event.addEventListener('change', () => { node.event = event.value; changed(); });
        fields.push(field('Moment', event),
          field('Décalage (minutes, négatif = avant)', numberInput(node.offset_min ?? 0, (value) => { node.offset_min = value ?? 0; changed(); }, { step: '5', min: '-360', max: '360' })),
          daysInput(node.days, (days) => { node.days = days; changed(); }));
        if (!doc.location) fields.push(h('p', { class: 'form-error' }, 'Indique la position de la maison dans la liste des automatisations.'));
        break;
      }
      case 'every':
        fields.push(field('Toutes les (minutes)', numberInput(node.minutes, (value) => { node.minutes = value ?? 1; changed(); }, { min: '1', max: '1440' })));
        break;
      case 'presence': {
        const event = h('select', {}, h('option', { value: 'first_arrival' }, 'Quelqu’un arrive dans la maison vide'), h('option', { value: 'last_departure' }, 'Le dernier part'));
        event.value = node.event;
        event.addEventListener('change', () => { node.event = event.value; changed(); });
        fields.push(field('Quand', event));
        break;
      }
      case 'if': {
        const all = h('select', {}, h('option', { value: 'true' }, 'Toutes les règles (et)'), h('option', { value: 'false' }, 'Une seule suffit (ou)'));
        all.value = String(node.all !== false);
        all.addEventListener('change', () => { node.all = all.value === 'true'; changed(); });
        const rules = h('div', { class: 'rules' });
        const renderRules = () => {
          rules.replaceChildren(...node.rules.map((rule, index) => {
            const remove = h('button', { class: 'ghost danger' }, 'Retirer');
            remove.addEventListener('click', () => { node.rules.splice(index, 1); changed(); renderRules(); });
            return h('div', { class: 'rule-row' }, ruleEditor(rule, changed), node.rules.length > 1 ? remove : null);
          }));
        };
        renderRules();
        const add = h('button', { class: 'pill' }, '+ Ajouter une règle');
        add.addEventListener('click', () => { node.rules.push({ kind: 'presence', is: 'someone' }); changed(); renderRules(); });
        fields.push(field('Il faut', all), rules, add);
        break;
      }
      case 'wait':
        fields.push(field('Durée', durationInput(node.seconds, (seconds) => { node.seconds = Math.max(1, seconds); changed(); })));
        break;
      case 'wait_for':
        fields.push(ruleEditor(node.rule, changed),
          field('Au plus', durationInput(node.timeout_s, (seconds) => { node.timeout_s = seconds; changed(); })));
        break;
      case 'set': {
        const valueBox = h('div');
        const renderValue = () => valueBox.replaceChildren(valueInput(specOf(node), node.value, (value) => { node.value = value; changed(); }));
        renderValue();
        fields.push(field('Appareil', pointPicker(node, writable, (spec) => { node.value = defaultValue(spec); changed(); renderValue(); })), field('Valeur', valueBox));
        break;
      }
      case 'toggle':
        fields.push(field('Appareil', pointPicker(node, (spec) => writable(spec) && spec.kind.type === 'binary', changed)));
        break;
      case 'scene': {
        const scene = h('select', {}, ...doc.scenes.map((option) => h('option', { value: option.id }, option.name)));
        scene.value = node.scene;
        scene.addEventListener('change', () => { node.scene = scene.value; changed(); });
        fields.push(field('Scène', scene));
        break;
      }
      default:
        fields.push(h('p', { class: 'meta' }, 'Rien à régler.'));
    }
    const remove = h('button', { class: 'ghost danger' }, 'Supprimer l’étape');
    remove.addEventListener('click', removeSelected);
    inspector.replaceChildren(h('h3', {}, meta?.title ?? node.type), ...fields, remove);
  }

  // -- Runs
  function showRun(run) {
    state.statuses = new Map(run.steps.map((step) => [step.node, step.status]));
    draw();
  }

  function drawHistory(runs) {
    history.replaceChildren(h('h3', {}, 'Exécutions'),
      runs.length
        ? h('ul', { class: 'item-list runs' }, ...runs.slice(0, 15).map((run) => {
          const steps = h('ol', { class: 'run-steps', hidden: true }, ...run.steps.map((step) => h('li', {},
            h('span', { class: `run-badge step-${step.status}` }, STATUS_LABELS[step.status] ?? step.status),
            ` ${STEPS[nodeById(step.node)?.type]?.title ?? step.node}`,
            step.detail ? h('span', { class: 'meta' }, ` — ${step.detail}`) : null)));
          const toggle = h('button', { class: 'ghost' }, `${new Date(run.started).toLocaleString('fr-FR', { dateStyle: 'short', timeStyle: 'medium' })} · ${run.dry ? 'essai' : run.why} · ${RUN_LABELS[run.status]}`);
          toggle.addEventListener('click', () => { steps.hidden = !steps.hidden; showRun(run); });
          return h('li', { class: `run-${run.status}` }, toggle, steps);
        }))
        : h('p', { class: 'meta' }, 'Pas encore d’exécution. « Tester » montre le chemin suivi sans rien commander.'));
  }

  async function refreshRuns() {
    if (!automation.id) return [];
    const runs = await api('GET', `/api/automations/runs?automation=${encodeURIComponent(automation.id)}&limit=15`).catch(() => []);
    drawHistory(runs);
    return runs;
  }

  testButton.addEventListener('click', async () => {
    testButton.disabled = true;
    try {
      const run = await api('POST', '/api/automations/test', automation);
      showRun(run);
      drawHistory([run, ...(await refreshRuns())]);
      toast(run.status === 'failed' ? 'Essai : une étape échouerait, voir le détail en bas.' : 'Essai terminé : le chemin suivi est coloré.');
    } catch (error) {
      if (error.status !== 401) toast(error.message);
    }
    testButton.disabled = false;
  });

  async function save() {
    try {
      const fresh = (await api('GET', '/api/automations')).document;
      const others = fresh.automations.filter((other) => other.id !== automation.id || !automation.id);
      const result = await api('PUT', '/api/automations', { ...fresh, automations: [...others, automation] });
      if (!automation.id) {
        const known = new Set(others.map((other) => other.id));
        automation.id = result.automations.find((other) => !known.has(other.id))?.id ?? '';
        history.hidden = false;
        window.history.replaceState(null, '', `#automatisations/${encodeURIComponent(automation.id)}`);
      }
      state.dirty = false;
      toast('Automatisation enregistrée.');
      return true;
    } catch (error) {
      if (error.status !== 401) toast(error.message);
      return false;
    }
  }
  saveButton.addEventListener('click', save);

  runButton.addEventListener('click', async () => {
    if ((state.dirty || !automation.id) && !(await save())) return;
    try {
      await api('POST', `/api/automations/${encodeURIComponent(automation.id)}/run`);
    } catch (error) {
      if (error.status !== 401) toast(error.message);
      return;
    }
    toast('Lancée : le chemin s’affiche au fur et à mesure.');
    for (let tick = 0; tick < 60; tick += 1) {
      await new Promise((resolve) => { setTimeout(resolve, 1000); });
      if (!root.isConnected) return;
      const [latest] = await refreshRuns();
      if (latest) showRun(latest);
      if (latest && latest.status !== 'running') return;
    }
  });

  deleteButton.addEventListener('click', async () => {
    if (!confirm(`Supprimer « ${automation.name} » ?`)) return;
    if (automation.id) {
      try {
        const fresh = (await api('GET', '/api/automations')).document;
        await api('PUT', '/api/automations', { ...fresh, automations: fresh.automations.filter((other) => other.id !== automation.id) });
      } catch (error) {
        if (error.status !== 401) toast(error.message);
        return;
      }
    }
    state.dirty = false;
    window.location.hash = '#automatisations';
  });

  applyView();
  draw();
  drawInspector();
  drawHistory(initialRuns);
  if (!automation.id) history.hidden = true;
  else refreshRuns();
}

// SVG elements need their namespace.
function document_createSvg(tag, attrs) {
  const element = document.createElementNS('http://www.w3.org/2000/svg', tag);
  for (const [key, value] of Object.entries(attrs)) element.setAttribute(key, value);
  return element;
}
