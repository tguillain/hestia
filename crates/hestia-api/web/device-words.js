// Plain words for what automations watch and do on devices: « Chambre s’allume »,
// « Chambre : allumé, 40 % »… Steps keep their stored form (points and values);
// these choices only translate between the two.

const STATE_WORDS = {
  on_off: ['allumé', 'éteint'],
  connectivity: ['en ligne', 'hors ligne'],
  presence: ['à la maison', 'absent'],
};

const EVENT_WORDS = {
  on_off: ['s’allume', 's’éteint', 's’allume ou s’éteint'],
  connectivity: ['se connecte', 'se déconnecte', 'se connecte ou se déconnecte'],
  presence: ['arrive à la maison', 'quitte la maison', 'arrive ou part'],
};

const NUMBER_OPS = [
  ['gt', 'au-dessus de'], ['lt', 'en dessous de'], ['ge', 'au moins'], ['le', 'au plus'], ['eq', 'égal à'], ['ne', 'différent de'],
];

export function kindOf(spec) {
  return spec?.kind?.type ?? 'text';
}

const writable = (spec) => spec.access === 'read_write';

export function stateWord(spec, on) {
  const words = STATE_WORDS[spec?.semantic];
  if (words) return words[on ? 0 : 1];
  return on ? 'oui' : 'non';
}

function warmth(kelvin) {
  if (kelvin <= 3500) return 'chaud';
  if (kelvin >= 5000) return 'froid';
  return 'neutre';
}

export function valueText(ui, spec, value) {
  if (kindOf(spec) === 'binary') return stateWord(spec, value !== false);
  if (spec?.semantic === 'color_temperature' && typeof value === 'number') return `${ui.formatValue({ ...spec, value })} (${warmth(value)})`;
  return ui.formatValue({ ...(spec ?? {}), value });
}

// Whether `value` suits the point, to keep it when the choice changes.
export function fits(spec, value) {
  switch (kindOf(spec)) {
    case 'binary': return typeof value === 'boolean';
    case 'numeric': return typeof value === 'number';
    case 'color': return typeof value === 'object' && value !== null;
    case 'enum': return spec.kind.values.includes(value);
    default: return typeof value === 'string';
  }
}

function lookup(ui, target) {
  const device = ui.devices.get(target.device);
  return { device, spec: device ? ui.point(device, target.point) : null, name: device?.name ?? target.device };
}

// ---- Actions: « Régler un appareil », the device as it should be afterwards

// Each setting the step makes, in a few words: « allumé », « 40 % », « blanc chaud »…
export function settingWords(ui, step) {
  const device = ui.devices.get(step.device);
  const specOf = (key) => (device ? ui.point(device, key) : null);
  const words = Object.entries(step.values ?? {}).map(([key, value]) => {
    const spec = specOf(key);
    if (!spec) return `${key} (introuvable)`;
    if (kindOf(spec) === 'binary') return STATE_WORDS[spec.semantic] ? stateWord(spec, value) : `${spec.label} ${value ? 'activé' : 'désactivé'}`;
    if (spec.semantic === 'brightness') return ui.formatValue({ ...spec, value });
    if (spec.semantic === 'color') return 'couleur';
    if (spec.semantic === 'color_temperature' && typeof value === 'number') return `blanc ${warmth(value)}`;
    return `${spec.label} ${valueText(ui, spec, value)}`;
  });
  if (step.toggle) {
    const spec = specOf(step.toggle);
    words.unshift(spec?.semantic === 'on_off' ? 'allumé ↔ éteint' : `${spec?.label ?? step.toggle} inversé`);
  }
  // Switched on or off first, as the engine does.
  const first = (word) => (word === 'allumé' || word === 'éteint' ? 0 : 1);
  return words.sort((a, b) => first(a) - first(b));
}

// The colour the lamp shows afterwards, for the swatch on the canvas; null when unknown.
export function appearance(ui, step) {
  const device = ui.devices.get(step.device);
  if (!device) return null;
  let shown = null;
  for (const [key, value] of Object.entries(step.values ?? {})) {
    const spec = ui.point(device, key);
    if (spec?.semantic === 'on_off' && value === false) return 'var(--muted)';
    if (spec?.semantic === 'color' && value) shown = `rgb(${value.r}, ${value.g}, ${value.b})`;
    if (spec?.semantic === 'color_temperature' && typeof value === 'number') shown = { chaud: '#ffc98f', neutre: '#fff3e0', froid: '#dce8ff' }[warmth(value)];
  }
  return shown;
}

// ---- Triggers: « Un appareil change »

export function eventChoices(device) {
  return device.points.flatMap((spec) => {
    const point = spec.key;
    if (kindOf(spec) === 'binary') {
      const [on, off, any] = EVENT_WORDS[spec.semantic]
        ?? [`« ${spec.label} » passe à oui`, `« ${spec.label} » passe à non`, `« ${spec.label} » change`];
      return [
        { key: `${point}:true`, label: on, patch: { point, to: true } },
        { key: `${point}:false`, label: off, patch: { point, to: false } },
        { key: `${point}:any`, label: any, patch: { point, to: null } },
      ];
    }
    return [
      { key: `${point}:any`, label: `« ${spec.label} » change`, patch: { point, to: null } },
      { key: `${point}:to`, label: `« ${spec.label} » prend une valeur précise`, spec, valueField: 'to', patch: { point } },
    ];
  });
}

export function eventKey(step, spec) {
  if (step.to === undefined || step.to === null) return `${step.point}:any`;
  if (kindOf(spec) === 'binary') return `${step.point}:${step.to !== false}`;
  return `${step.point}:to`;
}

export function eventTitle(ui, step) {
  const { spec, name } = lookup(ui, step);
  if (!spec) return `${name} : ${step.point} (introuvable)`;
  const any = step.to === undefined || step.to === null;
  const words = EVENT_WORDS[spec.semantic];
  if (kindOf(spec) === 'binary' && words) return `${name} ${words[any ? 2 : step.to === false ? 1 : 0]}`;
  if (any) return `${name} : ${spec.label} change`;
  return `${name} : ${spec.label} passe à ${valueText(ui, spec, step.to)}`;
}

// ---- Triggers: « Une mesure passe un seuil »

export function measureChoices(device) {
  return device.points
    .filter((spec) => kindOf(spec) === 'numeric')
    .map((spec) => ({ key: spec.key, label: spec.label, spec, patch: { point: spec.key } }));
}

export function thresholdTitle(ui, step) {
  const { spec, name } = lookup(ui, step);
  const label = spec?.label ?? step.point;
  const limits = [
    step.above != null ? `au-dessus de ${valueText(ui, spec, step.above)}` : '',
    step.below != null ? `en dessous de ${valueText(ui, spec, step.below)}` : '',
  ].filter(Boolean).join(' ou ');
  return `${name} : ${label} ${limits || '(aucun seuil)'}`;
}

// ---- Conditions: « Valeur d’un appareil »

export function conditionChoices(device) {
  return device.points.flatMap((spec) => {
    const point = spec.key;
    switch (kindOf(spec)) {
      case 'binary': {
        const prefix = STATE_WORDS[spec.semantic] ? '' : `« ${spec.label} » : `;
        return [true, false].map((on) => ({ key: `${point}:${on}`, label: `${prefix}${stateWord(spec, on)}`, patch: { point, op: 'eq', value: on } }));
      }
      case 'numeric':
        return NUMBER_OPS.map(([op, words]) => ({ key: `${point}:${op}`, label: `« ${spec.label} » ${words}…`, spec, valueField: 'value', patch: { point, op } }));
      default:
        return [['eq', 'est'], ['ne', 'n’est pas']].map(([op, words]) => ({ key: `${point}:${op}`, label: `« ${spec.label} » ${words}…`, spec, valueField: 'value', patch: { point, op } }));
    }
  });
}

export function conditionKey(rule, spec) {
  const op = rule.op ?? 'eq';
  if (kindOf(spec) === 'binary') return `${rule.point}:${(rule.value !== false) === (op !== 'ne')}`;
  return `${rule.point}:${op}`;
}

export function conditionTitle(ui, rule) {
  const { spec, name } = lookup(ui, rule);
  if (!spec) return `${name} : ${rule.point} (introuvable)`;
  const op = rule.op ?? 'eq';
  if (kindOf(spec) === 'binary') {
    const on = (rule.value !== false) === (op !== 'ne');
    return STATE_WORDS[spec.semantic] ? `${name} : ${stateWord(spec, on)}` : `${name} : ${spec.label} ${stateWord(spec, on)}`;
  }
  const words = kindOf(spec) === 'numeric'
    ? Object.fromEntries(NUMBER_OPS)[op]
    : { eq: 'est', ne: 'n’est pas' }[op] ?? op;
  return `${name} : ${spec.label} ${words} ${valueText(ui, spec, rule.value)}`;
}
