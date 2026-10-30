# Send notifications to Slack, Telegram or a webhook

How to have kausa post each new explanation to one webhook (a receiver of your own, a Slack
incoming webhook or a Telegram bot), check that it arrives, and turn it off.

Status as of 2026-10-29: released in v0.3.0, off by default. The
`json` format is checked by the kind test against a fake receiver, and the `telegram` format was
posted to a real Telegram chat on 2026-10-29 (the joiners kept mentions and bot commands inert;
URLs stay links). The `slack` format was posted to a real incoming webhook on 2026-10-30: a URL
stayed plain text, but **a plain `@here` or `@channel` in v0.4.0's Slack text notifies the
channel**; the next release puts a word joiner after every `@`, which is meant to stop it
([Data sent in notifications](../reference/data-sent-in-notifications.md#what-slack-and-telegram-do-with-it)).
Why it works this way: [0015. Post each new explanation to one optional webhook, as JSON, Slack or
Telegram](../adr/0015-notifications.md).

## Before you start

- kausa installed with the chart in its namespace (`kausa` below) and explaining incidents with a
  model: `model.endpoint` set and `dryRun` false ([Install kausa with
  Helm](install-with-helm.md)). Notifications need both, and the chart refuses them otherwise.
- Read [Data sent in notifications](../reference/data-sent-in-notifications.md) first: a
  notification leaves the cluster with the **real** names of the namespace and the failing
  workload, and the model's text, which can repeat redacted log lines. The receiver keeps it under
  its own retention.
- The webhook URL is a secret in every format (Telegram puts the bot token in it, and a Slack
  webhook URL lets anyone post to the channel). It lives in a Secret of yours, which the kubelet
  mounts as a file: never put it in a values file, a `--set`, an environment variable or a shell
  command line. kausa needs no permission on Secrets for it.

## Steps

1. **Get the URL.**
   - **`json`**: your receiver's URL. kausa `POST`s the body of [The `json`
     body](../reference/data-sent-in-notifications.md#the-json-body), optionally with
     `Authorization: Bearer <token>`, and reads only the status code.
   - **`slack`**: create an incoming webhook in your Slack app; Slack gives a URL that starts with
     `https://hooks.slack.com/services/`.
   - **`telegram`**: create a bot with BotFather and add it to the group or channel. The URL is
     `https://api.telegram.org/bot<token>/sendMessage`, with the bot's token, or the same path on
     a self-hosted Bot API server. kausa refuses any other path. You also need the chat id: an
     integer (a group's is negative, for example `-1001234567890`) or `@` and a public channel's
     username.

2. **Create the Secret** in kausa's namespace, from files you write with an editor, so that the
   URL and the token never appear in your shell history. Each file holds only the value (a trailing
   newline is trimmed):

   ```sh
   # webhook-url holds, for example, https://hooks.example.com/kausa
   kubectl -n kausa create secret generic kausa-notify --from-file=url=./webhook-url
   # json only, with a bearer token (webhook-token holds it):
   kubectl -n kausa create secret generic kausa-notify --from-file=url=./webhook-url \
     --from-file=token=./webhook-token
   ```

   Delete the two files afterwards.

3. **Turn notifications on**, keeping your other values. Each format in a values file of its own
   or in the one you install with:

   ```yaml
   # json (the default format), with the optional bearer token
   notifications:
     enabled: true
     format: json
     existingSecret: kausa-notify
     tokenKey: token
   ```

   ```yaml
   # slack
   notifications:
     enabled: true
     format: slack
     existingSecret: kausa-notify
   ```

   ```yaml
   # telegram
   notifications:
     enabled: true
     format: telegram
     existingSecret: kausa-notify
     telegramChatId: "-1001234567890"   # or "@your_channel"
   ```

   ```sh
   helm upgrade kausa <chart> -n kausa --reuse-values -f notifications.yaml
   ```

   `urlKey` (default `url`) names the key of the URL if your Secret uses another. `allowHttp:
   true` allows a plain `http://` URL, only for a receiver inside the cluster or on the same
   private network. A release installed with an earlier chart has no `notifications` values;
   upgraded with `--reuse-values`, it renders with notifications off until you set them. From the
   v0.2.0 or v0.1.0 chart, the first upgrade also needs `--set rbac.scope=cluster` ([Upgrade from a
   chart without rbac.scope](install-with-helm.md#upgrade-from-a-chart-without-rbacscope)). Quote the
   chat id: an unquoted number also works (the chart prints it as a whole number), but a quoted
   string says what it is.

4. **With `networkPolicy.enabled`, allow the webhook.** The chart refuses notifications without
   `networkPolicy.notifications.to`, and adds an egress rule to those peers on TCP 443 (or the
   `networkPolicy.notifications.ports` you give):

   ```yaml
   networkPolicy:
     notifications:
       to:
         - ipBlock:
             cidr: 192.0.2.0/24   # your receiver's addresses
   ```

   A NetworkPolicy cannot select a hostname. A receiver in the cluster takes a `podSelector` and
   `namespaceSelector`; a hosted one (Slack, Telegram) needs an `ipBlock` that covers its
   addresses, a CNI with FQDN policies, or an egress proxy. kausa ignores `HTTP_PROXY` and
   `HTTPS_PROXY`, so a cluster that reaches the internet only through an explicit proxy cannot
   use notifications yet. This page gives no address ranges for Slack or Telegram: whether they
   publish stable ones was not checked.

## Check that it arrives

1. The `started` line names the format and the host, never the path or the token:

   ```sh
   kubectl -n kausa logs deploy/kausa | grep '"msg":"started"'
   # ... "notifications":{"format":"json","host":"hooks.example.com"} ...
   ```

   If the Pod stays in `ContainerCreating`, `kubectl -n kausa get events` shows a `FailedMount`
   for the volume `notify`: the Secret, or its `url` or `token` key, does not exist. The same Pod
   starts once the Secret is fixed, with no restart (seen in kind on 2026-10-29).

2. Make a test workload fail in a watched namespace (`shop` here), and wait for the settle window
   (60 seconds by default) and the explanation. It uses one model call of the budget:

   ```sh
   kubectl -n shop create deployment kausa-notify-test --image=busybox:1.37 -- sh -c 'exit 1'
   ```

3. Look for the log line and the counter:

   ```sh
   kubectl -n kausa logs deploy/kausa | grep '"msg":"notification'
   kubectl get --raw /api/v1/namespaces/kausa/services/kausa-metrics:9090/proxy/metrics |
     grep kausa_notifications
   ```

   `notification sent` with `"status":200` (and `kausa_notifications_total{outcome="sent"}`)
   means the webhook answered `2xx`. `notification failed` gives the reason: `the webhook answered
   404` (a wrong URL or token), `a redirect is not followed: put the final URL in the file`, or
   `the request failed: ...` after its retry.

4. Delete the test workload:

   ```sh
   kubectl -n shop delete deployment kausa-notify-test
   ```

A repeat of the same incident within `budget.dedupHours` is not posted again once the webhook has
acknowledged it.

## Alert on failures

A failed notification is not tried again until its incident comes back, never stops kausa and
never shows in an Event. Alert on the counter, for example in a Prometheus rule:

```text
increase(kausa_notifications_total{outcome="failed"}[1h]) > 0
```

`kausa_notifications_total{outcome="dropped"}` counts notifications pushed out of the queue of 16
by a burst; `kausa_notifications_retried_total` counts second attempts.

## Turn it off

```sh
helm upgrade kausa <chart> -n kausa --reuse-values --set notifications.enabled=false
kubectl -n kausa delete secret kausa-notify
```

kausa then starts without `--notify-url-file` and posts nothing. If the URL may have leaked,
rotate it at the provider (a new Slack webhook, or a new bot token from BotFather): anyone who has
it can post as kausa.

## Another way: forward the Events

kausa's `Explained` Events carry the summary, the likely cause and the steps, cut to 1,024 bytes,
for one hour by default. An event exporter you already run (Robusta, kwatch or another) can forward
them instead, with its own formats and destinations, and kausa then sends nothing but its model
requests. It reads every Event, and must read them before they expire.
