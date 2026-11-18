#!/usr/bin/env python3
"""The soak's coverage check while it runs (docs/how-to/run-the-soak.md): reads the
samples-*.jsonl files tests/soak/sample.py writes in the directory DIR and prints, for the window
so far, the part of the 30-s slots that has a sample of each Pod, the reads that failed and the
age of the last sample.

A sample counts as summarize.py counts it for S1: a line that names a Pod, whatever other read
failed for it. The window is summarize.py's: --hours from the first sample, or from the first
sample at or after --from.

Exits 1 when a Pod has under 95% of the slots so far (S1 asks for 90% of the whole window, so the
check warns while the window can still be saved), when, with the window still running, the last
sample that names a Pod is more than 180 seconds old, or when there is no sample; 0 otherwise.
Run it every few hours: a sampler that lost the cluster keeps running and keeps writing lines
with "errors": ["pods"], so a look at the process says nothing.

It reads the files only: no call to the cluster, no process. Its output names the Pods: keep it
out of the repository. Needs python3 (stdlib only); run it with python3 -I.
"""

import argparse
import calendar
import glob
import json
import math
import os
import sys
import time

SLOT_SECONDS = 30
SOAK_HOURS = 168
# S1 asks for 0.90 of the whole window: under this so far, the window is close to lost.
MARGIN = 0.95
MAX_AGE_SECONDS = 180


def load(directory):
    """The sample lines of DIRECTORY that hold a time, and the number of lines that do not."""
    rows, bad = [], 0
    for path in sorted(glob.glob(os.path.join(directory, "samples-*.jsonl"))):
        with open(path, encoding="utf-8") as handle:
            for line in handle:
                if not line.strip():
                    continue
                try:
                    record = json.loads(line)
                except ValueError:
                    bad += 1
                    continue
                if isinstance(record, dict) and isinstance(record.get("ts"), (int, float)):
                    rows.append(record)
                else:
                    bad += 1
    return rows, bad


def utc_time(value):
    """A UTC time as the report prints it, YYYY-MM-DDTHH:MM:SSZ, as a Unix time."""
    try:
        return calendar.timegm(time.strptime(value, "%Y-%m-%dT%H:%M:%SZ"))
    except ValueError:
        raise argparse.ArgumentTypeError("not a UTC time YYYY-MM-DDTHH:MM:SSZ: %r" % value)


def stamp(ts):
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(ts))


def check(rows, hours, now):
    """(lines, problems) of the window's coverage at the time NOW."""
    start = min(r["ts"] for r in rows)
    end = start + hours * 3600
    running = now < end
    rows = [r for r in rows if r["ts"] < end]
    slots = max(1, int((min(now, end) - start) // SLOT_SECONDS))
    lines = [
        "window: %s to %s (%g hours), %s"
        % (stamp(start), stamp(end), hours, "running" if running else "ended"),
        "slots of %d s so far: %d" % (SLOT_SECONDS, slots),
    ]
    problems = []
    pods, failed = {}, {}
    for r in rows:
        if r.get("pod"):
            pods.setdefault(r["pod"], []).append(r)
        for read in r.get("errors") or []:
            failed[read] = failed.get(read, 0) + 1
    for pod in sorted(pods):
        part = min(1.0, len(pods[pod]) / slots)
        counts = [r["restarts"] for r in pods[pod] if isinstance(r.get("restarts"), int)]
        restarts = "unread"
        if counts:
            restarts = "%d" % counts[0]
            if max(counts) > counts[0]:
                restarts += " at its first sample, %d at most" % max(counts)
        lines.append(
            "%s: coverage %.1f%% (%d samples), restartCount %s"
            % (pod, 100 * part, len(pods[pod]), restarts)
        )
        if part < MARGIN:
            problems.append(
                "%s has %.1f%% of the slots so far, under %d%%" % (pod, 100 * part, 100 * MARGIN)
            )
    lines.append(
        "failed reads: %s"
        % (", ".join("%s %d" % (read, failed[read]) for read in sorted(failed)) or "none")
    )
    if not pods:
        problems.append("no sample names a Pod")
    else:
        named = now - max(r["ts"] for rows_of in pods.values() for r in rows_of)
        written = now - max(r["ts"] for r in rows)
        lines.append("last sample: %d s ago; last line: %d s ago" % (named, written))
        if running and named > MAX_AGE_SECONDS:
            if written > MAX_AGE_SECONDS:
                problems.append(
                    "no line for %d s: the sampler does not run, or its calls hang" % written
                )
            else:
                problems.append(
                    "no sample of a Pod for %d s: the sampler runs and its reads fail" % named
                )
    return lines, problems


def main():
    parser = argparse.ArgumentParser(
        prog="tests/soak/coverage.py",
        description="The soak's coverage so far (docs/how-to/run-the-soak.md).",
    )
    parser.add_argument("dir", help="the directory tests/soak/sample.py writes")
    parser.add_argument(
        "--hours", type=float, default=SOAK_HOURS, help="hours from the first sample (default 168)"
    )
    parser.add_argument(
        "--from",
        dest="since",
        type=utc_time,
        help="start the window at the first sample at or after this UTC time"
        " (YYYY-MM-DDTHH:MM:SSZ), for example after a restart caused outside kausa",
    )
    args = parser.parse_args()
    if not (args.hours > 0 and math.isfinite(args.hours)):
        parser.error("--hours must be above 0")
    rows, bad = load(args.dir)
    if not rows:
        print("coverage.py: no samples-*.jsonl lines in that directory", file=sys.stderr)
        return 1
    rows = [r for r in rows if args.since is None or r["ts"] >= args.since]
    if not rows:
        print("coverage.py: no samples at or after --from", file=sys.stderr)
        return 1
    lines, problems = check(rows, args.hours, time.time())
    for line in lines:
        print(line)
    if bad:
        print("unreadable lines skipped: %d" % bad)
    for problem in problems:
        print("PROBLEM: %s" % problem)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
