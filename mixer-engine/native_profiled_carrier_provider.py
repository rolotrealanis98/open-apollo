"""Profiled carrier provider: one runtime bundle (built by tools/plugin-mapper on this machine)
hosted on one input or input pair. The native backend owns device I/O.

Control changes are planned by running the plug-in's own setter in the offline emulator
(tools/plugin-mapper's executor, on the user's own DLL), so hosting needs numpy, unicorn,
capstone and pefile, and the DLL where the bundle was built from.
"""
from copy import deepcopy
from dataclasses import dataclass
from pathlib import Path
import hashlib, json, math, os, re, sys, threading

EXECUTOR = Path(__file__).resolve().parents[1] / 'tools/plugin-mapper/plugin_mapper/executor'
if str(EXECUTOR) not in sys.path:
    sys.path.append(str(EXECUTOR))
from native_carrier import GeneratedOrdinaryCarrier, MODULES, check, write as _write
from plugin_native import ControlPlan
from plugin_publication import LoadedPlugin


def _emulator():
    """Imported on first use: a daemon without native bundles never needs unicorn."""
    from generic_capture import GenericCapture
    from probe_transactions import Replay
    return GenericCapture, Replay

_FIELD = re.compile(r'parameters/(\d+)/(NormalizedValue|StepValue)$')

# A plug-in's parameter values and formatter texts at load, per bundle. Starting the emulator
# takes seconds (the plug-in's own constructor runs), so a load publishes these instead and the
# emulator starts in the background for the first control change. Learned the first time a
# bundle's emulator starts and kept next to it (<bundle>.defaults.json); keyed by the bundle's
# bytes, so a rebuilt bundle is learned again.
_DEFAULTS: dict = {}
_DEFAULTS_LOCK = threading.Lock()


def _bundle_key(path):
    st = os.stat(path)
    return (str(path), st.st_mtime_ns, st.st_size)


def _bundle_sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def defaults_path(bundle_path):
    return Path(str(bundle_path) + '.defaults.json')


def load_defaults(bundle_path):
    """(values, texts) for a bundle at load, or None when not learned yet."""
    try:
        key = _bundle_key(bundle_path)
    except OSError:
        return None
    with _DEFAULTS_LOCK:
        if key in _DEFAULTS:
            return _DEFAULTS[key]
    found = None
    try:
        saved = json.loads(defaults_path(bundle_path).read_text())
        if saved.get('bundle_sha256') == _bundle_sha256(bundle_path):
            found = (tuple(saved['values']), tuple(saved['texts']))
    except (OSError, ValueError, KeyError, TypeError):
        found = None
    with _DEFAULTS_LOCK:
        if found is not None:
            _DEFAULTS[key] = found
    return found


def save_defaults(bundle_path, values, texts):
    try:
        key = _bundle_key(bundle_path)
        with _DEFAULTS_LOCK:
            _DEFAULTS[key] = (tuple(values), tuple(texts))
        target = defaults_path(bundle_path)
        tmp = target.with_name(f'{target.name}.{os.getpid()}.tmp')
        tmp.write_text(json.dumps(dict(bundle_sha256=_bundle_sha256(bundle_path), values=list(values),
                                       texts=list(texts))))
        tmp.replace(target)
    except OSError:
        pass  # read-only maps: learned again next start

@dataclass(frozen=True)
class ProfiledState:
    values: tuple
    shadow: tuple
    control_index: int | None = None
    target: int | None = None  # value requested of the original setter (it may clamp)

    @classmethod
    def make(cls, values, shadow, control_index=None, target=None):
        if control_index is not None and (type(control_index) is not int or control_index < 0):
            raise ValueError("bounded originating control index required")
        return cls(tuple(values), tuple(sorted(shadow.items())), control_index, target)

    def setter_value(self):
        return self.values[self.control_index] if self.target is None else self.target

    def shadow_dict(self):
        return dict(self.shadow)

class ProfiledCarrierProvider:
    sample_rate = 48000
    dynamic_observation_refs = frozenset()
    remote_reads = {}
    hardware_qualified = False
    hardware_qualified_inputs = ()

    def __init__(self, bundle_path, input_index=0, *, data=None, module_store=MODULES):
        """data: the already parsed bundle (the registry shares one parse across inputs)."""
        if type(input_index) is not int or not 0 <= input_index < 4:
            raise ValueError('mono input 0..3 required')
        if data is None:
            raw = Path(bundle_path).read_bytes()
            data = json.loads(raw)
            data['_sha256'] = hashlib.sha256(raw).hexdigest()
        self.bundle_sha256 = data.get('_sha256')
        self.data = data
        self.module_store = module_store
        if self.data.get('status') != 'pass':
            raise ValueError('passing runtime bundle required')
        self.bundle_path = Path(bundle_path)
        self.input_index = input_index
        self.dynamic_observation_refs = frozenset(self.dynamic_observation_refs) | frozenset(
            (int(row), int(word)) for row, word in self.data.get('dynamic_observation_refs', ()))
        self.hardware_dynamic_observation_evidence = {}
        for item in self.data.get('hardware_dynamic_observation_evidence', ()):
            if set(item) != {'row', 'offset', 'observed_values'}:
                raise ValueError('exact dynamic observation evidence schema required')
            row, offset, values = item['row'], item['offset'], tuple(item['observed_values'])
            if (type(row) is not int or type(offset) is not int or not 0 <= row <= 0xff
                    or not 0 <= offset <= 0xffffff or len(values) < 2
                    or len(set(values)) < 2
                    or any(type(value) is not int or not 0 <= value <= 0xffffffff for value in values)):
                raise ValueError('repeated dynamic observation evidence required')
            key = (row, offset)
            if key in self.hardware_dynamic_observation_evidence:
                raise ValueError('duplicate dynamic observation evidence')
            self.hardware_dynamic_observation_evidence[key] = values
            self.dynamic_observation_refs = frozenset(self.dynamic_observation_refs | {key})
        self.hardware_observation_overrides = {}
        for item in self.data.get('hardware_observation_overrides', ()):
            if set(item) != {'row', 'offset', 'source_values', 'observed_values'}:
                raise ValueError('exact hardware observation calibration schema required')
            row, offset = item['row'], item['offset']
            source_values, observed_values = tuple(item['source_values']), tuple(item['observed_values'])
            if (type(row) is not int or type(offset) is not int or not 0 <= row <= 0xff
                    or not 0 <= offset <= 0xffffff or not source_values
                    or len(source_values) != len(observed_values)
                    or any(type(value) is not int or not 0 <= value <= 0xffffffff
                           for value in source_values + observed_values)):
                raise ValueError('bounded calibrated hardware observation required')
            for index, pair in enumerate(zip(source_values, observed_values)):
                key = (row, offset + index)
                if key in self.hardware_observation_overrides:
                    raise ValueError('duplicate calibrated hardware observation')
                self.hardware_observation_overrides[key] = pair
        self.slot = f'/devices/0/inputs/{input_index}/effects/0'
        self.name = self.data['name']
        self.maxima = tuple(self.data['parameter_maxima'])
        self.descriptor_latency_samples = self.data['descriptor']['latency_samples']
        # The chain composer sets the live record delay from the compensation plan; this budget
        # only bounds the standalone recipe. Unchanged (200) unless the latency needs Long.
        latency = self.data['descriptor'].get('latency_samples', 0)
        budget = 200 if latency <= 200 else 1000  # MAX_DELAY_COMP_SAMPLES Medium / Long
        self.capture_type, replay = _emulator()
        self.recipe = GeneratedOrdinaryCarrier(self.data['descriptor'], self.data['startup'], input_index=input_index,
                                               record_budget=budget, overload_ok=latency > 1000, shared_cells_ok=True,
                                               module_store=module_store, replay=replay)
        # A row reference is an 8-bit field; it must also name a private row this carrier really has.
        private_rows = len(self.recipe.composition_arguments()["private_rows"])
        for row, _ in (*self.hardware_dynamic_observation_evidence, *self.hardware_observation_overrides):
            if row >= private_rows:
                raise ValueError("hardware observation row outside the carrier private rows")
        self.atomic_pairs = {tuple(e[1:3]) for control in self.data['control_sweep']
                             for sample in control['samples'] for e in sample['events'] if e[0] == 'begin'}
        self.params = {}
        for item in self.data['parameter_metadata']['controls']:
            match = re.search(r'/parameters/(\d+)/(Type|Name|NormalizedValue|StringValue|StepValue)$', item['path'])
            if match:
                # The metadata keeps transport-only path/range details. Publication
                # accepts the canonical property schema in both state views.
                canonical = {key: deepcopy(item[key]) for key in ('type', 'value', 'values', 'read_only') if key in item and item[key] is not None}
                self.params.setdefault(int(match[1]), {})[match[2]] = canonical
        for props in self.params.values():  # display text is the original formatter's, published live
            props.setdefault('StringValue', {'type': 'string', 'value': ''})
        if set(self.params) != set(range(len(self.maxima))):
            raise ValueError('complete parameter schema required')
        self.profile = self.data['profile']
        if self.profile.get('name') != self.name:
            raise ValueError('original profile required')
        self.cap = None
        self.state = None
        self._texts = None  # (values, formatter texts), see _texts_now()
        # The daemon runs transactions on its worker thread; every change of cap/state holds this.
        self._lock = threading.RLock()

    def _ensure(self):
        with self._lock:
            if self.cap is None:
                if self.state is None:
                    self.reset()
                else:
                    self.cap = self._capture(self.state.values)

    def _capture(self, values=None):
        cap = self.capture_type(self.profile, auto_start=False)
        cap.start()
        if values is not None:
            if len(values) != len(cap.defaults_values):
                raise ValueError('complete original parameter vector required')
            for index, (default, target) in enumerate(zip(cap.defaults_values, values)):
                if target != default:
                    cap.set_control(index, target)
        return cap

    def _startup_shadow(self):
        return {int(key, 0) if isinstance(key, str) else key: value
                for key, value in self.data['startup']['shadow'].items()}

    def reset(self):
        """Back to the plug-in's load-time state. With learned defaults no emulator starts here
        (it warms in the background for the first control change); else it starts now."""
        learned = load_defaults(self.bundle_path) if self.bundle_path is not None else None
        if learned is not None and len(learned[0]) == len(self.maxima):
            with self._lock:
                self.cap = None
                self.state = ProfiledState.make(learned[0], self._startup_shadow())
                self._texts = (self.state.values, learned[1])
            self.warm()
            return
        self.learn_defaults()

    def learn_defaults(self):
        """Start the emulator at the load-time state and remember its values and texts."""
        with self._lock:
            self.cap = self._capture()
            self.state = ProfiledState.make(self.cap.defaults_values, self._startup_shadow())
            texts = self._texts_now()
            if self.bundle_path is not None:
                save_defaults(self.bundle_path, self.state.values, texts)

    def warm(self, background=True):
        """Start the emulator now (in a thread when `background`): a control change needs it."""
        if self.cap is not None or getattr(self, '_warming', False):
            return
        if not background:
            self._ensure()
            return
        self._warming = True

        def run():
            try:
                self._ensure()
            except Exception:
                pass  # the next control change starts it again and reports the error
            finally:
                self._warming = False
        threading.Thread(target=run, name='native-warm', daemon=True).start()

    def __deepcopy__(self, memo):
        clone = self.__class__(self.bundle_path, self.input_index, data=self.data, module_store=self.module_store)
        memo[id(self)] = clone
        with self._lock:
            # The accepted state travels even when this copy has no emulator yet (a copy of a
            # copy); the clone rebuilds its emulator from it (_ensure / warm()).
            clone.state = self.state
            clone._texts = self._texts
            clone._control_ref_cache = getattr(self, '_control_ref_cache', None)
        return clone

    def composition_arguments(self):
        """Expose only the already checked complete recipe to the chain composer."""
        return self.recipe.composition_arguments()

    def create_arguments(self):
        return self.recipe.create_arguments()

    def _filtered_observations(self, source, carrier_words=True, width=16):
        """Exclude host-owned carrier words and keep every driver read at <=width words (default 16, driver ABI max 64).

        carrier_words=False skips only the blanket rows>=11 header-word exclusion (words 0,1,2,6); calibration, dynamic refs and
        the row-10 root header stay excluded. Used when that exclusion would leave a control with nothing to observe."""
        output = []
        for row, offset, values in source:
            pending = []
            for index, value in enumerate(values):
                word = offset + index
                calibration = self.hardware_observation_overrides.get((row, word))
                if calibration is not None:
                    source_value, observed_value = calibration
                    # A calibration maps one recorded host value to its observed hardware value;
                    # at any other value the plain audit applies.
                    if value == source_value:
                        value = observed_value
                if ((row == 10 and word in (0, 1, 2, 4))
                        or value == 0xffffffff
                        or (carrier_words and row >= 11 and word in (0, 1, 2, 6))
                        or (row, word) in self.dynamic_observation_refs):
                    if pending:
                        output.append((row, pending[0][0], tuple(item[1] for item in pending)))
                        pending = []
                    continue
                if pending and word != pending[-1][0] + 1:
                    output.append((row, pending[0][0], tuple(item[1] for item in pending)))
                    pending = []
                pending.append((word, value))
                if len(pending) == width:
                    output.append((row, pending[0][0], tuple(item[1] for item in pending)))
                    pending = []
            if pending:
                output.append((row, pending[0][0], tuple(item[1] for item in pending)))
        return tuple(output)

    def observations(self):
        # Static (the recipe's startup words): no emulator needed, so a chain rebuild that
        # re-reads every member's observations doesn't re-run each plug-in's original code.
        # The chain carrier owns its root header words. They are not plug-in
        # coefficients and must not be replayed or observed as profile state.
        source = self.recipe.startup_observations()
        plan = self._filtered_observations(source)
        # Host plans are capped at 32768 runs: repack at the driver ABI maximum (64 words) only for a plan over 32000.
        return self._filtered_observations(source, width=64) if len(plan) > 32000 else plan

    def _field(self, field):
        match = _FIELD.fullmatch(field or '')
        if not match:
            raise ValueError('captured parameter field required')
        index, kind = int(match[1]), match[2]
        if index not in self.params or (kind == 'StepValue' and 'StepValue' not in self.params[index]):
            raise ValueError('unsupported captured parameter field')
        return index, kind

    def _values(self, field, values, cap):
        index, _ = self._field(field)
        output = {'NormalizedValue': values[index] / self.maxima[index],
                  'StringValue': cap.format_parameter(index)['console_trimmed']}
        if 'StepValue' in self.params[index]:
            output['StepValue'] = self._step_label(index, values[index], output['StringValue'])
        return output

    def _step_label(self, index, value, shown):
        """Step label for integer `value`. A stepped parameter whose original table defines no domain (raw maximum 0,
        maxima 32768) holds values far above the label count (On = 32768): the label is then the formatter text when it
        is a step, else the step the normalized value selects (the host rule generic_capture applies to startup text).
        Publication requires StepValue for every stepped parameter, so it is never omitted."""
        labels = self.params[index]['StepValue'].get('values') or []
        if not labels:
            raise ValueError('captured step labels required')
        if 0 <= value < len(labels):
            return labels[value]
        if shown in labels:
            return shown
        return labels[min(len(labels) - 1, round(value / self.maxima[index] * (len(labels) - 1)))]

    def current_values(self, field):
        """Readback values of `field` at the accepted state (coupled-parameter publication)."""
        self._ensure()
        return self._values(field, self.state.values, self.cap)

    def _texts_now(self):
        """The original formatter's text of every parameter at the accepted state, cached with
        that state: a copy made for chain planning publishes it without rebuilding an emulator."""
        with self._lock:
            if self._texts is None or self._texts[0] != self.state.values:
                self._ensure()
                self._texts = (self.state.values,
                               tuple(self.cap.format_parameter(i)['console_trimmed'] for i in range(len(self.maxima))))
            return self._texts[1]

    def _parameters(self):
        warm = False
        if self.state is None:
            learned = load_defaults(self.bundle_path) if self.bundle_path is not None else None
            if learned is not None and len(learned[0]) == len(self.maxima):
                # Load without the emulator; it starts in the background for the first control.
                with self._lock:
                    if self.state is None:
                        self.state = ProfiledState.make(learned[0], self._startup_shadow())
                        self._texts = (self.state.values, learned[1])
                warm = True
            else:
                self._ensure()
        texts = self._texts_now()
        output = []
        values = self.state.values
        for index, value in enumerate(values):
            props = deepcopy(self.params[index])
            props['NormalizedValue']['value'] = value / self.maxima[index]
            props['StringValue']['value'] = texts[index]
            if 'StepValue' in props:
                props['StepValue']['value'] = self._step_label(index, value, props['StringValue']['value'])
            output.append({'id': index, 'properties': props})
        if warm:
            self.warm()
        return tuple(output)

    def _observe_refs(self, shadow, refs):
        refs = [ref for ref in refs if ref in shadow]
        if not refs:
            return ()
        raw = self.recipe.codec.observations(shadow, refs)
        # A control whose every written word sits in the blanket carrier-header positions (a small memory the plug-in owns
        # entirely, e.g. a 2-dword switch memory) would otherwise have no authoritative observation at all.
        return self._filtered_observations(raw) or self._filtered_observations(raw, carrier_words=False)

    def _control_refs(self):
        """Every word any recorded control sample writes (the control-owned state)."""
        if getattr(self, '_control_ref_cache', None) is None:
            refs = set()
            for control in self.data['control_sweep']:
                for sample in control['samples']:
                    refs.update(self.recipe.event_plan(sample['events'], phase='control',
                                                       allowed_pairs=self.atomic_pairs)['shadow'])
            self._control_ref_cache = frozenset(refs)
        return self._control_ref_cache

    def _plan(self, events, prior_state, next_values, field, cap, values=None, control_index=None, target=None):
        plan = self.recipe.event_plan(events, phase='control', allowed_pairs=self.atomic_pairs)
        shadow = prior_state.shadow_dict()
        shadow.update(plan['shadow'])
        state = ProfiledState.make(next_values, shadow, control_index, target)
        readback = deepcopy(values) if values is not None else self._values(field, next_values, cap)
        # Verify exactly the words this change wrote (the full shadow is verified at load).
        observed = self._observe_refs(shadow, plan['shadow']) or self._observe_refs(shadow, self._control_refs())
        return ControlPlan(plan['writes'], observed, readback, state)

    def initial(self, lease):
        return LoadedPlugin(self.name, lease, True, self.descriptor_latency_samples, self._parameters())

    def snapshot(self, field):
        self._ensure()
        self._field(field)
        shadow = self.state.shadow_dict()
        # Before a change, verify the control-owned words, not internal tables/buffers.
        return ControlPlan((), self._observe_refs(shadow, self._control_refs()),
                           self._values(field, self.state.values, self.cap), self.state)

    def plan_control(self, field, value):
        self._ensure()
        index, kind = self._field(field)
        if kind == 'NormalizedValue':
            if type(value) not in (int, float) or not math.isfinite(value) or not 0 <= value <= 1:
                raise ValueError('normalized 0..1 required')
            target = round(value * self.maxima[index])
        else:
            labels = self.params[index]['StepValue'].get('values') or []
            if value not in labels:
                raise ValueError('captured step label required')
            target = labels.index(value)
        cap = self.cap
        if tuple(cap.read_values()) != self.state.values:
            raise ValueError('emulator/current state differs')
        first = len(cap.events)
        cap.set_control(index, target)
        values = tuple(cap.read_values())
        return self._plan(cap.events[first:], self.state, values, field, cap, control_index=index, target=target)

    def plan_restore(self, prior, proposal):
        self._ensure()
        if not isinstance(prior.state, ProfiledState) or not isinstance(proposal.state, ProfiledState):
            raise ValueError('profiled prior/proposal state required')
        differences = [i for i, pair in enumerate(zip(prior.state.values, proposal.state.values)) if pair[0] != pair[1]]
        index = proposal.state.control_index
        if type(index) is not int or not 0 <= index < len(prior.state.values):
            raise ValueError('control proposal must identify its originating parameter')
        cap = self.cap
        restore_to = prior.state.values[index] if prior.state.control_index != index else prior.state.setter_value()
        if not differences and restore_to == proposal.state.setter_value():  # nothing to restore
            return self._plan((), proposal.state, prior.state.values, None, cap, prior.values)
        if tuple(cap.read_values()) != proposal.state.values:
            raise ValueError('emulator/proposal state differs')
        first = len(cap.events)
        cap.set_control(index, restore_to)
        # The original code may move coupled parameters with the originating one (and restoring it can move
        # parameters the forward change did not: AMS DMX Tempo Sync converts VCO speed and both delays lossily);
        # put every remaining difference back through the original setter, in index order.
        for other in range(len(prior.state.values)):
            if tuple(cap.read_values())[other] != prior.state.values[other]:
                cap.set_control(other, prior.state.values[other])
        if tuple(cap.read_values()) != prior.state.values:
            raise ValueError("original control restore differs")
        return self._plan(cap.events[first:], proposal.state, prior.state.values, None, cap, prior.values,
                          control_index=prior.state.control_index, target=prior.state.target)

    def accept(self, plan):
        if not isinstance(plan, ControlPlan) or not isinstance(plan.state, ProfiledState):
            raise ValueError('profiled control plan required')
        shadow = plan.state.shadow_dict()
        base = self.recipe.codec.base_row
        refs = {(row - base) << 24 | (offset + i) for row, offset, values in plan.observations
                for i in range(len(values))}
        expected = self._observe_refs(shadow, refs)
        # Compare words, not run grouping: a filtered word inside a codec run regroups the runs.
        def words(observations):
            return {(row, offset + i): v for row, offset, values in observations for i, v in enumerate(values)}
        if words(plan.observations) != words(expected):
            raise ValueError('accepted observation/state mismatch')
        with self._lock:
            self.cap = self._advanced(plan.state)
            self.state = plan.state

    def _advanced(self, state):
        """The emulator at `state`: the live one moved by its originating control when that is
        the only difference, else a fresh capture (startup replay) as before."""
        cap = self.cap
        if cap is not None:
            current = tuple(cap.read_values())
            index = state.control_index
            if current == state.values and state.target is None:
                return cap
            differences = [i for i, pair in enumerate(zip(current, state.values)) if pair[0] != pair[1]]
            # Re-request a clamped target even when the readback already matches: the setter's
            # own state (e.g. its display) follows the request. Coupled parameters follow it.
            if type(index) is int and (index in differences or state.target is not None):
                cap.set_control(index, state.setter_value())
                if tuple(cap.read_values()) == state.values:
                    return cap
        return self._capture(state.values)


class StereoProfiledCarrierProvider(ProfiledCarrierProvider):
    """Paired-input wrapper for bundles proven with an original 2/2 descriptor."""
    def __init__(self, bundle_path, input_index=0, **kwargs):
        if input_index not in (0,2):
            raise ValueError('stereo provider requires input pair 1/2 or 3/4')
        super().__init__(bundle_path,input_index,**kwargs)
        if tuple(self.data['descriptor'].get('io',())) != (2,2):
            raise ValueError('original 2/2 descriptor required for stereo wrapper')
        self.channels=(input_index,input_index+1)

    def _stereo_arguments(self, result):
        if result['input_pair'] != (self.input_index,32):
            raise ValueError('mono carrier layout changed')
        commands=list(result['commands'])
        for row in (0,1,10):
            commands.extend(_write(row,{1:2,2:2}))
        check(result['private_rows'],commands)
        result.update(input_pair=self.channels,commands=tuple(commands))
        return result

    def composition_arguments(self):
        return self._stereo_arguments(super().composition_arguments())

    def create_arguments(self):
        return self._stereo_arguments(super().create_arguments())

    def observations(self):
        return super().observations()+tuple((row,1,(2,2)) for row in (0,1,10))


def learn(paths):
    """Learn the load-time defaults of these bundles (one emulator start each), for the daemon
    to load them without starting the emulator: python3 native_profiled_carrier_provider.py BUNDLE..."""
    done = 0
    for path in paths:
        if load_defaults(path) is not None:
            continue
        try:
            data = json.loads(Path(path).read_text())
            kind = StereoProfiledCarrierProvider if data.get('layout') == 'stereo' else ProfiledCarrierProvider
            provider = kind(path, 0, data=data, module_store=Path(os.environ.get('OA_MODULE_STORE') or MODULES))
            provider.learn_defaults()
            provider.cap = None
            done += 1
        except Exception as error:
            print(f'{Path(path).name}: defaults not learned: {type(error).__name__}: {error}', flush=True)
    return done


if __name__ == '__main__':
    print(f'learned the load-time defaults of {learn(sys.argv[1:])} bundles', flush=True)
