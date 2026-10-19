"""Learn source-profiled parent slots required by original descriptor execution.

This is a bounded offline-emulator augmentation.  It never loads a vendor DLL:
a slot is retained only after an unbound parent-vtable trap identifies it,
the generated profile attests its original function range, and a fresh
descriptor run passes existing resource validation.
"""
from pathlib import Path
import argparse, json, os, re, subprocess, sys, tempfile

HERE = Path(__file__).resolve().parent
BATCH = HERE.parent
SLOT = re.compile(r"^unknown parent (0x[0-9a-f]+)$")


def atomic(path, value):
    temporary = path.with_name(path.name + ".tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n")
    temporary.replace(path)


def run_profile(profile, output):
    with tempfile.TemporaryDirectory(prefix="openapollo-descriptor-slot-") as directory:
        directory = Path(directory)
        source, result = directory / "profile.json", directory / "result.json"
        atomic(source, profile)
        command = [
            sys.executable, str(BATCH / "executor" / "run_profile.py"),
            "--profile", str(source), "--output", str(result),
            "--descriptor-module", str(BATCH / "carrier" / "descriptor.py"),
        ]
        completed = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        if completed.returncode or not result.is_file():
            raise RuntimeError("bounded descriptor runner failed: " + completed.stderr[-400:])
        value = json.loads(result.read_text())
        output.append(dict(status=value.get("status"), stages=value.get("stages", {}),
                           error=value.get("error"), seconds=value.get("seconds")))
        return value


def candidate_slot(result, profile):
    unknown = (result.get("error") or {}).get("unknown") or {}
    match = SLOT.fullmatch(unknown.get("name", ""))
    if not match:
        return None
    slot = int(match.group(1), 16)
    if slot % 8 or not 0 <= slot < 0xc0 * 8:
        return None
    source = profile["parent"]["slots"].get(hex(slot))
    if not isinstance(source, dict) or not {"va", "function_end"} <= source.keys():
        return None
    integer = lambda value: int(value, 0) if isinstance(value, str) else int(value)
    va, end = integer(source["va"]), integer(source["function_end"])
    if not 0x180000000 <= va < end <= 0x184000000:
        return None
    return hex(slot)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--index", type=Path, default=BATCH / "catalog-discovery" / "index.json")
    parser.add_argument("--name", action="append")
    parser.add_argument("--output", type=Path, default=HERE / "descriptor-slot-augmentation.json")
    args = parser.parse_args()
    index = json.loads(args.index.read_text())
    selected = set(args.name or ())
    report = dict(schema="openapollo-descriptor-slot-augmentation/1", hardware_executed=False,
                  entries=[])
    for item in index.get("entries", []):
        if item.get("status") != "discovered" or selected and item["name"] not in selected:
            continue
        source = Path(item["profile_path"])
        document = json.loads(source.read_text())
        profiles = document.get("profiles", [document])
        changed = False
        for profile in profiles:
            if selected and profile["name"] not in selected:
                continue
            entry = dict(name=profile["name"], attempts=[])
            initial = run_profile(profile, entry["attempts"])
            slot = candidate_slot(initial, profile)
            if slot is None:
                entry["status"] = "not_applicable"
            else:
                descriptor = profile.setdefault("descriptor", {})
                current = descriptor.get("required_parent_slots", [])
                if not isinstance(current, list):
                    raise ValueError("descriptor parent slots must be a list")
                candidate = dict(profile)
                candidate["descriptor"] = dict(descriptor, required_parent_slots=current + [slot])
                retried = run_profile(candidate, entry["attempts"])
                if retried.get("stages", {}).get("descriptor") != "pass":
                    entry["status"] = "retry_rejected"
                else:
                    descriptor["required_parent_slots"] = current + [slot]
                    changed = True
                    entry.update(status="persisted", slot=slot,
                                 descriptor_sha256=retried["descriptor"]["descriptor_sha256"])
            report["entries"].append(entry)
        if changed:
            atomic(source, document)
    atomic(args.output, report)
    print(json.dumps(dict(entries=report["entries"], output=str(args.output))))


if __name__ == "__main__":
    main()
