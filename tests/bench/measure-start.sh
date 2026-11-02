#!/bin/bash
# The scale measurement (docs/research/2026-10-28-thousands-of-pods.md, and the list pages of
# docs/research/2026-11-02-list-pages.md). Run it next to a kubeconfig file named kubeconfig for
# the throwaway cluster. macOS only: the peak comes from /usr/bin/time -l.
# Usage: measure-start.sh <kausa binary> <label>: runs kausa through a logging kubectl proxy against the
# scale cluster, and reports time to ready, the slowest /healthz answer while starting, owner
# lookups made, resident memory once ready, peak resident memory from start to stop
# (peak_rss_kib), and the list requests made (list_requests: GETs with limit=, one per page).
set -u
BIN=$1; LABEL=$2; D=$(dirname "$0"); export KUBECONFIG=$D/kubeconfig
kubectl proxy --port=18010 -v=6 > "$D/proxy-$LABEL.log" 2>&1 & PROXY=$!
sleep 2
printf 'apiVersion: v1\nkind: Config\nclusters: [{name: p, cluster: {server: "http://127.0.0.1:18010"}}]\nusers: [{name: n, user: {}}]\ncontexts: [{name: p, context: {cluster: p, user: n}}]\ncurrent-context: p\n' > "$D/kc-proxy"
START=$(python3 -c 'import time;print(time.time())')
# time writes the child's rusage to its own file when the child exits; on macOS the maximum
# resident set size is in bytes.
KUBECONFIG=$D/kc-proxy /usr/bin/time -l -o "$D/time-$LABEL.txt" "$BIN" run --metrics-address 127.0.0.1:19095 > "$D/kausa-$LABEL.jsonl" 2>&1 & T=$!
K=""
while [ -z "$K" ]; do sleep 0.05; K=$(pgrep -P $T); done
MAXLAT=0; READY=""
for _ in $(seq 1 1200); do
  t=$(curl -s -o /dev/null -m 5 -w '%{time_total}' 127.0.0.1:19095/healthz 2>/dev/null || echo 0)
  MAXLAT=$(python3 -c "print(max($MAXLAT, float('$t' or 0)))")
  if [ -z "$READY" ] && [ "$(curl -s -m 5 -o /dev/null -w '%{http_code}' 127.0.0.1:19095/readyz)" = 200 ]; then
    READY=$(python3 -c "import time;print(round(time.time()-$START,2))")
    sleep 3; break
  fi
  sleep 0.05
done
RSS=$(ps -o rss= -p "$K")
LOOKUPS=$(grep -c -E 'GET .*/apis/apps/v1/namespaces/[^/]+/replicasets/' "$D/proxy-$LABEL.log")
# The proxy logs each request it forwards, with its query, in one of two formats by kubectl version.
LISTS=$(grep -c -E '(verb="GET" url=|\] GET )[^ ]*limit=' "$D/proxy-$LABEL.log")
# Stop kausa itself, not only the time wrapper, so that time reports its rusage and exits.
kill "$K" 2>/dev/null; wait $T 2>/dev/null
kill $PROXY 2>/dev/null; wait 2>/dev/null
PEAK=$(awk '/maximum resident set size/ {print int($1 / 1024)}' "$D/time-$LABEL.txt")
echo "$LABEL: ready_s=$READY max_healthz_s=$MAXLAT replicaset_lookups=$LOOKUPS rss_kib=$RSS peak_rss_kib=$PEAK list_requests=$LISTS"
