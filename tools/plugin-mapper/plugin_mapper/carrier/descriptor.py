"""Original shared Windows descriptor decoder; addresses come from discovery."""
from pathlib import Path
import hashlib
import os
import struct


def decode_descriptor(cap, address, *, plugin, dll_sha256):
    raw = bytes(cap.uc.mem_read(address, 0xb20))
    word = lambda offset: struct.unpack_from('<I', raw, offset)[0]
    count, rows_count = word(4), word(0x188)
    if not 1 <= count <= 48 or not 1 <= rows_count <= 144:
        raise ValueError('original descriptor counts exceed source struct bounds')
    rows = [list(struct.unpack_from('<4I', raw, 0x18c+16*i)) for i in range(rows_count)]
    resources = []
    for i in range(count):
        pointer = struct.unpack_from('<Q', raw, 8+8*i)[0]
        header = bytes(cap.uc.mem_read(pointer, 20))
        magic, ident, flags, size, functional = struct.unpack('<5I', header)
        if magic != 0x6c6c6942 or size > 16*1024*1024 or size % 4:
            raise ValueError('descriptor resource pointer is not a bounded Bill')
        blob = bytes(cap.uc.mem_read(pointer, size+20))
        digest = hashlib.sha256(blob).hexdigest()
        path = Path(os.environ.get('OA_MODULE_STORE', '/lib/firmware/universal-audio/plugins/modules'))/(digest+'.bill')
        if not path.exists() or hashlib.sha256(path.read_bytes()).hexdigest() != digest:
            raise ValueError('original descriptor resource absent or changed in module store')
        resources.append(dict(id=ident, architecture=flags >> 24, kind=flags & 255,
                              flags=(flags >> 8) & 65535, words=functional,
                              sha256=digest, length=len(blob), module_store_verified=True))
    return dict(plugin=plugin, plugin_sha256=dll_sha256,
                sample_rate=int(struct.unpack('<f', cap.uc.mem_read(cap.fs,4))[0]), block_size=32,
                rows=rows, resources=resources, scratch=[word(0xa9c),word(0xa98),word(0xaa0)],
                io=[word(0xa90),word(0xa94)], render_id=word(0xaa4), flags=word(0xaac),
                descriptor_latency_initial=word(0xab4),
                descriptor_sha256=hashlib.sha256(raw).hexdigest(), hardware_executed=False)


def extract_descriptor(cap, profile):
    """Execute generated descriptor entry on the established whole-parent object.

    Discovery must attest the receiver path. A detached synthetic prefix is not
    silently created: unresolved lifecycle stays an explicit extraction failure.
    """
    if profile.get('framework') == 'DSPAudioProcessor':
        from dspap_capture import extract_descriptor as dspap_extract
        return dspap_extract(cap, profile, decode_descriptor)
    if profile.get('framework') == 'VST2IUADPlug':
        if (profile.get('vst2') or {}).get('variant') == 'bx':
            from vst2_bx_host import extract_descriptor as vst2_extract
        elif (profile.get('vst2') or {}).get('variant') == 'relab':
            from vst2_relab_host import extract_descriptor as vst2_extract
        else:
            from vst2_host import extract_descriptor as vst2_extract
        return vst2_extract(cap, profile, decode_descriptor)
    if profile.get('framework') == 'Softube':
        from softube_capture import extract_descriptor as softube_extract
        return softube_extract(cap, profile, decode_descriptor)
    spec = profile['descriptor']
    integer = lambda value: int(value, 0) if isinstance(value, str) else int(value)
    offset = integer(spec['receiver_from_parameter_block'])
    fn = integer(spec['getter_va'])
    receiver = cap.this+offset
    if receiver != cap.parent+integer(spec['secondary_object_offset']):
        raise ValueError('descriptor receiver is not the initialized shared parent')
    before = dict(cap.shadow)
    first_event = len(cap.events)
    out = cap.reserve(0x1000)
    cap.call(fn, receiver, out)
    # First-time coefficient init may ADD shadow words; overwriting any existing word is refused.
    overwritten = sorted(hex(k) for k, v in before.items() if cap.shadow.get(k) != v)
    if overwritten:
        raise ValueError('descriptor call overwrote coefficient shadow words '+repr(overwritten))
    # Every added word must be recorded as a descriptor-phase event so startup replay stays exact.
    recorded = set()
    for event in cap.events[first_event:]:
        if event[0] == 'write':
            recorded.update(event[1]+i for i in range(len(event[2])))
    unrecorded = sorted(hex(k) for k in cap.shadow.keys()-before.keys()-recorded)
    if unrecorded:
        raise ValueError('descriptor shadow additions without recorded events '+repr(unrecorded))
    data = decode_descriptor(cap, out, plugin=profile['name'], dll_sha256=profile['dll']['sha256'])
    # Host channel count = carrier hosting: stereo iff the original descriptor IO is (2,2), as build_profile_runtime_bundle.py.
    # Opt-in per profile until a stereo-configured live carrier exists (carrier/adapter.py hosts
    # configured_input_channels == 1 only); default keeps offline results equal to the live bundles.
    carrier = 2 if list(data["io"]) == [2, 2] and profile.get("stereo_channel_request") is True else 1
    cap.host_channels = carrier
    # The original producer queries these same shared host channel methods.
    requested = [cap.call(cap.rq(cap.parentvt+slot), cap.parent) & 0xffffffff
                 for slot in (0x520,0x518)]
    if requested != [carrier, carrier] or any(n not in (1,2) for n in data["io"]):
        raise ValueError("host channel request must equal the carrier channel count and original IO capabilities")
    data['host_requested_io'] = requested
    data['normalized_io'] = [min(a,b) for a,b in zip(requested,data['io'])]
    data['configured_input_channels'] = data['normalized_io'][0]
    data['channel_normalization_evidence'] = str(Path(__file__).with_name('channel-normalization.json'))
    data['descriptor_function'] = hex(fn)
    data['receiver_evidence'] = spec
    return data
