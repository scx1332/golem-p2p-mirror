#!/usr/bin/env python3
"""What happened to the concurrency phase's requests on the lossy link.

  cutoff.py <campaign dir>... [--json OUT]

For every lab-netem-pr run of each campaign (lab/lab-netem-pr/run*/ and
debug/lossy-extra/run*/), from bench.jsonl and the listener's listen.jsonl:

* the phase: request size, sized_for_mbps, restart (at, cut off, first
  size), first_reply_ms, went_on (no restart, first reply after half the
  time limit), counted throughput, requests, unfinished;
* `sent_mbps`: what the bench's connection sent during the phase (QUIC UDP
  payload bytes, concurrency.quic.sent_bytes, over the phase), against
  `counted_mbps` (payload of the counted requests over their part of the
  phase): a large gap means data that was not counted (cut-off requests
  still being sent; also retransmissions and the loaded-RTT probes);
* `late`: events of the listener for the bench's identity (requests read,
  echoes that failed, resets seen) after the phase had ended, i.e. cut-off
  requests that still reached the listener; `late_max_s` how long after the
  end the last one came, `late_after_200ms` those more than 200 ms after it
  (beyond the reset's one-way trip on this link);
* `after_restart_big`: listener events of first-size requests more than
  200 ms after a restart (cut-off requests that kept arriving);
* `listener_errors`: the listener's error codes for the bench's identity
  (code 0: the echo was refused because the bench had dropped the stream,
  campaigns up to 4; code 11: CANCELLED, the bench reset the request).

Campaign 4 found that cut-off requests reached the listener up to 3.9 s
after the phase (7 of 13 runs); from campaign 5 on golem-p2p resets them.
"""

import argparse
import calendar
import glob
import json
import os
import re
import statistics
import time
from collections import Counter


def ts(s):
    base, _, frac = s.rstrip("Z").partition(".")
    return calendar.timegm(time.strptime(base, "%Y-%m-%dT%H:%M:%S")) + (float("0." + frac) if frac else 0.0)


def load(path):
    out = []
    try:
        for line in open(path):
            if line.startswith("{"):
                try:
                    out.append(json.loads(line))
                except json.JSONDecodeError:
                    pass
    except OSError:
        pass
    return out


def run(d):
    bench = load(os.path.join(d, "bench.jsonl"))
    listen = load(os.path.join(d, "listen.jsonl"))
    ready = next((e for e in bench if e.get("event") == "ready"), None)
    conc = next((e for e in bench if e.get("event") == "bench_phase" and e.get("phase") == "concurrency"), None)
    rep = next((e for e in bench if e.get("event") == "bench_report"), None)
    if not ready or not conc:
        return {"dir": d, "error": "no concurrency phase"}
    me = ready["node_id"]
    end = ts(conc["ts"])
    start = end - conc["duration_ms"] / 1000
    c = conc.get("result") or {}
    t_limit = ((rep or {}).get("settings") or {}).get("transfer_time_ms") or 8000
    rs = c.get("restart")
    q = c.get("quic") or {}
    counted_s = (c.get("duration_ms") or 0) / 1000
    res = {
        "dir": d,
        "request_bytes": c.get("request_bytes"),
        "sized_for_mbps": c.get("sized_for_mbps"),
        "restart": rs,
        "first_reply_ms": c.get("first_reply_ms"),
        "went_on": bool(not rs and c.get("first_reply_ms") is not None and c["first_reply_ms"] > t_limit / 2),
        "requests": c.get("requests"),
        "unfinished": c.get("unfinished"),
        "counted_mbps": c.get("throughput_mbps"),
        "phase_s": round(end - start, 3),
        "sent_mbps": round(q["sent_bytes"] * 8 / (end - start) / 1e6, 3) if q.get("sent_bytes") else None,
        "received_mbps": round(q["received_bytes"] * 8 / (end - start) / 1e6, 3) if q.get("received_bytes") else None,
        "counted_share_of_sent": round(c.get("total_bytes", 0) / q["sent_bytes"], 3) if q.get("sent_bytes") else None,
        "counted_s": round(counted_s, 3),
    }
    late = []
    big_after = []
    codes = Counter()
    first_size = rs.get("request_bytes") if rs else None
    for e in listen:
        who = e.get("from") or e.get("peer")
        if who != me or e.get("event") not in ("request", "error"):
            continue
        t = ts(e["ts"])
        if e.get("event") == "error":
            m = re.search(r"\(code (\d+)\)", e.get("message", ""))
            codes[(e.get("kind"), int(m.group(1)) if m else None)] += 1
        if start <= t and t > end:
            late.append(round(t - end, 3))
        if rs and t > start + rs["at_ms"] / 1000 + 0.2 and t <= end + 30:
            if e.get("event") == "error" or e.get("bytes") == first_size:
                big_after.append(round(t - start - rs["at_ms"] / 1000, 3))
    res["late"] = len(late)
    res["late_after_200ms"] = sum(1 for x in late if x > 0.2)
    res["late_max_s"] = max(late) if late else None
    res["after_restart_big"] = len(big_after) if rs else None
    res["listener_errors"] = {f"{k}:{c_}": n for (k, c_), n in sorted(codes.items(), key=str)}
    return res


def campaign(cdir):
    dirs = sorted(glob.glob(os.path.join(cdir, "lab", "lab-netem-pr", "run*"))) + sorted(
        glob.glob(os.path.join(cdir, "debug", "lossy-extra", "run*")), key=lambda p: int(re.sub(r"\D", "", os.path.basename(p)) or 0))
    runs = [run(d) for d in dirs if os.path.isfile(os.path.join(d, "bench.jsonl"))]
    ok = [r for r in runs if "error" not in r]
    tot = {
        "runs": len(runs),
        "restarts": sum(1 for r in ok if r["restart"]),
        "went_on": sum(1 for r in ok if r["went_on"]),
        "runs_with_late_events": sum(1 for r in ok if r["late"]),
        "runs_with_late_after_200ms": sum(1 for r in ok if r["late_after_200ms"]),
        "late_max_s": max((r["late_max_s"] or 0 for r in ok), default=None),
        "counted_mbps": [r["counted_mbps"] for r in ok],
        "counted_mbps_median": statistics.median([r["counted_mbps"] for r in ok if r["counted_mbps"] is not None]) if ok else None,
        "counted_share_of_sent_median": statistics.median([r["counted_share_of_sent"] for r in ok
                                                           if r["counted_share_of_sent"] is not None]) if ok else None,
        "listener_errors": dict(sum((Counter(r["listener_errors"]) for r in ok), Counter())),
    }
    return {"campaign": os.path.basename(os.path.abspath(cdir)), "totals": tot, "runs": runs}


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("dirs", nargs="+")
    ap.add_argument("--json")
    a = ap.parse_args()
    out = [campaign(d) for d in a.dirs]
    for c in out:
        t = c["totals"]
        print(f"== {c['campaign']}: {t['runs']} runs, restarts {t['restarts']}, went on {t['went_on']}, "
              f"late events in {t['runs_with_late_events']} runs ({t['runs_with_late_after_200ms']} > 200 ms, "
              f"max {t['late_max_s']} s), counted Mbps median {t['counted_mbps_median']}, "
              f"counted/sent median {t['counted_share_of_sent_median']}, listener errors {t['listener_errors']}")
        for r in c["runs"]:
            if "error" in r:
                print(f"  {r['dir']}: {r['error']}")
                continue
            rs = r["restart"]
            print(f"  {os.path.relpath(r['dir'], os.path.dirname(os.path.abspath(r['dir'])) + '/../..')}: "
                  f"size {r['request_bytes']} sized_for {r['sized_for_mbps']} "
                  f"restart {('at %.0f ms from %s' % (rs['at_ms'], rs['request_bytes'])) if rs else '-'} "
                  f"first_reply {r['first_reply_ms']} went_on {r['went_on']} counted {r['counted_mbps']} Mbps "
                  f"sent {r['sent_mbps']} Mbps share {r['counted_share_of_sent']} late {r['late']} "
                  f"(>200ms {r['late_after_200ms']}, max {r['late_max_s']}) big_after_restart {r['after_restart_big']} "
                  f"errors {r['listener_errors']}")
    if a.json:
        with open(a.json, "w") as f:
            json.dump(out, f, indent=1)


if __name__ == "__main__":
    main()
