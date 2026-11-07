#!/usr/bin/env python3
"""Compares benchmark campaigns scenario by scenario.

  compare.py <base campaign dir> <new campaign dir> [--out DIR]
  compare.py --series <campaign dir>... [--out DIR]

Both directories are summarized from their raw runs with summarize.py's rules
(summarize_dir, which reads both bench JSON formats), so the verdict
classification of the base campaign is recomputed, not read from its
summary.json. Writes compare.json and compare.md into --out (default: the new
campaign directory).

Per scenario, base -> new: outcome (final path direct/relay/failed, failed
includes connections gone at the end), RTT p50, upload/download Mbps, bind_ms
p50, run duration p50, connection losses (disconnect events), verdict
classification of the runs (right/misleading/wrong by their worst finding)
and the misleading/wrong check codes; then totals and automatic flags
(changes beyond the noise thresholds below). Hand-written highlights come from
the new campaign's notes.json, key `comparison` (a list of {title, detail}).

--series: the same rows for two or more campaigns, oldest first, side by
side (compare<N>.json and compare<N>.md, N = number of campaigns, in --out,
default the last campaign directory): totals (runs, final paths, lost
connections, verdict runs right/misleading/wrong) per campaign, and per
scenario the outcome, verdict runs, misleading/wrong checks, RTT,
throughputs, run duration and lost connections in each campaign.
Highlights come from the last campaign's notes.json, key
`comparison_series`.
"""

import argparse
import json
import os
import sys

sys.dont_write_bytecode = True  # no __pycache__ next to the scripts
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import summarize  # noqa: E402

# A change is flagged when it exceeds both thresholds (relative, absolute).
RTT_TOL = (0.10, 3.0)        # ms
TPUT_TOL = (0.25, 5.0)       # Mbps
BIND_TOL = (0.25, 100.0)     # ms
DUR_TOL = (0.25, 3.0)        # s


def g(d, *keys):
    return summarize.get(d, *keys)


def outcome(s):
    pf = s.get("path_final")
    if not pf:
        return None
    return {"direct": pf["direct"], "relay": pf["relay"], "fail": pf["failed"]}


def misleading_wrong(s):
    return sorted(f"{c['kind']}:{c['code']}" + (f" x{c['runs']}" if c["runs"] > 1 else "")
                  for c in s["verdict_checks"] if c["kind"] != "right")


def row(s):
    return {
        "runs": s["runs"],
        "ok_runs": s["ok_runs"],
        "outcome": outcome(s),
        "rtt_ms_p50": g(s, "rtt_ms", "p50"),
        "upload_mbps": s["upload_mbps"],
        "download_mbps": s["download_mbps"],
        "concurrency_mbps": s["concurrency_mbps"],
        "bind_ms_p50": g(s, "bind_ms", "p50"),
        "run_duration_s_p50": g(s, "run_duration_s", "p50"),
        "run_duration_s_max": max_duration(s),
        "disconnect_runs": s.get("disconnect_runs"),
        "quic_counters_runs": s.get("quic_counters_runs"),
        "loss_pct_connection": s.get("loss_pct"),
        "latency_loss": s.get("latency_loss"),
        "statuses": s["statuses"],
        "exit_codes": s["exit_codes"],
        "issues": s["issues"],
        "hints": s["hints"],
        "verdict_runs": {k: s["verdict_runs"].get(k, 0) for k in ("right", "misleading", "wrong")},
        "not_right_checks": misleading_wrong(s),
        "verdict_sample": s["verdict_sample"],
    }


def max_duration(s):
    v = g(s, "run_duration_s")
    return None if not v else v.get("p95")


def changed(a, b, tol):
    if a is None or b is None:
        return a != b
    d = b - a
    return abs(d) > tol[1] and abs(d) > tol[0] * max(abs(a), 1e-9)


def flags(sid, a, b):
    out = []
    oa, ob = a["outcome"], b["outcome"]
    if oa and ob and oa != ob:
        worse = ob["fail"] > oa["fail"] or ob["direct"] < oa["direct"]
        better = ob["fail"] < oa["fail"] or ob["direct"] > oa["direct"]
        kind = "regression" if worse and not better else "improvement" if better and not worse else "change"
        out.append((kind, f"outcome d/r/f {oa['direct']}/{oa['relay']}/{oa['fail']} -> {ob['direct']}/{ob['relay']}/{ob['fail']}"))
    for key, tol, unit, higher_better in (("rtt_ms_p50", RTT_TOL, "ms", False), ("upload_mbps", TPUT_TOL, "Mbps", True),
                                          ("download_mbps", TPUT_TOL, "Mbps", True),
                                          ("concurrency_mbps", TPUT_TOL, "Mbps", True), ("bind_ms_p50", BIND_TOL, "ms", False),
                                          ("run_duration_s_p50", DUR_TOL, "s", False)):
        x, y = a[key], b[key]
        if x is None and y is None:
            continue
        if x is None or y is None:
            out.append(("change", f"{key} {x} -> {y}"))
        elif changed(x, y, tol):
            better = (y > x) == higher_better
            out.append(("improvement" if better else "regression", f"{key} {x:g} -> {y:g} {unit}"))
    va, vb = a["verdict_runs"], b["verdict_runs"]
    bad_a, bad_b = va["misleading"] + 2 * va["wrong"], vb["misleading"] + 2 * vb["wrong"]
    if bad_b < bad_a:
        out.append(("improvement", f"verdicts r/m/w {va['right']}/{va['misleading']}/{va['wrong']} -> "
                                   f"{vb['right']}/{vb['misleading']}/{vb['wrong']}"))
    elif bad_b > bad_a:
        out.append(("regression", f"verdicts r/m/w {va['right']}/{va['misleading']}/{va['wrong']} -> "
                                  f"{vb['right']}/{vb['misleading']}/{vb['wrong']}"))
    if (a["disconnect_runs"] or 0) != (b["disconnect_runs"] or 0):
        kind = "improvement" if (b["disconnect_runs"] or 0) < (a["disconnect_runs"] or 0) else "regression"
        out.append((kind, f"runs with a lost connection {a['disconnect_runs']} -> {b['disconnect_runs']}"))
    return [{"kind": k, "what": w} for k, w in out]


def totals(scen, summaries):
    t = {"scenarios": len(summaries), "runs": 0, "ok_runs": 0, "direct": 0, "relay": 0, "fail": 0,
         "disconnect_runs": 0, "runs_with_quic_counters": 0, "peer_runs": 0}
    for s in summaries:
        t["runs"] += s["runs"]
        t["ok_runs"] += s["ok_runs"]
        o = outcome(s)
        if o:
            t["peer_runs"] += s["runs"]
            for k in ("direct", "relay", "fail"):
                t[k] += o[k]
        t["disconnect_runs"] += s.get("disconnect_runs") or 0
        t["runs_with_quic_counters"] += s.get("quic_counters_runs") or 0
    t["verdicts"] = summarize.verdict_totals(summaries)
    return t


def fmtv(v, nd=1):
    if v is None:
        return "-"
    if isinstance(v, float):
        return f"{v:.{nd}f}"
    return str(v)


def arrow(a, b, nd=1):
    return f"{fmtv(a, nd)} → {fmtv(b, nd)}"


def oc(r):
    o = (r or {}).get("outcome")
    return "-" if not o else f"{o['direct']}/{o['relay']}/{o['fail']}"


def vr(r):
    if not r:
        return "-"
    v = r.get("verdict_runs") or {}
    return f"{v.get('right', 0)}/{v.get('misleading', 0)}/{v.get('wrong', 0)}"


def series(dirs, out):
    """Two or more campaigns side by side, oldest first: compare<N>.json/.md."""
    camps = []
    for d in dirs:
        summ, notes = summarize.summarize_dir(d)
        camps.append({"dir": os.path.abspath(d), "name": os.path.basename(os.path.abspath(d)), "summaries": summ,
                      "notes": notes, "by": {s["id"]: s for s in summ}})
    out_dir = os.path.abspath(out or dirs[-1])
    ids = []
    for c in camps:
        ids += [s["id"] for s in c["summaries"] if s["id"] not in ids]
    scen = []
    for sid in ids:
        rows = [row(c["by"][sid]) if sid in c["by"] else None for c in camps]
        first = next(c["by"][sid] for c in camps if sid in c["by"])
        scen.append({"id": sid, "group": first["group"], "title": first["title"], "campaigns": rows})
    n = len(camps)
    result = {
        "campaigns": [{"name": c["name"], "dir": os.path.relpath(c["dir"], out_dir), "commit": c["notes"].get("commit"),
                       "binary": c["notes"].get("binary"), "totals": totals(None, c["summaries"])} for c in camps],
        "highlights": camps[-1]["notes"].get("comparison_series", []),
        "scenarios": scen,
    }
    with open(os.path.join(out_dir, f"compare{n}.json"), "w") as f:
        json.dump(result, f, indent=1, ensure_ascii=False)
        f.write("\n")

    names = [c["name"] for c in camps]
    md = [f"# Campaign series: {' → '.join(names)}", "",
          "All campaigns recomputed from their raw runs with `harness/campaign/summarize.py` (`compare.py --series`), "
          "each judged by the rules of its own bench; generated, except the highlights, which come from the last "
          "campaign's `notes.json` (`comparison_series`).", ""]
    for c in camps:
        md.append(f"* {c['name']}: commit {c['notes'].get('commit')}")
    md.append("")
    if result["highlights"]:
        md += ["## Highlights", ""]
        for h in result["highlights"]:
            md.append(f"* **{h['title']}** {h['detail']}")
        md.append("")
    ts = [c["totals"] for c in result["campaigns"]]

    def trow(label, fn):
        return f"| {label} | " + " | ".join(fn(t) for t in ts) + " |"
    md += ["## Totals", "", "| | " + " | ".join(names) + " |", "|---" * (n + 1) + "|",
           trow("scenarios / runs / runs with a report", lambda t: f"{t['scenarios']} / {t['runs']} / {t['ok_runs']}"),
           trow("verdict runs right / misleading / wrong (by worst finding)",
                lambda t: " / ".join(str(t["verdicts"]["runs_by_worst_finding"].get(k, 0))
                                     for k in ("right", "misleading", "wrong"))),
           trow("verdict checks right / misleading / wrong",
                lambda t: " / ".join(str(t["verdicts"]["checks"].get(k, 0)) for k in ("right", "misleading", "wrong"))),
           trow("peer runs: final direct / relay / failed", lambda t: f"{t['direct']} / {t['relay']} / {t['fail']}"),
           trow("runs whose connection closed during the run", lambda t: str(t["disconnect_runs"])),
           trow("peer runs with QUIC counters at the end", lambda t: str(t["runs_with_quic_counters"])),
           ""]
    md += ["## Per scenario", "",
           "Each cell lists the campaigns in order (" + " → ".join(names) + "). Verdicts = runs "
           "right/misleading/wrong by their worst finding; outcome = final path direct/relay/failed per run "
           "(failed includes connections gone at the end); RTT = latency-phase p50 pooled over runs; up/down/conc "
           "= median throughput (conc: concurrency phase, - = no run measured it); run = bench run duration p50; "
           "lost conn = runs with a `disconnect` event.", "",
           "| scenario | verdicts r/m/w | outcome d/r/f | RTT ms | up Mbps | down Mbps | conc Mbps | run s | lost conn |",
           "|---|---|---|---|---|---|---|---|---|"]

    def chain(rows, fn):
        return " → ".join(fn(r) for r in rows)

    def field(key):
        return lambda r: fmtv((r or {}).get(key))
    for s in scen:
        rs = s["campaigns"]
        md.append(f"| {s['id']} | {chain(rs, vr)} | {chain(rs, oc)} | {chain(rs, field('rtt_ms_p50'))} | "
                  f"{chain(rs, field('upload_mbps'))} | {chain(rs, field('download_mbps'))} | "
                  f"{chain(rs, field('concurrency_mbps'))} | {chain(rs, field('run_duration_s_p50'))} | "
                  f"{chain(rs, field('disconnect_runs'))} |")
    md += ["", "## Misleading and wrong verdict checks per campaign", ""]
    for s in scen:
        cs = [(r or {}).get("not_right_checks") or [] for r in s["campaigns"]]
        if any(cs):
            md.append(f"* `{s['id']}`: " + " → ".join(", ".join(c) or "none" for c in cs))
    md.append("")
    with open(os.path.join(out_dir, f"compare{n}.md"), "w") as f:
        f.write("\n".join(md))
    print("\n".join(md))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("dirs", nargs="+", metavar="DIR")
    ap.add_argument("--series", action="store_true",
                    help="two or more campaigns, oldest first: compare<N>.json/compare<N>.md")
    ap.add_argument("--out")
    args = ap.parse_args()
    if args.series:
        if len(args.dirs) < 2:
            ap.error("--series needs at least two campaign directories")
        series(args.dirs, args.out)
        return
    if len(args.dirs) != 2:
        ap.error("give a base and a new campaign directory (or --series)")
    args.base, args.new = args.dirs
    base, notes_a = summarize.summarize_dir(args.base)
    new, notes_b = summarize.summarize_dir(args.new)
    out_dir = os.path.abspath(args.out or args.new)
    by_a = {s["id"]: s for s in base}
    by_b = {s["id"]: s for s in new}
    ids = [s["id"] for s in base] + [s["id"] for s in new if s["id"] not in by_a]
    scen = []
    for sid in ids:
        a, b = by_a.get(sid), by_b.get(sid)
        ra, rb = (row(a) if a else None), (row(b) if b else None)
        scen.append({
            "id": sid,
            "group": (a or b)["group"],
            "title": (a or b)["title"],
            "base": ra,
            "new": rb,
            "flags": flags(sid, ra, rb) if ra and rb else [{"kind": "change", "what": "only in one campaign"}],
        })
    ta, tb = totals(scen, base), totals(scen, new)
    result = {
        "base": {"dir": os.path.relpath(os.path.abspath(args.base), out_dir), "commit": notes_a.get("commit"),
                 "binary": notes_a.get("binary"), "totals": ta},
        "new": {"dir": os.path.relpath(os.path.abspath(args.new), out_dir), "commit": notes_b.get("commit"),
                "binary": notes_b.get("binary"), "totals": tb},
        "thresholds": {"rtt_ms": RTT_TOL, "throughput_mbps": TPUT_TOL, "bind_ms": BIND_TOL, "run_duration_s": DUR_TOL,
                       "meaning": "(relative, absolute): a change is flagged when it exceeds both"},
        "highlights": notes_b.get("comparison", []),
        "scenarios": scen,
    }
    with open(os.path.join(out_dir, "compare.json"), "w") as f:
        json.dump(result, f, indent=1, ensure_ascii=False)
        f.write("\n")

    md = [f"# Campaign comparison: {os.path.basename(os.path.abspath(args.base))} → "
          f"{os.path.basename(os.path.abspath(args.new))}", "",
          f"Base: commit {notes_a.get('commit')}; new: commit {notes_b.get('commit')}. Both recomputed from the raw "
          "runs with `harness/campaign/summarize.py` (`compare.py`); generated, except the highlights, which come "
          "from the new campaign's `notes.json` (`comparison`).", ""]
    if result["highlights"]:
        md += ["## Highlights", ""]
        for h in result["highlights"]:
            md.append(f"* **{h['title']}** {h['detail']}")
        md.append("")
    va, vb = ta["verdicts"], tb["verdicts"]
    md += ["## Totals", "", "| | base | new |", "|---|---|---|",
           f"| scenarios / runs / runs with a report | {ta['scenarios']} / {ta['runs']} / {ta['ok_runs']} | "
           f"{tb['scenarios']} / {tb['runs']} / {tb['ok_runs']} |",
           f"| peer runs: final direct / relay / failed | {ta['direct']} / {ta['relay']} / {ta['fail']} | "
           f"{tb['direct']} / {tb['relay']} / {tb['fail']} |",
           f"| runs whose connection closed during the run | {ta['disconnect_runs']} | {tb['disconnect_runs']} |",
           f"| peer runs with QUIC counters at the end | {ta['runs_with_quic_counters']} | {tb['runs_with_quic_counters']} |"]
    for k in ("right", "misleading", "wrong"):
        md.append(f"| verdict runs {k} (by worst finding) | {va['runs_by_worst_finding'].get(k, 0)} | "
                  f"{vb['runs_by_worst_finding'].get(k, 0)} |")
    for k in ("right", "misleading", "wrong"):
        md.append(f"| verdict checks {k} | {va['checks'].get(k, 0)} | {vb['checks'].get(k, 0)} |")
    md += ["", "## Per scenario (base → new)", "",
           "Outcome = final path direct/relay/failed per run (failed includes connections gone at the end). "
           "RTT = latency-phase p50 pooled over runs; up/down/conc = median throughput (conc: concurrency phase); "
           "bind = `ready.bind_ms` p50; "
           "run = bench run duration p50; lost conn = runs with a `disconnect` event; verdicts = runs "
           "right/misleading/wrong by their worst finding.", "",
           "| scenario | outcome d/r/f | RTT ms | up Mbps | down Mbps | conc Mbps | bind ms | run s | lost conn | "
           "verdicts r/m/w |",
           "|---|---|---|---|---|---|---|---|---|---|"]
    for s in scen:
        a, b = s["base"] or {}, s["new"] or {}
        md.append(f"| {s['id']} | {oc(a)} → {oc(b)} | {arrow(a.get('rtt_ms_p50'), b.get('rtt_ms_p50'))} | "
                  f"{arrow(a.get('upload_mbps'), b.get('upload_mbps'))} | "
                  f"{arrow(a.get('download_mbps'), b.get('download_mbps'))} | "
                  f"{arrow(a.get('concurrency_mbps'), b.get('concurrency_mbps'))} | "
                  f"{arrow(a.get('bind_ms_p50'), b.get('bind_ms_p50'), 0)} | "
                  f"{arrow(a.get('run_duration_s_p50'), b.get('run_duration_s_p50'))} | "
                  f"{fmtv(a.get('disconnect_runs'))} → {fmtv(b.get('disconnect_runs'))} | {vr(a)} → {vr(b)} |")
    md += ["", "## Flags", "",
           "Changes beyond the noise thresholds (relative and absolute: RTT 10 % and 3 ms, throughput (also "
           "concurrency) 25 % and 5 Mbps, bind 25 % and 100 ms, run duration 25 % and 3 s), outcome and verdict changes.", ""]
    any_flag = False
    for s in scen:
        if s["flags"]:
            any_flag = True
            md.append(f"* `{s['id']}`: " + "; ".join(f"{f['kind']}: {f['what']}" for f in s["flags"]))
    if not any_flag:
        md.append("None.")
    md += ["", "## Misleading and wrong verdict checks (base → new)", ""]
    for s in scen:
        a, b = s["base"] or {}, s["new"] or {}
        ca, cb = a.get("not_right_checks") or [], b.get("not_right_checks") or []
        if ca or cb:
            md.append(f"* `{s['id']}`: {', '.join(ca) or 'none'} → {', '.join(cb) or 'none'}")
    md.append("")
    with open(os.path.join(out_dir, "compare.md"), "w") as f:
        f.write("\n".join(md))
    print("\n".join(md))


if __name__ == "__main__":
    main()
