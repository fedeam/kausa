#!/usr/bin/env python3
"""The soak's report and verdicts (docs/how-to/run-the-soak.md): reads the samples-*.jsonl and
cluster-*.jsonl files tests/soak/sample.py wrote in DIR, keeps the first --hours hours (168 by
default) from the first sample, and prints, per Pod: coverage, restarts and termination reasons,
working set (p50 and highest per day, highest overall), cAdvisor's highest usage, the growth slope
and CPU (mean and p99 of the rates between samples); over both Pods: per day, the increase of
kausa's counters (incidents, model calls by outcome, skipped and dropped explanations,
notifications, watch errors by watch, any relist series, leadership and lease counters); the
explanation queue's highest value and how often it was above 0; the budget's lowest per window; the
minutes with the breaker open; and the range of the cluster's counts. Then one line per verdict of
the decision rule, set on 2026-11-03 before any measurement:

    S1 VALID|INVALID: <reason>
    S2 PASS|FAIL: <reason>
    S3 PASS|FAIL|INCONCLUSIVE: <reason>

An INVALID soak decides nothing: run it again. Counter increases are taken per Pod name, so a
restart starts a new series instead of a negative increase. --self-test runs the verdicts on
synthetic samples and prints "self-test: ok". It reads files only: no cluster, no network. Python
stdlib only; run it with python3 -I.
"""

import argparse
import glob
import json
import math
import os
import sys
import time

MIB = 2**20
# The decision rule (docs/how-to/run-the-soak.md), set on 2026-11-03, before any measurement.
SOAK_HOURS = 168
SLOT_SECONDS = 30
COVERAGE = 0.90
WS_MAX = 32 * MIB
MAX_USAGE_MAX = 64 * MIB
SLOPE_PASS = 0.1  # MiB per day
SLOPE_FAIL = 0.5  # MiB per day
LAST_DAY_RISE = 1 * MIB
WARMUP_HOURS = 24
BREAKER_DAYS = 4
OK_CALL_DAYS = 6  # of 7
CPU_FLAG = 100  # millicores; reported, not gated
EXTENDED_HOURS = 336

OPEN = 'kausa_endpoint_state{state="open"}'
OK_CALLS = 'kausa_model_calls_total{outcome="ok"}'
QUEUE = "kausa_explanation_queue_length"
BUDGET = "kausa_budget_calls_remaining"
REPORTED_COUNTERS = (
    "kausa_incidents_total",
    "kausa_model_calls_total",
    "kausa_explanations_skipped_total",
    "kausa_explanations_dropped_total",
    "kausa_notifications_total",
    "kausa_watch_errors_total",
    "kausa_leadership_changes_total",
    "kausa_lease_errors_total",
)


def mib(value):
    return "n/a" if value is None else "%.1f MiB" % (value / MIB)


def percentile(values, share):
    if not values:
        return None
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, int(math.ceil(share * len(ordered))) - 1)]


def median(values):
    ordered = sorted(values)
    n = len(ordered)
    if n == 0:
        return None
    return ordered[n // 2] if n % 2 else (ordered[n // 2 - 1] + ordered[n // 2]) / 2


def slope_per_day(points):
    """The least-squares slope of (hour, bytes) points, in MiB per day; None for fewer than two."""
    if len(points) < 2:
        return None
    mean_x = sum(x for x, _ in points) / len(points)
    mean_y = sum(y for _, y in points) / len(points)
    var = sum((x - mean_x) ** 2 for x, _ in points)
    if var == 0:
        return None
    cov = sum((x - mean_x) * (y - mean_y) for x, y in points)
    return cov / var * 24 / MIB


class Window:
    """The samples of the first HOURS hours from the first sample, by Pod name."""

    def __init__(self, rows, cluster, hours):
        self.hours = hours
        timed = [r for r in rows if isinstance(r.get("ts"), (int, float))]
        self.start = min((r["ts"] for r in timed), default=0)
        self.end = self.start + hours * 3600
        self.rows = sorted((r for r in timed if r["ts"] < self.end), key=lambda r: r["ts"])
        self.cluster = [
            c
            for c in cluster
            if isinstance(c.get("ts"), (int, float)) and self.start <= c["ts"] < self.end
        ]
        self.pods = {}
        for r in self.rows:
            if r.get("pod"):
                self.pods.setdefault(r["pod"], []).append(r)
        self.days = max(1, int(math.ceil(hours / 24)))

    def day(self, ts):
        return int((ts - self.start) // 86400)

    def hour(self, ts):
        return int((ts - self.start) // 3600)

    def expected_slots(self):
        return max(1, int(self.hours * 3600 // SLOT_SECONDS))

    def coverage(self, pod):
        return min(1.0, len(self.pods[pod]) / self.expected_slots())

    def increases(self, accept):
        """{day: {series: increase}} summed over the Pods, for the kausa series ACCEPT keeps; a
        counter that falls (a restart) counts from 0 again. The Pod's first metrics read is the
        baseline; a labelled series that appears after it starts from 0."""
        out = {}
        for rows in self.pods.values():
            last, baseline = {}, None
            for r in rows:
                if r.get("kausa") is None:
                    continue
                first = baseline is None
                baseline = baseline or r["ts"]
                for series, value in r["kausa"].items():
                    if value is None or not accept(series):
                        continue
                    before = last.get(series)
                    last[series] = value
                    if before is None:
                        if first:
                            continue
                        before = 0
                    delta = value - before if value >= before else value
                    if delta:
                        day = out.setdefault(self.day(r["ts"]), {})
                        day[series] = day.get(series, 0) + delta
        return out

    def series_values(self, series):
        return [
            (r["ts"], (r.get("kausa") or {}).get(series))
            for r in self.rows
            if r.get("pod") and (r.get("kausa") or {}).get(series) is not None
        ]


def s1(w):
    reasons = []
    if w.hours < SOAK_HOURS:
        reasons.append("the window is %g hours, shorter than 168 hours" % w.hours)
    if len(w.pods) != 2:
        reasons.append("%d Pod names in the window, not the same two throughout" % len(w.pods))
    for pod in sorted(w.pods):
        if w.coverage(pod) < COVERAGE:
            reasons.append(
                "%s has %.0f%% of the 30-s slots, under 90%%" % (pod, 100 * w.coverage(pod))
            )
    restarted = sorted(
        {r["pod"] for rows in w.pods.values() for r in rows if (r.get("restarts") or 0) > 0}
    )
    if restarted:
        reasons.append(
            "restarts above 0 (%s): the 168-hour clock starts again" % ", ".join(restarted)
        )
    open_days = {w.day(ts) for ts, value in w.series_values(OPEN) if value == 1}
    if len(open_days) < BREAKER_DAYS:
        reasons.append(
            "the breaker was seen open on %d days, under %d" % (len(open_days), BREAKER_DAYS)
        )
    needed = int(math.ceil(OK_CALL_DAYS * w.days / 7))
    ok_days = [d for d, values in w.increases(lambda s: s == OK_CALLS).items() if values]
    if len(ok_days) < needed:
        reasons.append(
            "ok model calls increased on %d days, under %d of %d" % (len(ok_days), needed, w.days)
        )
    if not any(value > 0 for _, value in w.series_values(QUEUE)):
        reasons.append("the explanation queue was never above 0")
    if reasons:
        return "INVALID", "; ".join(reasons)
    summary = "%g hours, two Pods, coverage at least 90%%, no restart" % w.hours
    return "VALID", "%s, breaker open on %d days, ok calls on %d days" % (
        summary,
        len(open_days),
        len(ok_days),
    )


def s2(w):
    reasons = []
    oom = sorted(
        {
            r["pod"]
            for rows in w.pods.values()
            for r in rows
            if r.get("last_terminated_reason") == "OOMKilled"
        }
    )
    if oom:
        reasons.append("OOMKilled (%s)" % ", ".join(oom))
    for pod, rows in sorted(w.pods.items()):
        events = [r["oom_events"] for r in rows if r.get("oom_events") is not None]
        if events and max(events) > events[0]:
            reasons.append("oom_events increased (%s)" % pod)
    ws = [r["ws_bytes"] for rows in w.pods.values() for r in rows if r.get("ws_bytes") is not None]
    peak = [
        r["max_usage_bytes"]
        for rows in w.pods.values()
        for r in rows
        if r.get("max_usage_bytes") is not None
    ]
    if not ws:
        reasons.append("no working set was read")
    elif max(ws) > WS_MAX:
        reasons.append("highest working set %s, above 32 MiB" % mib(max(ws)))
    if peak and max(peak) > MAX_USAGE_MAX:
        reasons.append("highest cAdvisor max usage %s, above 64 MiB" % mib(max(peak)))
    if reasons:
        return "FAIL", "; ".join(reasons)
    return (
        "PASS",
        "no OOM; highest working set %s (at most 32 MiB); highest cAdvisor max usage %s%s"
        % (
            mib(max(ws)),
            mib(max(peak)) if peak else "not reported",
            " (at most 64 MiB)" if peak else "",
        ),
    )


def hourly_medians(w, rows):
    by_hour = {}
    for r in rows:
        if r.get("ws_bytes") is not None:
            by_hour.setdefault(w.hour(r["ts"]), []).append(r["ws_bytes"])
    return {h: median(v) for h, v in by_hour.items()}


def growth(w, rows):
    """(slope in MiB/day over hours 24 to the end, rise in bytes of the last 24 hours' highest
    hourly median over that of hours 24-48), or None when there are too few hours."""
    medians = hourly_medians(w, rows)
    end = int(math.ceil(w.hours))
    after = [(h + 0.5, m) for h, m in sorted(medians.items()) if WARMUP_HOURS <= h < end]
    early = [m for h, m in medians.items() if WARMUP_HOURS <= h < WARMUP_HOURS + 24]
    late = [m for h, m in medians.items() if end - 24 <= h < end]
    slope = slope_per_day(after)
    if slope is None or not early or not late or end < WARMUP_HOURS + 48:
        return None
    return slope, max(late) - max(early)


def s3(w):
    extend = (
        "extend to 14 days"
        if w.hours < EXTENDED_HOURS
        else "already 14 days or more, so not a PASS"
    )
    rank = {"PASS": 0, "INCONCLUSIVE": 1, "FAIL": 2}
    verdict, notes = "PASS", []
    for pod, rows in sorted(w.pods.items()):
        found = growth(w, rows)
        if found is None:
            verdict = max(verdict, "INCONCLUSIVE", key=rank.get)
            notes.append("%s: too few hours after the 24-hour warm-up to fit a slope" % pod)
            continue
        slope, rise = found
        note = "%s: %+.3f MiB/day, last day %+.2f MiB over hours 24-48" % (pod, slope, rise / MIB)
        if slope > SLOPE_FAIL:
            pod_verdict = "FAIL"
        elif slope > SLOPE_PASS or rise > LAST_DAY_RISE:
            pod_verdict = "INCONCLUSIVE"
        else:
            pod_verdict = "PASS"
        verdict = max(verdict, pod_verdict, key=rank.get)
        notes.append(note)
    if not w.pods:
        verdict, notes = "INCONCLUSIVE", ["no samples"]
    reason = "; ".join(notes)
    if verdict == "FAIL":
        reason += "; above 0.5 MiB/day: memory grows over days"
    elif verdict == "INCONCLUSIVE":
        reason += "; not at most 0.1 MiB/day with at most 1 MiB in the last day: " + extend
    else:
        reason += "; at most 0.1 MiB/day and at most 1 MiB in the last day"
    return verdict, reason


def verdicts(w):
    return [("S1",) + s1(w), ("S2",) + s2(w), ("S3",) + s3(w)]


def report(w):
    """The report's lines, before the verdicts."""
    utc = lambda ts: time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(ts))  # noqa: E731
    lines = [
        "window: %s to %s (%g hours), %d sample lines"
        % (utc(w.start), utc(w.end), w.hours, len(w.rows))
    ]
    for pod, rows in sorted(w.pods.items()):
        lines.append("pod %s:" % pod)
        restarts = max((r.get("restarts") or 0) for r in rows)
        reasons = sorted(
            {r["last_terminated_reason"] for r in rows if r.get("last_terminated_reason")}
        )
        lines.append(
            "  coverage %.1f%%, restarts %d, termination reasons: %s"
            % (100 * w.coverage(pod), restarts, ", ".join(reasons) or "none")
        )
        by_day = {}
        for r in rows:
            if r.get("ws_bytes") is not None:
                by_day.setdefault(w.day(r["ts"]), []).append(r["ws_bytes"])
        for day, values in sorted(by_day.items()):
            lines.append(
                "  day %d: working set p50 %s, highest %s"
                % (day + 1, mib(median(values)), mib(max(values)))
            )
        ws = [v for values in by_day.values() for v in values]
        peak = [r["max_usage_bytes"] for r in rows if r.get("max_usage_bytes") is not None]
        lines.append(
            "  working set highest %s; cAdvisor max usage highest %s"
            % (mib(max(ws)) if ws else "n/a", mib(max(peak)) if peak else "not reported")
        )
        found = growth(w, rows)
        lines.append(
            "  growth: %s"
            % (
                "too few hours after the warm-up"
                if found is None
                else "%+.3f MiB/day, last day %+.2f MiB" % (found[0], found[1] / MIB)
            )
        )
        rates, previous = [], None
        for r in rows:
            if r.get("cpu_ns") is None:
                continue
            if (
                previous is not None
                and r["ts"] > previous["ts"]
                and r["cpu_ns"] >= previous["cpu_ns"]
            ):
                rates.append((r["cpu_ns"] - previous["cpu_ns"]) / (r["ts"] - previous["ts"]) / 1e6)
            previous = r
        if rates:
            p99 = percentile(rates, 0.99)
            lines.append(
                "  CPU: mean %.1fm, p99 %.1fm%s"
                % (sum(rates) / len(rates), p99, "  FLAG: p99 above 100m" if p99 > CPU_FLAG else "")
            )
        else:
            lines.append("  CPU: n/a")
    accept = lambda s: s.split("{", 1)[0] in REPORTED_COUNTERS or "relist" in s  # noqa: E731
    increases = w.increases(accept)
    for day in range(w.days):
        values = increases.get(day, {})
        text = ", ".join("%s +%g" % (s, v) for s, v in sorted(values.items())) or "no increase"
        lines.append("day %d counters (both Pods): %s" % (day + 1, text))
    queue = [v for _, v in w.series_values(QUEUE)]
    if queue:
        lines.append(
            "queue: highest %g, above 0 in %.1f%% of samples"
            % (max(queue), 100 * sum(1 for v in queue if v > 0) / len(queue))
        )
    else:
        lines.append("queue: not reported")
    budgets = {}
    for r in w.rows:
        for series, value in (r.get("kausa") or {}).items():
            if series.startswith(BUDGET) and value is not None:
                budgets[series] = min(value, budgets.get(series, value))
    for series, value in sorted(budgets.items()):
        lines.append("budget: %s lowest %g" % (series, value))
    if not budgets:
        lines.append("budget: not reported")
    open_minutes = {int(ts // 60) for ts, value in w.series_values(OPEN) if value == 1}
    lines.append("breaker: open in %d sampled minutes" % len(open_minutes))
    for key in ("pods", "warning_events", "nodes"):
        values = [c[key] for c in w.cluster if c.get(key) is not None]
        lines.append(
            "cluster %s: %s"
            % (key, "%d to %d" % (min(values), max(values)) if values else "not reported")
        )
    return lines


def load(directory):
    rows, cluster, bad = [], [], 0
    for kind, into in (("samples", rows), ("cluster", cluster)):
        for path in sorted(glob.glob(os.path.join(directory, "%s-*.jsonl" % kind))):
            with open(path, encoding="utf-8") as handle:
                for line in handle:
                    if not line.strip():
                        continue
                    try:
                        record = json.loads(line)
                    except ValueError:
                        bad += 1
                        continue
                    if isinstance(record, dict):
                        into.append(record)
    return rows, cluster, bad


# --- The self-test: the verdicts on synthetic samples of two Pods at 30-s intervals. ---


def synthetic(case, hours=SOAK_HOURS):
    start = 1767225600  # 2026-01-01T00:00:00Z
    rate = {"grow-1": 1.0, "grow-0.3": 0.3}.get(case, 0.0)
    rows, cluster = [], []
    for i in range(int(hours * 3600 // SLOT_SECONDS)):
        ts = start + SLOT_SECONDS * i
        h = (ts - start) / 3600
        if case == "gap-40" and 40 <= h < 40 + 0.4 * hours:
            continue
        day, hour = int(h // 24), h % 24
        open_now = day in (1, 2, 3, 4, 5) and 12 <= hour < 12 + 10 / 60
        if i % 20 == 0:
            cluster.append({"ts": ts, "pods": 100, "warning_events": 40, "nodes": 3})
        for n, pod in enumerate(("synthetic-1", "synthetic-2")):
            ws = 20 * MIB + n * MIB + rate * MIB * h / 24
            restarts, reason = 0, None
            if h >= 100 and n == 1 and case in ("restart", "oom"):
                restarts, reason = 1, ("OOMKilled" if case == "oom" else "Error")
            kausa = {"kausa_leader": 1 - n}
            if n == 0:
                kausa.update(
                    {
                        OK_CALLS: int(h // 6),
                        QUEUE: 1 if open_now else 0,
                        BUDGET + '{window="hour"}': 30,
                        OPEN: 1 if open_now else 0,
                    }
                )
            cadvisor = i % 20 == 0
            rows.append(
                {
                    "ts": ts,
                    "pod": pod,
                    "node": n,
                    "holder": n == 0,
                    "ready": True,
                    "restarts": restarts,
                    "last_terminated_reason": reason,
                    "ws_bytes": int(ws),
                    "cpu_ns": int(5e6 * SLOT_SECONDS * i),
                    "max_usage_bytes": int(ws) + 4 * MIB if cadvisor else None,
                    "oom_events": 0 if cadvisor else None,
                    "kausa": kausa,
                    "errors": ["top"],
                }
            )
    return Window(rows, cluster, hours)


def self_test():
    cases = [
        ("flat", SOAK_HOURS, {"S1": "VALID", "S2": "PASS", "S3": "PASS"}),
        ("grow-1", SOAK_HOURS, {"S1": "VALID", "S2": "PASS", "S3": "FAIL"}),
        ("grow-0.3", SOAK_HOURS, {"S3": "INCONCLUSIVE"}),
        ("gap-40", SOAK_HOURS, {"S1": "INVALID"}),
        ("restart", SOAK_HOURS, {"S1": "INVALID"}),
        ("oom", SOAK_HOURS, {"S2": "FAIL"}),
        ("short", 10, {"S1": "INVALID"}),
    ]
    failures = []
    for case, hours, want in cases:
        w = synthetic(case, hours)
        report(w)
        got = {name: (verdict, reason) for name, verdict, reason in verdicts(w)}
        for name, verdict in want.items():
            if got[name][0] != verdict:
                failures.append(
                    "%s: %s is %s, not %s: %s" % (case, name, got[name][0], verdict, got[name][1])
                )
        if case == "grow-0.3" and not got["S3"][1].endswith("extend to 14 days"):
            failures.append("grow-0.3: the S3 reason does not end with 'extend to 14 days'")
        if case == "short" and "shorter than 168 hours" not in got["S1"][1]:
            failures.append("short: the S1 reason does not say 'shorter than 168 hours'")
    for failure in failures:
        print("self-test: " + failure, file=sys.stderr)
    if failures:
        return 1
    print("self-test: ok")
    return 0


def main():
    parser = argparse.ArgumentParser(
        prog="tests/soak/summarize.py",
        description="The soak's report and verdicts (docs/how-to/run-the-soak.md).",
    )
    parser.add_argument("dir", nargs="?", help="the directory tests/soak/sample.py wrote")
    parser.add_argument(
        "--hours", type=float, default=SOAK_HOURS, help="hours from the first sample (default 168)"
    )
    parser.add_argument(
        "--self-test", action="store_true", help="run the verdicts on synthetic samples"
    )
    args = parser.parse_args()
    if args.self_test:
        return self_test()
    if not args.dir:
        parser.error("DIR is required without --self-test")
    if not (args.hours > 0 and math.isfinite(args.hours)):
        parser.error("--hours must be above 0")
    rows, cluster, bad = load(args.dir)
    if not rows:
        print("summarize.py: no samples-*.jsonl lines in that directory", file=sys.stderr)
        return 1
    w = Window(rows, cluster, args.hours)
    for line in report(w):
        print(line)
    if bad:
        print("unreadable lines skipped: %d" % bad)
    for name, verdict, reason in verdicts(w):
        print("%s %s: %s" % (name, verdict, reason))
    return 0


if __name__ == "__main__":
    sys.exit(main())
