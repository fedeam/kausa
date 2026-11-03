//! The upgrade end-to-end test (`tests/e2e/upgrade.sh`, which shares `tests/e2e/lib.sh` with
//! `tests/e2e/run.sh`), the release it upgrades from (`tests/e2e/upgrade-from.txt`) and the soak
//! scripts (`tests/soak/`) run outside cargo, against a kind cluster or a cluster their caller
//! names. These tests keep, from the files alone, the rules those scripts must follow: a pin that
//! is read and never sourced, one upgrade with no retry and no side-loaded release image, helpers
//! that are sourced and define what both kind tests use, and soak scripts that only ever use the
//! kube context they are given. They need no cluster, network or tool, so `cargo test` runs them
//! wherever it runs. `tests/chart/render.sh` runs the scripts themselves against stubs.

use std::path::Path;

/// The text of a file of the repository, by its path from the root.
fn read(path: &str) -> String {
    let full = Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
    std::fs::read_to_string(&full).unwrap_or_else(|e| panic!("{path} cannot be read: {e}"))
}

/// The permission bits of a file of the repository.
#[cfg(unix)]
fn mode(path: &str) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    let full = Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
    let meta = std::fs::metadata(&full).unwrap_or_else(|e| panic!("{path} cannot be read: {e}"));
    meta.permissions().mode() & 0o777
}

/// The lines of a shell script with each backslash continuation joined to its line, runs of
/// blanks collapsed to one, and comment lines dropped; each with its indentation (in columns of
/// its first physical line).
fn logical_lines(script: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut current: Option<(usize, String)> = None;
    for line in script.lines() {
        let (indent, mut text) = match current.take() {
            Some((indent, text)) => (indent, text + " " + line.trim()),
            None => {
                if line.trim_start().starts_with('#') {
                    continue;
                }
                (line.len() - line.trim_start().len(), line.trim().to_owned())
            }
        };
        if let Some(stripped) = text.strip_suffix('\\') {
            text = stripped.trim_end().to_owned();
            current = Some((indent, text));
            continue;
        }
        let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if !collapsed.is_empty() {
            out.push((indent, collapsed));
        }
    }
    if let Some((indent, text)) = current {
        out.push((
            indent,
            text.split_whitespace().collect::<Vec<_>>().join(" "),
        ));
    }
    out
}

/// The commands of one logical line: the line cut at `$(`, backquotes, pipes, `&&`, `;`, braces
/// and parentheses, each piece without the shell keywords that may lead a command. Good enough
/// for the scripts here, which keep `kubectl` and `helm` out of quoted multi-command strings.
fn commands(line: &str) -> Vec<String> {
    let mut text = line.to_owned();
    for separator in ["$(", "`", "&&", "||", "|", ";", "{", "}", "(", ")"] {
        text = text.replace(separator, "\n");
    }
    let keywords = [
        "if", "then", "else", "elif", "do", "while", "until", "!", "exec", "command", "time",
    ];
    text.lines()
        .filter_map(|piece| {
            let mut words: Vec<&str> = piece.split_whitespace().collect();
            while words.first().is_some_and(|w| keywords.contains(w)) {
                words.remove(0);
            }
            (!words.is_empty()).then(|| words.join(" "))
        })
        .collect()
}

/// The comment block at the top of a script (after the shebang), its lines joined with spaces.
fn header(script: &str) -> String {
    script
        .lines()
        .skip_while(|l| l.starts_with("#!"))
        .take_while(|l| l.starts_with('#'))
        .map(|l| l.trim_start_matches('#').trim())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The index of the first logical line equal to `want`, or a failure naming `what`.
fn position(lines: &[(usize, String)], want: &str, what: &str) -> usize {
    lines
        .iter()
        .position(|(_, l)| l == want)
        .unwrap_or_else(|| panic!("{what}: no line `{want}`"))
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A version `X.Y.Z` of three decimal numbers.
fn version(s: &str) -> Option<(u64, u64, u64)> {
    let mut parts = s.split('.');
    let mut next = || {
        parts
            .next()
            .filter(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|p| p.parse().ok())
    };
    let v = (next()?, next()?, next()?);
    parts.next().is_none().then_some(v)
}

/// The private IPv4 addresses (RFC 1918) written in a text: none may be (AGENTS.md, Security
/// rules).
fn private_ipv4(text: &str) -> Vec<String> {
    text.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .map(|token| token.trim_matches('.'))
        .filter(|token| {
            let octets: Vec<u16> = token.split('.').filter_map(|o| o.parse().ok()).collect();
            token.split('.').count() == 4
                && octets.len() == 4
                && octets.iter().all(|o| *o <= 255)
                && (octets[0] == 10
                    || (octets[0] == 172 && (16..=31).contains(&octets[1]))
                    || (octets[0] == 192 && octets[1] == 168))
        })
        .map(str::to_owned)
        .collect()
}

// --- tests/e2e/upgrade-from.txt -------------------------------------------------------------

const UPGRADE_FROM: &str = "tests/e2e/upgrade-from.txt";
const UPGRADE_FROM_HEADER: [&str; 2] = [
    "# The release tests/e2e/upgrade.sh upgrades from: the latest one, pinned. Bumped by a pull request",
    "# after each release (docs/how-to/maintain.md, \"Cut a release\"). Read with sed, never sourced.",
];

/// The three values of the pin file, in its order, after checking its layout.
fn upgrade_from() -> [String; 3] {
    let text = read(UPGRADE_FROM);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines.len(),
        5,
        "{UPGRADE_FROM}: not two comment lines and three keys"
    );
    assert_eq!(
        lines[..2],
        UPGRADE_FROM_HEADER,
        "{UPGRADE_FROM}: the header comment is not the spec's"
    );
    let keys = [
        "KAUSA_UPGRADE_FROM_VERSION=",
        "KAUSA_UPGRADE_FROM_CHART_SHA256=",
        "KAUSA_UPGRADE_FROM_IMAGE_DIGEST=",
    ];
    let mut values: [String; 3] = Default::default();
    for (i, key) in keys.iter().enumerate() {
        values[i] = lines[2 + i]
            .strip_prefix(key)
            .unwrap_or_else(|| panic!("{UPGRADE_FROM}: line {} is not {key}...", 3 + i))
            .to_owned();
    }
    values
}

#[test]
fn the_upgrade_pin_names_a_release_its_chart_checksum_and_its_image_digest() {
    let [from, chart, digest] = upgrade_from();
    assert!(
        version(&from).is_some(),
        "{UPGRADE_FROM}: the version is not X.Y.Z"
    );
    assert!(
        is_hex(&chart, 64),
        "{UPGRADE_FROM}: the chart SHA-256 is not 64 lowercase hex digits"
    );
    assert!(
        digest
            .strip_prefix("sha256:")
            .is_some_and(|d| is_hex(d, 64)),
        "{UPGRADE_FROM}: the image digest is not sha256: and 64 lowercase hex digits"
    );
}

#[test]
fn the_upgrade_pin_is_not_newer_than_this_checkout() {
    // The release pull request bumps Cargo.toml past the pin; the pin follows after the release
    // (docs/how-to/maintain.md, "Cut a release"). A pin ahead of the checkout would upgrade
    // backwards.
    let [from, _, _] = upgrade_from();
    let cargo = read("Cargo.toml");
    let this = cargo
        .lines()
        .find_map(|l| l.strip_prefix("version = \"")?.strip_suffix('"'))
        .expect("Cargo.toml has a version line");
    let (from, this) = (version(&from).unwrap(), version(this).unwrap());
    assert!(
        from <= this,
        "{UPGRADE_FROM}: the pinned release is newer than Cargo.toml's version"
    );
}

// --- tests/e2e/lib.sh and tests/e2e/run.sh ----------------------------------------------------

const LIB: &str = "tests/e2e/lib.sh";
const RUN: &str = "tests/e2e/run.sh";
const UPGRADE: &str = "tests/e2e/upgrade.sh";

/// Every function `tests/e2e/run.sh` defined before its test body at v0.5.1 (lines 88-919), and
/// the fake endpoint's start, which moved to `tests/e2e/lib.sh` unchanged.
const SHARED_FUNCTIONS: &[&str] = &[
    "fail",
    "k",
    "h",
    "cleanup",
    "start_endpoint",
    "explained",
    "kausa_events",
    "log_has",
    "kausa_log",
    "holder",
    "kausa_pods",
    "pods_ready",
    "pod_get",
    "pod_metric",
    "kausa_metrics",
    "model_calls",
    "no_cluster_rbac",
    "crd_gone",
    "history_rule",
    "deployments_rule",
    "kausa_args",
    "explanations",
    "hand_made",
    "utc_now",
    "restart_kausa",
    "check_explanations",
    "explanations_of",
    "no_injected_values",
    "has_roles",
    "started_field",
    "log_count",
    "others_reported",
    "seed_failure",
    "wait_explained",
    "wait_pair",
    "wait_holder",
    "wait_other_holder",
    "leader_sum",
    "changes",
    "lease_transitions",
    "restart_count",
    "node_pids",
    "node_signal",
    "lease_manifest",
    "writes_snapshot",
    "lines_of",
    "check_endpoint_line",
    "endpoint_check",
    "metric_of",
    "endpoint_state_is",
];

/// How many lines of `script` define function `name` (`name() {` at the start of a line).
fn definitions(script: &str, name: &str) -> usize {
    let start = format!("{name}() {{");
    script.lines().filter(|l| l.starts_with(&start)).count()
}

#[test]
fn lib_sh_is_sourced_never_run() {
    let lib = read(LIB);
    assert!(
        !lib.starts_with("#!"),
        "{LIB}: has a shebang; it is sourced, never run"
    );
    #[cfg(unix)]
    assert_eq!(mode(LIB) & 0o111, 0, "{LIB}: is executable; it is sourced");
    let header = header(&lib);
    for words in ["sourced", "tests/e2e/run.sh", "tests/e2e/upgrade.sh"] {
        assert!(
            header.contains(words),
            "{LIB}: the header comment does not say `{words}`"
        );
    }
    // Sourcing it defines; each script starts its own cluster, endpoint and trap.
    let lines = logical_lines(&lib);
    for (indent, line) in &lines {
        if *indent > 0 {
            continue;
        }
        assert_ne!(
            line, "start_endpoint",
            "{LIB}: starts the endpoint when sourced"
        );
        for forbidden in ["trap ", "kind create cluster", "docker build", "h install"] {
            assert!(
                !line.starts_with(forbidden),
                "{LIB}: runs `{forbidden}` when sourced"
            );
        }
    }
}

#[test]
fn lib_sh_defines_every_shared_helper_once_and_the_scripts_none() {
    let lib = read(LIB);
    let run = read(RUN);
    let upgrade = read(UPGRADE);
    for name in SHARED_FUNCTIONS {
        assert_eq!(
            definitions(&lib, name),
            1,
            "{LIB}: does not define {name}() exactly once"
        );
        assert_eq!(
            definitions(&run, name),
            0,
            "{RUN}: still defines {name}(), which lives in {LIB}"
        );
        assert_eq!(
            definitions(&upgrade, name),
            0,
            "{UPGRADE}: defines {name}(), which lives in {LIB}"
        );
    }
}

#[test]
fn run_sh_sources_lib_sh_then_parses_traps_and_starts_the_endpoint_in_order() {
    let run = read(RUN);
    assert!(
        header(&run)
            .contains("The helpers live in tests/e2e/lib.sh, which tests/e2e/upgrade.sh shares."),
        "{RUN}: the header comment lacks the sentence about tests/e2e/lib.sh"
    );
    let lines = logical_lines(&run);
    let order = [
        "set -euo pipefail",
        ". tests/e2e/lib.sh",
        "SCOPE=${KAUSA_E2E_SCOPE:-cluster}",
        "REPLICAS=${KAUSA_E2E_REPLICAS:-1}",
        "trap cleanup EXIT",
        ": \"${KIND_NODE_IMAGE:?set KIND_NODE_IMAGE to a pinned kindest/node image}\"",
        "kind create cluster --name \"$CLUSTER\" --image \"$KIND_NODE_IMAGE\" --wait 120s",
        "kind load docker-image kausa:e2e --name \"$CLUSTER\"",
        "start_endpoint",
    ];
    let mut last = 0;
    for want in order {
        let at = position(&lines, want, RUN);
        assert!(
            at >= last,
            "{RUN}: `{want}` is out of the order of contract A2"
        );
        last = at;
    }
    for want in [". tests/e2e/lib.sh", "trap cleanup EXIT", "start_endpoint"] {
        let count = lines.iter().filter(|(_, l)| l == want).count();
        assert_eq!(count, 1, "{RUN}: `{want}` is not there exactly once");
    }
    let install = lines
        .iter()
        .position(|(_, l)| l.starts_with("h install "))
        .expect("tests/e2e/run.sh installs the chart");
    assert!(
        install > last,
        "{RUN}: installs before the fake endpoint starts"
    );
}

// --- tests/e2e/upgrade.sh ---------------------------------------------------------------------

/// The one upgrade of contract A3, step 4: the four image values are the only `--set`s.
const THE_UPGRADE: &str = "h upgrade \"$RELEASE\" charts/kausa -n \"$NAMESPACE\" --reuse-values \
    --wait --timeout 180s --set image.repository=kausa --set image.tag=e2e \
    --set-string image.digest= --set image.pullPolicy=Never";

#[test]
fn upgrade_sh_is_an_executable_bash_script_that_sources_lib_sh() {
    let script = read(UPGRADE);
    assert!(
        script.starts_with("#!/usr/bin/env bash\n"),
        "{UPGRADE}: not a bash script"
    );
    #[cfg(unix)]
    assert_eq!(mode(UPGRADE), 0o755, "{UPGRADE}: mode is not 0755");
    let lines = logical_lines(&script);
    let set = position(&lines, "set -euo pipefail", UPGRADE);
    let lib = position(&lines, ". tests/e2e/lib.sh", UPGRADE);
    assert!(
        set < lib,
        "{UPGRADE}: sources lib.sh before set -euo pipefail"
    );
    assert!(
        !script.contains("KAUSA_E2E_REPLICAS"),
        "{UPGRADE}: reads KAUSA_E2E_REPLICAS; it always runs two replicas"
    );
}

#[test]
fn upgrade_sh_names_its_own_cluster_before_the_trap_can_delete_one() {
    // lib.sh's cleanup deletes the cluster CLUSTER names, kausa-e2e by default: the upgrade test
    // must name kausa-upgrade before its trap is set, or a failure would delete run.sh's.
    let lines = logical_lines(&read(UPGRADE));
    let trap = position(&lines, "trap cleanup EXIT", UPGRADE);
    for want in [
        "REPLICAS=2",
        "CLUSTER=kausa-upgrade",
        "CONTEXT=\"kind-$CLUSTER\"",
    ] {
        let at = position(&lines, want, UPGRADE);
        assert!(at < trap, "{UPGRADE}: `{want}` comes after the trap");
    }
    let traps = lines.iter().filter(|(_, l)| l.starts_with("trap ")).count();
    assert_eq!(traps, 1, "{UPGRADE}: not exactly one trap");
}

#[test]
fn upgrade_sh_reads_the_pin_and_checks_the_chart_before_any_cluster_exists() {
    let script = read(UPGRADE);
    let lines = logical_lines(&script);
    for (_, line) in &lines {
        for command in commands(line) {
            let mut words = command.split_whitespace();
            let first = words.next().unwrap_or_default();
            assert!(
                !((first == "." || first == "source") && command.contains("upgrade-from.txt")),
                "{UPGRADE}: sources tests/e2e/upgrade-from.txt; it is read with sed"
            );
        }
    }
    assert!(
        lines
            .iter()
            .any(|(_, l)| l.contains("sed -n") && l.contains("tests/e2e/upgrade-from.txt")),
        "{UPGRADE}: does not read tests/e2e/upgrade-from.txt with sed -n"
    );
    for pattern in [
        r"^[0-9]+\.[0-9]+\.[0-9]+$",
        "^[0-9a-f]{64}$",
        "^sha256:[0-9a-f]{64}$",
    ] {
        assert!(
            script.contains(pattern),
            "{UPGRADE}: does not check a pin against {pattern}"
        );
    }
    let curl = position(
        &lines,
        "curl --proto '=https' --tlsv1.2 -fsSL --retry 5 -o \"$WORK/from.tgz\" \
         \"https://github.com/fedeam/kausa/releases/download/v$FROM_VERSION/kausa-$FROM_VERSION.tgz\"",
        UPGRADE,
    );
    let checked = lines
        .iter()
        .position(|(_, l)| l.contains("fail \"the v$FROM_VERSION chart is not the pinned one\""))
        .unwrap_or_else(|| panic!("{UPGRADE}: no check of the chart's SHA-256"));
    let create = lines
        .iter()
        .position(|(_, l)| l.starts_with("kind create cluster "))
        .unwrap_or_else(|| panic!("{UPGRADE}: creates no kind cluster"));
    assert!(
        curl < checked,
        "{UPGRADE}: checks the chart before downloading it"
    );
    assert!(
        checked < create,
        "{UPGRADE}: creates the cluster before the chart is checked"
    );
    assert!(
        script.contains("python3 -I"),
        "{UPGRADE}: does not hash the chart with python3 -I"
    );
}

#[test]
fn upgrade_sh_upgrades_once_without_a_retry() {
    let lines = logical_lines(&read(UPGRADE));
    let upgrades: Vec<&(usize, String)> = lines
        .iter()
        .filter(|(_, l)| commands(l).iter().any(|c| c.starts_with("h upgrade")))
        .collect();
    assert_eq!(upgrades.len(), 1, "{UPGRADE}: not exactly one helm upgrade");
    let (indent, line) = upgrades[0];
    assert_eq!(
        line, THE_UPGRADE,
        "{UPGRADE}: the upgrade is not contract A3's line"
    );
    // At the top level: not in a loop, a function or a condition that could run it again.
    assert_eq!(
        *indent, 0,
        "{UPGRADE}: the upgrade is inside a block; it runs once, at the top level"
    );
    assert!(
        !lines.iter().any(|(_, l)| l.contains("--reset-values")),
        "{UPGRADE}: uses --reset-values"
    );
}

#[test]
fn upgrade_sh_installs_the_release_from_the_checked_chart() {
    let lines = logical_lines(&read(UPGRADE));
    let installs: Vec<&String> = lines
        .iter()
        .map(|(_, l)| l)
        .filter(|l| commands(l).iter().any(|c| c.starts_with("h install")))
        .collect();
    assert_eq!(
        installs,
        [
            "h install \"$RELEASE\" \"$WORK/from.tgz\" -n \"$NAMESPACE\" --wait --timeout 180s \
             \"${from_values[@]}\""
        ],
        "{UPGRADE}: not exactly contract A3's install of the downloaded chart"
    );
    assert!(
        lines
            .iter()
            .any(|(_, l)| l.contains("--set image.digest=\"$FROM_IMAGE_DIGEST\"")),
        "{UPGRADE}: the release is not installed by the pinned digest"
    );
}

#[test]
fn upgrade_sh_never_side_loads_the_released_image() {
    // A pulled multi-architecture index loaded into kind ends in ErrImageNeverPull on Docker
    // Desktop (the spec's trap 1): the node pulls the release from GHCR, and only the checkout's
    // build is loaded.
    let lines = logical_lines(&read(UPGRADE));
    let loads: Vec<&String> = lines
        .iter()
        .map(|(_, l)| l)
        .filter(|l| l.contains("kind load"))
        .collect();
    assert_eq!(
        loads,
        ["kind load docker-image kausa:e2e --name \"$CLUSTER\""],
        "{UPGRADE}: loads something other than kausa:e2e into kind"
    );
    for (_, line) in &lines {
        for forbidden in ["docker pull", "docker tag", "docker save", "image-archive"] {
            assert!(
                !line.contains(forbidden),
                "{UPGRADE}: runs `{forbidden}`; the node pulls the release itself"
            );
        }
    }
    assert!(
        lines
            .iter()
            .any(|(_, l)| l == "docker build --target runtime -t kausa:e2e ."),
        "{UPGRADE}: does not build kausa:e2e from the runtime target"
    );
}

#[test]
fn upgrade_sh_prints_its_first_and_last_lines_and_the_contract_messages() {
    let script = read(UPGRADE);
    let lines = logical_lines(&script);
    assert!(
        lines.iter().any(|(_, l)| l
            == "echo \"e2e upgrade: v$FROM_VERSION to this checkout ($TO_VERSION), \
                rbac.scope=$SCOPE, two replicas\""),
        "{UPGRADE}: no first line naming both versions"
    );
    assert_eq!(
        lines.last().map(|(_, l)| l.as_str()),
        Some("echo \"e2e upgrade: v$FROM_VERSION to $TO_VERSION, rbac.scope=$SCOPE: ok\""),
        "{UPGRADE}: the last line is not the ok line"
    );
    assert!(
        lines.iter().any(|(_, l)| l
            == ": \"${KIND_NODE_IMAGE:?set KIND_NODE_IMAGE to a pinned kindest/node image}\""),
        "{UPGRADE}: KIND_NODE_IMAGE is not required"
    );
    for words in [
        "KAUSA_E2E_SCOPE must be cluster or namespaced",
        "no version in tests/e2e/upgrade-from.txt",
        "no chart SHA-256 in tests/e2e/upgrade-from.txt",
        "no image digest in tests/e2e/upgrade-from.txt",
        "the v$FROM_VERSION chart is not the pinned one",
        "no two Ready kausa Pods with one holding the Lease after installing v$FROM_VERSION",
        "a kausa Pod does not run the pinned v$FROM_VERSION image",
        "a started line has version $v before the upgrade",
        "the Deployment does not report two updated, ready and available replicas after the upgrade",
        "a kausa Pod from before the upgrade is still there",
        "a kausa Pod does not run kausa:e2e after the upgrade",
        "a kausa Pod restarted after the upgrade",
        "the release is not deployed at revision 2",
        "the image values after the upgrade are not the four the upgrade set",
        "the upgrade changed the user values: ",
        "no two Ready kausa Pods with one holding the Lease after the upgrade",
        // The leader and standby checks of tests/e2e/run.sh, word for word.
        "kausa_leader is not 1 on the Lease holder",
        "kausa_leader is not 0 on the standby",
        "/readyz of the Lease holder is not 'ready'",
        "/readyz of the standby is not 'standby'",
        "a kausa Pod does not run with exactly one --leader-lease",
        "a kausa Pod's --leader-lease is not followed by $NAMESPACE/$LEASE",
        "a started line has leader_lease $lease",
        "leading lines across both Pods' logs",
        "the standby logged leading",
        "the standby logged no standby line",
        "a started line has version $v after the upgrade",
        "the Lease's leaseTransitions did not increase in the upgrade",
        // What the new chart rendered from the reused values, which helm get values cannot show.
        "the new leader started with other settings than the old one: ",
        "the new leader's --max-calls-per-hour is not followed by 3600",
        "the new leader does not run with exactly one --history",
        "the new leader logged no incident about Job upgrade-after",
        "another Pod than the leader wrote about Job upgrade-after",
        "the standby logged an incident",
        "notifications about Job upgrade-after, not 1",
    ] {
        assert!(script.contains(words), "{UPGRADE}: no message `{words}`");
    }
    for words in [
        "seed_failure upgrade-before",
        "wait_explained Job upgrade-before",
        "seed_failure upgrade-after",
        "wait_explained Job upgrade-after",
        "endpoint_check ok fake",
    ] {
        assert!(
            lines.iter().any(|(_, l)| l.contains(words)),
            "{UPGRADE}: does not run `{words}`"
        );
    }
}

// --- tests/soak/ ------------------------------------------------------------------------------

const SOAK_SHELL: [&str; 3] = [
    "tests/soak/install.sh",
    "tests/soak/exercise.sh",
    "tests/soak/uninstall.sh",
];
const SOAK_PYTHON: [&str; 2] = ["tests/soak/sample.py", "tests/soak/summarize.py"];
const SOAK_ALL: [&str; 7] = [
    "tests/soak/values.yaml",
    "tests/soak/endpoint.yaml",
    "tests/soak/install.sh",
    "tests/soak/exercise.sh",
    "tests/soak/uninstall.sh",
    "tests/soak/sample.py",
    "tests/soak/summarize.py",
];

#[test]
fn the_soak_scripts_are_executable_bash_with_set_euo_pipefail() {
    for path in SOAK_SHELL {
        let script = read(path);
        assert!(
            script.starts_with("#!/usr/bin/env bash\n"),
            "{path}: not a bash script"
        );
        #[cfg(unix)]
        assert_eq!(mode(path), 0o755, "{path}: mode is not 0755");
        assert!(
            logical_lines(&script)
                .iter()
                .any(|(_, l)| l == "set -euo pipefail"),
            "{path}: no set -euo pipefail"
        );
    }
}

#[test]
fn the_soak_never_uses_or_changes_the_current_context() {
    for path in SOAK_ALL {
        let text = read(path);
        for words in [
            "use-context",
            "current-context",
            "set-context",
            "config view",
        ] {
            assert!(!text.contains(words), "{path}: mentions `{words}`");
        }
    }
}

#[test]
fn every_kubectl_and_helm_call_of_the_soak_shell_scripts_names_the_context() {
    for path in SOAK_SHELL {
        let script = read(path);
        let mut guard = 0;
        for (_, line) in logical_lines(&script) {
            for command in commands(&line) {
                let words: Vec<&str> = command.split_whitespace().collect();
                match words[0] {
                    "kubectl" if command.starts_with("kubectl config get-contexts -o name") => {
                        guard += 1;
                    }
                    "kubectl" => assert_eq!(
                        words.get(1),
                        Some(&"--context"),
                        "{path}: a kubectl call without --context first: `{command}`"
                    ),
                    "helm" => assert_eq!(
                        words.get(1),
                        Some(&"--kube-context"),
                        "{path}: a helm call without --kube-context first: `{command}`"
                    ),
                    _ => {}
                }
            }
        }
        assert_eq!(
            guard, 1,
            "{path}: does not check the context against kubectl config get-contexts -o name once"
        );
    }
}

#[test]
fn the_soak_python_scripts_call_kubectl_through_one_helper_or_not_at_all() {
    // sample.py: the guard's kubectl config get-contexts -o name, at most once, and one helper
    // that prepends ["kubectl", "--context", ctx]; no other kubectl.
    let sample = read(SOAK_PYTHON[0]).replace('\'', "\"");
    let (mut helper, mut guard) = (0, 0);
    for (at, _) in sample.match_indices("\"kubectl\"") {
        let rest = &sample[at + "\"kubectl\"".len()..];
        let rest: String = rest.split_whitespace().collect::<Vec<_>>().join(" ");
        if rest.starts_with(", \"--context\", ") {
            helper += 1;
        } else if rest.starts_with(", \"config\", \"get-contexts\", \"-o\", \"name\"]") {
            guard += 1;
        } else {
            panic!(
                "{}: a kubectl call that neither names the context nor reads the context names",
                SOAK_PYTHON[0]
            );
        }
    }
    assert_eq!(
        (helper, guard <= 1),
        (1, true),
        "{}: not one kubectl helper with --context and at most one guard",
        SOAK_PYTHON[0]
    );
    // summarize.py reads the sample files only: it imports nothing that runs a process or opens
    // a connection.
    let summarize = read(SOAK_PYTHON[1]);
    for line in summarize.lines().map(str::trim) {
        if line.starts_with("import ") || line.starts_with("from ") {
            for module in ["subprocess", "urllib", "http", "socket", "shutil"] {
                assert!(
                    !line
                        .split(|c: char| !c.is_alphanumeric() && c != '_')
                        .any(|w| w == module),
                    "{}: imports {module}",
                    SOAK_PYTHON[1]
                );
            }
        }
    }
    for words in ["os.system", "os.popen", "os.exec", "os.spawn"] {
        assert!(
            !summarize.contains(words),
            "{}: calls {words}",
            SOAK_PYTHON[1]
        );
    }
    assert!(
        summarize.contains("--self-test"),
        "{}: no --self-test",
        SOAK_PYTHON[1]
    );
    for path in SOAK_PYTHON {
        assert!(!read(path).contains("shell=True"), "{path}: runs a shell");
    }
}

#[test]
fn the_soak_scripts_print_their_usage_lines() {
    for (path, usage) in [
        (
            "tests/soak/install.sh",
            "usage: tests/soak/install.sh <kube-context>",
        ),
        (
            "tests/soak/exercise.sh",
            "usage: tests/soak/exercise.sh <kube-context> [minutes]",
        ),
        (
            "tests/soak/uninstall.sh",
            "usage: tests/soak/uninstall.sh <kube-context>",
        ),
    ] {
        assert!(read(path).contains(usage), "{path}: no line `{usage}`");
    }
}

#[test]
fn the_soak_values_are_spec_b3() {
    // Spec B3, exactly: the chart's defaults, an in-cluster fake endpoint, and nothing written
    // outside kausa-soak (no Explained Events, no Explanations and no CustomResourceDefinition).
    let want = "\
# The soak's values (docs/how-to/run-the-soak.md): v0.5.1's chart defaults, with an in-cluster
# fake endpoint and nothing written outside kausa-soak.
rbac:
  scope: cluster
replicaCount: 2
model:
  endpoint: http://kausa-soak-endpoint.kausa-soak.svc:8080/v1
  name: fake
  allowHttp: true
events: false
history:
  enabled: false
notifications:
  enabled: true
  format: json
  existingSecret: kausa-soak-notify
  allowHttp: true
";
    assert_eq!(
        read("tests/soak/values.yaml"),
        want,
        "tests/soak/values.yaml is not spec B3's"
    );
}

#[test]
fn the_soak_endpoint_is_pinned_and_never_selected_as_kausa() {
    let path = "tests/soak/endpoint.yaml";
    let text = read(path);
    let image = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("image: "))
        .unwrap_or_else(|| panic!("{path}: no image"));
    let digest = image
        .trim_matches('"')
        .strip_prefix("python:3.13-slim-trixie@sha256:")
        .unwrap_or_else(|| panic!("{path}: the image is not python:3.13-slim-trixie by digest"));
    assert!(
        is_hex(digest, 64),
        "{path}: the digest is not 64 hex digits"
    );
    for words in [
        "app.kubernetes.io/name: kausa-soak-endpoint",
        "/app/fake_endpoint.py",
        "--fail-file",
        "/tmp/fail",
        "--hook-log",
        "/dev/null",
        "automountServiceAccountToken: false",
        "readOnlyRootFilesystem: true",
        "runAsNonRoot: true",
        "allowPrivilegeEscalation: false",
        "RuntimeDefault",
    ] {
        assert!(text.contains(words), "{path}: no `{words}`");
    }
    // kausa's chart selects its Pods by these labels: the endpoint must never carry them.
    for line in text.lines().map(str::trim) {
        assert_ne!(
            line, "app.kubernetes.io/name: kausa",
            "{path}: carries kausa's name label"
        );
        assert!(
            !line.starts_with("app.kubernetes.io/instance:"),
            "{path}: carries an instance label kausa's selector could match"
        );
    }
}

#[test]
fn the_breaker_job_reuses_the_benchmarks_busybox() {
    let pinned = |path: &str| -> Vec<String> {
        read(path)
            .split(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ',')
            .filter(|w| w.starts_with("busybox") && w.contains("@sha256:"))
            .map(str::to_owned)
            .collect()
    };
    let bench = pinned("tests/bench/seeded-failures.yaml");
    let want = bench.first().expect("the benchmarks pin busybox");
    // Typed in or read from the benchmarks' file (spec C: reuse, do not retype).
    let exercise = pinned("tests/soak/exercise.sh");
    let reads = read("tests/soak/exercise.sh").contains("tests/bench/seeded-failures.yaml");
    assert!(
        exercise.iter().all(|r| r == want) && (reads || !exercise.is_empty()),
        "tests/soak/exercise.sh: busybox is not the reference of tests/bench/seeded-failures.yaml"
    );
    let script = read("tests/soak/exercise.sh");
    for words in [
        "soak-breaker-",
        "kausa-soak",
        "backoffLimit",
        "ttlSecondsAfterFinished",
        "86400",
        "exit 1",
        "printf \"401\\n\" > /tmp/fail",
        "rm -f /tmp/fail",
    ] {
        assert!(
            script.contains(words),
            "tests/soak/exercise.sh: no `{words}`"
        );
    }
    // One trap or several: together they cover the end, an interrupt and a stop.
    let traps: Vec<String> = logical_lines(&script)
        .into_iter()
        .map(|(_, l)| l)
        .filter(|l| l.starts_with("trap "))
        .collect();
    for signal in ["EXIT", "INT", "TERM"] {
        assert!(
            traps
                .iter()
                .any(|t| t.split_whitespace().any(|w| w == signal)),
            "tests/soak/exercise.sh: no trap covers {signal}"
        );
    }
}

#[test]
fn the_new_scripts_hold_no_private_address() {
    let mut paths = vec![LIB, UPGRADE, UPGRADE_FROM];
    paths.extend(SOAK_ALL);
    for path in paths {
        let found = private_ipv4(&read(path));
        assert!(
            found.is_empty(),
            "{path}: holds {} private IPv4 address(es)",
            found.len()
        );
    }
}

// --- the helpers of these tests ---------------------------------------------------------------

#[test]
fn logical_lines_join_continuations_and_drop_comments() {
    let script = "# a comment\nfoo \\\n  --bar   baz\n  # indented comment\n  qux\n";
    assert_eq!(
        logical_lines(script),
        [(0, "foo --bar baz".to_owned()), (2, "qux".to_owned())]
    );
}

#[test]
fn commands_split_a_line_at_its_separators() {
    assert_eq!(
        commands("if ! out=$(kubectl get x | grep -q y); then h upgrade z; fi"),
        ["out=", "kubectl get x", "grep -q y", "h upgrade z", "fi"]
    );
    assert_eq!(
        commands("k() { kubectl --context \"$CTX\" \"$@\"; }"),
        ["k", "kubectl --context \"$CTX\" \"$@\""]
    );
}

#[test]
fn versions_and_private_addresses_are_read_as_meant() {
    assert_eq!(version("0.5.1"), Some((0, 5, 1)));
    assert_eq!(version("0.5"), None);
    assert_eq!(version("0.5.1.2"), None);
    assert_eq!(version("v0.5.1"), None);
    assert!(version("0.10.0") > version("0.9.9"));
    let documentation = "192.0.2.10 198.51.100.7 2001:db8::1";
    assert!(private_ipv4(documentation).is_empty());
    // Built here, so that this file holds no private address either.
    let (a, b, c) = (10, 172, 192);
    let private = format!("x {a}.0.2.1 y {b}.20.0.1, {b}.32.0.1 {c}.{}.0.1", 168);
    assert_eq!(private_ipv4(&private).len(), 3);
    assert!(private_ipv4("version 1.36.4, 1.2.3.4.5").is_empty());
}
