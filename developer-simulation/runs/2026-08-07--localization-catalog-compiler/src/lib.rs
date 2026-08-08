use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

pub const DEFAULT_LOCALE: &str = "en-US";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogSet {
    pub catalogs: BTreeMap<String, Catalog>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Catalog {
    pub path: PathBuf,
    pub locale: String,
    pub fallbacks: Vec<String>,
    pub fallback_line: usize,
    pub messages: BTreeMap<String, Message>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub node: Node,
    pub line: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub line: usize,
    pub kind: NodeKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeKind {
    Text(String),
    Ref {
        locale: String,
        id: String,
    },
    Plural {
        selector: String,
        branches: BTreeMap<String, Node>,
    },
    Select {
        selector: String,
        branches: BTreeMap<String, Node>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub path: PathBuf,
    pub locale: Option<String>,
    pub message_id: Option<String>,
    pub branch: Option<String>,
    pub line: usize,
    pub text: String,
}

impl Diagnostic {
    fn new(
        path: &Path,
        locale: Option<&str>,
        message_id: Option<&str>,
        branch: Option<String>,
        line: usize,
        text: impl Into<String>,
    ) -> Self {
        Self {
            path: path.to_path_buf(),
            locale: locale.map(str::to_owned),
            message_id: message_id.map(str::to_owned),
            branch,
            line,
            text: text.into(),
        }
    }

    pub fn render(&self) -> String {
        let locale = self.locale.as_deref().unwrap_or("-");
        let id = self.message_id.as_deref().unwrap_or("-");
        let branch = self.branch.as_deref().unwrap_or("-");
        format!(
            "error catalog={} message={} branch={} source={}:{}: {}",
            locale,
            id,
            branch,
            self.path.display(),
            self.line,
            self.text
        )
    }
}

pub fn load_dir(dir: &Path) -> Result<CatalogSet, Vec<Diagnostic>> {
    let mut paths = fs::read_dir(dir)
        .map_err(|error| {
            vec![Diagnostic::new(
                dir,
                None,
                None,
                None,
                0,
                format!("cannot read catalog directory: {error}"),
            )]
        })?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "cat"))
        .collect::<Vec<_>>();
    paths.sort();

    let mut catalogs = BTreeMap::new();
    let mut diagnostics = Vec::new();
    for path in paths {
        match parse_catalog(&path) {
            Ok((catalog, mut parse_diagnostics)) => {
                diagnostics.append(&mut parse_diagnostics);
                if catalogs.insert(catalog.locale.clone(), catalog).is_some() {
                    diagnostics.push(Diagnostic::new(
                        &path,
                        None,
                        None,
                        None,
                        1,
                        "duplicate locale catalog",
                    ));
                }
            }
            Err(mut parse_diagnostics) => diagnostics.append(&mut parse_diagnostics),
        }
    }
    if catalogs.is_empty() && diagnostics.is_empty() {
        diagnostics.push(Diagnostic::new(
            dir,
            None,
            None,
            None,
            0,
            "catalog directory contains no .cat files",
        ));
    }
    let set = CatalogSet { catalogs };
    diagnostics.extend(set.validate());
    if diagnostics.is_empty() {
        Ok(set)
    } else {
        Err(diagnostics)
    }
}

pub fn load_table(path: &Path) -> Result<CatalogSet, Vec<Diagnostic>> {
    let source = fs::read_to_string(path).map_err(|error| {
        vec![Diagnostic::new(
            path,
            None,
            None,
            None,
            0,
            format!("cannot read runtime table: {error}"),
        )]
    })?;
    let mut blocks = Vec::new();
    let mut current = String::new();
    for line in source.lines() {
        if line.trim_start().starts_with("locale ")
            && current
                .lines()
                .any(|line| line.trim_start().starts_with("locale "))
        {
            blocks.push(std::mem::take(&mut current));
        }
        current.push_str(line);
        current.push('\n');
    }
    if !current.trim().is_empty() {
        blocks.push(current);
    }

    let mut catalogs = BTreeMap::new();
    let mut diagnostics = Vec::new();
    for block in blocks {
        match parse_catalog_source(path, &block) {
            Ok((catalog, mut parse_diagnostics)) => {
                diagnostics.append(&mut parse_diagnostics);
                if catalogs.insert(catalog.locale.clone(), catalog).is_some() {
                    diagnostics.push(Diagnostic::new(
                        path,
                        None,
                        None,
                        None,
                        0,
                        "duplicate locale in runtime table",
                    ));
                }
            }
            Err(mut parse_diagnostics) => diagnostics.append(&mut parse_diagnostics),
        }
    }
    let set = CatalogSet { catalogs };
    diagnostics.extend(set.validate());
    if diagnostics.is_empty() {
        Ok(set)
    } else {
        Err(diagnostics)
    }
}

fn parse_catalog(path: &Path) -> Result<(Catalog, Vec<Diagnostic>), Vec<Diagnostic>> {
    let source = fs::read_to_string(path).map_err(|error| {
        vec![Diagnostic::new(
            path,
            None,
            None,
            None,
            0,
            format!("cannot read catalog: {error}"),
        )]
    })?;
    parse_catalog_source(path, &source)
}

fn parse_catalog_source(
    path: &Path,
    source: &str,
) -> Result<(Catalog, Vec<Diagnostic>), Vec<Diagnostic>> {
    let lines = source.lines().collect::<Vec<_>>();
    let mut diagnostics = Vec::new();
    let mut locale = None;
    let mut fallbacks = Vec::new();
    let mut fallback_line = 0;
    let mut messages = BTreeMap::new();
    let mut index = 0;

    while index < lines.len() {
        let line_number = index + 1;
        let line = lines[index].trim();
        index += 1;
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut words = line.split_whitespace();
        match words.next() {
            Some("locale") => {
                let value = words.next().unwrap_or("");
                if value.is_empty() || words.next().is_some() {
                    diagnostics.push(Diagnostic::new(
                        path,
                        None,
                        None,
                        None,
                        line_number,
                        "locale expects exactly one value",
                    ));
                } else if locale.replace(value.to_owned()).is_some() {
                    diagnostics.push(Diagnostic::new(
                        path,
                        Some(value),
                        None,
                        None,
                        line_number,
                        "duplicate locale header",
                    ));
                }
            }
            Some("fallback") => {
                let value = words.next().unwrap_or("");
                if words.next().is_some() {
                    diagnostics.push(Diagnostic::new(
                        path,
                        locale.as_deref(),
                        None,
                        None,
                        line_number,
                        "fallback values must be comma-separated",
                    ));
                } else if value != "-" {
                    fallbacks = value.split(',').map(str::to_owned).collect();
                }
                fallback_line = line_number;
            }
            Some("message") => {
                let id = words.next().unwrap_or("").to_owned();
                if id.is_empty() || words.next().is_some() {
                    diagnostics.push(Diagnostic::new(
                        path,
                        locale.as_deref(),
                        None,
                        None,
                        line_number,
                        "message expects exactly one identifier",
                    ));
                    continue;
                }
                let Some(node) = parse_node(
                    &lines,
                    &mut index,
                    path,
                    locale.as_deref(),
                    Some(&id),
                    &mut diagnostics,
                ) else {
                    continue;
                };
                match messages.entry(id) {
                    Entry::Occupied(entry) => diagnostics.push(Diagnostic::new(
                        path,
                        locale.as_deref(),
                        Some(entry.key()),
                        None,
                        line_number,
                        "duplicate message identifier",
                    )),
                    Entry::Vacant(entry) => {
                        entry.insert(Message {
                            line: line_number,
                            node,
                        });
                    }
                }
            }
            Some(keyword) => diagnostics.push(Diagnostic::new(
                path,
                locale.as_deref(),
                None,
                None,
                line_number,
                format!("unknown directive {keyword}"),
            )),
            None => {}
        }
    }

    let Some(locale) = locale else {
        diagnostics.push(Diagnostic::new(
            path,
            None,
            None,
            None,
            0,
            "catalog has no locale header",
        ));
        return Err(diagnostics);
    };
    Ok((
        Catalog {
            path: path.to_path_buf(),
            locale,
            fallbacks,
            fallback_line,
            messages,
        },
        diagnostics,
    ))
}

fn parse_node(
    lines: &[&str],
    index: &mut usize,
    path: &Path,
    locale: Option<&str>,
    message_id: Option<&str>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Node> {
    if *index >= lines.len() {
        diagnostics.push(Diagnostic::new(
            path,
            locale,
            message_id,
            None,
            lines.len(),
            "message has no value",
        ));
        return None;
    }
    let line_number = *index + 1;
    let line = lines[*index].trim();
    *index += 1;
    let mut words = line.split_whitespace();
    match words.next() {
        Some("text") => Some(Node {
            line: line_number,
            kind: NodeKind::Text(
                line.strip_prefix("text")
                    .unwrap_or("")
                    .trim_start()
                    .to_owned(),
            ),
        }),
        Some("ref") => {
            let target_locale = words.next().unwrap_or("");
            let target_id = words.next().unwrap_or("");
            if target_locale.is_empty() || target_id.is_empty() || words.next().is_some() {
                diagnostics.push(Diagnostic::new(
                    path,
                    locale,
                    message_id,
                    None,
                    line_number,
                    "ref expects a locale and message identifier",
                ));
                None
            } else {
                Some(Node {
                    line: line_number,
                    kind: NodeKind::Ref {
                        locale: target_locale.to_owned(),
                        id: target_id.to_owned(),
                    },
                })
            }
        }
        Some(kind @ ("plural" | "select")) => {
            let selector = words.next().unwrap_or("");
            if selector.is_empty() || words.next().is_some() {
                diagnostics.push(Diagnostic::new(
                    path,
                    locale,
                    message_id,
                    None,
                    line_number,
                    format!("{kind} expects exactly one selector"),
                ));
                return None;
            }
            let mut branches = BTreeMap::new();
            loop {
                if *index >= lines.len() {
                    diagnostics.push(Diagnostic::new(
                        path,
                        locale,
                        message_id,
                        None,
                        line_number,
                        format!("unterminated {kind} block"),
                    ));
                    break;
                }
                let branch_line_number = *index + 1;
                let branch_line = lines[*index].trim();
                if branch_line == "end" {
                    *index += 1;
                    break;
                }
                let mut branch_words = branch_line.split_whitespace();
                if branch_words.next() != Some("branch") {
                    diagnostics.push(Diagnostic::new(
                        path,
                        locale,
                        message_id,
                        None,
                        branch_line_number,
                        format!("{kind} expects branch <name> or end"),
                    ));
                    *index += 1;
                    continue;
                }
                let branch_name = branch_words.next().unwrap_or("").to_owned();
                *index += 1;
                if branch_name.is_empty() || branch_words.next().is_some() {
                    diagnostics.push(Diagnostic::new(
                        path,
                        locale,
                        message_id,
                        None,
                        branch_line_number,
                        "branch expects exactly one name",
                    ));
                    continue;
                }
                let Some(node) = parse_node(lines, index, path, locale, message_id, diagnostics)
                else {
                    continue;
                };
                if branches.insert(branch_name.clone(), node).is_some() {
                    diagnostics.push(Diagnostic::new(
                        path,
                        locale,
                        message_id,
                        Some(format!("{kind}/{branch_name}")),
                        branch_line_number,
                        "duplicate branch",
                    ));
                }
            }
            let kind = if kind == "plural" {
                NodeKind::Plural {
                    selector: selector.to_owned(),
                    branches,
                }
            } else {
                NodeKind::Select {
                    selector: selector.to_owned(),
                    branches,
                }
            };
            Some(Node {
                line: line_number,
                kind,
            })
        }
        Some("end") | Some("branch") | Some(_) | None => {
            diagnostics.push(Diagnostic::new(
                path,
                locale,
                message_id,
                None,
                line_number,
                "expected text, ref, plural, or select",
            ));
            None
        }
    }
}

impl CatalogSet {
    pub fn validate(&self) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        let Some(default) = self.catalogs.get(DEFAULT_LOCALE) else {
            diagnostics.push(Diagnostic::new(
                Path::new("<catalog-set>"),
                None,
                None,
                None,
                0,
                "missing en-US baseline catalog",
            ));
            return diagnostics;
        };

        for catalog in self.catalogs.values() {
            for fallback in &catalog.fallbacks {
                if !self.catalogs.contains_key(fallback) {
                    diagnostics.push(Diagnostic::new(
                        &catalog.path,
                        Some(&catalog.locale),
                        None,
                        None,
                        catalog.fallback_line,
                        format!("fallback locale {fallback} does not exist"),
                    ));
                }
                if fallback == &catalog.locale {
                    diagnostics.push(Diagnostic::new(
                        &catalog.path,
                        Some(&catalog.locale),
                        None,
                        None,
                        catalog.fallback_line,
                        "fallback chain references itself",
                    ));
                }
            }
            if let Some(cycle) = self.fallback_cycle(&catalog.locale) {
                diagnostics.push(Diagnostic::new(
                    &catalog.path,
                    Some(&catalog.locale),
                    None,
                    None,
                    catalog.fallback_line,
                    format!("fallback chain contains a cycle: {}", cycle.join(" -> ")),
                ));
            }
            for (id, message) in &catalog.messages {
                self.validate_refs(
                    &catalog.path,
                    &catalog.locale,
                    id,
                    &message.node,
                    &mut diagnostics,
                );
                self.validate_internal_shape(
                    &catalog.path,
                    &catalog.locale,
                    id,
                    &message.node,
                    None,
                    &mut diagnostics,
                );
            }
        }

        for catalog in self.catalogs.values() {
            if catalog.locale == DEFAULT_LOCALE {
                continue;
            }
            for (id, message) in &catalog.messages {
                let Some(base) = default.messages.get(id) else {
                    diagnostics.push(Diagnostic::new(
                        &catalog.path,
                        Some(&catalog.locale),
                        Some(id),
                        None,
                        message.line,
                        "message identifier is not present in en-US",
                    ));
                    continue;
                };
                self.compare_nodes(
                    &catalog.path,
                    &catalog.locale,
                    id,
                    &message.node,
                    &base.node,
                    None,
                    &mut diagnostics,
                );
            }
        }
        diagnostics
    }

    fn validate_internal_shape(
        &self,
        path: &Path,
        locale: &str,
        id: &str,
        node: &Node,
        branch: Option<String>,
        diagnostics: &mut Vec<Diagnostic>,
    ) {
        match &node.kind {
            NodeKind::Text(text) => {
                if let Err(error) = placeholders(text) {
                    diagnostics.push(Diagnostic::new(
                        path,
                        Some(locale),
                        Some(id),
                        branch,
                        node.line,
                        error,
                    ));
                }
            }
            NodeKind::Ref { .. } => {}
            NodeKind::Plural { selector, branches } | NodeKind::Select { selector, branches } => {
                if selector.is_empty() || branches.is_empty() {
                    diagnostics.push(Diagnostic::new(
                        path,
                        Some(locale),
                        Some(id),
                        branch.clone(),
                        node.line,
                        "branching message must have a selector and at least one branch",
                    ));
                }
                for (name, child) in branches {
                    self.validate_internal_shape(
                        path,
                        locale,
                        id,
                        child,
                        Some(join_branch(branch.as_deref(), selector, name)),
                        diagnostics,
                    );
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn compare_nodes(
        &self,
        path: &Path,
        locale: &str,
        id: &str,
        actual: &Node,
        expected: &Node,
        branch: Option<String>,
        diagnostics: &mut Vec<Diagnostic>,
    ) {
        match (&actual.kind, &expected.kind) {
            (NodeKind::Text(actual_text), NodeKind::Text(expected_text)) => {
                let actual_set = placeholders(actual_text).unwrap_or_default();
                let expected_set = placeholders(expected_text).unwrap_or_default();
                if actual_set != expected_set {
                    diagnostics.push(Diagnostic::new(
                        path,
                        Some(locale),
                        Some(id),
                        branch,
                        actual.line,
                        format!(
                            "placeholder mismatch: expected {:?}, found {:?}",
                            expected_set, actual_set
                        ),
                    ));
                }
            }
            (NodeKind::Ref { .. }, NodeKind::Ref { .. }) => {}
            (
                NodeKind::Plural {
                    selector: actual_selector,
                    branches: actual_branches,
                },
                NodeKind::Plural {
                    selector: expected_selector,
                    branches: expected_branches,
                },
            )
            | (
                NodeKind::Select {
                    selector: actual_selector,
                    branches: actual_branches,
                },
                NodeKind::Select {
                    selector: expected_selector,
                    branches: expected_branches,
                },
            ) => {
                if actual_selector != expected_selector {
                    diagnostics.push(Diagnostic::new(
                        path,
                        Some(locale),
                        Some(id),
                        branch.clone(),
                        actual.line,
                        format!(
                            "selector mismatch: expected {expected_selector}, found {actual_selector}"
                        ),
                    ));
                }
                for (name, expected_child) in expected_branches {
                    let child_branch = Some(join_branch(branch.as_deref(), actual_selector, name));
                    match actual_branches.get(name) {
                        Some(actual_child) => self.compare_nodes(
                            path,
                            locale,
                            id,
                            actual_child,
                            expected_child,
                            child_branch,
                            diagnostics,
                        ),
                        None => diagnostics.push(Diagnostic::new(
                            path,
                            Some(locale),
                            Some(id),
                            child_branch,
                            actual.line,
                            "missing branch",
                        )),
                    }
                }
                for name in actual_branches
                    .keys()
                    .filter(|name| !expected_branches.contains_key(*name))
                {
                    diagnostics.push(Diagnostic::new(
                        path,
                        Some(locale),
                        Some(id),
                        Some(join_branch(branch.as_deref(), actual_selector, name)),
                        actual.line,
                        "unexpected branch",
                    ));
                }
            }
            _ => diagnostics.push(Diagnostic::new(
                path,
                Some(locale),
                Some(id),
                branch,
                actual.line,
                "message shape differs from en-US",
            )),
        }
    }

    fn validate_refs(
        &self,
        path: &Path,
        locale: &str,
        id: &str,
        node: &Node,
        diagnostics: &mut Vec<Diagnostic>,
    ) {
        match &node.kind {
            NodeKind::Ref {
                locale: target_locale,
                id: target_id,
            } => {
                if !self.catalogs.contains_key(target_locale) {
                    diagnostics.push(Diagnostic::new(
                        path,
                        Some(locale),
                        Some(id),
                        None,
                        node.line,
                        format!("fallback reference targets missing locale {target_locale}"),
                    ));
                } else if !self.catalogs[target_locale]
                    .messages
                    .contains_key(target_id)
                {
                    diagnostics.push(Diagnostic::new(
                        path,
                        Some(locale),
                        Some(id),
                        None,
                        node.line,
                        format!(
                            "fallback reference targets missing message {target_locale}/{target_id}"
                        ),
                    ));
                }
            }
            NodeKind::Text(_) => {}
            NodeKind::Plural { branches, .. } | NodeKind::Select { branches, .. } => {
                for child in branches.values() {
                    self.validate_refs(path, locale, id, child, diagnostics);
                }
            }
        }
    }

    fn fallback_cycle(&self, start: &str) -> Option<Vec<String>> {
        fn visit(
            catalogs: &BTreeMap<String, Catalog>,
            current: &str,
            path: &mut Vec<String>,
            active: &mut HashMap<String, usize>,
            finished: &mut HashSet<String>,
        ) -> Option<Vec<String>> {
            if let Some(&at) = active.get(current) {
                return Some(path[at..].to_vec());
            }
            if finished.contains(current) {
                return None;
            }
            let catalog = catalogs.get(current)?;
            active.insert(current.to_owned(), path.len());
            path.push(current.to_owned());
            for fallback in &catalog.fallbacks {
                if catalogs.contains_key(fallback)
                    && let Some(cycle) = visit(catalogs, fallback, path, active, finished)
                {
                    return Some(cycle);
                }
            }
            path.pop();
            active.remove(current);
            finished.insert(current.to_owned());
            None
        }

        visit(
            &self.catalogs,
            start,
            &mut Vec::new(),
            &mut HashMap::new(),
            &mut HashSet::new(),
        )
    }

    pub fn emit_table(&self) -> String {
        let mut out = String::new();
        out.push_str("# catalog-compiler-prototype runtime table v1\n");
        for catalog in self.catalogs.values() {
            let fallbacks = if catalog.fallbacks.is_empty() {
                "-".to_owned()
            } else {
                catalog.fallbacks.join(",")
            };
            writeln!(out, "locale {}\nfallback {}", catalog.locale, fallbacks).unwrap();
            for (id, message) in &catalog.messages {
                writeln!(out, "message {id}").unwrap();
                emit_node(&mut out, &message.node, 0);
            }
        }
        out
    }

    pub fn lookup(
        &self,
        locale: &str,
        id: &str,
        vars: &HashMap<String, String>,
    ) -> Result<String, String> {
        let mut locale_seen = HashSet::new();
        self.lookup_locale(locale, id, vars, &mut locale_seen, &mut Vec::new())
    }

    fn lookup_locale(
        &self,
        locale: &str,
        id: &str,
        vars: &HashMap<String, String>,
        locale_seen: &mut HashSet<String>,
        refs_seen: &mut Vec<(String, String)>,
    ) -> Result<String, String> {
        if !locale_seen.insert(locale.to_owned()) {
            return Err(format!("fallback cycle while looking up {locale}/{id}"));
        }
        let result = if let Some(catalog) = self.catalogs.get(locale) {
            if let Some(message) = catalog.messages.get(id) {
                self.render_node(locale, id, &message.node, vars, locale_seen, refs_seen)
            } else if !catalog.fallbacks.is_empty() {
                let mut result = Err(format!("message not found: {locale}/{id}"));
                for fallback in &catalog.fallbacks {
                    match self.lookup_locale(fallback, id, vars, locale_seen, refs_seen) {
                        Ok(value) => {
                            result = Ok(value);
                            break;
                        }
                        Err(error) => result = Err(error),
                    }
                }
                result
            } else {
                Err(format!("message not found: {locale}/{id}"))
            }
        } else {
            Err(format!("locale not found: {locale}"))
        };
        locale_seen.remove(locale);
        result
    }

    fn render_node(
        &self,
        _locale: &str,
        _id: &str,
        node: &Node,
        vars: &HashMap<String, String>,
        locale_seen: &mut HashSet<String>,
        refs_seen: &mut Vec<(String, String)>,
    ) -> Result<String, String> {
        match &node.kind {
            NodeKind::Text(text) => substitute(text, vars),
            NodeKind::Ref {
                locale: target_locale,
                id: target_id,
            } => {
                let key = (target_locale.clone(), target_id.clone());
                if refs_seen.contains(&key) {
                    return Err(format!(
                        "fallback reference cycle at {target_locale}/{target_id}"
                    ));
                }
                refs_seen.push(key);
                let result =
                    self.lookup_locale(target_locale, target_id, vars, locale_seen, refs_seen);
                refs_seen.pop();
                result
            }
            NodeKind::Plural { selector, branches } => {
                let value = vars
                    .get(selector)
                    .ok_or_else(|| format!("missing variable {selector}"))?;
                let branch = if value == "1" { "one" } else { "other" };
                let child = branches
                    .get(branch)
                    .or_else(|| branches.get("other"))
                    .ok_or_else(|| format!("plural has no {branch} or other branch"))?;
                self.render_node("", "", child, vars, locale_seen, refs_seen)
            }
            NodeKind::Select { selector, branches } => {
                let value = vars
                    .get(selector)
                    .ok_or_else(|| format!("missing variable {selector}"))?;
                let child = branches
                    .get(value)
                    .or_else(|| branches.get("other"))
                    .ok_or_else(|| format!("select has no {value} or other branch"))?;
                self.render_node("", "", child, vars, locale_seen, refs_seen)
            }
        }
    }
}

fn join_branch(parent: Option<&str>, selector: &str, name: &str) -> String {
    match parent {
        Some(parent) => format!("{parent}/{selector}={name}"),
        None => format!("{selector}={name}"),
    }
}

fn placeholders(text: &str) -> Result<BTreeSet<String>, String> {
    let mut names = BTreeSet::new();
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            return Err("unclosed placeholder".to_owned());
        };
        let name = &after[..end];
        if name.is_empty()
            || name.contains(['{', '}', ','])
            || name.chars().any(char::is_whitespace)
        {
            return Err(format!("invalid placeholder {{{name}}}"));
        }
        names.insert(name.to_owned());
        rest = &after[end + 1..];
    }
    Ok(names)
}

fn emit_node(out: &mut String, node: &Node, depth: usize) {
    let indent = "  ".repeat(depth);
    match &node.kind {
        NodeKind::Text(text) => {
            writeln!(out, "{indent}text {text}").unwrap();
        }
        NodeKind::Ref { locale, id } => {
            writeln!(out, "{indent}ref {locale} {id}").unwrap();
        }
        NodeKind::Plural { selector, branches } => {
            writeln!(out, "{indent}plural {selector}").unwrap();
            for (name, child) in branches {
                writeln!(out, "{indent}branch {name}").unwrap();
                emit_node(out, child, depth + 1);
            }
            writeln!(out, "{indent}end").unwrap();
        }
        NodeKind::Select { selector, branches } => {
            writeln!(out, "{indent}select {selector}").unwrap();
            for (name, child) in branches {
                writeln!(out, "{indent}branch {name}").unwrap();
                emit_node(out, child, depth + 1);
            }
            writeln!(out, "{indent}end").unwrap();
        }
    }
}

fn substitute(text: &str, vars: &HashMap<String, String>) -> Result<String, String> {
    let mut result = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        result.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            return Err("unclosed placeholder".to_owned());
        };
        let name = &after[..end];
        let value = vars
            .get(name)
            .ok_or_else(|| format!("missing variable {name}"))?;
        result.push_str(value);
        rest = &after[end + 1..];
    }
    result.push_str(rest);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(name: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("catalog-prototype-{name}-{stamp}"));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn write(path: &Path, body: &str) {
        fs::write(path, body).unwrap();
    }

    #[test]
    fn valid_fixture_has_zero_diagnostics_and_preserves_fallback_lookup() {
        let dir = temp_dir("valid");
        write(
            &dir.join("en-US.cat"),
            "locale en-US\nfallback -\nmessage hello\ntext Hello {name}\nmessage shared\ntext Shared {name}\nmessage apples\nplural count\nbranch one\ntext One apple for {name}\nbranch other\ntext {count} apples for {name}\nend\nmessage nested\nref en-US shared\n",
        );
        write(
            &dir.join("fr.cat"),
            "locale fr\nfallback en-US\nmessage hello\ntext Bonjour {name}\nmessage apples\nplural count\nbranch one\ntext Une pomme pour {name}\nbranch other\ntext {count} pommes pour {name}\nend\nmessage nested\nref en-US shared\n",
        );
        let catalogs = load_dir(&dir).unwrap();
        let vars = HashMap::from([
            (String::from("name"), String::from("Ada")),
            (String::from("count"), String::from("2")),
        ]);
        assert_eq!(
            catalogs.lookup("fr", "hello", &vars).unwrap(),
            "Bonjour Ada"
        );
        assert_eq!(
            catalogs.lookup("fr", "shared", &vars).unwrap(),
            "Shared Ada"
        );
        assert_eq!(
            catalogs.lookup("fr", "apples", &vars).unwrap(),
            "2 pommes pour Ada"
        );
        let table_path = dir.join("runtime.table");
        fs::write(&table_path, catalogs.emit_table()).unwrap();
        let runtime = load_table(&table_path).unwrap();
        assert_eq!(runtime.lookup("fr", "shared", &vars).unwrap(), "Shared Ada");
    }

    #[test]
    fn invalid_fixture_reports_all_seeded_categories() {
        let dir = temp_dir("invalid");
        write(
            &dir.join("en-US.cat"),
            "locale en-US\nfallback -\nmessage apples\nplural count\nbranch one\ntext One apple\nbranch other\ntext {count} apples\nend\nmessage greeting\ntext Hello {name}\nmessage duplicate\ntext first\nmessage duplicate\ntext second\nmessage existing\ntext Existing\n",
        );
        write(
            &dir.join("fr.cat"),
            "locale fr\nfallback missing-locale,en-US\nmessage apples\nplural count\nbranch one\ntext Une pomme\nend\nmessage greeting\ntext Bonjour {username}\nmessage bad-ref\nref missing-locale absent\n",
        );
        let diagnostics = load_dir(&dir).unwrap_err();
        let output = diagnostics
            .iter()
            .map(Diagnostic::render)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(output.contains("missing branch"));
        assert!(output.contains("placeholder mismatch"));
        assert!(output.contains("fallback locale missing-locale does not exist"));
        assert!(output.contains("duplicate message identifier"));
        assert!(output.contains("fallback reference targets missing locale"));
    }

    #[test]
    fn emission_is_canonical() {
        let dir = temp_dir("order");
        write(
            &dir.join("z.cat"),
            "locale z\nfallback en-US\nmessage z\ntext z\nmessage a\ntext a\n",
        );
        write(
            &dir.join("en-US.cat"),
            "locale en-US\nfallback -\nmessage z\ntext z\nmessage a\ntext a\n",
        );
        let catalogs = load_dir(&dir).unwrap();
        let table = catalogs.emit_table();
        assert!(table.find("message a").unwrap() < table.find("message z").unwrap());
    }

    #[test]
    fn lookup_tries_each_fallback_before_failing() {
        let dir = temp_dir("fallback-list");
        write(
            &dir.join("en-US.cat"),
            "locale en-US\nfallback -\nmessage hello\ntext Hello {name}\n",
        );
        write(&dir.join("xx.cat"), "locale xx\nfallback -\n");
        write(&dir.join("fr.cat"), "locale fr\nfallback xx,en-US\n");
        let catalogs = load_dir(&dir).unwrap();
        let vars = HashMap::from([(String::from("name"), String::from("Ada"))]);
        assert_eq!(catalogs.lookup("fr", "hello", &vars).unwrap(), "Hello Ada");
    }
}
