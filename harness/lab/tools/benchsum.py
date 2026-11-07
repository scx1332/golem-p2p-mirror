#!/usr/bin/env python3
"""Helpers for `lab.sh bench`/`benchpair`/`benchmatrix` (golem-p2p `bench --json`).

  benchsum.py meta <out.json>   key=value lines on stdin -> JSON object (run metadata)
  benchsum.py line <run dir>    one key=value summary line of a run

A run directory holds `bench.jsonl` (stdout of `golem-p2p --json bench`: the
`ready` event first, the `bench_report` event last), `bench.err`,
`bench.exit`, `meta.json` and, for benchpair, `listen.jsonl`/`listen.err`.

Summary fields ("-" = not measured):
  listener/bencher (+ _profile)  lab nodes
  exit, status     bench exit code and status (healthy, relay_only, ...)
  bind_ms          bench's `ready.bind_ms` (bind, incl. waiting for relay/addresses)
  connect_ms       peer.connect.connect_ms (directory lookup + handshakes)
  first_path       path right after connecting
  ttd_ms           peer.path.time_to_direct_ms
  final_path       peer.path.final_path; changes = peer.path.changes
  direct_frac      latency phase: share of replies over a direct path
  rtt_p50/rtt_p95/jitter   latency phase, ms
  lost_pct         QUIC packets sent by the bencher and declared lost
  up/down/conc     Mbps of upload, download, concurrency (path at the end in [])
  rc               --compare-relay: rtt p50 / up / down over the relay
  idle             --idle: survived/ok, path before->after, recovery ms
  listener_saw     paths of the requests the listener served (path:count)
  verdict          the bench verdict
"""

import json
import sys
from collections import Counter


def load_jsonl(path):
    out = []
    try:
        with open(path, errors="replace") as f:
            for line in f:
                line = line.strip()
                if line.startswith("{"):
                    try:
                        out.append(json.loads(line))
                    except json.JSONDecodeError:
                        pass
    except FileNotFoundError:
        pass
    return out


def fmt(v, nd=1):
    if v is None:
        return "-"
    if isinstance(v, float):
        return f"{v:.{nd}f}"
    return str(v)


def get(d, *keys):
    for k in keys:
        if not isinstance(d, dict):
            return None
        d = d.get(k)
    return d


def meta(out):
    m = {}
    for line in sys.stdin:
        line = line.rstrip("\n")
        if "=" not in line:
            continue
        k, v = line.split("=", 1)
        if k.endswith("_udp_timeout") and v.isdigit():
            v = int(v)
        elif v == "-":
            v = None
        m[k] = v
    with open(out, "w") as f:
        json.dump(m, f, indent=1, sort_keys=True)
        f.write("\n")


def line(d):
    ev = load_jsonl(f"{d}/bench.jsonl")
    try:
        m = json.load(open(f"{d}/meta.json"))
    except (FileNotFoundError, json.JSONDecodeError):
        m = {}
    try:
        exitc = open(f"{d}/bench.exit").read().strip()
    except FileNotFoundError:
        exitc = "-"
    ready = next((e for e in ev if e.get("event") == "ready"), {})
    rep = next((e for e in ev if e.get("event") == "bench_report"), None)
    res = {}
    for role in ("listener", "bencher"):
        if m.get(role):
            res[role] = m[role]
            res[f"{role}_profile"] = m.get(f"{role}_profile")
    res["run"] = m.get("run")
    res["exit"] = exitc
    res["bind_ms"] = fmt(ready.get("bind_ms"), 0)
    if rep is None:
        res["status"] = "no-report"
        errs = [e for e in ev if e.get("event") == "error"]
        if errs:
            res["error"] = errs[-1].get("message", "")[:200]
        return res
    res["status"] = rep.get("status")
    p = rep.get("peer") or {}
    c = p.get("connect") or {}
    res["connect_ms"] = fmt(c.get("connect_ms"), 0)
    res["first_path"] = c.get("path") or "-"
    path = p.get("path") or {}
    res["ttd_ms"] = fmt(path.get("time_to_direct_ms"), 0)
    res["final_path"] = path.get("final_path") or "-"
    res["changes"] = fmt(path.get("changes"))
    lat = p.get("latency") or {}
    res["direct_frac"] = fmt(lat.get("direct_fraction"), 2)
    res["rtt_p50"] = fmt(get(lat, "rtt_ms", "p50"))
    res["rtt_p95"] = fmt(get(lat, "rtt_ms", "p95"))
    res["jitter"] = fmt(lat.get("jitter_ms"))
    res["lost_pct"] = fmt(get(p, "quic", "lost_pct"), 2)
    for k, sec in (("up", "upload"), ("down", "download"), ("conc", "concurrency")):
        t = p.get(sec)
        if t is None:
            res[k] = "-"
        elif not t.get("ok"):
            res[k] = "FAIL:" + str(t.get("error_kind"))
        else:
            res[k] = fmt(t.get("throughput_mbps")) + (f"[{t['path']}]" if t.get("path") else "")
    rc = rep.get("relay_compare")
    if rc:
        if rc.get("skipped"):
            res["rc"] = "skipped"
        else:
            res["rc"] = "/".join(
                [
                    fmt(get(rc, "latency", "rtt_ms", "p50")),
                    fmt(get(rc, "upload", "throughput_mbps")),
                    fmt(get(rc, "download", "throughput_mbps")),
                ]
            ) + ("" if rc.get("ok") else ":FAIL")
    idle = p.get("idle")
    if idle:
        res["idle"] = (
            f"survived={idle.get('survived')},ok={idle.get('ok')},"
            f"{idle.get('path_before')}->{idle.get('path_after')},"
            f"recovery_ms={fmt(idle.get('recovery_ms'), 0)}"
        )
    lev = load_jsonl(f"{d}/listen.jsonl")
    if lev:
        saw = Counter(e.get("path") for e in lev if e.get("event") == "request")
        res["listener_saw"] = ",".join(f"{k}:{v}" for k, v in sorted(saw.items())) or "-"
    res["issues"] = ",".join(i["code"] for i in rep.get("issues", [])) or "-"
    res["hints"] = ",".join(i["code"] for i in rep.get("hints", [])) or "-"
    res["verdict"] = rep.get("verdict", "")
    return res


def render(res):
    parts = []
    for k, v in res.items():
        v = "-" if v is None else str(v)
        if any(ch in v for ch in " \"'\t"):
            v = json.dumps(v)
        parts.append(f"{k}={v}")
    return " ".join(parts)


def main():
    if len(sys.argv) != 3 or sys.argv[1] not in ("meta", "line"):
        sys.exit(__doc__)
    if sys.argv[1] == "meta":
        meta(sys.argv[2])
    else:
        print(render(line(sys.argv[2])))


if __name__ == "__main__":
    main()
