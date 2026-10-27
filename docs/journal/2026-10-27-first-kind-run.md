# 2026-10-27 — First run of the kind test

## Request

Run the kind test and the GitHub image build now instead of waiting for GitHub Actions (the
account's included minutes for private repositories are used up until 2026-11-01), then clean up.

## What was done

- kind v0.33.0, the version CI pins, installed on the maintainer's machine with Homebrew. The
  test ran with `KUBECONFIG` pointing at a file of its own: kind makes its context the current one,
  and the maintainer's kubeconfig was left unchanged (checked by its checksum before and after).
- **First run: failed, and showed a bug in the script.** The fake endpoint could not start
  (`Address already in use`: something on the machine held its fixed port, 18080), the script
  went on, and another program answered kausa with `404 Not Found`. Everything before the model
  call worked: the cluster, the chart under the restricted Pod Security Standard, the `D001` and
  `D008` incidents, and the `ExplanationSkipped` Events written about the Deployment and the Job.
- **The fix:** the script takes a free port and waits until the endpoint answers before it
  installs anything, and stops if the endpoint died.
- **Second run: `e2e: ok`**, on Docker Desktop (arm64): an `Explained` Event about the Deployment
  and one about the Job, with the placeholder `namespace-1` mapped back to the real namespace; no
  real name in the requests the endpoint received; the ClusterRole as in the reference (the
  allowed list and the refused list of `kubectl auth can-i`); `kausa_events_written_total`
  counting them; no restart of kausa. The Docker Desktop path, a hypothesis until now, works.
- **The amd64 image**, built under emulation with `docker build --platform linux/amd64 --target
  runtime`: it runs (`kausa 0.1.0-dev`), as user 65532, 4.6 MB compressed. The layers came from
  the cache of an earlier build of the same instructions.

## Pending

- The same two checks on GitHub's runners (the Linux path of the kind test, and the arm64 runner),
  once GitHub Actions start jobs again.
- Publication, when the maintainer validates the project.
