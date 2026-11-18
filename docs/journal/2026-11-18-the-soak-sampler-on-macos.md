# 2026-11-18 — The soak sampler on macOS

## Request

Bring to the repository what was changed outside it while the dry-run soak of v0.5.1 ([Run the
soak](../how-to/run-the-soak.md), "A dry-run soak") lost two windows: the reason of a failed
read in `tests/soak/sample.py`, a launcher for macOS and a coverage check.

## What happened

- **The first window, from 2026-11-03, is invalid:** S1 INVALID, with 24% of the 30-second
  samples against the 90% it asks for. The sampler stopped reading the cluster on 2026-11-05. The
  process kept running and wrote `"errors": ["pods"]` every 30 seconds. `sample.py` dropped
  `kubectl`'s error, so the cause was not recorded. The cluster's nodes also restarted in that
  window.
- **The second window, from 2026-11-09, is invalid too:** S1 INVALID, with 14% of the samples.
  The sampler stopped reading the cluster on 2026-11-10, less than a day after it started, and
  wrote `"errors": ["pods"]` for six days. The cluster was fine that week: no node restarted and
  the two Pods are the same.
- **It was not noticed in time.** The check of 2026-11-10 looked at whether the process ran and
  searched the `cluster-*.jsonl` files for `errors`. Those files have no such key: a failed read
  leaves their counts null. The failures were in `samples-*.jsonl`.

## The cause

Observed on 2026-11-16 on the Mac that runs the sampler:

- A `kubectl` started with no application responsible for it failed at once with
  `connect: no route to host`, five times of five. The same command with the same environment,
  started from an open application, worked. macOS decides the access to the local network by the
  application responsible for a process.
- The sampler of the second window was listed as responsible for itself. Its `kubectl` calls
  ended in under 0.1 seconds without opening a connection.
- A sampler started that day from a terminal application read the cluster for 20 minutes. The
  terminal quit, and from the next sample every call failed with `no route to host`. The process
  was then listed as responsible for itself, and it did not recover.
- A Python started as responsible for itself failed once and worked five seconds later; in
  between, the system's file of local network permissions got a new entry for that Python. From
  then on a process started that way read the cluster, eight calls of eight, while the sampler
  the terminal had left behind failed at the same time.

Not verified:

- Which application quit on 2026-11-10 was not seen. The first failure comes 36 seconds after
  the Mac's display woke.
- The first window probably failed the same way. The system's logs do not reach back to that
  date.
- Whether macOS can withdraw the permission of that Python on its own is not known. The coverage
  check would show it.
- `responsibility_spawnattrs_setdisclaim` is not in the SDK's headers, so a later macOS may drop
  or change it. It was tried on macOS 27 only.

## What was done

- **`sample.py` says why a read fails:** one line on its standard error with `kubectl`'s exit
  code and the last line of its error, at once the first time and then every 600 seconds while
  the same call fails for the same reason. The line does not go to the samples, because it may
  name a node or the server.
- **`tests/soak/launch.py`** starts the sampler on macOS as a process responsible for itself, in
  a session of its own, with six variables of the environment, its log in `<out>/sampler.log` and
  a `caffeinate -i` that lasts as long as the sampler.
- **`tests/soak/coverage.py`** reads the sample files and exits 1 when a Pod has under 95% of the
  slots so far, or when the last sample of a Pod is more than 180 seconds old with the window
  still running. It counts a sample as `summarize.py` does and tells a sampler that stopped from
  one that runs and reads nothing.
- **Tests:** `tests/chart/render.sh` runs both scripts with a stub `kubectl` and synthetic
  samples, and on macOS starts a sampler through the launcher; `tests/upgrade_and_soak.rs` checks
  that the launcher starts `sample.py` alone and names no machine.
- **[Run the soak](../how-to/run-the-soak.md)** has the launcher in step 3 and the coverage check
  as step 4.
- **The third window** started on 2026-11-16 at 16:13 UTC, with local versions of the launcher
  and the check that these two replace. On 2026-11-18 `coverage.py` gives both Pods a sample in
  each of the 5386 slots so far; in 44 of those samples the read of kausa's metrics failed, with
  `MethodNotAllowed` from the API server, which was not looked into. `launch.py` as it is in the
  repository ran once against that cluster, for 100 seconds: three samples of each Pod in three
  slots.

## The failed reads of the metrics

Looked into later the same day. The `405` is kausa's own answer, not the API server's.

- **How often:** in the third window, 72 of 12050 reads of the metrics failed, all with
  `MethodNotAllowed` and none with another status: 64 on the standby, which runs on the node of
  the API server, and 8 on the leader, on another node.
- **Who answers:** 1000 reads through the API server's proxy gave four `405`, each with the body
  `method not allowed`, which is the text of `respond` in `src/metrics.rs`. 1000 reads of the same
  Pod through `kubectl port-forward`, which does not use that proxy, gave none.
- **Why:** `answer` decided on its first read of the connection. k3s's API server reaches a Pod
  through a tunnel that takes an HTTP `CONNECT`, answers `200`, takes the connection over and
  copies its bytes. Go's HTTP server reads one byte in the background while a handler runs, and
  after the takeover that byte is passed on alone. kausa then read `G` as the whole request and
  answered `405`. A small proxy in Go that does the same gave its target a first read of
  one byte in 6 of 20000 requests, and the whole request in the others.
- **Not verified:** no capture was taken on the node, so the lone `G` was not seen arriving at
  kausa. The mode of k3s's tunnel was not read from the server's configuration: its arguments do
  not set it, so the default is assumed.
- **What was done:** `answer` reads the request line to its end, the client's end of sending or
  1024 bytes, within the 2 seconds it already allowed. Five tests in `src/metrics.rs` send a
  request in pieces over a local connection; four of them fail with a single read.
- **The soak is not affected:** `summarize.py` skips a sample without metrics, and the coverage
  counts it. The Pods of the window run v0.5.1 and keep failing about one read in 170 until it
  ends.

## The stalls of the Event watch

Looked into the same day, because `kausa_watch_errors_total{watch="events"}` kept rising in the
third window: about 25 an hour, and up to 70 in some hours.

- **What the log shows:** 60 hours of the leader's log (v0.5.1) have 590 `event watch error`
  lines: 358 with an expired resource version and 232 with `etcd event received with PrevKv=nil`,
  every one about a deletion. They come in a fixed order: 230 times an expired version follows
  the `PrevKv=nil` lines, a median of 19 seconds after the first of them, 232 seconds or more in
  a quarter of the cases and 350 at most. v0.5.1 folds repeats, so the log has about half of
  what the counter counts.
- **Why:** kausa watches only Warnings (`type=Warning`), which are rare, so its resource version
  hardly moves, and no bookmark moves it either: the API server serves Events without a watch
  cache. Every watch ends at its timeout of 290 seconds and is resumed from that old version. The
  API server then replays what happened since, and fails with `PrevKv=nil` on a deletion in
  between, of any Event, before it applies the selector. kube-runtime 4.2.0 keeps its resource
  version after that error, so each retry fails on the same deletion until the version expires
  and the `410` makes the watcher list again.
- **What it cost:** between the first `PrevKv=nil` and the relist the Event watch brings
  nothing: 9% of those 60 hours, and about 44% of each of three hours on 2026-11-18. The relist
  brings the Warnings that still exist, so they arrive late, by up to 6 minutes, and are not
  lost. That an incident was explained late because of it is inferred, not measured.
- **Reproduced:** with `kubectl get --raw` doing what the watcher does (a list, then watches of
  290 seconds, each from the last resource version seen). With the selector, the first watch
  brought nothing, and the resume failed 27 times in 139 seconds with the same error about the
  same deletion, then got a `410`. Without the selector, eight watches in a row brought 48
  additions and 48 deletions and never failed.
- **Where the error comes from, a hypothesis:** this k3s (v1.36.5+k3s1) ships kine v0.16.5,
  which, when it replays for a watch that starts at a revision, drops the previous value of an
  event if that value is older than the compacted revision. An Event deleted by its one-hour TTL
  always is. The API server refuses a deletion without its previous value. This comes from
  reading both sources: it was not confirmed with kine's maintainers, and not tried with etcd.
- **This corrects** the issue "Event watch restarts with PrevKv=nil on k3s" and [Memory and CPU
  on a Raspberry Pi 4](../research/2026-10-26-resource-use-on-a-raspberry-pi.md), which said the
  error costs kausa a log line only.
- **What was done:** `Restarting` starts a new watcher, which lists again, after any error
  inside an open watch, as it did after a `403` ([0013. Offer a namespaced install mode: one
  watch pair and one Role per listed namespace, and nothing
  cluster-wide](../adr/0013-namespaced-install-mode.md), update of 2026-11-18).
  client-go's reflector does the same by default: it lists again after an internal error in a
  watch. So that a watch that fails after every list is not listed again at the shortest delay,
  only an object the watch brings resets the delay, no longer a list that works.
- **Not verified:** the fix has not run on a cluster. Expected from the mechanism: one error and
  one relist per watch timeout with a deletion in it, instead of a stall.

## Next steps

- Measure the Event watch errors and relists on k3s with the release that carries the fix.
- Summarize the third window from 2026-11-23 at 16:13 UTC, with `summarize.py --dry-run`.
