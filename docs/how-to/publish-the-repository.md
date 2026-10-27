# Publish the repository on GitHub

The checklist to make the GitHub repository `fedeam/kausa` public once the project is
validated: every step is a gate, in this order, and a failed step stops the publication. Why
publication works this way: [0006. Make the GitHub repository public only after a
checklist](../adr/0006-publication-checklist.md) and [0007. Take contributions and merge pull
requests on GitHub](../adr/0007-contributions-and-merges-on-github.md).

Status as of 2026-10-26: the project is not yet validated, and nothing in this checklist has
been run. The copy on the maintainer's Gitea instance, which runs the arm64 CI, stays private;
"public" here means the GitHub repository. Already done when the private GitHub repository was
created on 2026-10-26: the merge settings (squash only, title and message from the pull request),
the labels, Dependabot alerts, the description and the topics ([2026-10-26 — GitHub repository
and first CI runs](../journal/2026-10-26-github-repository-and-first-ci.md)).

## Before you start

- The maintainer, FedeAM, has decided that kausa is validated and ready to be public.
- **GitHub Actions can run for the repository.** Status as of 2026-10-23 they do not: the
  account's included minutes for private repositories were used up for October by another
  repository's CI (they renew on the 1st of each month), and public repositories on GitHub-hosted
  runners do not use them. No payment is needed. [0007. Take contributions and merge pull
  requests on GitHub](../adr/0007-contributions-and-merges-on-github.md) makes GitHub's CI the
  pull request gate, so it has to run before step 8 adds it as a required check: if the
  repository is opened before the minutes renew, push to `main` or re-run `CI` right after step 6
  and confirm that the `ci` job starts and passes (that public repositories do not use the minutes
  is GitHub's documented rule; that the account's limit does not block them is a hypothesis
  checked then). As soon as they run, update in one pull
  request the text that says they do not, and drop the local build that stands in for them:
  `CONTRIBUTING.md` (CI), `AGENTS.md` (Remotes, merges and CI), `SECURITY.md` (Secrets),
  `docs/index.md` (the status and the CI line of the key facts), [Maintain the
  repository](maintain.md) (the status, step 4 of "Review and merge a pull request" and step 4 of
  "Bump the pinned tools"), the status of [Set up a development environment and run the
  checks](develop.md), and the header comments of `.github/workflows/ci.yml` and
  `.github/workflows/e2e.yml`. ADRs and journal
  entries keep what was true when they were written. Then this prints nothing:

  ```sh
  git grep -n -E 'does not start|do not start|not start jobs|[Oo]nce (GitHub )?Actions run|Until GitHub Actions run|Actions do not run|waits? (until|for) (GitHub )?Actions|until they start jobs' \
    -- ':!docs/adr' ':!docs/journal' ':!docs/how-to/publish-the-repository.md'
  ```

  If you publish before the minutes renew, skip this grep for now: steps 4 and 5 leave these
  statements in place, and the pull request that removes them is merged right after step 6, once
  the first `ci` run on GitHub passes, and before step 8.

- CI is green on `main` on Gitea Actions (linux/arm64), and on GitHub Actions (linux/amd64) if
  they run before publication; otherwise right after step 6, as said above.
- `gh auth status` shows the `fedeam` account, with admin rights on `fedeam/kausa`.
- The local clone has every branch and tag of both forges, `main` is synced ([Maintain the
  repository](maintain.md#sync-main-to-gitea)), and no work is left only on other machines:

  ```sh
  git fetch origin --tags --prune
  git fetch github --tags --prune
  ```

- The [Contributor Covenant Code of Conduct](../../CODE_OF_CONDUCT.md) has a real contact address:
  `git grep -n 'TODO(maintainer)' -- CODE_OF_CONDUCT.md` prints nothing. Status as of
  2026-10-23: done, `conduct@fedeam.com`, a forwarding address on the maintainer's domain, so their
  personal address is not published. Before publishing, check that it still forwards: a message
  to it from another account arrives.
- A **private patterns file** outside the repository (for example
  `~/kausa-private-patterns.txt`) lists, one extended regular expression per line, what must
  never be published: the internal domain and the Gitea hostname, the maintainer's usernames on
  their own systems, their personal email address, and the names of their clusters, namespaces and
  services. It is never committed: it is itself the list of secrets
  to look for.

## Steps

1. **Clone what GitHub would publish.** Publishing exposes every ref of the GitHub repository:
   branches and tags pushed only there, and the pull request refs (`refs/pull/*`), not only what
   the local clone fetched from Gitea. Steps 2 and 3 scan a mirror clone of the GitHub
   repository, made with `gh` while the repository is still private:

   ```sh
   MIRROR="$(mktemp -d)/kausa.git"
   gh repo clone fedeam/kausa "$MIRROR" -- --mirror
   ```

2. **Scan the whole history for secrets**, every ref, not only the current tree. Publishing
   exposes every old commit.

   ```sh
   gitleaks git --redact --log-opts="--all" "$MIRROR"
   ```

   Expected: `no leaks found`. Any finding stops the publication: a real secret is rotated at
   its source and removed from the history before going on.

3. **Look for internal names and personal data in the whole history.** Review every match by
   hand; false positives are expected.

   ```sh
   git -C "$MIRROR" log --all -p | grep -n -i -E -f ~/kausa-private-patterns.txt
   git -C "$MIRROR" log --all --format='%B' | grep -n -i -E -f ~/kausa-private-patterns.txt
   git -C "$MIRROR" log --all -p | grep -n -i -E '\.(local|lan|internal|home\.arpa)\b'
   git -C "$MIRROR" log --all -p | grep -n -E '(^|[^0-9.])(10\.[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}|192\.168\.[0-9]{1,3}\.[0-9]{1,3}|172\.(1[6-9]|2[0-9]|3[01])\.[0-9]{1,3}\.[0-9]{1,3})'
   ```

   Commit metadata is public too. Every author and committer identity, and every tagger, must be
   one the maintainer chose to publish:

   ```sh
   git -C "$MIRROR" log --all --format='%an <%ae>%n%cn <%ce>' | sort -u
   git -C "$MIRROR" for-each-ref refs/tags --format='%(taggername) %(taggeremail)' | sort -u
   ```

   The patterns file holds the maintainer's legal name too, so the first command also finds it in
   file contents, which the identity listing does not show. On 2026-10-27 an audit found it in the
   content of old commits after a rewrite of the metadata only; the history was then rewritten
   with `git filter-repo --replace-text` and the GitHub repository recreated
   ([2026-10-27 — First run of the kind test](../journal/2026-10-27-first-kind-run.md)).

   The text of pull requests, issues and comments becomes public as well; scan it the same way:

   ```sh
   { gh api --paginate 'repos/fedeam/kausa/pulls?state=all&per_page=100' --jq '.[] | .title, .body'
     gh api --paginate 'repos/fedeam/kausa/issues?state=all&per_page=100' --jq '.[] | .title, .body'
     gh api --paginate 'repos/fedeam/kausa/issues/comments?per_page=100' --jq '.[].body'
     gh api --paginate 'repos/fedeam/kausa/pulls/comments?per_page=100' --jq '.[].body'
   } | grep -n -i -E -f ~/kausa-private-patterns.txt
   ```

   Then delete the mirror clone, whatever the result: `rm -rf "$MIRROR"`.

4. **Read the current tree as a stranger would.** `README.md`, `AGENTS.md`, `SECURITY.md`,
   `CONTRIBUTING.md`, `docs/`, and the comments in `.github/`, `.gitea/` and the `Dockerfile`:
   nothing that only makes sense in private, no internal URL, no pending `TODO` that promises
   something to users, and no statement that GitHub Actions do not run (the grep in [Before you
   start](#before-you-start) prints nothing, unless you publish before the minutes renew; then it
   prints only those statements).

   Then two checks that are particular to kausa:

   - **Fixtures and test corpora** hold no real cluster output: every log line, event message,
     name and address in `tests/` and in the docs is invented, with `example.com`,
     `192.0.2.0/24`, `2001:db8::/32` and visibly fake tokens.
   - **Repeat the competitive survey**: check the tools in [Competitive
     landscape](../research/2026-10-23-competitive-landscape.md) again (activity, redaction,
     permissions, Events), write the result as a new dated page in `docs/research/`, and update
     the comparison in `README.md`. If a tool now covers the combination kausa offers, say so in
     the README before publishing.

5. **Commit the badge and status change before the visibility change**, as the maintainer's
   standard requires (history reviewed, then the badge commit, then the visibility):
   - In `README.md`, replace
     `![repository: private](https://img.shields.io/badge/repository-private-0969da)` with
     `![repository: public](https://img.shields.io/badge/repository-public-bc4c00)`.
   - In `docs/index.md`, update "Status as of" and the repository line in the key facts; in
     [Maintain the repository](maintain.md), the status line.
   - In `SECURITY.md`, update both statements about privacy. In "Reporting a vulnerability",
     remove the sentence saying that reporting will be enabled when the repository goes public
     (step 7 enables it). In "Exposure", replace the sentence about the repository with "The
     GitHub repository is public; the copy that runs the arm64 CI stays private."
   - In `AGENTS.md`, under README badges, drop the sentence that limits the project badges to
     static ones while the repository is private.
   - If GitHub Actions have not run yet, reword the privacy part of the Actions statements in
     `AGENTS.md` (Remotes, merges and CI), in the header comments of `.github/workflows/ci.yml`
     and `.github/workflows/e2e.yml`, in [Set up a development environment and run the
     checks](develop.md) and in the status lines above to "GitHub Actions did not start jobs before the repository was
     public"; the pull request from [Before you start](#before-you-start) removes them after
     step 6.
   - Then this prints nothing:

     ```sh
     git grep -n -E 'repository is private|private until|repository-private|is private|private repositor|while private' \
       -- ':!docs/adr' ':!docs/journal' ':!docs/research' ':!docs/how-to/publish-the-repository.md'
     ```

   - Merge it like any change, with the maintainer's approval: a pull request (for example
     `docs(readme): mark the repository as public`), squash-merged on GitHub, then the sync to
     Gitea ([Maintain the repository](maintain.md)).

6. **Change the visibility:**

   ```sh
   gh repo edit fedeam/kausa --visibility public --accept-visibility-change-consequences
   ```

   If GitHub Actions had not run before, push to `main` or re-run `CI`, confirm that the `ci` job
   passes, then merge the pull request from [Before you start](#before-you-start) and run its
   grep, which must now print nothing, before step 8.

   Do steps 6, 7 and 8 in one sitting: between them the public repository has no ruleset, no
   Actions policy and no private vulnerability reporting.

7. **Turn on the security features and restrict GitHub Actions.** Private vulnerability reporting
   is the channel that
   `SECURITY.md` and the issue template point to; secret scanning and its push protection are
   available on public repositories.

   ```sh
   gh api -X PUT repos/fedeam/kausa/private-vulnerability-reporting
   gh repo edit fedeam/kausa --enable-secret-scanning
   gh repo edit fedeam/kausa --enable-secret-scanning-push-protection
   gh api -X PUT repos/fedeam/kausa/automated-security-fixes
   ```

   The last command turns on Dependabot **security updates**: pull requests that fix a known
   vulnerability without waiting for the weekly version updates of `.github/dependabot.yml`.
   Dependabot **alerts** have been on since 2026-10-26; the [Verify](#verify) section checks
   them.

   Then restrict GitHub Actions. Only GitHub's own actions (`actions/*` and `github/*`) and the
   Scorecard action of step 11 may run, each pinned by a full commit SHA, as the workflows already
   pin them; and the workflow runs of every outside contributor's pull request wait for the
   maintainer's approval:

   ```sh
   gh api -X PUT repos/fedeam/kausa/actions/permissions \
     -F enabled=true -f allowed_actions=selected -F sha_pinning_required=true
   gh api -X PUT repos/fedeam/kausa/actions/permissions/selected-actions --input - <<'EOF'
   { "github_owned_allowed": true, "verified_allowed": false,
     "patterns_allowed": ["ossf/scorecard-action@*"] }
   EOF
   gh api -X PUT repos/fedeam/kausa/actions/permissions/fork-pr-contributor-approval \
     -f approval_policy=all_external_contributors
   ```

   Both need the public repository: GitHub's REST documentation says that `patterns_allowed`
   applies only to public repositories, and on 2026-10-23, while kaution was private, its approval
   endpoint answered "Fork PR approval is not allowed for private repositories". Dependabot's jobs
   bypass the Actions policy (GitHub's documentation on Dependabot on GitHub Actions runners), so
   they still run. The job token stays read-only by default (`default_workflow_permissions: read`,
   checked on kausa on 2026-10-26).

8. **Add a branch ruleset on `main`.** Rulesets need a public repository or GitHub Pro: on
   2026-10-26, while private, `gh api repos/fedeam/kausa/rulesets` answered "Upgrade to GitHub
   Pro or make this repository public to enable this feature". The ruleset requires a pull
   request with one approving review, given after the last push (so nothing can change after the
   maintainer approved it),
   the `ci` check of the `CI` workflow (the check run name of the first run, on 2026-10-26, which
   GitHub took from the job id in `.github/workflows/ci.yml`), a linear history, and blocks force
   pushes and deletion. Once the `image` jobs and the `kind` job of `e2e` have passed on GitHub,
   add their check names too, read from that run (the image jobs are probably
   `image (ubuntu-24.04)` and `image (ubuntu-24.04-arm)`), so that a change that breaks the image,
   the chart or the end-to-end test cannot merge:

   ```sh
   gh api -X POST repos/fedeam/kausa/rulesets --input - <<'EOF'
   {
     "name": "main",
     "target": "branch",
     "enforcement": "active",
     "conditions": { "ref_name": { "include": ["~DEFAULT_BRANCH"], "exclude": [] } },
     "bypass_actors": [
       { "actor_id": 5, "actor_type": "RepositoryRole", "bypass_mode": "pull_request" }
     ],
     "rules": [
       { "type": "deletion" },
       { "type": "non_fast_forward" },
       { "type": "required_linear_history" },
       {
         "type": "pull_request",
         "parameters": {
           "required_approving_review_count": 1,
           "dismiss_stale_reviews_on_push": true,
           "require_code_owner_review": false,
           "require_last_push_approval": true,
           "required_review_thread_resolution": false,
           "allowed_merge_methods": ["squash"]
         }
       },
       {
         "type": "required_status_checks",
         "parameters": {
           "strict_required_status_checks_policy": true,
           "required_status_checks": [{ "context": "ci" }]
         }
       }
     ]
   }
   EOF
   ```

   `strict_required_status_checks_policy` requires a pull request to be up to date with `main`
   before it merges, so the check ran on top of the latest `main`; the update-branch button brings
   it up to date.

   Only the maintainer can land a change: nobody else has write access, so a contributor can only
   open a pull request from a fork, which needs the maintainer's approving review and the
   maintainer's merge. GitHub does not let an author approve their own pull request, so the
   ruleset has one bypass actor, the repository's admin role (the maintainer), in
   `"bypass_mode": "pull_request"`: it lets the maintainer merge their own pull requests without a
   second person's review, and it does not allow a direct push, a force push or a deletion, which
   the ruleset still blocks for everyone ([0007. Take contributions and merge pull requests on
   GitHub](../adr/0007-contributions-and-merges-on-github.md)). That `actor_id` 5 is the admin
   role is a hypothesis to check in the ruleset GitHub returns (its `bypass_actors`). OpenSSF
   Scorecard counts the bypass against Branch-Protection (step 11). The sync needs no bypass:
   GitHub's `main` already has the commit, so git sends GitHub no update.

   An agent that works with the maintainer's GitHub credentials acts as the maintainer, so these
   rules do not stop it; what stops it is `AGENTS.md` (no merge without the maintainer's
   approval). To enforce that too, give agents an account or a token without write access.

9. **Set the homepage and the social preview.** The description and the topics were set on
   2026-10-26; check them:

   ```sh
   gh repo view fedeam/kausa --json description,repositoryTopics,homepageUrl
   ```

   - **Homepage:** set it with `gh repo edit fedeam/kausa --homepage <url>` to the
     documentation URL once one exists; until then leave it empty rather than pointing at the
     repository itself.
   - **Social preview:** in **Settings → General → Social preview → Edit → Upload an image…**.
     GitHub asks for a PNG, JPG or GIF under 1 MB, at least 640 × 320 pixels (1280 × 640 for
     best display), and shows it only for public repositories. The image has to be made first;
     none exists on 2026-10-23.

10. **Add the CI badge.** GitHub's workflow badges are not accessible outside GitHub for a
    private repository, and the workflow did not run before the account's Actions were fixed, so
    it is added now, at the end of the project badge line in `README.md`, in a pull request
    merged as usual:

    ```markdown
    [![CI](https://github.com/fedeam/kausa/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/fedeam/kausa/actions/workflows/ci.yml)
    ```

    The Gitea run has no badge: an image served by the internal hostname would show broken on
    GitHub.

11. **Add OpenSSF Scorecard.** [scorecard-action](https://github.com/ossf/scorecard-action) runs
    as a GitHub Action and publishes its results, and with them the badge, only with
    `publish_results: true`, which works only on public repositories.
    - Add `.github/workflows/scorecard.yml` from the example in the action's README, with
      `publish_results: true`, `id-token: write` and `security-events: write` only on that job,
      `contents: read` at the top, and the results uploaded to code scanning as SARIF.
    - Pin `ossf/scorecard-action`, and every other action it uses, by the full commit SHA of a
      release, taken from the release page and verified, never typed from memory.
    - **Expected results**, compared with the first run and recorded in step 14. Sources: the
      check documentation of Scorecard (`docs/checks.md` in `ossf/scorecard`) and the README of
      scorecard-action, read on 2026-10-23.
      - **Branch-Protection:** a middling score expected (hypothesis). The ruleset blocks force
        pushes and deletion (tier 1) and requires one approval (tier 2), but has a bypass actor,
        which Scorecard counts against it. No personal access token: the
        scorecard-action README says that the workflow's default token can read rulesets, so
        `SECURITY.md` ("No repository secrets are configured") stays true.
      - **Code-Review:** a low score expected. Scorecard counts a change as reviewed when someone
        approved it or when whoever merged it is not its committer; the maintainer's own pull
        requests are neither.
      - **SAST:** 0 expected. Scorecard recognises CodeQL and SonarCloud only; clippy does not
        count. GitHub's CodeQL documentation lists Rust among its languages (checked on
        2026-10-23). Decide here between CodeQL code scanning and accepting 0, and record the
        decision in an ADR.
      - **Dangerous-Workflow:** a gate. No `pull_request_target` or `workflow_run` trigger, and no
        `${{ github.event.* }}` expression inside a `run:` script.
      - **Token-Permissions:** a gate. Read-only permissions at the top of every workflow,
        `scorecard.yml` included, and write permissions only on a job.
      - **Pinned-Dependencies:** a gate. Actions by full commit SHA, the `FROM` image by digest,
        downloads checked by SHA-256, no `curl | sh`. Hypothesis: whether Scorecard counts the
        Debian packages and `rustup component add` in the `Dockerfile`, the exceptions to pinning
        in [0003. Run CI in one pinned container build, on GitHub Actions and Gitea
        Actions](../adr/0003-ci-in-one-pinned-container-build.md); its documentation does not
        say.
      - **Vulnerabilities:** a gate. Scorecard asks OSV (Open Source Vulnerabilities) about the
        dependencies; `cargo audit` is green on `Cargo.lock`.
      - **Security-Policy:** `SECURITY.md`, with its link to private vulnerability reporting and
        its disclosure text.
    - After the first run, add the badge to the project badge line:

      ```markdown
      [![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/fedeam/kausa/badge)](https://scorecard.dev/viewer/?uri=github.com/fedeam/kausa)
      ```

12. **Register with OpenSSF Best Practices.** Sign up or log in at
    <https://www.bestpractices.dev>, add the project from "Get Your Badge Now!" and answer the
    criteria of the "passing" level, each answer true for the project as it is. The maintainer's
    standard leaves this badge out for repositories without outside contributors; kausa takes
    them ([0007. Take contributions and merge pull requests on
    GitHub](../adr/0007-contributions-and-merges-on-github.md)). The site gives the project a
    number; then add its badge:

    ```markdown
    [![OpenSSF Best Practices](https://www.bestpractices.dev/projects/<id>/badge)](https://www.bestpractices.dev/projects/<id>)
    ```

13. **Leave the crates.io name for later, on purpose.** Reserve `kausa` on crates.io with a
    real, usable release, not a placeholder. Check that the name is still free and read the
    crates.io usage policy at that time. With that release, add the crates.io badge
    (`https://img.shields.io/crates/v/kausa`). A docs.rs badge only fits if kausa gets a
    library target: docs.rs documents libraries, and kausa is a binary (hypothesis to check on
    docs.rs at that time).

14. **Record it.** A journal entry for the day (`docs/journal/YYYY-MM-DD-publication.md`) with
    what was checked and what was enabled, the first Scorecard result of each check against
    step 11's expectations, and a line in `CHANGELOG.md` if users will notice.

## Verify

```sh
gh repo view fedeam/kausa --json visibility --jq .visibility               # PUBLIC
gh api repos/fedeam/kausa/private-vulnerability-reporting --jq .enabled    # true
gh api repos/fedeam/kausa --jq .security_and_analysis                      # secret_scanning and secret_scanning_push_protection: enabled
gh api -i repos/fedeam/kausa/vulnerability-alerts | head -1                # status 204 (404 means alerts are off)
gh api repos/fedeam/kausa/automated-security-fixes --jq .enabled           # true
gh api repos/fedeam/kausa/actions/permissions                              # allowed_actions: selected, sha_pinning_required: true
gh api repos/fedeam/kausa/actions/permissions/selected-actions             # github_owned_allowed: true, patterns_allowed: ossf/scorecard-action@*
gh api repos/fedeam/kausa/actions/permissions/fork-pr-contributor-approval --jq .approval_policy   # all_external_contributors
gh api repos/fedeam/kausa/actions/permissions/workflow                     # default_workflow_permissions: read, can_approve_pull_request_reviews: false
gh api repos/fedeam/kausa/rulesets --jq '.[].name'                         # main
gh api repos/fedeam/kausa/rulesets/$(gh api repos/fedeam/kausa/rulesets --jq '.[0].id') \
  --jq '.bypass_actors, (.rules[] | select(.type == "pull_request") | .parameters.required_approving_review_count)'   # the admin role in pull_request mode; 1
grep -oE 'badge/(repository|internet|status)-[^-]*' README.md                # repository-public, internet-no, status-...
```

Open `https://github.com/fedeam/kausa` in a private browser window: the repository is visible
without logging in.

## If something fails

- **`gh repo edit --visibility` refuses** → the flag `--accept-visibility-change-consequences` is
  missing, or the account lacks admin rights → add the flag, or check `gh auth status`.
- **A push to `main` is rejected after step 8** → during a sync, another pull request was merged
  after your fetch: run the sync again ([Maintain the repository](maintain.md#if-something-fails));
  otherwise the push carries new commits, which the ruleset blocks on purpose → move them to a
  branch and open a pull request. Never force push.
- **The `ci` check never reports on a pull request** → GitHub Actions still do not start, or the
  check name changed → check the Actions tab; if the job was renamed, update the required check
  in the ruleset.
- **Private vulnerability reporting does not enable** → the repository is still private → finish
  step 6 first.

## Roll back

If something private turns up after publication, make the repository private at once, then
change the badge back (when closing a repository the standard changes the visibility first):

```sh
gh repo edit fedeam/kausa --visibility private --accept-visibility-change-consequences
```

Assume that anything that was public has been copied. Rotate any exposed secret at its source,
and record the incident, without the value, in the journal.
