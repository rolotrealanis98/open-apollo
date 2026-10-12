"""Bounded .uadmix scalar mapping; opaque plugin/unknown XML is preserved.

Native shape: console-port/surfaces/sessions.md. Loading metadata does not
instantiate DSP plugins. Unmapped XML stays in the document on subsequent save.
"""
import json
import math
import re
import xml.etree.ElementTree as ET

KINDS = {"kRoot": "", "kMixer": "devices", "kInput": "inputs",
         "kOutput": "outputs", "kAuxBus": "auxs", "kEffect": "effects",
         "kPreamp": "preamps", "kPluginScenes": "pluginscenes",
         "kPluginScene": "scenes"}
ALIASES = {"48VEnable": "preamps/0/48V", "Phase": "preamps/0/Phase",
           "Pad": "preamps/0/Pad", "Gain": "preamps/0/Gain"}
SEND = re.compile(r"(Aux[12]|HP[1-4])(?:Send)?(Level|Pan|Bypass|PrePost)$")
TYPES = {"bool": "kPTBoolean", "int": "kPTInt", "int64": "kPTInt64",
         "float": "kPTFloat", "string": "kPTString"}


def parse(data):
    if len(data) > 8 * 1024 * 1024:
        raise ValueError("session XML exceeds 8 MiB")
    text = data.decode("utf-8-sig")
    if "<!DOCTYPE" in text.upper() or "<!ENTITY" in text.upper():
        raise ValueError("DTD/entity declarations are unsupported")
    try:
        root = ET.fromstring(text, parser=ET.XMLParser(target=ET.TreeBuilder(insert_comments=True)))
    except ET.ParseError as error:
        raise ValueError("malformed session XML") from error
    tree = root.find("mixer_tree")
    if root.tag != "uad_mixer_session" or tree is None or tree.get("version") not in ("1", "2", "3", "4"):
        raise ValueError("unsupported .uadmix mixer tree")
    pending = [(root, 0)]
    count = 0
    while pending:
        node, depth = pending.pop()
        count += 1
        if depth > 128 or count > 100000:
            raise ValueError("session XML depth/node limit")
        pending.extend((child, depth + 1) for child in node)
    return root


def objects(root):
    def walk(node, parent):
        if node.tag == "mixer_object":
            kind = node.get("type")
            index = node.get("relative_index", "0")
            if kind not in KINDS or not index.isdigit():
                return  # Retain unknown subtrees without guessing their paths.
            if kind == "kRoot":
                base = ""
            elif kind == "kMixer":
                base = "/devices/" + index
            elif kind == "kPluginScenes":
                base = "/pluginscenes/" + index
            else:
                base = parent + "/" + KINDS[kind] + "/" + index
            yield base, node
            parent = base
        for child in node:
            yield from walk(child, parent)
    yield from walk(root, "")


def property_path(base, name, state):
    if not name.startswith("kProp"):
        return None
    name = name[5:]
    match = SEND.fullmatch(name)
    if match:
        bus, leaf = match.groups()
        index = int(bus[-1]) - 1 + (2 if bus.startswith("HP") else 0)
        leaf = {"Level": "Gain", "PrePost": "PostFaderOverride"}.get(leaf, leaf)
        path = f"{base}/sends/{index}/{leaf}"
    else:
        path = base + "/" + name
        if state.get_prop_dict(path) is None and name in ALIASES:
            path = base + "/" + ALIASES[name]
    return path if state.get_prop_dict(path) is not None else None


def decode_value(text, meta):
    kind = meta.get("type")
    if kind == "bool":
        if text.lower() not in ("true", "false", "0", "1"):
            raise ValueError("invalid boolean")
        return text.lower() in ("true", "1")
    if kind in ("int", "int64"):
        return int(text)
    if kind == "float":
        value = float(text)
        if not math.isfinite(value):
            raise ValueError("nonfinite session value")
        return value
    return text


def read_values(root, state):
    values = {}
    for base, obj in objects(root):
        for node in obj.findall("property"):
            path = property_path(base, node.get("id", ""), state)
            if path:
                values[path] = decode_value(node.text or "", state.get_prop_dict(path))
        # Plugin chunk bytes remain opaque; only represent the saved name.
        for insert in obj.findall("insert_effect"):
            name = insert.findtext("plugin_name")
            index = insert.get("relative_index")
            if name is not None and index is not None and index.isdigit():
                path = f"{base}/effects/{index}/EffectName"
                if state.get_prop_dict(path):
                    values[path] = name
    # Our extension preserves paths that native kProp flattening cannot express.
    for node in root.findall("open_apollo/property"):
        path = node.get("path", "")
        if state.get_prop_dict(path):
            values[path] = json.loads(node.text or "null")
    return values


def save_values(values, state, original=None):
    root = original if original is not None else ET.Element("uad_mixer_session")
    if original is None:
        ET.SubElement(root, "console_settings", version="1")
        tree = ET.SubElement(root, "mixer_tree", version="4")
        ET.SubElement(tree, "mixer_object", type="kRoot", relative_index="0", version="1")
    represented = set()
    # Update mapped native fields in a loaded document, retaining unknown data.
    for base, obj in objects(root):
        for node in obj.findall("property"):
            path = property_path(base, node.get("id", ""), state)
            if path in values:
                node.text = str(values[path]).lower() if isinstance(values[path], bool) else str(values[path])
                represented.add(path)
    # Also emit native object/property nodes for supported unrepresented paths.
    root_obj = root.find("mixer_tree/mixer_object")
    if root_obj is None:
        root_obj = ET.SubElement(root.find("mixer_tree"), "mixer_object",
                                 type="kRoot", relative_index="0", version="1")
    nodes = dict(objects(root))
    nodes.setdefault("", root_obj)
    reverse = {v: k for k, v in KINDS.items() if v}
    for path, value in sorted(values.items()):
        if path in represented:
            continue
        parent, _, leaf = path.rpartition("/")
        send = re.fullmatch(r"(.+)/sends/([0-5])", parent)
        if send and leaf in ("Gain", "Pan", "Bypass", "PostFaderOverride"):
            parent, index = send.group(1), int(send.group(2))
            bus = f"Aux{index + 1}" if index < 2 else f"HP{index - 1}"
            leaf = bus + ("Send" if index < 2 else "") + {"Gain": "Level", "PostFaderOverride": "PrePost"}.get(leaf, leaf)
        elif parent.endswith("/preamps/0") and leaf in ("48V", "Phase", "Pad", "Gain"):
            parent = parent.removesuffix("/preamps/0")
            leaf = {"48V": "48VEnable"}.get(leaf, leaf)
        parts = parent.strip("/").split("/") if parent else []
        base = ""
        obj = nodes[""]
        for pos in range(0, len(parts), 2):
            if pos + 1 >= len(parts) or parts[pos] not in reverse:
                break
            base += "/" + parts[pos] + "/" + parts[pos + 1]
            if base not in nodes:
                nodes[base] = ET.SubElement(obj, "mixer_object", type=reverse[parts[pos]],
                                            relative_index=parts[pos + 1], version="1")
            obj = nodes[base]
        else:
            node = ET.SubElement(obj, "property", id="kProp" + leaf,
                                 type=TYPES.get(state.get_prop_dict(path).get("type"), "kPTString"))
            node.text = str(value).lower() if isinstance(value, bool) else str(value)
    extension = root.find("open_apollo")
    if extension is not None:
        root.remove(extension)
    extension = ET.SubElement(root, "open_apollo", version="1")
    for path, value in sorted(values.items()):
        ET.SubElement(extension, "property", path=path).text = json.dumps(value)
    return ET.tostring(root, encoding="utf-8", xml_declaration=True)
