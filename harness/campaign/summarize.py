#!/usr/bin/env python3
"""Summarizes a benchmark campaign directory into summary.json (and table.md).

  summarize.py <campaign dir> [--copy-to FILE]

Input (written by campaign1.sh / campaign2.sh):
  scenarios.jsonl                 one scenario per line: id, group, title, a, b, netem, args
  <group>/<scenario>/<run>/       bench.jsonl (ready ... bench_report), bench.exit,
                                  listen.jsonl (peer, optional), meta.json
  notes.json (optional)           hand-written: environment notes, per-scenario notes,
                                  observations, limitations
  relay-metrics.txt (optional)    the real relay's byte counters before/after steps

Per scenario: runs (run directories), ok_runs (a bench_report, and connected
if the scenario has a peer), final paths, and medians/percentiles over runs.
`rtt_ms` pools the latency-phase replies of all runs (not the first request of
a connection), like bench does within one run; the throughputs, jitter and
loss are medians of the per-run values. Percentiles are nearest-rank.

All bench JSON formats are understood: campaign 1's (commit 9c37664: QUIC
counters only for the whole connection, `peer.quic`), campaign 2's (per-phase
`quic` counters such as `peer.latency.quic`, `peer.connection_lost`,
`relays[].quic_port_explicit`, the `connection_lost` issue and the
`hard_nat_likely` hint; docs/cli-events.md), and the one after it, which adds
`peer.latency.loss` (latency-phase loss counted once the path had settled;
the packet_loss hint judges that instead of `peer.latency.quic`),
`relay_compare.reachable` (with the `relay_fallback_degraded` hint) and
`concurrency.sized_for_mbps`. The verdict checks judge each format by the
rules of its bench (e.g. the packet_loss threshold: 2 % of the whole
connection in campaign 1, 0.5 % of at least 50 latency-phase packets after,
counted from when the path had settled once `latency.loss` exists).
`loss_pct` is always the whole connection's; `latency_loss` (campaign 2 on)
pools the latency-phase counters the hint judges.

summarize_dir() is also used by compare.py.
"""

import argparse
import glob
import json
import math
import os
import re
import statistics
import subprocess
from collections import Counter

# Ground truth for the bench verdict checks: what the bench side's net report
# should show, per profile (lab) or location.
EXPECT_NET = {
    "public": {"behind_nat_v4": False, "port_preserved_v4": True, "udp": True},
    "easy": {"behind_nat_v4": True, "port_preserved_v4": True, "udp": True},
    "restricted": {"behind_nat_v4": True, "port_preserved_v4": True, "udp": True},
    "port-restricted": {"behind_nat_v4": True, "port_preserved_v4": True, "udp": True},
    "double": {"behind_nat_v4": True, "port_preserved_v4": True, "udp": True},
    "cgnat": {"behind_nat_v4": True, "port_preserved_v4": True, "udp": True},
    "symmetric": {"behind_nat_v4": True, "port_preserved_v4": False, "udp": True},
    "udp-blocked": {"udp": False},
}

# Where a direct path is possible in principle (no port prediction): at least
# one side must accept the other's packets on a predictable mapping.
HARD = {"symmetric"}
NO_UDP = {"udp-blocked"}


def pct(values, p):
    v = sorted(x for x in values if x is not None)
    if not v:
        return None
    k = max(1, math.ceil(p / 100 * len(v)))
    return round(v[k - 1], 3)


def med(values):
    v = [x for x in values if x is not None]
    return round(statistics.median(v), 3) if v else None


def p50_p95(values):
    v = [x for x in values if x is not None]
    if not v:
        return None
    return {"p50": pct(v, 50), "p95": pct(v, 95), "n": len(v)}


def load_jsonl(path):
    out = []
    if not os.path.exists(path):
        return out
    with open(path, errors="replace") as f:
        for line in f:
            line = line.strip()
            if line.startswith("{"):
                try:
                    out.append(json.loads(line))
                except json.JSONDecodeError:
                    pass
    return out


def get(d, *keys):
    for k in keys:
        if not isinstance(d, dict):
            return None
        d = d.get(k)
    return d


def load_run(d):
    ev = load_jsonl(os.path.join(d, "bench.jsonl"))
    lev = load_jsonl(os.path.join(d, "listen.jsonl"))
    try:
        meta = json.load(open(os.path.join(d, "meta.json")))
    except (FileNotFoundError, json.JSONDecodeError):
        meta = {}
    try:
        exitc = int(open(os.path.join(d, "bench.exit")).read().strip())
    except (FileNotFoundError, ValueError):
        exitc = None
    ready = next((e for e in ev if e.get("event") == "ready"), None)
    rep = next((e for e in reversed(ev) if e.get("event") == "bench_report"), None)
    lready = next((e for e in lev if e.get("event") == "ready"), None)
    replies = [e for e in ev if e.get("event") == "reply"]
    return {
        "dir": d,
        "name": os.path.basename(d),
        "meta": meta,
        "exit": exitc,
        "ready": ready,
        "report": rep,
        "listener_ready": lready,
        "replies": replies,
        "listen_requests": [e for e in lev if e.get("event") == "request"],
        "errors": [e for e in ev if e.get("event") == "error"],
        "phases": [e for e in ev if e.get("event") == "bench_phase"],
        # The bench's connection closed during the run (both formats).
        "disconnects": [e for e in ev if e.get("event") == "disconnect"],
    }


def new_format(rep):
    """The bench JSON of the verdict/request-timeout fixes (after 9c37664)."""
    if rep is None:
        return False
    peer = rep.get("peer") or {}
    relays = get(rep, "local", "relays") or []
    return "connection_lost" in peer or any("quic_port_explicit" in rl for rl in relays)


def latency_loss_counters(peer):
    """The latency-phase counters the packet_loss hint judges: `latency.loss`
    (counted once the path had settled; may be null) where the bench reports
    it, else `latency.quic` (campaign 2)."""
    lat = (peer or {}).get("latency") or {}
    return lat.get("loss") if "loss" in lat else lat.get("quic")


def side_profile(side):
    if not side:
        return None
    p = side.get("profile", "")
    for k in EXPECT_NET:
        if p == k:
            return k
    if p.startswith("public IP"):
        return "public"
    if p.startswith("relay only"):
        return "relay-only"
    return p


def fmt(v):
    return "-" if v is None else f"{v:.0f}"


def check_run(sc, run):
    """Compares one run's verdict and net report with the ground truth.
    Returns a list of (kind, code, text): kind is right, misleading or wrong."""
    rep = run["report"]
    out = []
    if rep is None:
        return out
    a, b = sc.get("a"), sc.get("b")
    pa, pb = side_profile(a), side_profile(b)
    nr = get(rep, "local", "net_report")
    issues = {i["code"] for i in rep.get("issues", [])}
    hints = {i["code"] for i in rep.get("hints", [])}
    unknown = {u["item"] for u in rep.get("unknowns", [])}
    verdict = rep.get("verdict", "")
    newf = new_format(rep)
    hard_hint = hints & {"hard_nat", "hard_nat_likely"}
    # Emulated packet loss on the path (netem also describes other faults,
    # e.g. the NAT flush of lab-natflush-pr, whose link is clean).
    lossy_link = "loss" in (sc.get("netem") or "")
    exp = EXPECT_NET.get(pa)
    real = "real uplink" in (a or {}).get("location", "")
    # The real relay serves QAD on 56001 only: without ,quic=56001 iroh probes
    # port 7842, which never answers (startup scenarios *-qad-7842).
    qad_unanswered = sc["id"].endswith("-qad-7842")
    qad_off = sc["id"].endswith("-qad-off")
    if qad_off:
        if "udp" in unknown and "udp_blocked" not in issues:
            out.append(("right", "qad_off_unknown", "QAD off: UDP, public address and NAT listed as unknown"))
        else:
            out.append(("wrong", "qad_off", "QAD off, but UDP was judged"))
        return out
    if rep.get("transport") == "relay_only":
        if "--relay-only" in verdict and rep.get("status") == "relay_only":
            out.append(("right", "relay_only_mode", "verdict names --relay-only as the reason"))
        return out
    if nr and exp:
        udp = nr.get("udp_v4") or nr.get("udp_v6")
        if qad_unanswered:
            if udp:
                out.append(("wrong", "udp", "a QAD answer on an unanswered port?"))
        elif udp != exp["udp"]:
            out.append(("wrong", "udp", f"net report udp={udp}, but the {pa} side {'has' if exp['udp'] else 'has no'} UDP"))
        if exp["udp"] and udp:
            for k in ("behind_nat_v4", "port_preserved_v4"):
                if nr.get(k) is not None and nr.get(k) != exp[k]:
                    out.append(("wrong", k, f"{k}={nr.get(k)}, expected {exp[k]} for {pa}"))
            if nr.get("behind_nat_v4") == exp["behind_nat_v4"] and nr.get("port_preserved_v4") == exp["port_preserved_v4"]:
                out.append(("right", "nat_facts", f"behind_nat/port_preserved match the {pa} profile"))
            if pa == "symmetric" and not hard_hint and rep.get("status") != "healthy":
                why = ("mapping_varies_by_dest needs two relays (one configured)"
                       if nr.get("mapping_varies_by_dest") is None else f"mapping_varies_by_dest={nr.get('mapping_varies_by_dest')}")
                out.append(("misleading", "hard_nat_missed",
                            f"symmetric NAT on this side not named ({why}), although port_preserved_v4=false was measured"))
    elif exp and exp["udp"] and nr is None and rep.get("transport") != "relay_only" and rep.get("local"):
        out.append(("misleading", "no_net_report", "no net report"))
    if hard_hint and pa not in HARD:
        out.append(("wrong", "hard_nat_false", f"{'/'.join(sorted(hard_hint))} hint, but the {pa} side keeps its port"))
    udp_msg = next((i.get("message", "") for i in rep.get("issues", []) if i["code"] == "udp_blocked"), "")
    if pa in NO_UDP:
        if "udp_blocked" in issues:
            out.append(("right", "udp_blocked", "udp_blocked issue on the UDP-blocked side"))
        else:
            out.append(("wrong", "udp_blocked_missed", "UDP is blocked on this side, but no udp_blocked issue"))
    elif "udp_blocked" in issues:
        if qad_unanswered and "Either outgoing UDP is blocked" in udp_msg and "port 7842" in udp_msg:
            out.append(("right", "udp_blocked_or_qad_port",
                        "udp_blocked on an unanswered QAD port (7842, iroh's default; quic_port_explicit=false): the "
                        "message names both causes as equally likely, blocked UDP or a relay that does not serve QAD "
                        "on port 7842, and says how to give the port (,quic=PORT)"))
        elif qad_unanswered:
            out.append(("misleading", "udp_blocked_wrong_qad_port",
                        "udp_blocked: 'Outgoing UDP is probably blocked on your network', but UDP works; only the relay's "
                        "QAD port (7842, iroh's default) does not answer (the message names the port)"))
        else:
            out.append(("wrong", "udp_blocked_false", f"udp_blocked reported, but the {pa} side has UDP"))
    peer = rep.get("peer")
    if peer and peer.get("connect", {}).get("ok"):
        status = rep.get("status")
        if status == "relay_only":
            if pa in NO_UDP:
                pass  # covered above
            elif pa in HARD:
                if "hard NAT" in verdict or hard_hint:
                    out.append(("right", "relay_reason", "relay-only blamed on the (likely) hard NAT on this side"))
                elif "one of the two NATs" in verdict:
                    out.append(("misleading", "relay_reason_vague",
                                "relay-only: verdict says 'UDP works on your side; the peer's network may block UDP, or one of "
                                "the two NATs prevents hole punching' while this side's symmetric NAT is (part of) the cause"))
                elif "Your side looks fine" in verdict:
                    out.append(("wrong", "relay_reason", "relay-only: verdict says this side looks fine, but it is a symmetric NAT"))
            else:
                if pb in NO_UDP or pb in HARD or pb == "public" or (pb or "").startswith("public IP"):
                    if "peer's network" in verdict or "peer" in verdict:
                        out.append(("right", "relay_reason", f"relay-only blamed on the peer side ({pb})"))
                if "Your side looks fine" in verdict and pa not in ("public",) and pb not in NO_UDP | HARD:
                    out.append(("misleading", "relay_reason", "verdict says this side looks fine"))
        if status == "relay_only" and nr and nr.get("behind_nat_v4") is False:
            if "two NATs" in verdict:
                out.append(("misleading", "two_nats_without_nat",
                            "relay-only: verdict says 'one of the two NATs prevents hole punching', but this side measured "
                            "behind_nat_v4=false (no NAT; here a stateful firewall, which bench cannot see)"))
            elif "not behind a NAT" in verdict and "firewall" in verdict:
                out.append(("right", "relay_reason_no_nat",
                            "relay-only without a NAT on this side (behind_nat_v4=false): the verdict names the peer's "
                            "network or a firewall in front of this machine (here ufw)"))
        lq = latency_loss_counters(peer)
        settled = "loss" in (peer.get("latency") or {})
        if "packet_loss" in hints and not lossy_link:
            up = get(peer, "upload", "throughput_mbps")
            down = get(peer, "download", "throughput_mbps")
            if newf:
                out.append(("misleading", "packet_loss_unshaped",
                            f"packet_loss hint on a path without emulated loss: {get(lq, 'lost_packets')} of "
                            f"{get(lq, 'sent_packets')} latency-phase packets lost ({get(lq, 'lost_pct')} %"
                            + (f", counted from {fmt(get(lq, 'from_ms'))} ms when the path had settled"
                               if settled else "")
                            + f"), transfers at {fmt(up)}/{fmt(down)} Mbps up/down"))
            else:
                out.append(("misleading", "packet_loss_congestion",
                            f"packet_loss hint ({get(peer, 'quic', 'lost_pct')} % of sent packets lost) on an unshaped path: "
                            f"the losses are congestion losses of the bulk phases ({fmt(up)}/{fmt(down)} Mbps up/down, "
                            "as fast as the other direction), not a lossy link"))
        if status == "healthy" and peer.get("path", {}).get("final_path") == "direct":
            out.append(("right", "healthy_direct", "healthy with a direct path"))
        fp = peer.get("path", {}).get("final_path")
        tl = peer.get("path", {}).get("timeline") or []
        if "through a relay" in verdict and fp != "relay" and tl and tl[-1].get("path") == "direct":
            out.append(("wrong", "says_relay", f"verdict says 'through a relay', but the last path seen was direct "
                        f"(final_path={fp}: the connection was closed, e.g. after request timeouts)"))
        if status == "degraded" and fp == "unknown":
            if "connection_lost" in issues or "connection was lost" in verdict:
                out.append(("right", "connection_lost_explained",
                            "the connection was gone at the end (final_path unknown), and the verdict/connection_lost "
                            "issue says so"))
            else:
                out.append(("misleading", "connection_closed_unexplained",
                            "the connection was gone at the end (final_path unknown), the verdict does not say so"))
        rc = rep.get("relay_compare") or {}
        # Newer bench: the relay fallback is judged by what got through
        # (`reachable`); a failed transfer over it is the relay_fallback_degraded hint.
        if rc.get("reachable") and not rc.get("ok") and "relay_fallback_degraded" in hints \
                and "relay_fallback_failed" not in issues:
            out.append(("right", "relay_fallback_degraded",
                        "the relay-only connection worked, but not everything over it: relay_fallback_degraded "
                        f"hint, not relay_fallback_failed ({get(rc, 'latency', 'ok')} latency replies; failed: "
                        f"{get(rc, 'upload', 'error') or get(rc, 'download', 'error') or 'requests'})"))
        if "relay_fallback_failed" in issues and get(rc, "connect", "ok") and (
                get(rc, "latency", "ok") or get(rc, "download", "ok")):
            out.append(("misleading", "relay_fallback_failed_but_works",
                        "relay_fallback_failed ('the relay-only connection did not work ... this peer cannot be "
                        f"reached'), but it connected and worked: {get(rc, 'latency', 'ok')} latency replies, "
                        f"download {fmt(get(rc, 'download', 'throughput_mbps'))} Mbps; failed: "
                        f"{get(rc, 'upload', 'error') or get(rc, 'download', 'error')}"))
        cl = peer.get("connection_lost")
        if cl and cl.get("phase") != "idle" and "connection_lost" not in issues:
            out.append(("wrong", "connection_lost_missed",
                        f"the connection was lost during {cl.get('phase')} ({cl.get('cause')}), no connection_lost issue"))
        idle = peer.get("idle")
        if idle:
            if idle.get("survived") and "idle_reconnect" not in hints and "idle_failed" not in issues:
                out.append(("right", "idle", "idle: survived, no idle finding"))
            elif not idle.get("survived") and "idle_reconnect" in hints:
                out.append(("right", "idle", "idle: reconnect reported"))
        q = peer.get("quic") or {}
        if lossy_link:
            if newf:
                lost, sent, lpct = get(lq, "lost_packets"), get(lq, "sent_packets"), get(lq, "lost_pct")
                seen = f"{lost} of {sent} latency-phase packets lost ({lpct} %)"
                if settled:
                    seen += (f", counted from {fmt(get(lq, 'from_ms'))} ms when the path had settled"
                             if lq else ", path not settled (no loss count)")
                if "packet_loss" in hints:
                    out.append(("right", "packet_loss", f"packet_loss hint: {seen}"))
                elif lpct is not None and sent is not None and lpct >= 0.5 and sent >= 50:
                    out.append(("wrong", "packet_loss_threshold",
                                f"{seen}, at or above the hint's threshold (0.5 % of >= 50 packets), but no packet_loss hint"))
                else:
                    out.append(("misleading", "packet_loss_missed",
                                f"1 % loss each way on the link, no packet_loss hint: {seen}, threshold 0.5 % of >= 50 "
                                + ("(about 140 packets: one run in four sees no loss at 1 %)" if settled else
                                   "(about 110 packets: one run in three sees no loss at 1 %)")))
            elif "packet_loss" in hints:
                out.append(("right", "packet_loss", f"packet_loss hint ({q.get('lost_pct')} % lost)"))
            else:
                out.append(("misleading", "packet_loss_missed",
                            f"1 % loss each way on the link, no packet_loss hint (lost_pct={q.get('lost_pct')}, threshold 2 %)"))
    if "nat_mapping" in unknown:
        out.append(("right", "nat_mapping_unknown", "nat_mapping listed as unknown (one relay)"))
    if real and nr and nr.get("public_v4") and not nr["public_v4"].startswith("213.163.75.157:"):
        out.append(("wrong", "public_v4", f"public_v4 {nr['public_v4']}, expected 213.163.75.157"))
    return out


def phase_medians(runs):
    by = {}
    for r in runs:
        for e in r["phases"]:
            by.setdefault(e.get("phase"), []).append(e.get("duration_ms"))
    return {k: med(v) for k, v in by.items()} or None


def latency_loss(peers):
    """Pooled latency-phase counters that the packet_loss hint judges
    (campaign 2 on: `latency.loss` where present, else `latency.quic`), and how
    many runs lost any packet there or got the packet_loss hint's inputs."""
    qs = [latency_loss_counters(p) for p in peers]
    qs = [q for q in qs if q]
    if not qs:
        return None
    sent = sum(q.get("sent_packets") or 0 for q in qs)
    lost = sum(q.get("lost_packets") or 0 for q in qs)
    return {
        "runs": len(qs),
        "sent_packets": sent,
        "lost_packets": lost,
        "lost_pct": round(100 * lost / sent, 3) if sent else None,
        "runs_with_loss": sum(1 for q in qs if (q.get("lost_packets") or 0) > 0),
        "runs_at_threshold": sum(1 for q in qs if (q.get("sent_packets") or 0) >= 50
                                 and (q.get("lost_pct") or 0) >= 0.5),
        "per_run": [f"{q.get('lost_packets')}/{q.get('sent_packets')}" for q in qs],
    }


def summarize_scenario(sc, runs, notes):
    reps = [r["report"] for r in runs if r["report"]]
    has_peer = any(get(r, "peer") for r in reps) or any((r["meta"] or {}).get("to") for r in runs)
    ok_runs = 0
    final = Counter()
    for r in runs:
        rep = r["report"]
        if rep is None:
            if has_peer:
                final["failed"] += 1
            continue
        peer = rep.get("peer")
        if has_peer:
            if peer and peer["connect"].get("ok"):
                ok_runs += 1
                fp = peer["path"].get("final_path")
                final["direct" if fp == "direct" else "relay" if fp == "relay" else "failed"] += 1
                if fp not in ("direct", "relay"):
                    final["unknown"] += 1
            else:
                final["failed"] += 1
        else:
            ok_runs += 1
    peers = [r.get("peer") for r in reps if r.get("peer") and r["peer"]["connect"].get("ok")]
    rtts = []
    for r in runs:
        for e in r["replies"]:
            if e.get("phase") == "latency" and e.get("ok") and not e.get("new_conn"):
                rtts.append(e.get("rtt_ms"))
    rcs = [r.get("relay_compare") for r in reps if r.get("relay_compare") and not r["relay_compare"].get("skipped")]
    idles = [p.get("idle") for p in peers if p.get("idle")]

    def tput(sec):
        return med([get(p, sec, "throughput_mbps") for p in peers if get(p, sec, "ok")])

    s = {
        "id": sc["id"],
        "group": sc["group"],
        "title": sc["title"],
        "a": sc.get("a"),
        "b": sc.get("b"),
        "netem": sc.get("netem"),
        "args": sc.get("args"),
        "runs": len(runs),
        "ok_runs": ok_runs,
        "path_final": {"direct": final["direct"], "relay": final["relay"], "failed": final["failed"]} if has_peer else None,
        # connected, but no connection at the end (counted in "failed" above)
        "path_final_unknown": final["unknown"] if has_peer else None,
        "direct_seen": sum(1 for p in peers if get(p, "path", "time_to_direct_ms") is not None) if has_peer else None,
        "first_path": dict(Counter(p["connect"].get("path") for p in peers)) if has_peer else None,
        "connect_ms": p50_p95([p["connect"].get("connect_ms") for p in peers]),
        "lookup_ms": p50_p95([p["connect"].get("lookup_ms") for p in peers]),
        "time_to_direct_ms": p50_p95([get(p, "path", "time_to_direct_ms") for p in peers]),
        "rtt_ms": p50_p95(rtts),
        "jitter_ms": med([get(p, "latency", "jitter_ms") for p in peers]),
        "loss_pct": med([get(p, "quic", "lost_pct") for p in peers]),
        "direct_fraction": med([get(p, "latency", "direct_fraction") for p in peers]),
        "upload_mbps": tput("upload"),
        "download_mbps": tput("download"),
        "concurrency_mbps": tput("concurrency"),
        "loaded_rtt_ms_p50": med([get(p, "concurrency", "loaded_rtt_ms", "p50") for p in peers]),
        "transfer_relay_capped": any(get(p, "upload", "relay_capped") for p in peers) or None,
        # Whole bench run (bench_report.duration_ms), and the median duration of each phase.
        "run_duration_s": p50_p95([round(r["report"]["duration_ms"] / 1000, 1) for r in runs
                                   if r["report"] and r["report"].get("duration_ms") is not None]),
        "phase_ms_p50": phase_medians(runs),
        # The connection made in the connect phase closed before the end of the
        # run: disconnect events (both formats), peer.connection_lost (newer format).
        "disconnect_runs": sum(1 for r in runs if r["disconnects"]) if has_peer else None,
        "connection_lost": dict(Counter(f"{c.get('phase')}:{c.get('cause')}" for c in
                                        (p.get("connection_lost") for p in peers) if c)) or None,
        # QUIC counters of the connection still there at the end (peer.quic not null).
        "quic_counters_runs": sum(1 for p in peers if p.get("quic")) if has_peer else None,
        "latency_loss": latency_loss(peers),
        "concurrency": {
            "ok": sum(1 for p in peers if get(p, "concurrency", "ok")),
            "runs": sum(1 for p in peers if p.get("concurrency")),
            "failed": sum(get(p, "concurrency", "failed") or 0 for p in peers),
            "unfinished": sum(get(p, "concurrency", "unfinished") or 0 for p in peers),
            "requests": sum(get(p, "concurrency", "requests") or 0 for p in peers),
            "throughput_null_but_ok": sum(1 for p in peers if get(p, "concurrency", "ok")
                                          and get(p, "concurrency", "throughput_mbps") is None),
            # Newer bench: requests made smaller for a slow link.
            **({"sized_runs": sum(1 for p in peers if get(p, "concurrency", "sized_for_mbps") is not None),
                "request_bytes": sorted({get(p, "concurrency", "request_bytes") for p in peers
                                         if get(p, "concurrency", "request_bytes") is not None})}
               if any("sized_for_mbps" in (p.get("concurrency") or {}) for p in peers) else {}),
        } if any(p.get("concurrency") for p in peers) else None,
        "transfer_limited_by": dict(Counter(f"{sec}:{get(p, sec, 'limited_by')}" for p in peers
                                            for sec in ("upload", "download", "concurrency")
                                            if get(p, sec, "limited_by"))) or None,
        "relay_compare": None,
        "idle": None,
        "bind_ms": p50_p95([get(r["ready"], "bind_ms") for r in runs if r["ready"]]),
        "listener_bind_ms": p50_p95([get(r["listener_ready"], "bind_ms") for r in runs if r["listener_ready"]]),
        "local": None,
        "exit_codes": dict(sorted(Counter(str(r["exit"]) for r in runs).items())),
        "statuses": dict(Counter(r.get("status") for r in reps)),
        "issues": dict(Counter(i["code"] for r in reps for i in r.get("issues", []))),
        "hints": dict(Counter(i["code"] for r in reps for i in r.get("hints", []))),
        "verdict_sample": None,
        "notes": notes.get("scenario_notes", {}).get(sc["id"], ""),
        "run_dirs": [os.path.relpath(r["dir"], notes["_root"]) for r in runs],
    }
    if rcs:
        s["relay_compare"] = {
            "rtt_ms_p50": med([get(rc, "latency", "rtt_ms", "p50") for rc in rcs]),
            "upload_mbps": med([get(rc, "upload", "throughput_mbps") for rc in rcs if get(rc, "upload", "ok")]),
            "download_mbps": med([get(rc, "download", "throughput_mbps") for rc in rcs if get(rc, "download", "ok")]),
            "connect_ms_p50": med([get(rc, "connect", "connect_ms") for rc in rcs]),
            "ok": sum(1 for rc in rcs if rc.get("ok")),
            "runs": len(rcs),
        }
    if idles:
        s["idle"] = {
            "survived": sum(1 for i in idles if i.get("survived")),
            "ok": sum(1 for i in idles if i.get("ok")),
            "runs": len(idles),
            "recovery_ms": med([i.get("recovery_ms") for i in idles]),
            "path_after": dict(Counter(i.get("path_after") for i in idles)),
            "idle_ms": idles[0].get("idle_ms"),
        }
    locs = [r.get("local") for r in reps if r.get("local")]
    if locs:
        nrs = [l.get("net_report") for l in locs if l.get("net_report")]
        relays = [rl for l in locs for rl in l.get("relays", [])]
        s["local"] = {
            "duration_ms": p50_p95([l.get("duration_ms") for l in locs]),
            "net_report_runs": len(nrs),
            "udp_v4": dict(Counter(str(n.get("udp_v4")).lower() for n in nrs)),
            "public_v4_sample": next((n.get("public_v4") for n in nrs if n.get("public_v4")), None),
            "behind_nat_v4": dict(Counter(str(n.get("behind_nat_v4")).lower() for n in nrs)),
            "port_preserved_v4": dict(Counter(str(n.get("port_preserved_v4")).lower() for n in nrs)),
            "mapping_varies_by_dest": dict(Counter(str(n.get("mapping_varies_by_dest")).lower() for n in nrs)),
            "qad_v4_ms_p50": med([rl.get("qad_v4_ms") for rl in relays]),
            "relay_tls_ms_p50": med([rl.get("tls_ms") for rl in relays]),
            "relay_https_latency_ms_p50": med([rl.get("https_latency_ms") for rl in relays]),
            "relay_ok": sum(1 for rl in relays if rl.get("ok")),
            "relay_checks": len(relays),
            "home_relay_connected": sum(1 for l in locs if get(l, "home_relay", "connected")),
            "record_published": sum(1 for l in locs if l.get("record_published")),
        }
    # Parallel relay runs: sum of the per-bench throughputs per round.
    if sc["group"] == "relay" and any("-" in r["name"] for r in runs):
        rounds = {}
        for r in runs:
            rd = r["name"].split("-")[0]
            rounds.setdefault(rd, []).append(r)
        agg_up, agg_down = [], []
        for rr in rounds.values():
            ups = [get(x["report"], "peer", "upload", "throughput_mbps") for x in rr if x["report"]]
            downs = [get(x["report"], "peer", "download", "throughput_mbps") for x in rr if x["report"]]
            if ups and all(u is not None for u in ups):
                agg_up.append(round(sum(ups), 3))
            if downs and all(d is not None for d in downs):
                agg_down.append(round(sum(downs), 3))
        s["parallel_aggregate"] = {"rounds": len(rounds), "upload_mbps_sum_per_round": agg_up, "download_mbps_sum_per_round": agg_down}
    # A representative verdict: the most common status's first run.
    if reps:
        common = Counter(r.get("status") for r in reps).most_common(1)[0][0]
        s["verdict_sample"] = next(r.get("verdict") for r in reps if r.get("status") == common)
    checks = Counter()
    texts = {}
    per_run = Counter()
    for r in runs:
        kinds = set()
        for kind, code, text in check_run(sc, r):
            checks[(kind, code)] += 1
            texts.setdefault((kind, code), text)
            kinds.add(kind)
        if r["report"] is not None:
            # A run counts by its worst finding.
            per_run["wrong" if "wrong" in kinds else "misleading" if "misleading" in kinds else "right"] += 1
    s["verdict_runs"] = dict(per_run)
    s["verdict_checks"] = [
        {"kind": k, "code": c, "runs": n, "detail": texts[(k, c)]} for (k, c), n in sorted(checks.items())
    ]
    return s


def relay_traffic(path):
    if not os.path.exists(path):
        return None
    snaps = []
    cur = None
    for line in open(path):
        line = line.strip()
        if line.startswith("== "):
            cur = {"label": line[3:]}
            snaps.append(cur)
        elif cur is not None and line.startswith("relayserver_"):
            k, v = line.split()
            cur[k] = int(float(v))
    steps = []
    for x, y in zip(snaps, snaps[1:]):
        if x["label"].split()[0].endswith("-start") and y["label"].split()[0].endswith("-end"):
            rx = y.get("relayserver_bytes_recv_total", 0) - x.get("relayserver_bytes_recv_total", 0)
            tx = y.get("relayserver_bytes_sent_total", 0) - x.get("relayserver_bytes_sent_total", 0)
            steps.append({"step": x["label"].split()[0][:-6], "from": x["label"].split()[1], "to": y["label"].split()[1],
                          "relay_bytes_recv": rx, "relay_bytes_sent": tx})
    return {"steps": steps, "total_bytes_recv": sum(s["relay_bytes_recv"] for s in steps),
            "total_bytes_sent": sum(s["relay_bytes_sent"] for s in steps)}


def summarize_dir(root):
    """All scenarios of a campaign directory, and its notes (with `_root`)."""
    root = os.path.abspath(root)
    scenarios = [json.loads(l) for l in open(os.path.join(root, "scenarios.jsonl")) if l.strip()]
    try:
        notes = json.load(open(os.path.join(root, "notes.json")))
    except FileNotFoundError:
        notes = {}
    notes["_root"] = root
    out = []
    for sc in scenarios:
        dirs = sorted(
            d for d in glob.glob(os.path.join(root, sc["group"], sc["id"], "run*")) if os.path.isdir(d)
        )
        dirs.sort(key=lambda d: [int(x) if x.isdigit() else x for x in re.split(r"(\d+)", os.path.basename(d))])
        runs = [load_run(d) for d in dirs]
        if not runs:
            continue
        out.append(summarize_scenario(sc, runs, notes))
    return out, notes


def verdict_totals(scenarios):
    acc = Counter()
    acc_runs = Counter()
    for sc in scenarios:
        for c in sc["verdict_checks"]:
            acc[c["kind"]] += c["runs"]
        acc_runs.update(sc["verdict_runs"])
    return {"checks": dict(acc), "runs_by_worst_finding": dict(acc_runs)}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("dir")
    ap.add_argument("--copy-to")
    a = ap.parse_args()
    root = os.path.abspath(a.dir)
    out, notes = summarize_dir(root)
    try:
        commit = subprocess.run(["git", "-C", root, "rev-parse", "--short", "HEAD"], capture_output=True, text=True).stdout.strip()
    except OSError:
        commit = None
    deployed = None
    for sc in out:
        for d in sc["run_dirs"]:
            try:
                m = json.load(open(os.path.join(root, d, "meta.json")))
                if m.get("deployed"):
                    deployed = m["deployed"]
                    break
            except (FileNotFoundError, json.JSONDecodeError):
                pass
        if deployed:
            break
    # Verdict accuracy, over all scenarios.
    totals = verdict_totals(out)
    summary = {
        "commit": notes.get("commit") or commit,
        "binary": notes.get("binary") or deployed,
        "date": notes.get("date"),
        "environment": notes.get("environment", {}),
        "relay_traffic": relay_traffic(os.path.join(root, "relay-metrics.txt")),
        "verdict_check_totals": totals,
        "scenarios": out,
        "observations": notes.get("observations", []),
        "limitations": notes.get("limitations", []),
    }
    path = os.path.join(root, "summary.json")
    with open(path, "w") as f:
        json.dump(summary, f, indent=1)
        f.write("\n")
    if a.copy_to:
        os.makedirs(os.path.dirname(a.copy_to), exist_ok=True)
        with open(a.copy_to, "w") as f:
            json.dump(summary, f, indent=1)
            f.write("\n")
    # A Markdown table for a quick look.
    rows = ["| scenario | runs ok | final d/r/f | connect p50 ms | ttd p50 ms | RTT p50/p95 ms | up/down/conc Mbps "
            "| relay cmp rtt/up/down | idle | exit codes | run s p50 | loss % latency (lost/sent) / connection |",
            "|---|---|---|---|---|---|---|---|---|---|---|---|"]

    def f(v, nd=0):
        return "-" if v is None else (f"{v:.{nd}f}" if isinstance(v, (int, float)) else str(v))

    for s in out:
        pf = s["path_final"]
        rc = s["relay_compare"]
        idle = s["idle"]
        ll = s["latency_loss"]
        rows.append(
            f"| {s['id']} | {s['ok_runs']}/{s['runs']} | "
            + (f"{pf['direct']}/{pf['relay']}/{pf['failed']}" if pf else "-")
            + f" | {f(get(s, 'connect_ms', 'p50'))} | {f(get(s, 'time_to_direct_ms', 'p50'))} | "
            f"{f(get(s, 'rtt_ms', 'p50'), 1)}/{f(get(s, 'rtt_ms', 'p95'), 1)} | "
            f"{f(s['upload_mbps'], 1)}/{f(s['download_mbps'], 1)}/{f(s['concurrency_mbps'], 1)} | "
            + (f"{f(rc['rtt_ms_p50'], 1)}/{f(rc['upload_mbps'], 1)}/{f(rc['download_mbps'], 1)}" if rc else "-")
            + " | "
            + (f"{idle['survived']}/{idle['runs']} survived, rec {f(idle['recovery_ms'])} ms" if idle else "-")
            + f" | {' '.join(k + ':' + str(v) for k, v in s['exit_codes'].items())} | "
            + f"{f(get(s, 'run_duration_s', 'p50'), 1)} | "
            + (f"{f(ll['lost_pct'], 2)} ({ll['lost_packets']}/{ll['sent_packets']})" if ll else "-")
            + f" / {f(s['loss_pct'], 2)} |"
        )
    with open(os.path.join(root, "table.md"), "w") as fh:
        fh.write("\n".join(rows) + "\n")
    print("\n".join(rows))
    print(f"verdict checks: {totals['checks']}; runs by worst finding: {totals['runs_by_worst_finding']}")


if __name__ == "__main__":
    main()
