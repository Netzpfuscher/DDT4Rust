#!/usr/bin/env python3
"""Generate golden value-codec vectors from the original ddt4all Python code.

Run from the repository root:

    PYTHONPATH=reference/ddt4all/src python3 tools/gen_vectors.py

The resulting tests/fixtures/value_vectors.json is consumed by the Rust tests.
"""
import glob
import json
import os
import random
import sys

sys.path.insert(0, os.path.join("reference", "ddt4all", "src"))
from ddt4all.core.ecu.ecu_data import EcuData  # noqa: E402
from ddt4all.core.ecu.data_item import DataItem  # noqa: E402

random.seed(0xC0FFEE)
DATA_DIR = os.environ.get("DDT_DATA", "data/ecu")
OUT = os.path.join("crates", "ddtrust-core", "tests", "fixtures", "value_vectors.json")


def pick_value(data, rng):
    bits = data.bitscount
    if data.bytesascii:
        return "TEST"
    raw = rng.getrandbits(min(bits, 32))
    if data.scaled:
        val = (raw * data.step + data.offset) / data.divideby
        # use a decimal representation that Python and Rust parse identically
        return format(val, ".15g")
    width = max(1, (bits + 7) // 8)
    return raw.to_bytes(width, "big").hex()


def main():
    files = [
        "UCH_84_J84_05_00.json",
        "EDC16C36___S40___V4___(Diag_On_CAN)_20100927T101813.json",
    ]
    # add a Little endian CAN file and a KWP file
    for f in sorted(glob.glob(os.path.join(DATA_DIR, "*.json"))):
        try:
            d = json.load(open(f))
        except Exception:
            continue
        if d.get("endian") == "Little" and (d.get("obd") or {}).get("protocol") == "CAN":
            files.append(os.path.basename(f))
            break
    for f in sorted(glob.glob(os.path.join(DATA_DIR, "*.json"))):
        try:
            d = json.load(open(f))
        except Exception:
            continue
        if (d.get("obd") or {}).get("protocol") == "KWP2000":
            files.append(os.path.basename(f))
            break

    decode, encode, requests = [], [], []
    for fname in files:
        path = os.path.join(DATA_DIR, fname)
        if not os.path.exists(path):
            continue
        raw = json.load(open(path))
        endian = raw.get("endian") or "Big"
        data_specs = raw.get("data") or {}
        rng = random.Random(hash(fname) & 0xFFFF)
        seen = set()
        count = 0
        for req in raw.get("requests") or []:
            for item_name, item in (req.get("receivebyte_dataitems") or {}).items():
                spec = data_specs.get(item_name)
                if spec is None:
                    continue
                key = (item_name, item.get("firstbyte"), item.get("bitoffset"))
                if key in seen:
                    continue
                seen.add(key)
                bits = spec.get("bitscount", 8)
                if bits > 96:
                    continue
                firstbyte = item.get("firstbyte", 0)
                bitoffset = item.get("bitoffset", 0)
                if firstbyte < 1:
                    continue
                length = firstbyte - 1 + (bits + bitoffset + 7) // 8
                if length <= 0 or length > 512:
                    continue
                response = bytes(rng.getrandbits(8) for _ in range(length))
                eu = EcuData(spec, item_name)
                di = DataItem(item, endian)
                try:
                    disp = eu.getDisplayValue(response.hex(), di, endian)
                    hexv = eu.getHexValue(response.hex(), di, endian)
                except Exception:
                    continue
                decode.append({
                    "file": fname,
                    "data_name": item_name,
                    "spec": spec,
                    "item": item,
                    "endian": endian,
                    "response": response.hex().upper(),
                    "display": disp,
                    "hex": hexv,
                })
                count += 1
                if count >= 35:
                    break
            if count >= 35:
                break

        # encoding vectors from send data items (and synthetic bit offsets)
        send_seen = set()
        enc_count = 0
        for req in raw.get("requests") or []:
            sent = req.get("sentbytes") or ""
            if len(sent) < 8:
                continue
            for item_name, item in (req.get("sendbyte_dataitems") or {}).items():
                spec = data_specs.get(item_name)
                if spec is None or spec.get("bitscount", 8) > 64:
                    continue
                key = (item_name, item.get("firstbyte"), item.get("bitoffset"))
                if key in send_seen:
                    continue
                send_seen.add(key)
                stream = [sent[i:i + 2] for i in range(0, len(sent), 2)]
                eu = EcuData(spec, item_name)
                di = DataItem(item, endian)
                value = pick_value(eu, rng)
                out = eu.setValue(value, list(stream), di, endian)
                if out is None:
                    continue
                encode.append({
                    "file": fname,
                    "data_name": item_name,
                    "spec": spec,
                    "item": item,
                    "endian": endian,
                    "stream": stream,
                    "value": value,
                    "expected": out,
                })
                enc_count += 1
                if enc_count >= 30:
                    break
            if enc_count >= 30:
                break

        # full request building vectors
        for req in (raw.get("requests") or [])[:400]:
            if not req.get("sentbytes") or not req.get("sendbyte_dataitems"):
                continue
            inputs = {}
            ok = True
            for item_name, item in req["sendbyte_dataitems"].items():
                spec = data_specs.get(item_name)
                if spec is None or spec.get("bitscount", 8) > 64:
                    ok = False
                    break
                eu = EcuData(spec, item_name)
                inputs[item_name] = pick_value(eu, rng)
            if not ok:
                continue
            try:
                stream = req["sentbytes"]
                # emulate EcuRequest.build_data_stream without the GUI mapping branch
                for k, v in inputs.items():
                    spec = data_specs[k]
                    item = req["sendbyte_dataitems"][k]
                    eu = EcuData(spec, k)
                    di = DataItem(item, endian)
                    stream = eu.setValue(v, [stream[i:i + 2] for i in range(0, len(stream), 2)], di, endian)
                    if stream is None:
                        ok = False
                        break
                if ok:
                    requests.append({
                        "file": fname,
                        "name": req["name"],
                        "sentbytes": req["sentbytes"],
                        "inputs": inputs,
                        "expected": "".join(stream),
                    })
            except Exception:
                continue
            if len(requests) >= 12:
                break

    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    json.dump({"decode": decode, "encode": encode, "requests": requests}, open(OUT, "w"), indent=1)
    print(f"wrote {OUT}: {len(decode)} decode, {len(encode)} encode, {len(requests)} request vectors")


if __name__ == "__main__":
    main()
