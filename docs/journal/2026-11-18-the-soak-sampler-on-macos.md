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

## Next steps

- Summarize the third window from 2026-11-23 at 16:13 UTC, with `summarize.py --dry-run`.
