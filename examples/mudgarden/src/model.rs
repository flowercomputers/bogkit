use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde::Deserialize;
use serde_json::json;

use crate::commands::{self, Command};
use crate::content::GameContent;
use crate::domain::{
    AgentActionStatus, AgentActionStep, AgentActionStepKind, AgentActionTrace, AgentTurn,
};

const DEFAULT_MODEL: &str = "gpt-5.6-terra";
const DEFAULT_REASONING_EFFORT: &str = "medium";
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
const MAX_WORLD_QUERIES: usize = 4;

#[derive(Clone)]
pub struct ModelPlanner {
    client: Client,
    api_key: String,
    base_url: String,
    model: String,
    content: Arc<GameContent>,
}

#[derive(Debug)]
pub struct ModelPlan {
    pub command: Command,
    pub command_text: String,
    pub intention: String,
    pub response_id: String,
}

#[derive(Deserialize)]
struct ResponseEnvelope {
    id: String,
    output: Vec<ResponseItem>,
}

#[derive(Deserialize)]
struct ResponseItem {
    #[serde(rename = "type")]
    kind: String,
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct ActionArguments {
    command: String,
    intention: String,
}

#[derive(Deserialize)]
struct QueryArguments {
    command: String,
    rationale: String,
}

#[derive(Debug)]
enum PlannerChoice {
    Act {
        command: Command,
        command_text: String,
        intention: String,
        response_id: String,
    },
    Query {
        command_text: String,
        rationale: String,
        response_id: String,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RequiredResponse {
    Speech,
    Knock,
}

#[derive(serde::Serialize)]
struct QueryObservation {
    command: String,
    rationale: String,
    result: String,
}

impl ModelPlanner {
    pub fn from_env() -> Result<Option<Self>> {
        Self::from_env_with_content(GameContent::bundled())
    }

    pub fn from_env_with_content(content: Arc<GameContent>) -> Result<Option<Self>> {
        let Some(api_key) = std::env::var("OPENAI_API_KEY")
            .ok()
            .filter(|key| !key.trim().is_empty())
        else {
            return Ok(None);
        };
        let base_url = std::env::var("OPENAI_BASE_URL")
            .unwrap_or_else(|_| DEFAULT_BASE_URL.to_string())
            .trim_end_matches('/')
            .to_string();
        let model =
            std::env::var("MUDGARDEN_AGENT_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());
        let timeout_seconds = std::env::var("MUDGARDEN_MODEL_TIMEOUT_SECONDS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(30);
        let client = Client::builder()
            .timeout(Duration::from_secs(timeout_seconds.max(1)))
            .build()
            .context("could not construct model HTTP client")?;
        Ok(Some(Self {
            client,
            api_key,
            base_url,
            model,
            content,
        }))
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn new_trace(&self, id: u64, turn: &AgentTurn) -> Result<AgentActionTrace> {
        Ok(AgentActionTrace {
            id,
            actor_id: turn.actor_id,
            actor_name: turn.name.clone(),
            model: self.model.clone(),
            started_at_unix_ms: unix_ms(),
            completed_at_unix_ms: None,
            status: AgentActionStatus::Failed,
            instructions: agent_instructions(&self.content, turn)?,
            context: turn.clone(),
            steps: Vec::new(),
            final_command: None,
            final_intention: None,
            response_id: None,
            execution_output: Vec::new(),
            error: None,
        })
    }

    pub async fn plan<F, Fut>(
        &self,
        turn: &AgentTurn,
        trace: &mut AgentActionTrace,
        mut query_world: F,
    ) -> Result<ModelPlan>
    where
        F: FnMut(String) -> Fut,
        Fut: Future<Output = Result<Vec<String>, String>>,
    {
        let mut observations = Vec::<QueryObservation>::new();
        let required_response = if !turn.triggering_knocks.is_empty() {
            Some(RequiredResponse::Knock)
        } else if !turn.triggering_speech.is_empty() {
            Some(RequiredResponse::Speech)
        } else {
            None
        };
        loop {
            let force_action = observations.len() >= MAX_WORLD_QUERIES;
            let action_required = force_action || required_response.is_some();
            let input = serde_json::to_string_pretty(&json!({
                "initial_context": turn,
                "world_query_history": observations,
                "next_step": if required_response == Some(RequiredResponse::Knock) {
                    "Respond to the triggering knock now with one admit command."
                } else if required_response == Some(RequiredResponse::Speech) {
                    "Respond to the triggering speech now with one say command."
                } else if force_action {
                    "Choose an action now. The world-query budget is exhausted."
                } else {
                    "Either query the live world for missing information or choose an action."
                }
            }))?;
            trace.steps.push(AgentActionStep {
                kind: AgentActionStepKind::ModelRequest,
                label: if action_required {
                    "Model request · action required".to_string()
                } else {
                    "Model request · query or act".to_string()
                },
                rationale: None,
                command: None,
                result: None,
                response_id: None,
                input: Some(input.clone()),
            });

            match self
                .request_choice(
                    &trace.instructions,
                    &input,
                    action_required,
                    required_response,
                )
                .await?
            {
                PlannerChoice::Act {
                    command,
                    command_text,
                    intention,
                    response_id,
                } => {
                    match required_response {
                        Some(RequiredResponse::Speech) if !matches!(command, Command::Say(_)) => {
                            bail!("model did not answer triggering speech with a say command");
                        }
                        Some(RequiredResponse::Knock) if !matches!(command, Command::Admit(_)) => {
                            bail!("model did not answer triggering knock with an admit command");
                        }
                        _ => {}
                    }
                    trace.steps.push(AgentActionStep {
                        kind: AgentActionStepKind::Action,
                        label: "Action selected".to_string(),
                        rationale: Some(intention.clone()),
                        command: Some(command_text.clone()),
                        result: None,
                        response_id: Some(response_id.clone()),
                        input: None,
                    });
                    trace.final_command = Some(command_text.clone());
                    trace.final_intention = Some(intention.clone());
                    trace.response_id = Some(response_id.clone());
                    return Ok(ModelPlan {
                        command,
                        command_text,
                        intention,
                        response_id,
                    });
                }
                PlannerChoice::Query {
                    command_text,
                    rationale,
                    response_id,
                } => {
                    if force_action {
                        bail!("model queried the world after its query budget was exhausted");
                    }
                    let result = match query_world(command_text.clone()).await {
                        Ok(lines) => lines.join("\n"),
                        Err(error) => format!("Query rejected: {error}"),
                    };
                    trace.steps.push(AgentActionStep {
                        kind: AgentActionStepKind::WorldQuery,
                        label: "Live world query".to_string(),
                        rationale: Some(rationale.clone()),
                        command: Some(command_text.clone()),
                        result: Some(result.clone()),
                        response_id: Some(response_id),
                        input: None,
                    });
                    observations.push(QueryObservation {
                        command: command_text,
                        rationale,
                        result,
                    });
                }
            }
        }
    }

    async fn request_choice(
        &self,
        instructions: &str,
        input: &str,
        force_action: bool,
        required_response: Option<RequiredResponse>,
    ) -> Result<PlannerChoice> {
        let command_schema = match required_response {
            Some(RequiredResponse::Speech) => json!({
                "type": "string",
                "description": "A direct in-character reply using exactly `say <message>`.",
                "pattern": "^say\\s+.+"
            }),
            Some(RequiredResponse::Knock) => json!({
                "type": "string",
                "description": "Open the garden gate for the visitor using exactly `admit <person>`.",
                "pattern": "^admit\\s+.+"
            }),
            None => json!({
                "type": "string",
                "description": self.content.dialogue.command_description
            }),
        };
        let action_tool = json!({
            "type": "function",
            "name": "act_in_mudgarden",
            "description": self.content.dialogue.tool_description,
            "strict": true,
            "parameters": {
                "type": "object",
                "properties": {
                    "command": command_schema,
                    "intention": {
                        "type": "string",
                        "description": self.content.dialogue.intention_description
                    }
                },
                "required": ["command", "intention"],
                "additionalProperties": false
            }
        });
        let query_tool = json!({
            "type": "function",
            "name": "query_mudgarden",
            "description": "Read current live world state through a non-mutating observation command.",
            "strict": true,
            "parameters": {
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "A read-only command: look, garden, gardens, inspect, inventory, weather, bog, survey, or who."
                    },
                    "rationale": {
                        "type": "string",
                        "description": "A concise explanation of what missing fact this query will resolve."
                    }
                },
                "required": ["command", "rationale"],
                "additionalProperties": false
            }
        });
        let tools = if force_action {
            vec![action_tool]
        } else {
            vec![action_tool, query_tool]
        };
        let tool_choice = if force_action {
            json!({ "type": "function", "name": "act_in_mudgarden" })
        } else {
            json!("auto")
        };
        let body = json!({
            "model": self.model,
            "instructions": instructions,
            "input": input,
            "reasoning": { "effort": DEFAULT_REASONING_EFFORT },
            "max_output_tokens": 320,
            "store": false,
            "parallel_tool_calls": false,
            "tools": tools,
            "tool_choice": tool_choice,
            "metadata": {
                "application": "mudgarden"
            }
        });

        let response = self
            .client
            .post(format!("{}/responses", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .context("model request failed")?;
        let status = response.status();
        let response_body = response
            .text()
            .await
            .context("model response was unreadable")?;
        if !status.is_success() {
            let message = response_body.chars().take(600).collect::<String>();
            bail!("model returned {status}: {message}");
        }
        parse_choice_with_content(&response_body, &self.content)
    }
}

fn parse_choice_with_content(response_body: &str, content: &GameContent) -> Result<PlannerChoice> {
    let envelope: ResponseEnvelope =
        serde_json::from_str(response_body).context("model response had an unknown shape")?;
    let call = envelope
        .output
        .into_iter()
        .find(|item| {
            item.kind == "function_call"
                && matches!(
                    item.name.as_deref(),
                    Some("act_in_mudgarden" | "query_mudgarden")
                )
        })
        .context("model did not choose a MUDGarden tool")?;
    let arguments = call
        .arguments
        .as_deref()
        .context("model tool call had no arguments")?;
    match call.name.as_deref() {
        Some("act_in_mudgarden") => {
            let arguments: ActionArguments =
                serde_json::from_str(arguments).context("model action arguments were invalid")?;
            let command_text = arguments.command.trim().to_string();
            let command = commands::parse_with_content(&command_text, content)
                .map_err(anyhow::Error::msg)
                .context("model proposed an invalid command")?;
            if matches!(command, Command::Quit | Command::Changes | Command::Help) {
                bail!("model proposed a command unavailable to world residents");
            }
            Ok(PlannerChoice::Act {
                command,
                command_text,
                intention: arguments.intention.trim().to_string(),
                response_id: envelope.id,
            })
        }
        Some("query_mudgarden") => {
            let arguments: QueryArguments =
                serde_json::from_str(arguments).context("model query arguments were invalid")?;
            let command_text = arguments.command.trim().to_string();
            if command_text.is_empty() {
                bail!("model proposed an empty world query");
            }
            Ok(PlannerChoice::Query {
                command_text,
                rationale: arguments.rationale.trim().to_string(),
                response_id: envelope.id,
            })
        }
        _ => unreachable!("recognized tool names are filtered above"),
    }
}

fn agent_instructions(content: &GameContent, turn: &AgentTurn) -> Result<String> {
    let npc = content
        .npc_for_actor(&turn.npc_id, &turn.name)
        .with_context(|| format!("no dialogue profile is configured for {}", turn.name))?;
    let speech_policy = if turn.triggering_speech.is_empty() {
        String::new()
    } else {
        let recent_speech = if turn.recent_speech.is_empty() {
            "none".to_string()
        } else {
            turn.recent_speech.join(" | ")
        };
        format!(
            "\nSpeech-response policy:\n- A human in your current room just spoke to the room: {}\n- Recent lines you already said: {recent_speech}\n- Respond now with one `say <message>` command that directly addresses the actual content of the human's speech.\n- If several lines triggered this turn, prioritize any substantive or unresolved statement; do not let a greeting displace it.\n- Acknowledge a reported loss, setback, correction, or complaint before returning to your own interests.\n- Do not repeat a recent greeting, observation, sentence frame, or topic unless you add a genuinely new fact. If you already greeted the speaker, continue the conversation instead of greeting them again.\n- Stay in character, be concise, and do not choose a non-speech action for this turn.",
            turn.triggering_speech.join(" | "),
        )
    };
    let knock_policy = if turn.triggering_knocks.is_empty() {
        String::new()
    } else {
        format!(
            "\nKnock-response policy:\n- A human just knocked at your garden gate: {}\n- Respond now with one `admit <person>` command naming the visitor who knocked.\n- Stay in character and do not choose another action for this turn.",
            turn.triggering_knocks.join(" | ")
        )
    };
    Ok(format!(
        "{}{speech_policy}{knock_policy}\nWorld-query policy:\n- You may use query_mudgarden to resolve missing live-world facts before acting.\n- Queries must be read-only observation commands and each query needs a concise rationale.\n- Your tool choices, rationales, supplied context, and query results are visible in the operator action trace.",
        content.dialogue_instructions(npc, &turn.goal)
    ))
}

fn unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_typed_model_action() {
        let response = json!({
            "id": "resp_test",
            "output": [{
                "type": "function_call",
                "name": "act_in_mudgarden",
                "arguments": "{\"command\":\"plant blue cornflower at C4 as rain-blue\",\"intention\":\"begin a blue flower experiment\"}"
            }]
        });
        let choice =
            parse_choice_with_content(&response.to_string(), &GameContent::bundled()).unwrap();
        let PlannerChoice::Act {
            command,
            command_text,
            response_id,
            ..
        } = choice
        else {
            panic!("expected an action");
        };
        assert_eq!(response_id, "resp_test");
        assert_eq!(command_text, "plant blue cornflower at C4 as rain-blue");
        assert_eq!(
            command,
            Command::Plant {
                species: "blue cornflower".to_string(),
                position: "C4".parse().unwrap(),
                name: Some("rain-blue".to_string())
            }
        );
    }

    #[test]
    fn parses_a_world_query() {
        let response = json!({
            "id": "resp_query",
            "output": [{
                "type": "function_call",
                "name": "query_mudgarden",
                "arguments": "{\"command\":\"survey 3 4\",\"rationale\":\"check the driest nearby cell\"}"
            }]
        });
        let choice =
            parse_choice_with_content(&response.to_string(), &GameContent::bundled()).unwrap();
        let PlannerChoice::Query {
            command_text,
            rationale,
            response_id,
        } = choice
        else {
            panic!("expected a query");
        };
        assert_eq!(command_text, "survey 3 4");
        assert_eq!(rationale, "check the driest nearby cell");
        assert_eq!(response_id, "resp_query");
    }

    #[test]
    fn rejects_non_world_commands() {
        let response = json!({
            "id": "resp_test",
            "output": [{
                "type": "function_call",
                "name": "act_in_mudgarden",
                "arguments": "{\"command\":\"quit\",\"intention\":\"leave\"}"
            }]
        });
        assert!(
            parse_choice_with_content(&response.to_string(), &GameContent::bundled())
                .unwrap_err()
                .to_string()
                .contains("unavailable")
        );
    }

    #[test]
    fn speech_triggered_turns_require_an_immediate_spoken_reply() {
        let dir = tempfile::tempdir().unwrap();
        let mut world = crate::store::World::open(dir.path());
        let ivo = world
            .ensure_world_agents()
            .unwrap()
            .into_iter()
            .find(|actor| actor.name == "Ivo")
            .unwrap();
        world.tick().unwrap();
        let mut turn = world.prepare_due_agent_turns().unwrap();
        let mut turn = turn
            .drain(..)
            .find(|candidate| candidate.actor_id == ivo.id)
            .unwrap();
        turn.recent_speech =
            vec!["Ivo says, “Hello, Daniel. The cornflower is holding.”".to_string()];
        turn.triggering_speech = vec![
            "Daniel says, “mine died”".to_string(),
            "Daniel says, “hello”".to_string(),
        ];

        let instructions = agent_instructions(&GameContent::bundled(), &turn).unwrap();

        assert!(instructions.contains("Respond now with one `say <message>` command"));
        assert!(instructions.contains("Daniel says, “mine died”"));
        assert!(instructions.contains("Recent lines you already said"));
        assert!(instructions.contains("do not let a greeting displace it"));
        assert!(instructions.contains("Acknowledge a reported loss"));
        assert!(instructions.contains("continue the conversation instead of greeting"));
        assert!(instructions.contains("do not choose a non-speech action"));
    }

    #[test]
    fn knock_triggered_turns_require_admitting_the_visitor() {
        let dir = tempfile::tempdir().unwrap();
        let mut world = crate::store::World::open(dir.path());
        let ivo = world
            .ensure_world_agents()
            .unwrap()
            .into_iter()
            .find(|actor| actor.name == "Ivo")
            .unwrap();
        world.tick().unwrap();
        let mut turn = world.prepare_due_agent_turns().unwrap();
        let mut turn = turn
            .drain(..)
            .find(|candidate| candidate.actor_id == ivo.id)
            .unwrap();
        turn.triggering_knocks = vec!["Daniel knocks at Ivo's garden gate.".to_string()];

        let instructions = agent_instructions(&GameContent::bundled(), &turn).unwrap();

        assert!(instructions.contains("Respond now with one `admit <person>` command"));
        assert!(instructions.contains("Daniel knocks at Ivo's garden gate."));
        assert!(instructions.contains("do not choose another action"));
    }
}
