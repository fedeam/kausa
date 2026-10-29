//! Redaction: what never leaves the cluster (docs/reference/rules.md, "Redaction rules";
//! docs/adr/0008-data-leaving-the-cluster.md).
//!
//! A `Redactor` lives for one incident. It gives the names kausa knows stable placeholders, numbered
//! in order of appearance (`R002`), applies the patterns `R003` to `R013` to free text, counts what
//! each rule replaced, and maps the `R002` placeholders back in the model's answer. Everything is
//! deterministic: the same incident produces the same request.

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
    counts: BTreeMap<&'static str, usize>,
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
        self.count("R002");
        match tag {
            Some(tag) => format!("{registry}/{path}:{tag}"),
            None => format!("{registry}/{path}"),
        }
    }

    /// Redacts free text. First the secrets (`R009`, `R007`, `R008`, `R006`, `R010`, `R011`), so
    /// that a known name inside a secret's key cannot break the key apart; then the known names
    /// (`R002`); then addresses, Kubernetes names, hostnames and high-entropy strings (`R005`,
    /// `R004`, `R003`, `R013`, `R012`). Text that already looks like one of kausa's placeholders is
    /// neutralized first, so the answer cannot map it to a real name.
    pub fn text(&mut self, text: &str) -> String {
        let mut out = self.neutralize_placeholders(text);
        out = self.apply(&PEM, "R009", out, |_| "<redacted:pem>".into());
        out = self.apply(&AUTHORIZATION_HEADER, "R007", out, |c| {
            format!("{}<redacted:authorization>", &c[1])
        });
        out = self.apply(&COOKIE_HEADER, "R007", out, |c| {
            format!("{}<redacted:cookie>", &c[1])
        });
        out = self.apply(&AUTHORIZATION_SCHEME, "R007", out, |c| {
            format!("{} <redacted:authorization>", &c[1])
        });
        out = self.apply(&JWT, "R008", out, |_| "<redacted:jwt>".into());
        out = self.apply(&URL_USERINFO, "R006", out, |c| {
            format!("{}<redacted:userinfo>@", &c[1])
        });
        out = self.apply(&SECRET_KEY_VALUE, "R010", out, |c| {
            let value = &c[2];
            let marker = if value.starts_with("\\\"") {
                "\\\"<redacted:value>\\\""
            } else if value.starts_with('"') {
                "\"<redacted:value>\""
            } else if value.starts_with('\'') {
                "'<redacted:value>'"
            } else {
                "<redacted:value>"
            };
            format!("{}{marker}", &c[1])
        });
        out = self.apply(&SECRET_FLAG, "R010", out, |c| {
            format!("{} <redacted:value>", &c[1])
        });
        out = self.apply(&SECRET_ELEMENT, "R010", out, |c| {
            format!("{}<redacted:value>", &c[1])
        });
        out = self.apply(&KNOWN_TOKEN, "R011", out, |_| "<redacted:token>".into());
        out = self.replace_names(&out);
        out = self.apply(&EMAIL, "R005", out, |_| "<redacted:email>".into());
        out = self.apply(&IPV4, "R004", out, |_| "<redacted:ip>".into());
        out = self.ipv6(out);
        out = self.kubernetes_names(out);
        out = self.hostnames(out);
        self.high_entropy(out)
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

    /// How many values each rule replaced, by rule id.
    pub fn counts(&self) -> &BTreeMap<&'static str, usize> {
        &self.counts
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

    fn count(&mut self, rule: &'static str) {
        *self.counts.entry(rule).or_insert(0) += 1;
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
            if n > 0 {
                *self.counts.entry("R002").or_insert(0) += n;
            }
            out = replaced;
        }
        out
    }

    fn apply(
        &mut self,
        pattern: &Regex,
        rule: &'static str,
        text: String,
        replacement: impl Fn(&Captures<'_>) -> String,
    ) -> String {
        let mut n = 0;
        let out = pattern
            .replace_all(&text, |c: &Captures<'_>| {
                n += 1;
                replacement(c)
            })
            .into_owned();
        if n > 0 {
            *self.counts.entry(rule).or_insert(0) += n;
        }
        out
    }

    /// IPv6 addresses: a candidate must parse as one, after dropping the colons a message puts
    /// around it (`… 2001:db8::1: refused`), and must not touch a word (`Foo::Bar` stays).
    fn ipv6(&mut self, text: String) -> String {
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
        if n > 0 {
            *self.counts.entry("R004").or_insert(0) += n;
        }
        out
    }

    fn kubernetes_names(&mut self, text: String) -> String {
        let placeholders: Vec<String> = self.names.iter().map(|(_, p)| p.clone()).collect();
        let ours = |name: &str| placeholders.iter().any(|p| p == name);
        let mut n = 0;
        let mut out = KUBERNETES_NAME
            .replace_all(&text, |c: &Captures<'_>| {
                if ours(&c[2]) {
                    c[0].to_owned()
                } else {
                    n += 1;
                    format!("{} \"<redacted:name>\"", &c[1])
                }
            })
            .into_owned();
        out = KUBERNETES_NAMESPACED
            .replace_all(&out, |c: &Captures<'_>| {
                if ours(&c[3]) {
                    c[0].to_owned()
                } else {
                    n += 1;
                    format!("{}{}/<redacted:name>", &c[1], &c[2])
                }
            })
            .into_owned();
        out = SERVICE_ACCOUNT_USER
            .replace_all(&out, |c: &Captures<'_>| {
                if ours(&c[2]) {
                    c[0].to_owned()
                } else {
                    n += 1;
                    format!("{}<redacted:name>", &c[1])
                }
            })
            .into_owned();
        if n > 0 {
            *self.counts.entry("R003").or_insert(0) += n;
        }
        out
    }

    fn host_placeholder(&mut self, host: &str) -> Option<String> {
        let host = host.to_ascii_lowercase();
        let public = PUBLIC_HOSTS
            .iter()
            .chain(PUBLIC_REGISTRIES.iter())
            .any(|p| host == *p || host.ends_with(&format!(".{p}")));
        if public {
            return None;
        }
        self.count("R013");
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
    fn hostnames(&mut self, text: String) -> String {
        let mut out = self.replace_matches(&HOSTNAME, &text, 0);
        out = self.replace_matches(&URL_HOST, &out, 1);
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
            if let Some(placeholder) = self.host_placeholder(&host) {
                result.push_str(&out[last..start]);
                result.push_str(&placeholder);
                last = end;
            }
        }
        result.push_str(&out[last..]);
        result
    }

    /// Replaces capture group `group` of every match of `pattern` with a host placeholder.
    fn replace_matches(&mut self, pattern: &Regex, text: &str, group: usize) -> String {
        let mut out = String::with_capacity(text.len());
        let mut last = 0;
        let spans: Vec<(usize, usize)> = pattern
            .captures_iter(text)
            .filter_map(|c| c.get(group).map(|m| (m.start(), m.end())))
            .collect();
        for (start, end) in spans {
            if let Some(placeholder) = self.host_placeholder(&text[start..end]) {
                out.push_str(&text[last..start]);
                out.push_str(&placeholder);
                last = end;
            }
        }
        out.push_str(&text[last..]);
        out
    }

    fn high_entropy(&mut self, text: String) -> String {
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
        if n > 0 {
            *self.counts.entry("R012").or_insert(0) += n;
        }
        out
    }
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
static SECRET_KEY_VALUE: LazyLock<Regex> = LazyLock::new(|| {
    regex(&format!(
        r#"(?i)(\b[a-z0-9_.-]*{SECRET_KEYS}\b\\?["']?\s*(?:[:=]|%3[ad])\s*)(\\"(?:[^"\\]|\\[^"])*\\"|"(?:[^"\\]|\\.)*"|'[^']*'|[^\s"',;&}}<\\]+)"#
    ))
});
static SECRET_FLAG: LazyLock<Regex> = LazyLock::new(|| {
    regex(&format!(
        r#"(?i)(\s--?[a-z0-9_.-]*{SECRET_KEYS})\s+[^\s"'-][^\s"']*"#
    ))
});
static SECRET_ELEMENT: LazyLock<Regex> =
    LazyLock::new(|| regex(&format!(r"(?i)(<[a-z0-9_.:-]*{SECRET_KEYS}>)[^<]*")));
static KNOWN_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    regex(concat!(
        r"\b(?:",
        r"gh[pousr]_[A-Za-z0-9]{36}",
        r"|github_pat_[A-Za-z0-9_]{22,}",
        r"|glpat-[A-Za-z0-9_-]{20,}",
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
static KUBERNETES_NAME: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r#"(?i)\b(secrets?|configmaps?|persistentvolumeclaims?|pvc|serviceaccounts?|services?) ["']([^"']+)["']"#,
    )
});
static KUBERNETES_NAMESPACED: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?i)\b(secret\s+|configmap\s+)([a-z0-9.-]+)/([a-z0-9.-]+)"));
static SERVICE_ACCOUNT_USER: LazyLock<Regex> =
    LazyLock::new(|| regex(r#"(?i)(system:serviceaccount:[^:\s"']+:)([a-z0-9.-]+)"#));
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
                "https://host-1/c?sv=2020&sig=<redacted:value>",
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
}
