#!/usr/bin/env python3
"""Key facts of a remote soak (remote-lab.sh) in DIR/summary.json.

  remote_summary.py DIR

Reads DIR/main and DIR/keycache (soak.sh run directories, summarized by
summarize.py first) and DIR/raw (the relay VM's samples of both relays and
their metrics, the lab VM's samples). Writes DIR/summary.json:

* `runs`: summarize.py's key facts of main and keycache;
* `relays`: per relay (relay = p2p-relay1's iroh-relay with the default key
  cache, relay2 = the second one with a small key_cache_capacity): resident
  and anonymous memory, fds and threads at idle / after warm-up / load end /
  after cool-down, the metrics' deltas over the load (bytes, accepts, unique
  client keys, QAD connections), and memory against new client keys:
  the slope of anonymous memory over `relayserver_unique_client_keys_total`
  (KiB per new key) after warm-up, for the whole window and its two halves;
  for relay2 also before and after its key cache filled;
* `hosts`: load average and available memory of the three hosts over the
  load (median, max);
* `relay_traffic_mb`: bytes received and sent by each relay during the run.
"""

import json
import os
import statistics
import sys
import time

sys.dont_write_bytecode = True
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import summarize as S  # noqa: E402


def series(rows, role):
    out = {}
    for e in rows:
        if e.get("kind") == "proc" and e.get("role") == role:
            t = S.ts_epoch(e["ts"])
            for k in ("rss_kb", "anon_kb", "fds", "threads", "sockets", "cpu_s"):
                out.setdefault(k, []).append((t, e.get(k)))
    return out


def prom(rows, name):
    return sorted((S.ts_epoch(e["ts"]), e.get("metrics") or {}) for e in rows
                  if e.get("kind") == "prom" and e.get("name") == name and "_error" not in (e.get("metrics") or {}))


def per_key(mem, keys, ph, t0, t1):
    """Slope of memory (KiB) over the unique-key counter between t0 and t1."""
    ks = sorted(keys)
    pairs, j = [], 0
    for t, y in sorted(mem):
        if not (t0 <= t <= t1) or y is None:
            continue
        while j + 1 < len(ks) and ks[j + 1][0] <= t:
            j += 1
        if ks and ks[j][0] <= t:
            pairs.append((ks[j][1], y))
    if len(pairs) < 5 or pairs[-1][0] == pairs[0][0]:
        return None
    f = S.fit(pairs)
    return {"kib_per_key": S.r(f[0], 4), "r2": S.r(f[2]), "keys": [pairs[0][0], pairs[-1][0]],
            "mem_kb": [pairs[0][1], pairs[-1][1]]} if f else None


def relay_facts(rows, name, ph, cache=None):
    ser = series(rows, name)
    pr = prom(rows, name)
    out = {k: S.analyze(v, ph, "mem_kb" if k.endswith("_kb") else k) for k, v in ser.items() if k != "cpu_s"}
    cpu = sorted((t, v) for t, v in ser.get("cpu_s", []) if v is not None and ph.load_start <= t <= ph.load_end)
    if len(cpu) >= 2:
        out["cpu_load_pct"] = S.r((cpu[-1][1] - cpu[0][1]) / (cpu[-1][0] - cpu[0][0]) * 100, 2)
    if pr:
        first = next((m for t, m in pr if t >= (ph.idle or 0)), pr[0][1])
        at_end = [m for t, m in pr if t <= ph.load_end + 30] or [pr[-1][1]]
        end = at_end[-1]
        out["metrics_delta_run"] = {k: v - first.get(k, 0) for k, v in sorted(end.items())
                                    if isinstance(v, (int, float)) and v - first.get(k, 0)}
        keys = [(t, m.get("relayserver_unique_client_keys_total")) for t, m in pr
                if m.get("relayserver_unique_client_keys_total") is not None]
        anon = ser.get("anon_kb", [])
        rss = ser.get("rss_kb", [])
        mid = ph.fit_start + (ph.load_end - ph.fit_start) / 2
        out["anon_per_new_key"] = {
            "fit": per_key(anon, keys, ph, ph.fit_start, ph.load_end),
            "first_half": per_key(anon, keys, ph, ph.fit_start, mid),
            "second_half": per_key(anon, keys, ph, mid, ph.load_end),
            "whole_load": per_key(anon, keys, ph, ph.load_start, ph.load_end),
        }
        out["rss_per_new_key"] = {"whole_load": per_key(rss, keys, ph, ph.load_start, ph.load_end),
                                  "fit": per_key(rss, keys, ph, ph.fit_start, ph.load_end)}
        if cache and keys:
            base = next((k for t, k in keys if t >= ph.load_start), keys[0][1])
            full = next((t for t, k in keys if k - base >= cache), None)
            out["key_cache"] = {"capacity": cache, "full_at_min": S.r((full - ph.load_start) / 60, 1) if full else None}
            if full:
                out["anon_per_new_key"]["before_cache_full"] = per_key(anon, keys, ph, ph.load_start, full)
                out["anon_per_new_key"]["after_cache_full"] = per_key(anon, keys, ph, full + 300, ph.load_end)
    return out


def host_facts(rows, ph):
    load = [e["load1"] for e in rows if e.get("kind") == "host" and "load1" in e
            and ph.load_start <= S.ts_epoch(e["ts"]) <= ph.load_end]
    mem = [e["mem_available_kb"] for e in rows if e.get("kind") == "host" and "mem_available_kb" in e
           and ph.load_start <= S.ts_epoch(e["ts"]) <= ph.load_end]
    if not load:
        return None
    return {"load1_median": S.r(statistics.median(load), 2), "load1_max": max(load),
            "load1_min": min(load), "mem_available_mb_min": S.r(min(mem) / 1024, 0) if mem else None}


def main():
    d = sys.argv[1]
    ph = S.Phases(os.path.join(d, "main"))
    raw = os.path.join(d, "raw")
    relay_rows = S.load_jsonl(os.path.join(raw, "samples-relay1.jsonl"))
    cache = None
    try:
        meta = json.load(open(os.path.join(d, "keycache", "meta.json")))
        cache = int(meta["relay"].split("key_cache_capacity ")[1].rstrip(")"))
    except (OSError, ValueError, KeyError, IndexError):
        pass
    out = {"runs": {}, "relays": {}, "hosts": {}}
    for run in ("main", "keycache"):
        try:
            out["runs"][run] = S.key_facts(json.load(open(os.path.join(d, run, "summary.json"))))
        except (OSError, ValueError):
            pass
    out["phases_utc"] = {k: time.strftime("%H:%M:%S", time.gmtime(v)) for k, v in ph.p.items()}
    out["relays"]["relay"] = relay_facts(relay_rows, "relay", ph)
    out["relays"]["relay2"] = relay_facts(relay_rows, "relay2", ph, cache)
    out["relay_traffic_mb"] = {}
    for name in ("relay", "relay2"):
        m = (out["relays"][name] or {}).get("metrics_delta_run") or {}
        out["relay_traffic_mb"][name] = {"recv": S.r(m.get("relayserver_bytes_recv_total", 0) / 1e6, 1),
                                         "sent": S.r(m.get("relayserver_bytes_sent_total", 0) / 1e6, 1)}
    out["hosts"]["hetzner"] = host_facts(S.load_jsonl(os.path.join(d, "main", "samples.jsonl")), ph)
    out["hosts"]["natlab"] = host_facts(S.load_jsonl(os.path.join(raw, "natlab-main", "samples.jsonl")), ph)
    out["hosts"]["relay1"] = host_facts(relay_rows, ph)
    with open(os.path.join(d, "summary.json"), "w") as f:
        json.dump(out, f, indent=1, default=str)
    print(json.dumps({"relays": {k: {kk: v.get(kk) for kk in ("anon_per_new_key", "key_cache", "cpu_load_pct")}
                                 for k, v in out["relays"].items() if v},
                      "traffic": out["relay_traffic_mb"], "hosts": out["hosts"]}, indent=1))


if __name__ == "__main__":
    main()
