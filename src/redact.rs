//! Redaction: what never leaves the cluster (docs/reference/rules.md, "Redaction rules";
//! docs/adr/0008-data-leaving-the-cluster.md).
//!
//! A `Redactor` lives for one incident. It gives the names kausa knows stable placeholders, numbered
//! in order of appearance (`R002`), applies the patterns `R003` to `R014` to free text, counts what
//! each rule replaced, and maps the `R002` placeholders back in the model's answer. The answer is
//! untrusted, so the same patterns run on it first (`answer`, without `R002`), counted apart
//! (docs/adr/0022-redact-the-answer.md). Everything is deterministic: the same incident produces
//! the same request.

use std::collections::BTreeMap;
use std::net::Ipv6Addr;
use std::sync::LazyLock;

use regex::{Captures, Regex};

/// Registries whose names are public, so image references from them are sent as they are.
const PUBLIC_REGISTRIES: [&str; 9] = [
    "docker.io",
    "registry-1.docker.io",
    "index.docker.io",
    "ghcr.io",
    "quay.io",
    "registry.k8s.io",
    "gcr.io",
    "mcr.microsoft.com",
    "public.ecr.aws",
];

/// Hostnames that are public and say nothing about a cluster, kept by `R013`. Hostnames are
/// matched in lowercase only, so Java packages and class names (`com.example.App`) stay.
const PUBLIC_HOSTS: [&str; 6] = [
    "github.com",
    "kubernetes.io",
    "k8s.io",
    "example.com",
    "example.net",
    "example.org",
];

/// Last labels that make `name.ext:12` a file and line, not a host and port.
const FILE_EXTENSIONS: [&str; 40] = [
    "rs", "py", "go", "js", "ts", "jsx", "tsx", "java", "kt", "rb", "php", "c", "h", "cpp", "cc",
    "hpp", "cs", "swift", "scala", "ex", "exs", "erl", "lua", "pl", "sh", "sql", "yaml", "yml",
    "json", "toml", "xml", "txt", "log", "conf", "cfg", "ini", "md", "html", "css", "proto",
];

/// The kinds of names `R002` replaces, and the prefix of their placeholders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameKind {
    Namespace,
    /// An owner or another object, by its kind (`Deployment` becomes `deployment-1`).
    Object(&'static str),
    Pod,
    Container,
    Node,
    ServiceAccount,
}

impl NameKind {
    fn prefix(self) -> String {
        match self {
            NameKind::Namespace => "namespace".into(),
            NameKind::Object(kind) => kind.to_ascii_lowercase(),
            NameKind::Pod => "pod".into(),
            NameKind::Container => "container".into(),
            NameKind::Node => "node".into(),
            NameKind::ServiceAccount => "serviceaccount".into(),
        }
    }
}

/// The kind name used for an owner kind that is not one of the built-in ones: custom resources
/// become `object-1`, so their kind does not leak either.
pub fn object_kind(kind: &str) -> &'static str {
    match kind {
        "Deployment" => "Deployment",
        "StatefulSet" => "StatefulSet",
        "DaemonSet" => "DaemonSet",
        "ReplicaSet" => "ReplicaSet",
        "Job" => "Job",
        "CronJob" => "CronJob",
        "Pod" => "Pod",
        "PersistentVolumeClaim" => "PersistentVolumeClaim",
        "HorizontalPodAutoscaler" => "HorizontalPodAutoscaler",
        _ => "Object",
    }
}

#[derive(Debug, Default)]
pub struct Redactor {
    /// Known names and their placeholders, mapped back in answers.
    names: Vec<(String, String)>,
    /// Hostnames and their placeholders (`R013`), never mapped back.
    hosts: Vec<(String, String)>,
    counters: BTreeMap<String, usize>,
    /// What each rule replaced in the request.
    counts: BTreeMap<&'static str, usize>,
    /// What each rule replaced in the model's answer.
    answer_counts: BTreeMap<&'static str, usize>,
}

/// The pattern rules that also run on the model's answer, in the order they run
/// (docs/adr/0022-redact-the-answer.md): every rule of `Redactor::text` but `R002`, which replaces
/// known real names (the model saw only placeholders and public image references). `R001` is the
/// allowlist of fields and applies to no text.
pub const ANSWER_RULES: [&str; 12] = [
    "R009", "R014", "R007", "R008", "R006", "R010", "R011", "R005", "R004", "R003", "R013", "R012",
];

/// Which text a pass of the pattern rules redacts: the request, or the model's answer.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pass {
    Request,
    Answer,
}

impl Redactor {
    pub fn new() -> Self {
        Self::default()
    }

    /// The placeholder of a known name (`R002`), registering it on first use.
    pub fn name(&mut self, kind: NameKind, real: &str) -> String {
        if real.is_empty() {
            return String::new();
        }
        self.name_of_kind(&kind.prefix(), real)
    }

    /// An image reference: kept as it is from a public registry or with no registry (Docker
    /// Hub), otherwise `registry-N/image-N` with the tag kept and the digest dropped.
    pub fn image(&mut self, image: &str) -> String {
        let (name, tag) = split_tag(image);
        let Some((host, path)) = name.split_once('/') else {
            return image.to_owned();
        };
        let is_host = host.contains('.') || host.contains(':') || host == "localhost";
        if !is_host || PUBLIC_REGISTRIES.contains(&host) {
            return image.to_owned();
        }
        let registry = self.name_of_kind("registry", host);
        let path = self.name_of_kind("image", path);
        self.tally(Pass::Request, "R002", 1);
        match tag {
            Some(tag) => format!("{registry}/{path}:{tag}"),
            None => format!("{registry}/{path}"),
        }
    }

    /// Redacts free text. First the secrets (`R009`, then the query and fragment of URLs and of
    /// HTTP request lines, `R014`, whatever they hold, then `R007`, `R008`, `R006`, `R010`,
    /// `R011`), so that a known name inside a secret's key cannot break the key apart; then the
    /// known names (`R002`); then addresses, Kubernetes names, hostnames and high-entropy strings
    /// (`R005`, `R004`, `R003`, `R013`, `R012`). Text that already looks like one of kausa's placeholders is neutralized
    /// first, so the answer cannot map it to a real name.
    pub fn text(&mut self, text: &str) -> String {
        let out = self.neutralize_placeholders(text);
        self.patterns(out, Pass::Request)
    }

    /// Redacts one text of the model's answer (docs/adr/0022-redact-the-answer.md): the pattern
    /// rules of `text`, in the same order, without the neutralization of placeholders (which
    /// would break the restore) and without `R002`. What it replaces is counted apart, in
    /// `answer_counts`. Kausa's own output echoed by the model (a marker, `host-N`, a placeholder
    /// in any case) is left as it is and not counted.
    pub fn answer(&mut self, text: &str) -> String {
        self.patterns(text.to_owned(), Pass::Answer)
    }

    /// The one chain of pattern rules, for the request or for the answer: only the request pass
    /// replaces known names (`R002`).
    fn patterns(&mut self, text: String, pass: Pass) -> String {
        let mut out = self.apply(&PEM, "R009", text, pass, |_| "<redacted:pem>".into());
        out = self.query_and_fragment(&URL_QUERY, out, pass);
        out = self.query_and_fragment(&REQUEST_LINE_QUERY, out, pass);
        out = self.apply(&AUTHORIZATION_HEADER, "R007", out, pass, |c| {
            format!("{}<redacted:authorization>", &c[1])
        });
        out = self.apply(&COOKIE_HEADER, "R007", out, pass, |c| {
            format!("{}<redacted:cookie>", &c[1])
        });
        out = self.apply(&AUTHORIZATION_SCHEME, "R007", out, pass, |c| {
            format!("{} <redacted:authorization>", &c[1])
        });
        out = self.apply(&JWT, "R008", out, pass, |_| "<redacted:jwt>".into());
        out = self.apply(&URL_USERINFO, "R006", out, pass, |c| {
            format!("{}<redacted:userinfo>@", &c[1])
        });
        // A kind before a quoted name (`volume "credentials" : secret "<name>" not found`) is not
        // the key's value: it stays, so that `R003` redacts the name after it.
        let kind_before_name = |c: &Captures<'_>| c.get(3).is_some() && KIND_WORD.is_match(&c[2]);
        out = self.apply_unless(
            &SECRET_KEY_VALUE,
            "R010",
            out,
            pass,
            kind_before_name,
            |c| {
                let value = &c[2];
                let quoted_name = c.get(3).map_or("", |m| m.as_str());
                let marker = if value.starts_with("\\\"") {
                    "\\\"<redacted:value>\\\""
                } else if value.starts_with('"') {
                    "\"<redacted:value>\""
                } else if value.starts_with('\'') {
                    "'<redacted:value>'"
                } else {
                    "<redacted:value>"
                };
                format!("{}{marker}{quoted_name}", &c[1])
            },
        );
        out = self.apply(&SECRET_FLAG, "R010", out, pass, |c| {
            format!("{} <redacted:value>", &c[1])
        });
        out = self.apply(&SECRET_ELEMENT, "R010", out, pass, |c| {
            // In an answer, kausa's own `<redacted:token>` or `<redacted:secret>` is not an
            // element: the words after it stay.
            if pass == Pass::Answer && is_marker(&c[1]) {
                c[0].to_owned()
            } else {
                format!("{}<redacted:value>", &c[1])
            }
        });
        out = self.apply(&KNOWN_TOKEN, "R011", out, pass, |_| {
            "<redacted:token>".into()
        });
        if pass == Pass::Request {
            out = self.replace_names(&out);
        }
        out = self.apply(&EMAIL, "R005", out, pass, |_| "<redacted:email>".into());
        out = self.apply(&IPV4, "R004", out, pass, |_| "<redacted:ip>".into());
        out = self.ipv6(out, pass);
        out = self.kubernetes_names(out, pass);
        out = self.hostnames(out, pass);
        self.high_entropy(out, pass)
    }

    /// Maps the `R002` placeholders in `text` back to the real names, in one pass, so a restored
    /// name is never replaced again. A placeholder capitalized by the model (`Pod-1`) is restored
    /// too. Placeholders of other rules (`<redacted:…>`, `host-N`) stay.
    pub fn restore(&self, text: &str) -> String {
        self.restore_if(text, |_| true)
    }

    /// Like `restore`, but maps a placeholder back only when its real name is one of `names`
    /// (for a notification, the namespace and the top-level owner: docs/adr/0015-notifications.md).
    /// Every other placeholder stays, so the names of Pods, containers, nodes, service accounts
    /// and private images never leave in a notification.
    pub fn restore_only(&self, text: &str, names: &[&str]) -> String {
        self.restore_if(text, |real| names.contains(&real))
    }

    /// Maps each placeholder in `text` whose real name passes `keep` back to that name, in one
    /// pass; the rest stay as they are.
    fn restore_if(&self, text: &str, keep: impl Fn(&str) -> bool) -> String {
        PLACEHOLDER
            .replace_all(text, |c: &Captures<'_>| {
                let found = &c[0];
                let mut lowered = found.to_owned();
                lowered[..1].make_ascii_lowercase();
                self.names
                    .iter()
                    .find(|(_, placeholder)| *placeholder == lowered)
                    .filter(|(real, _)| keep(real))
                    .map_or_else(|| found.to_owned(), |(real, _)| real.clone())
            })
            .into_owned()
    }

    /// How many values each rule replaced in the request, by rule id.
    pub fn counts(&self) -> &BTreeMap<&'static str, usize> {
        &self.counts
    }

    /// How many values each rule replaced in the model's answer, by rule id (an id of
    /// `ANSWER_RULES`).
    pub fn answer_counts(&self) -> &BTreeMap<&'static str, usize> {
        &self.answer_counts
    }

    fn next(&mut self, prefix: String) -> String {
        let n = self.counters.entry(prefix.clone()).or_insert(0);
        *n += 1;
        format!("{prefix}-{n}")
    }

    fn name_of_kind(&mut self, prefix: &str, real: &str) -> String {
        if let Some((_, placeholder)) = self.names.iter().find(|(name, _)| name == real) {
            return placeholder.clone();
        }
        let placeholder = self.next(prefix.to_owned());
        self.names.push((real.to_owned(), placeholder.clone()));
        placeholder
    }

    /// Adds `n` replacements by `rule` to the counts of `pass`.
    fn tally(&mut self, pass: Pass, rule: &'static str, n: usize) {
        if n == 0 {
            return;
        }
        let counts = match pass {
            Pass::Request => &mut self.counts,
            Pass::Answer => {
                debug_assert!(ANSWER_RULES.contains(&rule), "{rule} runs on requests only");
                &mut self.answer_counts
            }
        };
        *counts.entry(rule).or_insert(0) += n;
    }

    /// `pod-1` in text from the cluster becomes `pod_1`, unless it is a registered real name, so
    /// that `restore` maps only kausa's own placeholders.
    fn neutralize_placeholders(&self, text: &str) -> String {
        PLACEHOLDER_LITERAL
            .replace_all(text, |c: &Captures<'_>| {
                let found = &c[0];
                if self
                    .names
                    .iter()
                    .any(|(real, _)| real.eq_ignore_ascii_case(found))
                {
                    found.to_owned()
                } else {
                    format!("{}_{}", &c[1], &c[2])
                }
            })
            .into_owned()
    }

    fn replace_names(&mut self, text: &str) -> String {
        let mut names: Vec<(String, String)> = self.names.clone();
        names.sort_by_key(|(real, _)| std::cmp::Reverse(real.len()));
        let mut out = text.to_owned();
        for (real, placeholder) in names {
            let (replaced, n) = replace_word(&out, &real, &placeholder);
            self.tally(Pass::Request, "R002", n);
            out = replaced;
        }
        out
    }

    /// Replaces every match of `pattern`. On the answer, a match counts only when its
    /// replacement changes it, so an echoed marker is not counted.
    fn apply(
        &mut self,
        pattern: &Regex,
        rule: &'static str,
        text: String,
        pass: Pass,
        replacement: impl Fn(&Captures<'_>) -> String,
    ) -> String {
        self.apply_unless(pattern, rule, text, pass, |_| false, replacement)
    }

    /// Like `apply`, but a match for which `keep` holds stays as it is and is not counted.
    fn apply_unless(
        &mut self,
        pattern: &Regex,
        rule: &'static str,
        text: String,
        pass: Pass,
        keep: impl Fn(&Captures<'_>) -> bool,
        replacement: impl Fn(&Captures<'_>) -> String,
    ) -> String {
        let mut n = 0;
        let out = pattern
            .replace_all(&text, |c: &Captures<'_>| {
                if keep(c) {
                    return c[0].to_owned();
                }
                let replaced = replacement(c);
                if pass == Pass::Request || replaced != c[0] {
                    n += 1;
                }
                replaced
            })
            .into_owned();
        self.tally(pass, rule, n);
        out
    }

    /// The query and fragment (`R014`) of what `pattern` finds, a URL or an HTTP request line,
    /// whatever they hold: `?<redacted:query>` and `#<redacted:fragment>`. `pattern` captures
    /// what is kept (group 1), then the query and fragment (group 2). Punctuation that ends a
    /// sentence after them is kept, and a `?` or `#` with nothing after it is left alone.
    fn query_and_fragment(&mut self, pattern: &Regex, text: String, pass: Pass) -> String {
        let mut n = 0;
        let out = pattern
            .replace_all(&text, |c: &Captures<'_>| {
                let tail = &c[2];
                let kept = tail.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '}']);
                let (query, fragment) = match kept.find('#') {
                    Some(at) => (&kept[..at], &kept[at..]),
                    None => (kept, ""),
                };
                let mut out = c[1].to_string();
                if query.len() > 1 {
                    out.push_str("?<redacted:query>");
                } else {
                    out.push_str(query);
                }
                if fragment.len() > 1 {
                    out.push_str("#<redacted:fragment>");
                } else {
                    out.push_str(fragment);
                }
                if query.len() > 1 || fragment.len() > 1 {
                    n += 1;
                }
                out.push_str(&tail[kept.len()..]);
                out
            })
            .into_owned();
        self.tally(pass, "R014", n);
        out
    }

    /// IPv6 addresses: a candidate must parse as one, after dropping the colons a message puts
    /// around it (`… 2001:db8::1: refused`), and must not touch a word (`Foo::Bar` stays).
    fn ipv6(&mut self, text: String, pass: Pass) -> String {
        let mut out = String::with_capacity(text.len());
        let mut last = 0;
        let mut n = 0;
        for m in IPV6_CANDIDATE.find_iter(&text) {
            let word = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
            if word(text[..m.start()].chars().next_back()) || word(text[m.end()..].chars().next()) {
                continue;
            }
            let mut start = m.start();
            let mut end = m.end();
            let candidate = |s: usize, e: usize| text[s..e].parse::<Ipv6Addr>().is_ok();
            while end - start > 2 && !candidate(start, end) && text[..end].ends_with(':') {
                end -= 1;
            }
            if !candidate(start, end)
                && text[start..].starts_with(':')
                && !text[start..].starts_with("::")
            {
                start += 1;
            }
            if end > start && candidate(start, end) {
                out.push_str(&text[last..start]);
                out.push_str("<redacted:ip>");
                last = end;
                n += 1;
            }
        }
        out.push_str(&text[last..]);
        self.tally(pass, "R004", n);
        out
    }

    fn kubernetes_names(&mut self, text: String, pass: Pass) -> String {
        let placeholders: Vec<String> = self.names.iter().map(|(_, p)| p.clone()).collect();
        // On the answer, a placeholder the model capitalized (`Pod-1`) is ours too, as `restore`
        // maps it back.
        let ours = |name: &str| {
            let mut name = name.to_owned();
            if pass == Pass::Answer
                && let Some(first) = name.get_mut(..1)
            {
                first.make_ascii_lowercase();
            }
            placeholders.contains(&name)
        };
        // On the answer, a match counts only when its replacement changes it.
        let changed = |replaced: &str, matched: &str| pass == Pass::Request || replaced != matched;
        let mut n = 0;
        // The pattern of v0.3 first, unchanged, so the second pass can only add redactions
        // (invariant 4): a quote left open after a kind of the second pass cannot hide a name the
        // first pass catches.
        let mut out = KUBERNETES_NAME
            .replace_all(&text, |c: &Captures<'_>| {
                if ours(&c[2]) || (pass == Pass::Answer && &c[2] == "<redacted:name>") {
                    c[0].to_owned()
                } else {
                    let replaced = format!("{} \"<redacted:name>\"", &c[1]);
                    if changed(&replaced, &c[0]) {
                        n += 1;
                    }
                    replaced
                }
            })
            .into_owned();
        out = KUBERNETES_NAME_LIST
            .replace_all(&out, |c: &Captures<'_>| {
                // Every name of a list (`containers "a", "b"`), each on its own; a name the first
                // pass already replaced is neither replaced nor counted again.
                let names = QUOTED_NAME.replace_all(&c[2], |q: &Captures<'_>| {
                    if ours(&q[1]) || &q[1] == "<redacted:name>" {
                        q[0].to_owned()
                    } else {
                        let replaced = "\"<redacted:name>\"".to_owned();
                        if changed(&replaced, &q[0]) {
                            n += 1;
                        }
                        replaced
                    }
                });
                format!("{} {names}", &c[1])
            })
            .into_owned();
        out = EXCEEDED_QUOTA
            .replace_all(&out, |c: &Captures<'_>| {
                if ours(&c[2]) {
                    c[0].to_owned()
                } else {
                    let replaced = format!("{}<redacted:name>", &c[1]);
                    if changed(&replaced, &c[0]) {
                        n += 1;
                    }
                    replaced
                }
            })
            .into_owned();
        out = KUBERNETES_NAMESPACED
            .replace_all(&out, |c: &Captures<'_>| {
                if ours(&c[3]) {
                    c[0].to_owned()
                } else {
                    let replaced = format!("{}{}/<redacted:name>", &c[1], &c[2]);
                    if changed(&replaced, &c[0]) {
                        n += 1;
                    }
                    replaced
                }
            })
            .into_owned();
        out = SERVICE_ACCOUNT_USER
            .replace_all(&out, |c: &Captures<'_>| {
                if ours(&c[2]) {
                    c[0].to_owned()
                } else {
                    let replaced = format!("{}<redacted:name>", &c[1]);
                    if changed(&replaced, &c[0]) {
                        n += 1;
                    }
                    replaced
                }
            })
            .into_owned();
        out = JOB_POLICY_NAMES
            .replace_all(&out, |c: &Captures<'_>| {
                // The container and the Pod of a Job's `podFailurePolicy` note; the namespace
                // between them is left to `R002`.
                let mut name = |found: &str| {
                    if ours(found) {
                        found.to_owned()
                    } else {
                        n += 1;
                        "<redacted:name>".to_owned()
                    }
                };
                if c.get(1).is_some() {
                    let container = name(&c[2]);
                    let pod = name(&c[4]);
                    format!("{}{container}{}{pod}{}", &c[1], &c[3], &c[5])
                } else {
                    let pod = name(&c[7]);
                    format!("{}{pod}{}", &c[6], &c[8])
                }
            })
            .into_owned();
        self.tally(pass, "R003", n);
        out
    }

    fn host_placeholder(&mut self, host: &str, pass: Pass) -> Option<String> {
        let host = host.to_ascii_lowercase();
        let public = PUBLIC_HOSTS
            .iter()
            .chain(PUBLIC_REGISTRIES.iter())
            .any(|p| host == *p || host.ends_with(&format!(".{p}")));
        if public {
            return None;
        }
        self.tally(pass, "R013", 1);
        if let Some((_, placeholder)) = self.hosts.iter().find(|(h, _)| *h == host) {
            return Some(placeholder.clone());
        }
        let placeholder = self.next("host".into());
        self.hosts.push((host, placeholder.clone()));
        Some(placeholder)
    }

    /// Hostnames in three forms: a dotted name with a known suffix, the host of a URL whatever its
    /// suffix (`http://svc.ns/`), and `name.other:port` (a Service in another namespace), except
    /// when its last label is a file extension (`main.rs:10`).
    fn hostnames(&mut self, text: String, pass: Pass) -> String {
        let mut out = self.replace_matches(&HOSTNAME, &text, 0, pass);
        out = self.replace_matches(&URL_HOST, &out, 1, pass);
        let mut result = String::with_capacity(out.len());
        let mut last = 0;
        let matches: Vec<(usize, usize, String)> = HOST_PORT
            .captures_iter(&out)
            .filter_map(|c| {
                let host = c.get(1)?;
                let last_label = host.as_str().rsplit('.').next()?;
                let is_file = FILE_EXTENSIONS.contains(&last_label);
                let has_letter = last_label.bytes().any(|b| b.is_ascii_alphabetic());
                (!is_file && has_letter)
                    .then(|| (host.start(), host.end(), host.as_str().to_owned()))
            })
            .collect();
        for (start, end, host) in matches {
            if let Some(placeholder) = self.host_placeholder(&host, pass) {
                result.push_str(&out[last..start]);
                result.push_str(&placeholder);
                last = end;
            }
        }
        result.push_str(&out[last..]);
        result
    }

    /// Replaces capture group `group` of every match of `pattern` with a host placeholder.
    fn replace_matches(&mut self, pattern: &Regex, text: &str, group: usize, pass: Pass) -> String {
        let mut out = String::with_capacity(text.len());
        let mut last = 0;
        let spans: Vec<(usize, usize)> = pattern
            .captures_iter(text)
            .filter_map(|c| c.get(group).map(|m| (m.start(), m.end())))
            .collect();
        for (start, end) in spans {
            if let Some(placeholder) = self.host_placeholder(&text[start..end], pass) {
                out.push_str(&text[last..start]);
                out.push_str(&placeholder);
                last = end;
            }
        }
        out.push_str(&text[last..]);
        out
    }

    fn high_entropy(&mut self, text: String, pass: Pass) -> String {
        let mut n = 0;
        let out = TOKEN_RUN
            .replace_all(&text, |c: &Captures<'_>| {
                let run = &c[0];
                // A path or URL is judged segment by segment, so only a token inside it goes.
                if run.matches('/').count() >= 2 {
                    return run
                        .split('/')
                        .map(|segment| {
                            if segment.len() >= 20 && looks_secret(segment) {
                                n += 1;
                                "<redacted:secret>"
                            } else {
                                segment
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("/");
                }
                if looks_secret(run) {
                    n += 1;
                    "<redacted:secret>".to_owned()
                } else {
                    run.to_owned()
                }
            })
            .into_owned();
        self.tally(pass, "R012", n);
        out
    }
}

/// Whether `text` is one whole marker of kausa's (`<redacted:token>`).
fn is_marker(text: &str) -> bool {
    text.strip_prefix("<redacted:")
        .and_then(|rest| rest.strip_suffix('>'))
        .is_some_and(|kind| {
            !kind.is_empty() && kind.bytes().all(|b| b.is_ascii_lowercase() || b == b':')
        })
}

/// Whether a run of token characters looks like a secret: a letter and a digit, and a Shannon
/// entropy of at least 4 bits per character, or 3 for a hexadecimal run (whose ceiling is 4).
fn looks_secret(run: &str) -> bool {
    let has_digit = run.bytes().any(|b| b.is_ascii_digit());
    let has_letter = run.bytes().any(|b| b.is_ascii_alphabetic());
    let hex = run.bytes().all(|b| b.is_ascii_hexdigit());
    let threshold = if hex { 3.0 } else { 4.0 };
    has_digit && has_letter && shannon_entropy(run) >= threshold
}

/// Replaces `word` in `text`, ignoring ASCII case, where it is not part of a longer alphanumeric
/// word, and returns the text and the number of replacements. Hyphens, dots, slashes and
/// underscores are boundaries, so a Deployment named `api` is replaced inside its ReplicaSet's
/// name `api-7d9c8b6f4d` and in `API_SERVICE_HOST`; so is a percent escape (`%2Fapi`).
fn replace_word(text: &str, word: &str, replacement: &str) -> (String, usize) {
    if word.is_empty() {
        return (text.to_owned(), 0);
    }
    let haystack = text.to_ascii_lowercase();
    let needle = word.to_ascii_lowercase();
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut n = 0;
    let mut last = 0;
    for (start, _) in haystack.match_indices(&needle) {
        if start < last {
            continue;
        }
        let end = start + needle.len();
        let escape_before = start >= 3
            && bytes[start - 3] == b'%'
            && bytes[start - 2].is_ascii_hexdigit()
            && bytes[start - 1].is_ascii_hexdigit();
        let before = text[..start].chars().next_back();
        let after = text[end..].chars().next();
        let boundary = |c: Option<char>| !c.is_some_and(|c| c.is_ascii_alphanumeric());
        if (boundary(before) || escape_before) && boundary(after) {
            out.push_str(&text[last..start]);
            out.push_str(replacement);
            last = end;
            n += 1;
        }
    }
    out.push_str(&text[last..]);
    (out, n)
}

/// Splits `name:tag@digest` into the name and the tag, dropping the digest. A colon in the first
/// path component is a registry port, not a tag.
fn split_tag(image: &str) -> (&str, Option<&str>) {
    let image = image.split('@').next().unwrap_or(image);
    let last_slash = image.rfind('/').map_or(0, |i| i + 1);
    match image[last_slash..].rfind(':') {
        Some(i) => (&image[..last_slash + i], Some(&image[last_slash + i + 1..])),
        None => (image, None),
    }
}

/// Shannon entropy of the characters of `s`, in bits per character.
fn shannon_entropy(s: &str) -> f64 {
    let mut counts: BTreeMap<char, usize> = BTreeMap::new();
    for c in s.chars() {
        *counts.entry(c).or_insert(0) += 1;
    }
    let len = s.chars().count() as f64;
    counts
        .values()
        .map(|&n| {
            let p = n as f64 / len;
            -p * p.log2()
        })
        .sum()
}

fn regex(pattern: &str) -> Regex {
    // The patterns are constants, checked by the tests.
    Regex::new(pattern).expect("a valid redaction pattern")
}

/// The keys whose values `R010` redacts, as the end of a key name.
const SECRET_KEYS: &str = r"(?:password|passwd|passphrase|pass|pwd|secret(?:[_-]?key(?:[_-]?base)?)?|(?:signing|encryption|master|auth)[_-]?key|token|api[_-]?key|access[_-]?key|private[_-]?key|credentials?|auth|signature|sig|session(?:[_-]?id)?|sid)";

static PLACEHOLDER: LazyLock<Regex> = LazyLock::new(|| regex(r"\b[A-Za-z]+-[0-9]+\b"));
static PLACEHOLDER_LITERAL: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"(?i)\b(namespace|pod|container|node|serviceaccount|registry|image|object|host|deployment|statefulset|daemonset|replicaset|job|cronjob)-([0-9]+)\b",
    )
});
static PEM: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?is)-----BEGIN [A-Z0-9 ]+-----.*?-----END [A-Z0-9 ]+-----"));
static AUTHORIZATION_HEADER: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r#"(?i)(\b(?:proxy-)?authorization\\?["']?\s*[:=]\s*)(?:\\?"[^"\r\n]*"|'[^'\r\n]*'|[^\r\n]+)"#,
    )
});
static COOKIE_HEADER: LazyLock<Regex> = LazyLock::new(|| {
    regex(r#"(?i)(\b(?:set-)?cookie\\?["']?\s*[:=]\s*)(?:\\?"[^"\r\n]*"|'[^'\r\n]*'|[^\r\n]+)"#)
});
static AUTHORIZATION_SCHEME: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?i)\b(bearer|basic)\s+[A-Za-z0-9._~+/=-]{8,}"));
static JWT: LazyLock<Regex> =
    LazyLock::new(|| regex(r"\beyJ[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{5,}"));
static URL_USERINFO: LazyLock<Regex> = LazyLock::new(|| {
    regex(r"(?i)\b([a-z][a-z0-9+.-]*://)(?:[^/\s:@]*:[^\s@]*(?:@[^\s@]*)*|[^/\s:@]+)@")
});
/// A URL with a scheme up to its query or fragment (group 1), then the query and fragment (group 2).
/// Quotes, angle brackets, backticks, backslashes and whitespace end the URL.
static URL_QUERY: LazyLock<Regex> =
    LazyLock::new(|| regex(r#"(?i)(\b[a-z][a-z0-9+.-]*://[^\s?#"'<>`\\]*)([?#][^\s"'<>`\\]*)"#));
/// An HTTP request line, as an access log writes it (`GET /orders?page=2 HTTP/1.1`): the method,
/// in capitals, and the path up to its query or fragment (group 1), then the query and fragment
/// (group 2), ended as `URL_QUERY` ends a URL. `CONNECT` takes no path.
static REQUEST_LINE_QUERY: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r#"(\b(?:GET|HEAD|POST|PUT|PATCH|DELETE|OPTIONS|TRACE)[ \t]+/[^\s?#"'<>`\\]*)([?#][^\s"'<>`\\]*)"#,
    )
});
/// A secret's key and separator (group 1), its value (group 2) and a quoted Kubernetes name after
/// it (group 3), kept as it is: no key or separator fits in it, so it hides no other `R010` match.
static SECRET_KEY_VALUE: LazyLock<Regex> = LazyLock::new(|| {
    regex(&format!(
        r#"(?i)(\b[a-z0-9_.-]*{SECRET_KEYS}\b\\?["']?\s*(?:[:=]|%3[ad])\s*)(\\"(?:[^"\\]|\\[^"])*\\"|"(?:[^"\\]|\\.)*"|'[^']*'|[^\s"',;&}}<\\]+)( ["'][a-z0-9][a-z0-9.-]*["'])?"#
    ))
});
/// A kind of `KUBERNETES_NAME`, alone: as a bare value before a quoted name, it is the start of
/// `<kind> "<name>"`, which `R003` redacts, not a secret.
static KIND_WORD: LazyLock<Regex> = LazyLock::new(|| {
    regex(r"(?i)^(?:secrets?|configmaps?|persistentvolumeclaims?|pvc|serviceaccounts?|services?)$")
});
static SECRET_FLAG: LazyLock<Regex> = LazyLock::new(|| {
    regex(&format!(
        r#"(?i)(\s--?[a-z0-9_.-]*{SECRET_KEYS})\s+[^\s"'-][^\s"']*"#
    ))
});
static SECRET_ELEMENT: LazyLock<Regex> =
    LazyLock::new(|| regex(&format!(r"(?i)(<[a-z0-9_.:-]*{SECRET_KEYS}>)[^<]*")));
/// Token formats by their issuer's prefix. Sources of the formats added after v0.4.1: GitLab's
/// prefixes in <https://docs.gitlab.com/security/tokens/>; npm's `npm_`, with a six-character
/// Base62 checksum and 178 bits of entropy (at least 30 random Base62 characters before it), in
/// <https://github.blog/security/announcing-npms-new-access-token-format>; PyPI's pattern in
/// <https://docs.pypi.org/api/secrets/>; Docker's `dckr_pat_` and `dckr_oat_` in
/// <https://docs.docker.com/reference/api/ai-governance/latest/>.
static KNOWN_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    regex(concat!(
        r"\b(?:",
        r"gh[pousr]_[A-Za-z0-9]{36}",
        r"|github_pat_[A-Za-z0-9_]{22,}",
        r"|gl(?:pat|dt|rtr?|cbt|ptt|ft|imt|agent|wt|soat|ffct|oas)-[A-Za-z0-9_-]{20,}",
        r"|npm_[A-Za-z0-9]{36,}",
        r"|pypi-[A-Za-z0-9_-]{85,}",
        r"|dckr_(?:pat|oat)_[A-Za-z0-9_-]{10,}",
        r"|xox[abposr]-[A-Za-z0-9-]{10,}",
        r"|(?:AKIA|ASIA)[0-9A-Z]{16}",
        r"|(?:sk|rk)_(?:live|test)_[A-Za-z0-9]{10,}",
        r"|sk-(?:ant-|proj-)?[A-Za-z0-9_-]{20,}",
        r"|AIza[0-9A-Za-z_-]{35}",
        r")"
    ))
});
static EMAIL: LazyLock<Regex> =
    LazyLock::new(|| regex(r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b"));
static IPV4: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"\b(?:(?:25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9])\.){3}(?:25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9])\b",
    )
});
static IPV6_CANDIDATE: LazyLock<Regex> =
    LazyLock::new(|| regex(r"[0-9A-Fa-f]{0,4}(?::[0-9A-Fa-f]{0,4}){2,7}"));
/// The quoted name after a kind (`secret "<name>"`): the pattern of v0.3, kept unchanged as the
/// first pass so that no later pattern can let through a name it catches (invariant 4).
static KUBERNETES_NAME: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r#"(?i)\b(secrets?|configmaps?|persistentvolumeclaims?|pvc|serviceaccounts?|services?) ["']([^"']+)["']"#,
    )
});
/// The second pass, over the first one's output: the quoted names, one or a list, after a kind
/// (`containers "<a>", "<b>"`, `StorageClass "<name>"`) or after a resource qualified by its API
/// group, as the API server's `NotFound` error writes it (`storageclass.storage.k8s.io "<name>" not
/// found`, `deployments.apps "<name>" not found`). A Kubernetes name has no whitespace, so a quote
/// left open cannot run over the words after it.
static KUBERNETES_NAME_LIST: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r#"(?i)\b(secrets?|configmaps?|persistentvolumeclaims?|pvc|serviceaccounts?|services?|pods?|containers?|storageclass(?:es)?|[a-z0-9-]+(?:\.[a-z0-9-]+)+) (["'][^"'\s]+["'](?:, ["'][^"'\s]+["'])*)"#,
    )
});
/// One quoted name of a `KUBERNETES_NAME_LIST` list.
static QUOTED_NAME: LazyLock<Regex> = LazyLock::new(|| regex(r#"["']([^"'\s]+)["']"#));
/// The quota in the API server's refusal of an object by a ResourceQuota: `exceeded quota: <name>,
/// requested: …` (`staging/src/k8s.io/apiserver/pkg/admission/plugin/resourcequota/controller.go`).
static EXCEEDED_QUOTA: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?i)\b(exceeded quota: )([a-z0-9][a-z0-9.-]*)"));
static KUBERNETES_NAMESPACED: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?i)\b(secret\s+|configmap\s+)([a-z0-9.-]+)/([a-z0-9.-]+)"));
static SERVICE_ACCOUNT_USER: LazyLock<Regex> =
    LazyLock::new(|| regex(r#"(?i)(system:serviceaccount:[^:\s"']+:)([a-z0-9.-]+)"#));
/// The container and the Pod in the Job controller's `podFailurePolicy` notes, in their two
/// formats: `Container <container> for pod <namespace>/<pod> failed with exit code <n> matching
/// <action> rule at index <i>` and `Pod <namespace>/<pod> has condition <type> matching <action>
/// rule at index <i>` (`pkg/controller/job/pod_failure_policy.go:57` and `:74` at v1.36.4). A
/// `D008` incident gathers no Pod, so neither name is registered.
static JOB_POLICY_NAMES: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"\b(?:(Container )([^\s/]+)( for pod [^\s/]+/)([^\s/]+)( failed with exit code )|(Pod [^\s/]+/)([^\s/]+)( has condition ))",
    )
});
static HOSTNAME: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"\b(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+(?:svc\.cluster\.local|home\.arpa|com|net|org|io|dev|app|cloud|ai|co|me|info|biz|us|uk|de|fr|es|it|nl|ar|br|mx|cl|eu|local|lan|internal|corp|svc)\b",
    )
});
static URL_HOST: LazyLock<Regex> =
    LazyLock::new(|| regex(r"\b[a-z][a-z0-9+.-]*://([a-z0-9-]+(?:\.[a-z0-9-]+)+)"));
static HOST_PORT: LazyLock<Regex> =
    LazyLock::new(|| regex(r"\b([a-z0-9-]+(?:\.[a-z0-9-]+)+):[0-9]{2,5}\b"));
static TOKEN_RUN: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?:[A-Za-z0-9+/_-]|%[0-9A-Fa-f]{2}){32,}={0,2}"));

#[cfg(test)]
mod tests {
    use super::*;

    // A corpus of invented lines (AGENTS.md: no real cluster output, and tokens built to match a
    // format, never ones that were issued).

    fn redact(line: &str) -> String {
        Redactor::new().text(line)
    }

    #[test]
    fn known_names_become_stable_placeholders_and_come_back() {
        let mut r = Redactor::new();
        assert_eq!(r.name(NameKind::Namespace, "shop"), "namespace-1");
        assert_eq!(
            r.name(NameKind::Object("Deployment"), "api"),
            "deployment-1"
        );
        assert_eq!(r.name(NameKind::Pod, "api-7d9c8b6f4d-2xkqp"), "pod-1");
        assert_eq!(r.name(NameKind::Namespace, "shop"), "namespace-1");
        let text =
            r.text("pod shop/api-7d9c8b6f4d-2xkqp of api (replicaset api-7d9c8b6f4d) failed");
        assert_eq!(
            text,
            "pod namespace-1/pod-1 of deployment-1 (replicaset deployment-1-7d9c8b6f4d) failed"
        );
        assert_eq!(
            r.restore("deployment-1 in namespace-1 keeps crashing; pod-1 too"),
            "api in shop keeps crashing; api-7d9c8b6f4d-2xkqp too"
        );
        // Names are words: `api` inside `rapid` stays.
        assert_eq!(r.text("a rapid restart"), "a rapid restart");
    }

    #[test]
    fn capitalized_placeholders_are_restored_too() {
        let mut r = Redactor::new();
        r.name(NameKind::Pod, "toobig-6c57d8cfd5-ncs7b");
        assert_eq!(
            r.restore("Pod-1 is waiting; pod-1 needs memory"),
            "toobig-6c57d8cfd5-ncs7b is waiting; toobig-6c57d8cfd5-ncs7b needs memory"
        );
    }

    #[test]
    fn placeholders_with_more_digits_are_restored_first() {
        let mut r = Redactor::new();
        for i in 1..=10 {
            r.name(NameKind::Pod, &format!("p{i}"));
        }
        assert_eq!(r.restore("pod-10 and pod-1"), "p10 and p1");
    }

    #[test]
    fn images_from_public_registries_stay_and_private_ones_do_not() {
        let mut r = Redactor::new();
        assert_eq!(r.image("busybox:1.37"), "busybox:1.37");
        assert_eq!(
            r.image("docker.io/library/nginx:1.27"),
            "docker.io/library/nginx:1.27"
        );
        assert_eq!(r.image("ghcr.io/example/app:v2"), "ghcr.io/example/app:v2");
        assert_eq!(
            r.image("registry.example.com:5000/team/api:1.4.2@sha256:abc"),
            "registry-1/image-1:1.4.2"
        );
        // The registry and path are known names afterwards.
        assert_eq!(
            r.text("pulling registry.example.com:5000/team/api"),
            "pulling registry-1/image-1"
        );
    }

    #[test]
    fn secrets_are_redacted() {
        let cases = [
            (
                "Authorization: Bearer abcdefghijklmnop",
                "Authorization: <redacted:authorization>",
            ),
            (
                "sent header authorization=Basic dXNlcjpwYXNz",
                "sent header authorization=<redacted:authorization>",
            ),
            (
                "token rejected: Bearer abcdefghijklmnop.qrs",
                "token rejected: Bearer <redacted:authorization>",
            ),
            (
                "jwt eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ4In0.c2lnbmF0dXJl ok",
                "jwt <redacted:jwt> ok",
            ),
            (
                "connecting to postgres://app:hunter2@db:5432/shop",
                "connecting to postgres://<redacted:userinfo>@db:5432/shop",
            ),
            (
                "DB_PASSWORD=not-a-real-password",
                "DB_PASSWORD=<redacted:value>",
            ),
            (
                r#"{"api_key": "abc123", "user": "x"}"#,
                r#"{"api_key": "<redacted:value>", "user": "x"}"#,
            ),
            (
                "client_secret: s3cr3t value",
                "client_secret: <redacted:value> value",
            ),
            (
                "using ghp_000000000000000000000000000000000000 for git",
                "using <redacted:token> for git",
            ),
            (
                "aws key AKIA0000000000000000 found",
                "aws key <redacted:token> found",
            ),
            (
                "key sk-test-00000000000000000000000000",
                "key <redacted:token>",
            ),
            (
                "runner glrt-00000000000000000000 registered",
                "runner <redacted:token> registered",
            ),
            (
                "deploy gldt-00000000000000000000 used",
                "deploy <redacted:token> used",
            ),
            (
                "npm publish npm_000000000000000000000000000000000000 failed",
                "npm publish <redacted:token> failed",
            ),
            (
                concat!(
                    "upload pypi-0000000000000000000000000000000000000000000",
                    "000000000000000000000000000000000000000000 rejected"
                ),
                "upload <redacted:token> rejected",
            ),
            (
                "docker login -p dckr_pat_0000000000000000000000000",
                "docker login -p <redacted:token>",
            ),
            (
                "pull with dckr_oat_0000000000000000000000000 denied",
                "pull with <redacted:token> denied",
            ),
            ("mail admin@example.com now", "mail <redacted:email> now"),
            (
                "-----BEGIN EXAMPLE KEY-----\nTm90IGEga2V5\n-----END EXAMPLE KEY-----",
                "<redacted:pem>",
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(redact(input), expected, "{input}");
        }
    }

    #[test]
    fn known_token_prefixes_need_their_minimum_length() {
        // Each prefix and the shortest tail it is redacted with; one character less stays.
        let formats = [
            ("glpat-", 20),
            ("gldt-", 20),
            ("glrt-", 20),
            ("glrtr-", 20),
            ("glcbt-", 20),
            ("glptt-", 20),
            ("glft-", 20),
            ("glimt-", 20),
            ("glagent-", 20),
            ("glwt-", 20),
            ("glsoat-", 20),
            ("glffct-", 20),
            ("gloas-", 20),
            ("npm_", 36),
            ("pypi-", 85),
            ("dckr_pat_", 10),
            ("dckr_oat_", 10),
        ];
        for (prefix, min) in formats {
            let long = format!("use {prefix}{} now", "0".repeat(min));
            assert_eq!(redact(&long), "use <redacted:token> now", "{long}");
            let short = format!("use {prefix}{} now", "0".repeat(min - 1));
            assert_eq!(redact(&short), short, "{short}");
        }
    }

    #[test]
    fn addresses_are_redacted_and_ports_kept() {
        assert_eq!(
            redact("connect ECONNREFUSED 192.0.2.10:5432"),
            "connect ECONNREFUSED <redacted:ip>:5432"
        );
        assert_eq!(
            redact("listen on [2001:db8::1]:8080"),
            "listen on [<redacted:ip>]:8080"
        );
        assert_eq!(
            redact("dial tcp: lookup db.shop.svc.cluster.local: no such host"),
            "dial tcp: lookup host-1: no such host"
        );
        assert_eq!(
            redact("GET https://api.internal.corp/v1"),
            "GET https://host-1/v1"
        );
    }

    #[test]
    fn kubernetes_names_in_messages_are_redacted() {
        assert_eq!(
            redact(r#"Error: secret "db-credentials" not found"#),
            r#"Error: secret "<redacted:name>" not found"#
        );
        assert_eq!(
            redact(r#"configmap "app-config" not found"#),
            r#"configmap "<redacted:name>" not found"#
        );
    }

    #[test]
    fn long_high_entropy_strings_are_redacted() {
        assert_eq!(
            redact("value=Zq3x9Lm2Pw8Rt5Vb1Nc7Hd4Kf6Jg0Ys2Ua9 here"),
            "value=<redacted:secret> here"
        );
    }

    #[test]
    fn ordinary_lines_stay() {
        // Numbers, versions, timestamps, durations, paths, public hosts and plain words.
        let lines = [
            "2026-10-26T10:05:00Z INFO server started on port 8080 in 2.5s",
            "version 1.4.2 (build 20261026)",
            "GET /api/v1/orders 200 12ms",
            "panic: runtime error: index out of range [3] with length 3",
            "Back-off 5m0s restarting failed container",
            "0/3 nodes are available: 3 Insufficient memory.",
            "Readiness probe failed: HTTP probe failed with statuscode: 503",
            "see https://github.com/example/project/issues/12 and https://kubernetes.io/docs/",
            "at 10:05:00 the job finished with exit code 1",
            "java.lang.NullPointerException at com.example.App.main(App.java:12)",
            "read config.yaml and main.rs",
            // Words that start like a known token prefix.
            "npm_config_cache=/tmp/npm and npm_package_version 1.4.2",
            "pip install from pypi-mirror or pypi-simple-index",
            "the glrt- and gldt- prefixes, glft-feed-reader",
            "docker login with a dckr_pat_ token",
        ];
        for line in lines {
            assert_eq!(redact(line), line, "{line}");
        }
    }

    #[test]
    fn names_in_other_cases_and_after_escapes_are_replaced() {
        let mut r = Redactor::new();
        r.name(NameKind::Namespace, "shop");
        r.name(NameKind::Object("Deployment"), "payments");
        r.name(NameKind::Pod, "payments-7d9c8b6f4d-2xkqp");
        assert_eq!(
            r.text("SHOP and Payments: PAYMENTS_SERVICE_HOST=x"),
            "namespace-1 and deployment-1: deployment-1_SERVICE_HOST=x"
        );
        assert_eq!(
            r.text("GET /api/v1/namespaces/shop%2Fpayments-7d9c8b6f4d-2xkqp"),
            "GET /api/v1/namespaces/namespace-1%2Fpod-1"
        );
    }

    #[test]
    fn a_known_name_inside_a_secret_key_does_not_save_the_value() {
        let mut r = Redactor::new();
        r.name(NameKind::Container, "auth");
        r.name(NameKind::Object("Deployment"), "api");
        assert_eq!(
            r.text("AUTH_TOKEN=abc123 API_KEY=xyz"),
            "container-1_TOKEN=<redacted:value> deployment-1_KEY=<redacted:value>"
        );
    }

    #[test]
    fn more_secret_shapes_are_redacted() {
        let cases = [
            (
                "redis://:supersecretpw@redis:6379/0",
                "redis://<redacted:userinfo>@redis:6379/0",
            ),
            (
                "postgres://admin:p@ss@db:5432",
                "postgres://<redacted:userinfo>@db:5432",
            ),
            (
                "postgres://billing:W1nter/2026@db:5432/ledger",
                "postgres://<redacted:userinfo>@db:5432/ledger",
            ),
            (
                r#"{"body":"{\"user\":\"admin\",\"password\":\"Winter2026\"}"}"#,
                r#"{"body":"{\"user\":\"admin\",\"password\":\"<redacted:value>\"}"}"#,
            ),
            (
                "<password>hunter2</password>",
                "<password><redacted:value></password>",
            ),
            (
                "run --password hunter2 --verbose",
                "run --password <redacted:value> --verbose",
            ),
            (
                "client_secret%3Dhunter2&x=1",
                "client_secret%3D<redacted:value>&x=1",
            ),
            (
                "SECRET_KEY=django-insecure-k7#m$2p!q",
                "SECRET_KEY=<redacted:value>",
            ),
            (
                "MYSQL_PWD=Winter2026! REDIS_PASS=Summer2026",
                "MYSQL_PWD=<redacted:value> REDIS_PASS=<redacted:value>",
            ),
            (
                "passphrase=hunter2 signing_key=hunter2",
                "passphrase=<redacted:value> signing_key=<redacted:value>",
            ),
            (
                r#"{"password": "correct horse battery staple"}"#,
                r#"{"password": "<redacted:value>"}"#,
            ),
            (
                "Cookie: JSESSIONID=8F3A2B1C9D4E5F6A; theme=dark",
                "Cookie: <redacted:cookie>",
            ),
            ("sessionid=abc123def", "sessionid=<redacted:value>"),
            (
                "{'Authorization': 'Token abc', 'Host': 'h'}",
                "{'Authorization': <redacted:authorization>, 'Host': 'h'}",
            ),
            (
                "Authorization: Digest username=\"u\", response=\"6629fae49393a05397450978507c4ef1\"",
                "Authorization: <redacted:authorization>",
            ),
            (
                "https://acct.blob.storage.net/c?sv=2020&sig=abc%2Bdef%3D",
                "https://host-1/c?<redacted:query>",
            ),
            (
                "-----begin example key-----\nTm90\n-----end example key-----",
                "<redacted:pem>",
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(redact(input), expected, "{input}");
        }
    }

    #[test]
    fn hexadecimal_secrets_are_redacted() {
        assert_eq!(
            redact("hex api 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"),
            "hex api <redacted:secret>"
        );
        assert_eq!(
            redact("dd 3d1e2a4b5c6d7e8f9a0b1c2d3e4f5a6b"),
            "dd <redacted:secret>"
        );
    }

    #[test]
    fn kubernetes_names_in_other_message_formats() {
        assert_eq!(
            redact("couldn't find key password in Secret prod/db-credentials"),
            "couldn't find key password in Secret prod/<redacted:name>"
        );
        assert_eq!(
            redact("forbidden: User \"system:serviceaccount:prod:billing-bot\" cannot list"),
            "forbidden: User \"system:serviceaccount:prod:<redacted:name>\" cannot list"
        );
        assert_eq!(
            redact("secret 'db-credentials' not found"),
            "secret \"<redacted:name>\" not found"
        );
    }

    #[test]
    fn cluster_dns_names_and_urls_are_redacted() {
        assert_eq!(
            redact("dial payments.billing:8080 failed"),
            "dial host-1:8080 failed"
        );
        assert_eq!(
            redact("GET http://api.shop/health"),
            "GET http://host-1/health"
        );
        assert_eq!(
            redact("at main.rs:10 and app.py:42"),
            "at main.rs:10 and app.py:42"
        );
    }

    #[test]
    fn the_query_and_fragment_of_urls_are_redacted() {
        // Issue #48: a kubelet note for a probe that cannot connect quotes the whole URL.
        let cases = [
            (
                r#"Readiness probe failed: Get "http://192.0.2.7:8080/ready?tenant=acme&plan=gold#top": dial tcp 192.0.2.7:8080: connect: connection refused"#,
                r#"Readiness probe failed: Get "http://<redacted:ip>:8080/ready?<redacted:query>#<redacted:fragment>": dial tcp <redacted:ip>:8080: connect: connection refused"#,
            ),
            (
                "GET https://api.example.com/v1/users?email=someone.else",
                "GET https://api.example.com/v1/users?<redacted:query>",
            ),
            (
                "open https://docs.example.com/guide#customer-acme-setup now",
                "open https://docs.example.com/guide#<redacted:fragment> now",
            ),
            (
                "fetched http://api.shop/items?page=2.",
                "fetched http://host-1/items?<redacted:query>.",
            ),
            (
                "(see https://example.com/a?b=c), then retry",
                "(see https://example.com/a?<redacted:query>), then retry",
            ),
            (
                "postgres://app:s3cret@db.example.com:5432/orders?sslmode=require",
                "postgres://<redacted:userinfo>@db.example.com:5432/orders?<redacted:query>",
            ),
            (
                "callback https://example.com/cb?state=eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJl",
                "callback https://example.com/cb?<redacted:query>",
            ),
            (
                r#"{\"url\":\"http://example.com/x?id=7\"}"#,
                r#"{\"url\":\"http://example.com/x?<redacted:query>\"}"#,
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(redact(input), expected, "{input}");
        }
        let mut r = Redactor::new();
        r.text("a http://example.com/a?x=1#y and https://example.com/b#z");
        assert_eq!(r.counts().get("R014"), Some(&2));
    }

    #[test]
    fn urls_without_a_query_or_fragment_stay() {
        let lines = [
            "is it up at https://example.com/health?",
            "see https://example.com/page# for more",
            "what? and #hashtag without a URL",
        ];
        for line in lines {
            let mut r = Redactor::new();
            assert_eq!(r.text(line), line, "{line}");
            assert_eq!(r.counts().get("R014"), None, "{line}");
        }
    }

    #[test]
    fn ipv6_next_to_colons_and_rust_paths() {
        assert_eq!(
            redact("dial 2001:db8::1: connection refused"),
            "dial <redacted:ip>: connection refused"
        );
        let paths = "thread panicked at std::io::Error and Foo::Bar::new()";
        assert_eq!(redact(paths), paths);
        assert_eq!(redact("listen [::1]:80"), "listen [<redacted:ip>]:80");
    }

    #[test]
    fn long_paths_keep_their_ordinary_segments() {
        let path = "GET /api/v1/namespaces/default/pods/web-0/status 404";
        assert_eq!(redact(path), path);
        assert_eq!(
            redact("GET /files/Zq3x9Lm2Pw8Rt5Vb1Nc7Hd4Kf6Jg0Ys2Ua9/raw"),
            "GET /files/<redacted:secret>/raw"
        );
    }

    #[test]
    fn literal_placeholders_from_the_cluster_cannot_be_restored() {
        let mut r = Redactor::new();
        r.name(NameKind::Pod, "billing-0");
        let sent = r.text("an attacker writes pod-1 and Deployment-2 in a log");
        assert_eq!(sent, "an attacker writes pod_1 and Deployment_2 in a log");
        // A real name that looks like a placeholder is still replaced.
        let mut r = Redactor::new();
        r.name(NameKind::Pod, "job-3");
        assert_eq!(r.text("job-3 failed"), "pod-1 failed");
        assert_eq!(r.restore("pod-1 failed"), "job-3 failed");
    }

    #[test]
    fn restore_is_one_pass() {
        let mut r = Redactor::new();
        r.name(NameKind::Pod, "pod-2");
        r.name(NameKind::Pod, "web");
        // pod-1 is the real `pod-2`, pod-2 the real `web`: restoring pod-1 must not chain.
        assert_eq!(r.restore("pod-1 then pod-2"), "pod-2 then web");
    }

    #[test]
    fn counts_say_what_each_rule_replaced() {
        let mut r = Redactor::new();
        r.text("a 192.0.2.1 b 192.0.2.2 mail x@example.com");
        assert_eq!(r.counts().get("R004"), Some(&2));
        assert_eq!(r.counts().get("R005"), Some(&1));
    }

    #[test]
    fn a_volume_named_like_a_secret_key_does_not_hide_the_name_after_its_kind() {
        // The kubelet's `FailedMount` note: a volume named after a secret key made `R010` take the
        // kind for the key's value, so `R003` no longer saw `secret "<name>"` and the name stayed.
        for volume in ["credentials", "api-token", "password", "auth", "db_secret"] {
            for kind in ["secret", "configmap", "Secret", "serviceaccount"] {
                let line = format!(
                    r#"MountVolume.SetUp failed for volume "{volume}" : {kind} "kausa-no-such-object" not found"#
                );
                for pass in [Pass::Request, Pass::Answer] {
                    let mut r = Redactor::new();
                    let out = match pass {
                        Pass::Request => r.text(&line),
                        Pass::Answer => r.answer(&line),
                    };
                    // The messages name the case only, never the text (CodeQL
                    // rust/cleartext-logging).
                    assert_eq!(
                        out,
                        format!(
                            r#"MountVolume.SetUp failed for volume "{volume}" : {kind} "<redacted:name>" not found"#
                        ),
                        "{volume} {kind}"
                    );
                    let counts = match pass {
                        Pass::Request => r.counts(),
                        Pass::Answer => r.answer_counts(),
                    };
                    assert_eq!(counts.get("R003"), Some(&1), "{volume} {kind}");
                    assert_eq!(counts.get("R010"), None, "{volume} {kind}");
                }
            }
        }
    }

    #[test]
    fn a_kind_word_is_still_a_value_without_a_quoted_name_and_a_quoted_name_hides_nothing() {
        for (line, expected) in [
            ("password=secret", "password=<redacted:value>"),
            ("token: configmap,", "token: <redacted:value>,"),
            (
                r#"password: hunter2 "next""#,
                r#"password: <redacted:value> "next""#,
            ),
            (
                r#"token="a" "password=xyz""#,
                r#"token="<redacted:value>" "password=<redacted:value>""#,
            ),
        ] {
            assert_eq!(redact(line), expected);
        }
    }

    #[test]
    fn redaction_is_deterministic() {
        let line = "db.shop.svc.cluster.local at 192.0.2.10 with token=abc";
        assert_eq!(redact(line), redact(line));
    }

    // Acceptance tests for notifications (docs/adr/0015-notifications.md), written before the
    // code. They use `Redactor::restore_only(&self, text: &str, names: &[&str]) -> String`, which
    // maps a placeholder back only when its real name is one of `names`.

    /// A redactor with the names `context::build` registers for a Deployment `web` in `shop`:
    /// its Pod, container, node, service account and private image.
    fn incident_redactor() -> Redactor {
        let mut r = Redactor::new();
        assert_eq!(r.name(NameKind::Namespace, "shop"), "namespace-1");
        assert_eq!(
            r.name(NameKind::Object("Deployment"), "web"),
            "deployment-1"
        );
        assert_eq!(r.name(NameKind::Pod, "web-7d9c8b6f4d-2xkqp"), "pod-1");
        assert_eq!(r.name(NameKind::Container, "server"), "container-1");
        assert_eq!(r.name(NameKind::Node, "worker-7"), "node-1");
        assert_eq!(
            r.name(NameKind::ServiceAccount, "web-runner"),
            "serviceaccount-1"
        );
        assert_eq!(
            r.image("registry.example.com/team/web:1.4.2"),
            "registry-1/image-1:1.4.2"
        );
        r
    }

    const ANSWER: &str = "deployment-1 in namespace-1 crashes: pod-1 on node-1 runs container-1 \
                          as serviceaccount-1 from registry-1/image-1:1.4.2";

    #[test]
    fn restore_only_maps_back_the_namespace_and_the_owner_and_nothing_else() {
        let r = incident_redactor();
        assert_eq!(
            r.restore_only(ANSWER, &["shop", "web"]),
            "web in shop crashes: pod-1 on node-1 runs container-1 as serviceaccount-1 from \
             registry-1/image-1:1.4.2"
        );
        // The full restore, for the Event and the log, maps every name back.
        assert_eq!(
            r.restore(ANSWER),
            "web in shop crashes: web-7d9c8b6f4d-2xkqp on worker-7 runs server as web-runner \
             from registry.example.com/team/web:1.4.2"
        );
    }

    #[test]
    fn restore_only_maps_capitalized_placeholders_back_too() {
        let r = incident_redactor();
        assert_eq!(
            r.restore_only(
                "Deployment-1 in Namespace-1; Pod-1 on Node-1",
                &["shop", "web"]
            ),
            "web in shop; Pod-1 on Node-1"
        );
    }

    #[test]
    fn restore_only_maps_a_bare_pod_owner_back() {
        let mut r = Redactor::new();
        r.name(NameKind::Namespace, "shop");
        assert_eq!(r.name(NameKind::Object("Pod"), "solo"), "pod-1");
        assert_eq!(r.name(NameKind::Pod, "solo"), "pod-1");
        assert_eq!(r.name(NameKind::Container, "main"), "container-1");
        assert_eq!(r.name(NameKind::Node, "worker-7"), "node-1");
        assert_eq!(
            r.restore_only(
                "Pod-1 in namespace-1 on node-1: container-1 exits",
                &["shop", "solo"]
            ),
            "solo in shop on node-1: container-1 exits"
        );
    }

    #[test]
    fn restore_only_maps_a_container_named_like_the_owner_to_the_owner() {
        let mut r = Redactor::new();
        r.name(NameKind::Namespace, "shop");
        r.name(NameKind::Object("Deployment"), "web");
        // The same real name gets the same placeholder.
        assert_eq!(r.name(NameKind::Container, "web"), "deployment-1");
        assert_eq!(
            r.restore_only("container deployment-1 of deployment-1", &["shop", "web"]),
            "container web of web"
        );
    }

    #[test]
    fn restore_only_leaves_every_other_placeholder() {
        let r = incident_redactor();
        assert_eq!(r.restore_only(ANSWER, &[]), ANSWER);
        assert_eq!(r.restore_only(ANSWER, &["not-registered"]), ANSWER);
        // Unknown placeholders and the markers of other rules stay.
        assert_eq!(
            r.restore_only(
                "pod-9, host-1 and <redacted:ip> in namespace-1",
                &["shop", "web"]
            ),
            "pod-9, host-1 and <redacted:ip> in shop"
        );
        // A name that is registered but not listed stays a placeholder.
        assert_eq!(
            r.restore_only(ANSWER, &["shop", "web", "worker-7"]),
            "web in shop crashes: pod-1 on worker-7 runs container-1 as serviceaccount-1 from \
             registry-1/image-1:1.4.2"
        );
    }

    #[test]
    fn restore_only_is_one_pass() {
        let mut r = Redactor::new();
        // A namespace whose real name looks like a node's placeholder.
        assert_eq!(r.name(NameKind::Namespace, "node-1"), "namespace-1");
        r.name(NameKind::Object("Deployment"), "web");
        assert_eq!(r.name(NameKind::Node, "worker-7"), "node-1");
        assert_eq!(
            r.restore_only("namespace-1 and node-1", &["node-1", "web"]),
            "node-1 and node-1"
        );
    }

    // Acceptance tests for the names in the notes of `D011`, `D012` and `D013` (new cases of
    // `R003`), written before the code. The controllers write the API server's own errors into
    // these events: a refused Pod (`pods "<name>" is forbidden`, with the generated name of a Pod
    // that never existed), the quota that refused it (`exceeded quota: <name>`, in
    // `staging/src/k8s.io/apiserver/pkg/admission/plugin/resourcequota/controller.go`), the
    // containers Pod Security admission names (`container "<name>"`, `containers "<a>", "<b>"`,
    // in `staging/src/k8s.io/pod-security-admission/policy/`), a missing object named with its
    // API group (`deployments.apps "<name>" not found`), and the StorageClass an external
    // provisioner names (`with StorageClass "<name>"`). Names, quantities and numbers invented.

    #[test]
    fn names_in_the_errors_of_controllers_are_redacted() {
        for (line, leaked, kept) in [
            (
                "Error creating: pods \"web-7d9c8b6f4d-d98v6\" is forbidden: exceeded quota: \
                 compute-budget, requested: pods=1, used: pods=3, limited: pods=3",
                &["web-7d9c8b6f4d-d98v6", "d98v6", "compute-budget"][..],
                &[
                    "Error creating: pods \"<redacted:name>\" is forbidden: exceeded quota: ",
                    ", requested: pods=1, used: pods=3, limited: pods=3",
                ][..],
            ),
            (
                "Create Pod db-0 in StatefulSet db failed error: pods \"db-0\" is forbidden: \
                 exceeded quota: compute-budget, requested: pods=1, used: pods=3, limited: pods=3",
                &["\"db-0\"", "compute-budget"][..],
                &["pods \"<redacted:name>\" is forbidden: exceeded quota: "][..],
            ),
            (
                "Error creating: pods \"agent-x7k2p\" is forbidden: violates PodSecurity \
                 \"restricted:latest\": privileged (containers \"collector\", \"shipper\" must \
                 not set securityContext.privileged=true), runAsNonRoot != true (pod or \
                 container \"collector\" must set securityContext.runAsNonRoot=true)",
                &["agent-x7k2p", "collector", "shipper"][..],
                &[
                    "is forbidden: violates PodSecurity \"restricted:latest\": privileged (",
                    " must not set securityContext.privileged=true), runAsNonRoot != true (pod or ",
                    " must set securityContext.runAsNonRoot=true)",
                ][..],
            ),
            (
                r#"deployments.apps "storefront" not found"#,
                &["storefront"][..],
                &[r#"deployments.apps "<redacted:name>" not found"#][..],
            ),
            (
                r#"storageclass.storage.k8s.io "fast-ssd" not found"#,
                &["fast-ssd"][..],
                &[r#"storageclass.storage.k8s.io "<redacted:name>" not found"#][..],
            ),
            (
                "failed to provision volume with StorageClass \"fast-ssd\": NodePath only \
                 supports ReadWriteOnce and ReadWriteOncePod (1.22+) access modes",
                &["fast-ssd"][..],
                &[
                    "failed to provision volume with StorageClass \"<redacted:name>\": NodePath \
                     only supports ReadWriteOnce and ReadWriteOncePod (1.22+) access modes",
                ][..],
            ),
        ] {
            let mut r = Redactor::new();
            let out = r.text(line);
            for name in leaked {
                assert!(!out.contains(name), "{name} in {out}");
            }
            for words in kept {
                assert!(out.contains(words), "{words:?} not in {out}");
            }
            assert!(r.counts().contains_key("R003"), "{out}");
        }
    }

    #[test]
    fn the_words_around_names_in_errors_stay() {
        // The same words with no name after them, and the metrics API's own resource names.
        for line in [
            "the server could not find the requested resource (get pods.metrics.k8s.io)",
            "invalid metrics (1 invalid out of 1), first error is: failed to get cpu resource \
             metric value",
            "0/3 nodes are available: pod has unbound immediate PersistentVolumeClaims.",
        ] {
            assert_eq!(redact(line), line, "{line}");
        }
    }

    #[test]
    fn a_quote_left_open_after_a_new_kind_does_not_hide_a_name_caught_before() {
        // Invariant 4: before the names of `D011` to `D013`, `secret "<name>"` was caught in these
        // lines; an unclosed quote after `pod`, `pods` or a dotted word must not swallow it.
        for (case, (line, name)) in [
            (r#"pod "y secret "topsecret-x" not found"#, "topsecret-x"),
            (r#"see pods "a", "b secret "topsecret-y""#, "topsecret-y"),
            (r#"app.conf "z, secret "topsecret-z""#, "topsecret-z"),
        ]
        .into_iter()
        .enumerate()
        {
            let mut r = Redactor::new();
            let out = r.text(line);
            // The messages name the case only, never the text (CodeQL rust/cleartext-logging).
            assert!(
                !out.contains(name),
                "case {case}: the name was not redacted"
            );
            assert!(
                out.contains(r#"secret "<redacted:name>""#),
                "case {case}: no redacted name"
            );
        }
    }

    // Acceptance tests for the names in the Job controller's `podFailurePolicy` notes (a new case
    // of `R003`), written before the code. A D008 incident gathers no Pod, so the container and
    // the Pod these notes name are no registered names (pkg/controller/job/pod_failure_policy.go:57
    // and :74 at v1.36.4). Names invented.

    #[test]
    fn names_in_job_pod_failure_policy_notes_are_redacted() {
        for (case, (line, expected, replaced)) in [
            (
                "Container exporter for pod shop/report-29000000-x7k2p failed with exit code 42 \
                 matching FailJob rule at index 0",
                "Container <redacted:name> for pod namespace-1/<redacted:name> failed with exit \
                 code 42 matching FailJob rule at index 0",
                2,
            ),
            (
                "Container db-migrate for pod shop/migrate-x7k2p failed with exit code 2 \
                 matching Ignore rule at index 3",
                "Container <redacted:name> for pod namespace-1/<redacted:name> failed with exit \
                 code 2 matching Ignore rule at index 3",
                2,
            ),
            (
                "Pod shop/migrate-q9w8e has condition DisruptionTarget matching FailJob rule at \
                 index 1",
                "Pod namespace-1/<redacted:name> has condition DisruptionTarget matching FailJob \
                 rule at index 1",
                1,
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let mut r = Redactor::new();
            r.name(NameKind::Namespace, "shop");
            let out = r.text(line);
            // The messages name the case only, never the text (CodeQL rust/cleartext-logging).
            assert!(out == expected, "case {case}: not the expected redaction");
            assert_eq!(r.counts().get("R003"), Some(&replaced), "case {case}");
        }
        // With no name registered, the container and the Pod still do not pass.
        let out = redact(
            "Container exporter for pod shop/report-29000000-x7k2p failed with exit code 42 \
             matching FailJob rule at index 0",
        );
        for (case, name) in ["exporter", "report-29000000", "x7k2p"].iter().enumerate() {
            assert!(!out.contains(name), "case {case}: a name was not redacted");
        }
        assert!(
            out.ends_with(" failed with exit code 42 matching FailJob rule at index 0"),
            "the words after the names changed"
        );
    }

    #[test]
    fn the_lines_around_pod_failure_policy_notes_stay() {
        // The kubelet's `ErrImageNeverPull` message (its image is the `R002` case of an image),
        // the Job controller's fixed notes, and other lines that start with `Container` or `Pod`.
        for line in [
            "Container image \"busybox:x\" is not present with pull policy of Never",
            "Job has failed indexes",
            "Job has exceeded the specified maximal number of failed indexes",
            "Job has reached the specified backoff limit",
            "Job was active longer than specified deadline",
            "Container started",
            "Container exited with exit code 1",
            "Pod sandbox changed, it will be killed and re-created.",
            "Pod was active on the node longer than the specified deadline",
        ] {
            assert_eq!(redact(line), line, "{line}");
        }
    }

    #[test]
    fn every_counted_id_is_in_the_rules_reference() {
        // The ids this file counts: its string literals `"R…"` before the tests. `R001` is the
        // allowlist of context.rs, which no redactor counts.
        let source = include_str!("redact.rs");
        let code = &source[..source.find("#[cfg(test)]").unwrap()];
        let mut ids: std::collections::BTreeSet<_> = code
            .split('"')
            .filter(|s| {
                s.len() == 4 && s.starts_with('R') && s[1..].bytes().all(|b| b.is_ascii_digit())
            })
            .collect();
        ids.insert("R001");
        // The id in backquotes at the start of each row of the redaction table, in order.
        let documented: Vec<_> = include_str!("../docs/reference/rules.md")
            .lines()
            .filter_map(|line| line.strip_prefix("| `")?.split_once('`'))
            .map(|(id, _)| id)
            .filter(|id| id.starts_with('R'))
            .collect();
        assert_eq!(documented, ids.into_iter().collect::<Vec<_>>());
    }

    // Acceptance tests for docs/adr/0022-redact-the-answer.md, written before the code. They use
    // `ANSWER_RULES: [&str; 12]`, `Redactor::answer(&mut self, text: &str) -> String` and
    // `Redactor::answer_counts(&self) -> &BTreeMap<&'static str, usize>`: the pattern rules of
    // `text`, in the same order, without the neutralization of placeholders and without `R002`,
    // counted apart from the request. Values are invented: the canary `zq7canary`, documentation
    // addresses, `example.org` and tokens of zeros. Failure messages name the case, never the text
    // (CodeQL rust/cleartext-logging).

    use std::collections::BTreeSet;

    /// What the answer pass of `r` counted, by rule.
    fn answer_counted(r: &Redactor) -> Vec<(&'static str, usize)> {
        r.answer_counts()
            .iter()
            .map(|(rule, n)| (*rule, *n))
            .collect()
    }

    /// A rule, an answer text, what the answer pass of a fresh redactor makes of it, and what it
    /// counts.
    type RuleCase = (
        &'static str,
        &'static str,
        &'static str,
        &'static [(&'static str, usize)],
    );

    /// One answer text per rule of `ANSWER_RULES`.
    const ONE_PER_RULE: [RuleCase; 12] = [
        (
            "R009",
            "key -----BEGIN EXAMPLE KEY----- Tm90IGEga2V5 -----END EXAMPLE KEY----- end",
            "key <redacted:pem> end",
            &[("R009", 1)],
        ),
        (
            "R014",
            "open https://zq7canary.internal/x?y=1",
            "open https://host-1/x?<redacted:query>",
            &[("R013", 1), ("R014", 1)],
        ),
        (
            "R007",
            "Authorization: Bearer abcdefgh12345678",
            "Authorization: <redacted:authorization>",
            &[("R007", 1)],
        ),
        (
            "R008",
            "jwt eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ4In0.c2lnbmF0dXJl ok",
            "jwt <redacted:jwt> ok",
            &[("R008", 1)],
        ),
        (
            "R006",
            "postgres://u:p@db:5432",
            "postgres://<redacted:userinfo>@db:5432",
            &[("R006", 1)],
        ),
        (
            "R010",
            "password=x",
            "password=<redacted:value>",
            &[("R010", 1)],
        ),
        (
            "R011",
            "use ghp_000000000000000000000000000000000000 now",
            "use <redacted:token> now",
            &[("R011", 1)],
        ),
        (
            "R005",
            "mail root@example.org",
            "mail <redacted:email>",
            &[("R005", 1)],
        ),
        (
            "R004",
            "192.0.2.7 and 2001:db8::7",
            "<redacted:ip> and <redacted:ip>",
            &[("R004", 2)],
        ),
        (
            "R003",
            r#"secret "real" not found"#,
            r#"secret "<redacted:name>" not found"#,
            &[("R003", 1)],
        ),
        (
            "R013",
            "dial zq7canary.internal",
            "dial host-1",
            &[("R013", 1)],
        ),
        (
            "R012",
            "hash 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b",
            "hash <redacted:secret>",
            &[("R012", 1)],
        ),
    ];

    #[test]
    fn the_answer_pass_runs_every_pattern_rule() {
        let mut covered = BTreeSet::new();
        for (rule, input, expected, counted) in ONE_PER_RULE {
            let mut r = Redactor::new();
            assert_eq!(r.answer(input), expected, "{rule}");
            assert_eq!(answer_counted(&r), counted.to_vec(), "{rule}");
            assert!(r.counts().is_empty(), "{rule}: counted as a request");
            covered.insert(rule);
        }
        assert_eq!(covered, ANSWER_RULES.into_iter().collect::<BTreeSet<_>>());
        // After a request, the answers leave the request's counts as they were.
        let mut r = incident_redactor();
        r.text("web at 192.0.2.1 mails x@example.com");
        let request = r.counts().clone();
        assert!(!request.is_empty());
        for (_, input, _, _) in ONE_PER_RULE {
            r.answer(input);
        }
        assert_eq!(r.counts(), &request);
        assert_eq!(r.answer_counts().get("R004"), Some(&2));
    }

    #[test]
    fn answer_rules_match_the_request_chain() {
        assert_eq!(
            ANSWER_RULES,
            [
                "R009", "R014", "R007", "R008", "R006", "R010", "R011", "R005", "R004", "R003",
                "R013", "R012",
            ]
        );
        let listed: BTreeSet<&str> = ANSWER_RULES.into_iter().collect();
        assert_eq!(listed.len(), ANSWER_RULES.len(), "an id is listed twice");
        assert!(!listed.contains("R001") && !listed.contains("R002"));
        // Every id the code counts, minus `R002`, runs on answers: a rule added to the chain
        // must join `ANSWER_RULES` (docs/how-to/add-a-rule.md).
        let source = include_str!("redact.rs");
        let code = &source[..source.find("#[cfg(test)]").unwrap()];
        let mut in_code: BTreeSet<&str> = code
            .split('"')
            .filter(|s| {
                s.len() == 4 && s.starts_with('R') && s[1..].bytes().all(|b| b.is_ascii_digit())
            })
            .collect();
        assert!(in_code.remove("R002"));
        assert_eq!(in_code, listed);
        // And every listed id is one that `text` counts: one line per rule, and a known name.
        let mut r = Redactor::new();
        r.name(NameKind::Object("Deployment"), "storefront");
        let mut lines: Vec<&str> = ONE_PER_RULE.iter().map(|(_, input, _, _)| *input).collect();
        lines.push("storefront restarts");
        r.text(&lines.join("\n"));
        let mut counted: BTreeSet<&str> = r.counts().keys().copied().collect();
        assert!(counted.remove("R002"), "R002 not counted on the request");
        assert_eq!(counted, listed);
    }

    #[test]
    fn echoes_of_kausas_output_are_kept_and_not_counted() {
        const MARKERS: [&str; 13] = [
            "<redacted:pem>",
            "<redacted:authorization>",
            "<redacted:cookie>",
            "<redacted:jwt>",
            "<redacted:userinfo>",
            "<redacted:value>",
            "<redacted:token>",
            "<redacted:email>",
            "<redacted:ip>",
            "<redacted:name>",
            "<redacted:secret>",
            "<redacted:query>",
            "<redacted:fragment>",
        ];
        let mut echoes: Vec<String> = Vec::new();
        for marker in MARKERS {
            echoes.push(marker.to_owned());
            echoes.push(format!("{marker} was rejected; rotate it"));
            echoes.push(format!("the value \"{marker}\" in the log"));
            echoes.push(format!("the value '{marker}' in the log"));
        }
        for echo in [
            // Each marker where the request writes it.
            r#"Error: secret "<redacted:name>" not found"#,
            r#"secret '<redacted:name>' not found"#,
            r#"pods "<redacted:name>" is forbidden"#,
            "password: <redacted:value>",
            "the token: <redacted:value> expired",
            "run --token <redacted:value> --verbose",
            "Authorization: <redacted:authorization>",
            "Cookie: <redacted:cookie>",
            "https://host-1/x?<redacted:query>",
            "https://host-1/x?<redacted:query>#<redacted:fragment>",
            "GET /orders?<redacted:query> HTTP/1.1",
            "host-1:5432",
            "<redacted:ip>:8080",
            "<redacted:token> was rejected; rotate it",
            "<redacted:secret> in the log",
            "check host-1 and host-2, then host-12",
            // Lines of this file's request corpus, as the request pass leaves them.
            r#"Readiness probe failed: Get "http://<redacted:ip>:8080/ready?<redacted:query>#<redacted:fragment>": dial tcp <redacted:ip>:8080: connect: connection refused"#,
            "GET https://api.example.com/v1/users?<redacted:query>",
            "fetched http://host-1/items?<redacted:query>.",
            "connecting to postgres://<redacted:userinfo>@db:5432/shop",
            r#"{"api_key": "<redacted:value>", "user": "x"}"#,
            r#"{"body":"{\"user\":\"admin\",\"password\":\"<redacted:value>\"}"}"#,
            "MYSQL_PWD=<redacted:value> REDIS_PASS=<redacted:value>",
            "client_secret%3D<redacted:value>&x=1",
            "run --password <redacted:value> --verbose",
            "jwt <redacted:jwt> ok",
            "using <redacted:token> for git",
            "mail <redacted:email> now",
            "connect ECONNREFUSED <redacted:ip>:5432",
            "listen on [<redacted:ip>]:8080",
            "dial <redacted:ip>: connection refused",
            "dial tcp: lookup host-1: no such host",
            "GET https://host-1/v1",
            "dial host-1:8080 failed",
            "https://host-1/c?<redacted:query>",
            "value=<redacted:secret> here",
            "GET /files/<redacted:secret>/raw",
            "couldn't find key password in Secret prod/<redacted:name>",
            r#"forbidden: User "system:serviceaccount:prod:<redacted:name>" cannot list"#,
            "Error creating: pods \"<redacted:name>\" is forbidden: exceeded quota: \
             <redacted:name>, requested: pods=1, used: pods=3, limited: pods=3",
            r#"privileged (containers "<redacted:name>", "<redacted:name>" must not set it)"#,
            r#"deployments.apps "<redacted:name>" not found"#,
            r#"failed to provision volume with StorageClass "<redacted:name>": NodePath"#,
            "GET /orders?<redacted:query> HTTP/1.1 500",
            // Left out on purpose, interactions of the chain that exist on requests too
            // (docs/adr/0022-redact-the-answer.md, Context): `SERVICE_ACCOUNT_USER` after a marker
            // (`system:serviceaccount:<redacted:secret>:x`), an IPv6 prefix before an `R012`
            // marker (`2001:db8::<redacted:secret>`) and `R007`'s quoted cookie across unbalanced
            // quotes. `R007`'s header form after a marker is the next test.
        ] {
            echoes.push(echo.to_owned());
        }
        for (case, echo) in echoes.iter().enumerate() {
            let mut r = incident_redactor();
            assert_eq!(&r.answer(echo), echo, "echo {case}");
            assert!(r.answer_counts().is_empty(), "echo {case}: counted");
        }
    }

    #[test]
    fn a_value_after_an_echoed_authorization_marker_is_still_redacted() {
        let mut r = Redactor::new();
        assert_eq!(
            r.answer("Authorization: <redacted:authorization> was rejected"),
            "Authorization: <redacted:authorization>"
        );
        assert_eq!(answer_counted(&r), [("R007", 1)]);
        let mut r = Redactor::new();
        assert_eq!(
            r.answer("Authorization: <redacted:authorization> abcdefgh12345678"),
            "Authorization: <redacted:authorization>"
        );
        assert_eq!(answer_counted(&r), [("R007", 1)]);
    }

    #[test]
    fn placeholders_in_any_case_survive_the_answer_and_come_back() {
        let pod = "web-7d9c8b6f4d-2xkqp";
        for (case, (answer, restored, narrow)) in [
            ("deployment-1", "web".to_owned(), "web".to_owned()),
            ("Deployment-1", "web".to_owned(), "web".to_owned()),
            ("pod-1", pod.to_owned(), "pod-1".to_owned()),
            (
                "Pod-1 restarts",
                format!("{pod} restarts"),
                "Pod-1 restarts".to_owned(),
            ),
            (
                r#"pods "Pod-1" is waiting"#,
                format!(r#"pods "{pod}" is waiting"#),
                r#"pods "Pod-1" is waiting"#.to_owned(),
            ),
            (
                r#"pods "pod-1" is waiting"#,
                format!(r#"pods "{pod}" is waiting"#),
                r#"pods "pod-1" is waiting"#.to_owned(),
            ),
            (
                r#"container "Container-1" of pods "pod-1""#,
                format!(r#"container "server" of pods "{pod}""#),
                r#"container "Container-1" of pods "pod-1""#.to_owned(),
            ),
            (
                "kubectl -n namespace-1 logs pod-1 --previous",
                format!("kubectl -n shop logs {pod} --previous"),
                "kubectl -n shop logs pod-1 --previous".to_owned(),
            ),
            (
                "Namespace-1 runs node-1 as serviceaccount-1",
                "shop runs worker-7 as web-runner".to_owned(),
                "shop runs node-1 as serviceaccount-1".to_owned(),
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let mut r = incident_redactor();
            let answered = r.answer(answer);
            assert_eq!(answered, answer, "case {case}");
            assert!(r.answer_counts().is_empty(), "case {case}: counted");
            assert_eq!(r.restore(&answered), restored, "case {case}");
            assert_eq!(
                r.restore_only(&answered, &["shop", "web"]),
                narrow,
                "case {case}"
            );
        }
    }

    #[test]
    fn real_names_are_not_replaced_in_answers() {
        let mut r = Redactor::new();
        r.name(NameKind::Namespace, "shop");
        assert_eq!(
            r.name(NameKind::Object("Deployment"), "api"),
            "deployment-1"
        );
        // Public image references are sent as they are, so the model may repeat them.
        assert_eq!(r.image("ghcr.io/example/api:v2"), "ghcr.io/example/api:v2");
        for (case, text) in [
            "check the API server",
            "the api answers slowly; check the Shop namespace's quota",
            "pull ghcr.io/example/api:v2 again",
            "run busybox:1.37 with a shell",
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(r.answer(text), text, "case {case}");
        }
        assert!(r.answer_counts().is_empty());
    }

    #[test]
    fn answer_hosts_continue_the_request_numbering() {
        let mut r = incident_redactor();
        assert_eq!(
            r.text("dial db.billing.svc.cluster.local:5432 failed"),
            "dial host-1:5432 failed"
        );
        let answered =
            r.answer("check db.billing.svc.cluster.local, zq7canary.internal and host-1");
        assert_eq!(answered, "check host-1, host-2 and host-1");
        assert_eq!(answer_counted(&r), [("R013", 2)]);
        assert_eq!(r.counts().get("R013"), Some(&1));
        // A host keeps its number in later answers of the incident.
        assert_eq!(
            r.answer("open https://zq7canary.internal/"),
            "open https://host-2/"
        );
        // `host-N` is never mapped back.
        assert_eq!(r.restore(&answered), answered);
        assert_eq!(r.restore_only(&answered, &["shop", "web"]), answered);
    }

    #[test]
    fn the_answer_pass_is_deterministic() {
        let answer = "deployment-1 calls https://zq7canary.internal/fix?token=abc from 192.0.2.7 \
                      and db.shop.svc.cluster.local; mail root@example.org";
        let run = || {
            let mut r = incident_redactor();
            let sent = r.text("dial db.shop.svc.cluster.local:5432 from 192.0.2.1");
            let answered = r.answer(answer);
            let again = r.answer(answer);
            (
                sent,
                answered,
                again,
                r.counts().clone(),
                r.answer_counts().clone(),
            )
        };
        let (sent, answered, again, request, counted) = run();
        assert_eq!(answered, again);
        assert_eq!((sent, answered, again, request, counted), run());
    }

    #[test]
    fn allowlisted_hosts_stay_in_answers() {
        for (case, (text, expected, counted)) in [
            (
                "see https://kubernetes.io/docs/x#probes",
                "see https://kubernetes.io/docs/x#<redacted:fragment>",
                &[("R014", 1)][..],
            ),
            (
                "see https://github.com/x/y?tab=readme and github.com/x/y",
                "see https://github.com/x/y?<redacted:query> and github.com/x/y",
                &[("R014", 1)][..],
            ),
            ("pull ghcr.io/x/y:1", "pull ghcr.io/x/y:1", &[][..]),
            (
                "read https://example.com/zq7canary/install.sh",
                "read https://example.com/zq7canary/install.sh",
                &[][..],
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let mut r = Redactor::new();
            assert_eq!(r.answer(text), expected, "case {case}");
            assert_eq!(answer_counted(&r), counted.to_vec(), "case {case}");
        }
    }

    #[test]
    fn the_injected_step_of_the_end_to_end_test_is_redacted() {
        // The same text and result as tests/e2e/fake_endpoint.py and tests/e2e/run.sh.
        let mut r = Redactor::new();
        assert_eq!(
            r.answer("open http://192.0.2.7:8080/fix?token=not-a-real-token on zq7canary.internal"),
            "open http://<redacted:ip>:8080/fix?<redacted:query> on host-1"
        );
        assert_eq!(answer_counted(&r), [("R004", 1), ("R013", 1), ("R014", 1)]);
        assert!(r.counts().is_empty());
    }

    #[test]
    fn request_lines_lose_their_query() {
        let redacted = [
            (
                "GET /orders?page=2 HTTP/1.1",
                "GET /orders?<redacted:query> HTTP/1.1",
            ),
            (
                r#""POST /login?next=/a#x""#,
                r#""POST /login?<redacted:query>#<redacted:fragment>""#,
            ),
            (
                "GET /api/v1/orders?page=2 200 12ms",
                "GET /api/v1/orders?<redacted:query> 200 12ms",
            ),
            ("HEAD /x#top", "HEAD /x#<redacted:fragment>"),
            (
                "GET /orders?customer=zq7canary-acme HTTP/1.1 500",
                "GET /orders?<redacted:query> HTTP/1.1 500",
            ),
            (
                "PUT\t/a/b?c=d, then retry",
                "PUT\t/a/b?<redacted:query>, then retry",
            ),
        ];
        let kept = [
            "GET /health",
            "GET /?",
            "GET /healthz?",
            "get /x?y=1",
            "Did you GET it?",
            "/tmp/file?.log",
            "is it up? GET /x.",
            "CONNECT /x?y=1",
        ];
        for (case, (input, expected)) in redacted.into_iter().enumerate() {
            let mut r = Redactor::new();
            assert_eq!(r.text(input), expected, "request {case}");
            assert_eq!(r.counts().get("R014"), Some(&1), "request {case}");
            let mut r = Redactor::new();
            assert_eq!(r.answer(input), expected, "answer {case}");
            assert_eq!(answer_counted(&r), [("R014", 1)], "answer {case}");
        }
        for (case, input) in kept.into_iter().enumerate() {
            let mut r = Redactor::new();
            assert_eq!(r.text(input), input, "request {case}");
            assert_eq!(r.counts().get("R014"), None, "request {case}");
            let mut r = Redactor::new();
            assert_eq!(r.answer(input), input, "answer {case}");
            assert!(r.answer_counts().is_empty(), "answer {case}");
        }
    }
}
