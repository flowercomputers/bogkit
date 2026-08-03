use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, BufRead, Write};
use std::path::Path;

use fold::pipeline::terminal;
use fold::stream::Stream;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Command {
    Artifact {
        id: String,
    },
    Edge {
        artifact: String,
        dependency: String,
    },
    Release {
        id: String,
        artifact: String,
    },
    Revoke {
        id: String,
    },
    Query {
        release: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
enum Fact {
    Artifact {
        id: String,
    },
    Edge {
        artifact: String,
        dependency: String,
    },
    Release {
        id: String,
        artifact: String,
    },
    Revoke {
        id: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Engine {
    Candidate,
    Reference,
}

impl Engine {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "candidate" => Ok(Self::Candidate),
            "reference" => Ok(Self::Reference),
            _ => Err(format!(
                "unknown engine {value:?}; use candidate or reference"
            )),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Candidate => "one_hop_negative_control",
            Self::Reference => "slow_reference",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct Decision {
    decision: &'static str,
    reason: &'static str,
    path: Vec<String>,
}

impl Decision {
    fn approved() -> Self {
        Self {
            decision: "approved",
            reason: "complete",
            path: Vec::new(),
        }
    }

    fn blocked(reason: &'static str, path: Vec<String>) -> Self {
        Self {
            decision: "blocked",
            reason,
            path,
        }
    }
}

#[derive(Debug, Serialize)]
struct Output<'a> {
    engine: &'static str,
    release: &'a str,
    #[serde(flatten)]
    decision: Decision,
}

#[derive(Default)]
struct Model {
    artifacts: BTreeSet<String>,
    edges: BTreeMap<String, BTreeSet<String>>,
    releases: BTreeMap<String, String>,
    revoked: BTreeSet<String>,
}

impl Model {
    fn add(&mut self, fact: Fact) {
        match fact {
            Fact::Artifact { id } => {
                self.artifacts.insert(id);
            }
            Fact::Edge {
                artifact,
                dependency,
            } => {
                self.edges.entry(artifact).or_default().insert(dependency);
            }
            Fact::Release { id, artifact } => {
                self.releases.insert(id, artifact);
            }
            Fact::Revoke { id } => {
                self.revoked.insert(id);
            }
        }
    }

    /// A deliberately narrow model of what the documented built-ins can
    /// express without a cross-stream join or recursive fixed point. It
    /// checks the release artifact and one edge hop only.
    fn candidate_decision(&self, release: &str) -> Decision {
        let Some(root) = self.releases.get(release) else {
            return Decision::blocked("missing_release", vec![release.to_string()]);
        };
        if !self.artifacts.contains(root) {
            return Decision::blocked("missing_manifest", vec![root.clone()]);
        }
        if self.revoked.contains(root) {
            return Decision::blocked("revoked", vec![root.clone()]);
        }

        for dependency in self.edges.get(root).into_iter().flatten() {
            if dependency == root {
                return Decision::blocked("invalid_cycle", vec![root.clone(), dependency.clone()]);
            }
            if !self.artifacts.contains(dependency) {
                return Decision::blocked(
                    "missing_manifest",
                    vec![root.clone(), dependency.clone()],
                );
            }
            if self.revoked.contains(dependency) {
                return Decision::blocked("revoked", vec![root.clone(), dependency.clone()]);
            }
        }

        Decision::approved()
    }

    fn reference_decision(&self, release: &str) -> Decision {
        let Some(root) = self.releases.get(release) else {
            return Decision::blocked("missing_release", vec![release.to_string()]);
        };
        let mut stack = Vec::new();
        let mut complete = BTreeSet::new();
        self.visit(root, &mut stack, &mut complete)
            .unwrap_or_else(Decision::approved)
    }

    fn visit(
        &self,
        artifact: &str,
        stack: &mut Vec<String>,
        complete: &mut BTreeSet<String>,
    ) -> Option<Decision> {
        if let Some(position) = stack.iter().position(|item| item == artifact) {
            let mut path = stack.clone();
            path.push(artifact.to_string());
            debug_assert!(position < path.len());
            return Some(Decision::blocked("invalid_cycle", path));
        }
        if complete.contains(artifact) {
            return None;
        }

        stack.push(artifact.to_string());
        if !self.artifacts.contains(artifact) {
            return Some(Decision::blocked("missing_manifest", stack.clone()));
        }
        if self.revoked.contains(artifact) {
            return Some(Decision::blocked("revoked", stack.clone()));
        }

        for dependency in self.edges.get(artifact).into_iter().flatten() {
            if let Some(decision) = self.visit(dependency, stack, complete) {
                return Some(decision);
            }
        }
        let removed = stack.pop();
        debug_assert_eq!(removed.as_deref(), Some(artifact));
        complete.insert(artifact.to_string());
        None
    }
}

fn fact_from_command(command: Command) -> Result<Fact, String> {
    match command {
        Command::Artifact { id } => Ok(Fact::Artifact { id }),
        Command::Edge {
            artifact,
            dependency,
        } => Ok(Fact::Edge {
            artifact,
            dependency,
        }),
        Command::Release { id, artifact } => Ok(Fact::Release { id, artifact }),
        Command::Revoke { id } => Ok(Fact::Revoke { id }),
        Command::Query { .. } => Err("query is not a persistent fact".to_string()),
    }
}

fn load_model(stream: &Stream<Fact, terminal::Bag<Fact>>) -> Model {
    stream.rtx(|facts| {
        let mut model = Model::default();
        for (fact, multiplicity) in facts.iter() {
            if multiplicity > 0 {
                model.add(fact);
            }
        }
        model
    })
}

fn crash_spec() -> Option<(String, usize)> {
    let value = std::env::var("PROVENANCE_CRASH").ok()?;
    let (phase, number) = value.split_once(':')?;
    let number = number.parse().ok()?;
    Some((phase.to_string(), number))
}

fn run(state_path: &Path, engine: Engine) -> Result<(), String> {
    let mut stream = Stream::new(state_path, terminal::Bag::<Fact>::new("facts"));
    let stdin = io::stdin();
    let mut stdout = io::BufWriter::new(io::stdout().lock());
    let crash = crash_spec();
    let mut fact_number = 0usize;

    for (line_number, line) in stdin.lock().lines().enumerate() {
        let line = line.map_err(|error| format!("read line {}: {error}", line_number + 1))?;
        if line.trim().is_empty() {
            continue;
        }
        let command: Command = serde_json::from_str(&line)
            .map_err(|error| format!("parse line {}: {error}", line_number + 1))?;
        match command {
            Command::Query { release } => {
                let model = load_model(&stream);
                let decision = match engine {
                    Engine::Candidate => model.candidate_decision(&release),
                    Engine::Reference => model.reference_decision(&release),
                };
                serde_json::to_writer(
                    &mut stdout,
                    &Output {
                        engine: engine.name(),
                        release: &release,
                        decision,
                    },
                )
                .map_err(|error| format!("write decision: {error}"))?;
                writeln!(stdout).map_err(|error| format!("write newline: {error}"))?;
                stdout
                    .flush()
                    .map_err(|error| format!("flush output: {error}"))?;
            }
            persistent => {
                fact_number += 1;
                let fact = fact_from_command(persistent)?;
                stream.wtx(|tx| {
                    tx.insert(&fact);
                    if crash.as_ref() == Some(&("before_commit".to_string(), fact_number)) {
                        panic!("injected crash before commit at fact {fact_number}");
                    }
                });
                if crash.as_ref() == Some(&("after_commit".to_string(), fact_number)) {
                    panic!("injected crash after commit at fact {fact_number}");
                }
            }
        }
    }
    Ok(())
}

fn generate() -> Result<(), String> {
    const CORPUS: &[&str] = &[
        r#"{"op":"artifact","id":"app"}"#,
        r#"{"op":"artifact","id":"middle"}"#,
        r#"{"op":"artifact","id":"revoked-base"}"#,
        r#"{"op":"artifact","id":"cycle-a"}"#,
        r#"{"op":"artifact","id":"cycle-b"}"#,
        r#"{"op":"edge","artifact":"app","dependency":"middle"}"#,
        r#"{"op":"edge","artifact":"app","dependency":"middle"}"#,
        r#"{"op":"edge","artifact":"middle","dependency":"revoked-base"}"#,
        r#"{"op":"edge","artifact":"cycle-a","dependency":"cycle-b"}"#,
        r#"{"op":"edge","artifact":"cycle-b","dependency":"cycle-a"}"#,
        r#"{"op":"release","id":"transitive","artifact":"app"}"#,
        r#"{"op":"release","id":"cyclic","artifact":"cycle-a"}"#,
        r#"{"op":"release","id":"unknown","artifact":"missing-root"}"#,
        r#"{"op":"revoke","id":"revoked-base"}"#,
        r#"{"op":"query","release":"transitive"}"#,
        r#"{"op":"query","release":"cyclic"}"#,
        r#"{"op":"query","release":"unknown"}"#,
    ];
    let mut stdout = io::BufWriter::new(io::stdout().lock());
    for line in CORPUS {
        writeln!(stdout, "{line}").map_err(|error| format!("write corpus: {error}"))?;
    }
    Ok(())
}

fn usage(program: &str) -> String {
    format!("usage:\n  {program} generate\n  {program} run <state-directory> <candidate|reference>")
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let result = match args.as_slice() {
        [_, command] if command == "generate" => generate(),
        [_, command, path, engine] if command == "run" => {
            Engine::parse(engine).and_then(|engine| run(Path::new(path), engine))
        }
        [program, ..] => Err(usage(program)),
        [] => Err("missing program name".to_string()),
    };
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(facts: &[Fact]) -> Model {
        let mut model = Model::default();
        for fact in facts {
            model.add(fact.clone());
        }
        model
    }

    fn artifact(id: &str) -> Fact {
        Fact::Artifact { id: id.to_string() }
    }

    fn edge(artifact: &str, dependency: &str) -> Fact {
        Fact::Edge {
            artifact: artifact.to_string(),
            dependency: dependency.to_string(),
        }
    }

    fn release(id: &str, artifact: &str) -> Fact {
        Fact::Release {
            id: id.to_string(),
            artifact: artifact.to_string(),
        }
    }

    fn revoke(id: &str) -> Fact {
        Fact::Revoke { id: id.to_string() }
    }

    #[test]
    fn one_hop_candidate_misses_transitive_revocation() {
        let fixture = model(&[
            artifact("app"),
            artifact("middle"),
            artifact("base"),
            edge("app", "middle"),
            edge("middle", "base"),
            release("prod", "app"),
            revoke("base"),
        ]);
        assert_eq!(fixture.candidate_decision("prod"), Decision::approved());
        assert_eq!(
            fixture.reference_decision("prod"),
            Decision::blocked(
                "revoked",
                vec!["app".to_string(), "middle".to_string(), "base".to_string()]
            )
        );
    }

    #[test]
    fn reference_blocks_missing_transitive_manifest() {
        let fixture = model(&[
            artifact("app"),
            artifact("middle"),
            edge("app", "middle"),
            edge("middle", "absent"),
            release("prod", "app"),
        ]);
        assert_eq!(
            fixture.reference_decision("prod"),
            Decision::blocked(
                "missing_manifest",
                vec![
                    "app".to_string(),
                    "middle".to_string(),
                    "absent".to_string()
                ]
            )
        );
    }

    #[test]
    fn reference_blocks_cycle_and_reports_stable_path() {
        let fixture = model(&[
            artifact("app"),
            artifact("a"),
            artifact("b"),
            edge("app", "a"),
            edge("a", "b"),
            edge("b", "a"),
            release("prod", "app"),
        ]);
        assert_eq!(
            fixture.reference_decision("prod"),
            Decision::blocked(
                "invalid_cycle",
                vec![
                    "app".to_string(),
                    "a".to_string(),
                    "b".to_string(),
                    "a".to_string()
                ]
            )
        );
    }

    #[test]
    fn reference_is_independent_of_fact_order_and_duplicate_edges() {
        let mut facts = vec![
            artifact("app"),
            artifact("a"),
            artifact("z"),
            edge("app", "z"),
            edge("app", "a"),
            edge("app", "a"),
            release("prod", "app"),
            revoke("a"),
            revoke("z"),
        ];
        let forward = model(&facts).reference_decision("prod");
        facts.reverse();
        let reverse = model(&facts).reference_decision("prod");
        assert_eq!(forward, reverse);
        assert_eq!(
            forward,
            Decision::blocked("revoked", vec!["app".to_string(), "a".to_string()])
        );
    }

    #[test]
    fn reference_approves_complete_acyclic_graph() {
        let fixture = model(&[
            artifact("app"),
            artifact("base"),
            edge("app", "base"),
            release("prod", "app"),
        ]);
        assert_eq!(fixture.reference_decision("prod"), Decision::approved());
    }
}
