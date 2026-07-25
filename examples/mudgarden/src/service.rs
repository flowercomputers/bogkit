use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::{broadcast, mpsc, oneshot, watch};

use crate::commands::Command;
use crate::content::GameContent;
use crate::domain::{
    ActorId, ActorState, AgentActionStatus, AgentActionStep, AgentActionStepKind, AgentActionTrace,
    AgentTurn, DebugSnapshot, WorldEvent, WorldOutput,
};
use crate::model::ModelPlanner;
use crate::store::World;

struct TickCycle {
    events: Vec<WorldEvent>,
    agent_turns: Vec<AgentTurn>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorldClockControl {
    pub paused: bool,
    pub tick_interval: Duration,
}

enum Request {
    EnsureHuman {
        name: String,
        fingerprint: Option<String>,
        reply: oneshot::Sender<Result<ActorState, String>>,
    },
    Execute {
        actor_id: ActorId,
        command: Command,
        reply: oneshot::Sender<Result<WorldOutput, String>>,
    },
    Tick {
        reply: oneshot::Sender<Result<TickCycle, String>>,
    },
    ExecuteAgentPlan {
        actor_id: ActorId,
        command: Command,
        intention: String,
        reply: oneshot::Sender<Result<WorldOutput, String>>,
    },
    QueryAgent {
        actor_id: ActorId,
        command: Command,
        reply: oneshot::Sender<Result<WorldOutput, String>>,
    },
    DebugSnapshot {
        reply: oneshot::Sender<DebugSnapshot>,
    },
    Shutdown {
        reply: oneshot::Sender<()>,
    },
}

#[derive(Clone)]
pub struct WorldHandle {
    tx: mpsc::Sender<Request>,
    events: broadcast::Sender<WorldEvent>,
    changes: broadcast::Sender<()>,
    clock_control: watch::Sender<WorldClockControl>,
    content: Arc<GameContent>,
    agent_actions: Arc<Mutex<VecDeque<AgentActionTrace>>>,
    next_agent_action_id: Arc<AtomicU64>,
}

impl WorldHandle {
    pub fn start(path: PathBuf, tick_interval: Duration) -> Self {
        Self::start_with_content(path, tick_interval, GameContent::bundled())
    }

    pub fn start_with_content(
        path: PathBuf,
        tick_interval: Duration,
        content: Arc<GameContent>,
    ) -> Self {
        let (tx, mut rx) = mpsc::channel::<Request>(256);
        let (events, _) = broadcast::channel(1024);
        let (changes, _) = broadcast::channel(128);
        let (clock_control, mut clock_control_rx) = watch::channel(WorldClockControl {
            paused: false,
            tick_interval,
        });
        let (reactive_turn_sender, mut reactive_turn_receiver) =
            mpsc::unbounded_channel::<Vec<AgentTurn>>();
        let event_sender = events.clone();
        let change_sender = changes.clone();
        let world_content = content.clone();
        let agent_actions = Arc::new(Mutex::new(VecDeque::new()));
        let next_agent_action_id = Arc::new(AtomicU64::new(1));

        thread::Builder::new()
            .name("mudgarden-world".to_string())
            .spawn(move || {
                let mut world = World::open_with_content(path, world_content);
                if let Err(error) = world.ensure_world_agents() {
                    eprintln!("could not wake world agents: {error}");
                }
                while let Some(request) = rx.blocking_recv() {
                    match request {
                        Request::EnsureHuman {
                            name,
                            fingerprint,
                            reply,
                        } => {
                            let result = world
                                .ensure_human(&name, fingerprint.as_deref())
                                .map_err(|error| error.to_string());
                            if result.is_ok() {
                                let _ = change_sender.send(());
                            }
                            let _ = reply.send(result);
                        }
                        Request::Execute {
                            actor_id,
                            command,
                            reply,
                        } => {
                            let result = world
                                .execute(actor_id, command)
                                .and_then(|output| {
                                    let turns =
                                        world.prepare_reactive_agent_turns(&output.events)?;
                                    if !turns.is_empty() {
                                        let _ = reactive_turn_sender.send(turns);
                                    }
                                    Ok(output)
                                })
                                .map_err(|error| error.to_string());
                            if let Ok(output) = &result {
                                for event in &output.events {
                                    let _ = event_sender.send(event.clone());
                                }
                                let _ = change_sender.send(());
                            }
                            let _ = reply.send(result);
                        }
                        Request::Tick { reply } => {
                            let result = (|| {
                                let events = world.tick()?;
                                let agent_turns = world.prepare_due_agent_turns()?;
                                for event in &events {
                                    let _ = event_sender.send(event.clone());
                                }
                                Ok::<_, crate::store::WorldError>(TickCycle {
                                    events,
                                    agent_turns,
                                })
                            })()
                            .map_err(|error| error.to_string());
                            if result.is_ok() {
                                let _ = change_sender.send(());
                            }
                            let _ = reply.send(result);
                        }
                        Request::ExecuteAgentPlan {
                            actor_id,
                            command,
                            intention,
                            reply,
                        } => {
                            let result = world
                                .execute_agent_plan(actor_id, command, &intention)
                                .map_err(|error| error.to_string());
                            if let Ok(output) = &result {
                                for event in &output.events {
                                    let _ = event_sender.send(event.clone());
                                }
                                let _ = change_sender.send(());
                            }
                            let _ = reply.send(result);
                        }
                        Request::QueryAgent {
                            actor_id,
                            command,
                            reply,
                        } => {
                            let result = world
                                .query(actor_id, command)
                                .map_err(|error| error.to_string());
                            let _ = reply.send(result);
                        }
                        Request::DebugSnapshot { reply } => {
                            let _ = reply.send(world.debug_snapshot(250));
                        }
                        Request::Shutdown { reply } => {
                            world.checkpoint();
                            let _ = reply.send(());
                            break;
                        }
                    }
                }
                world.checkpoint();
            })
            .expect("world service thread must start");

        let handle = Self {
            tx,
            events,
            changes,
            clock_control,
            content: content.clone(),
            agent_actions,
            next_agent_action_id,
        };
        let ticker = handle.clone();
        let planner = match ModelPlanner::from_env_with_content(content) {
            Ok(Some(planner)) => {
                eprintln!(
                    "world residents are planning with live model {}",
                    planner.model()
                );
                Some(planner)
            }
            Ok(None) => {
                eprintln!(
                    "OPENAI_API_KEY is missing; world residents will wake but cannot choose actions"
                );
                None
            }
            Err(error) => {
                eprintln!("could not configure model planner: {error:#}");
                None
            }
        };
        tokio::spawn(async move {
            let mut planner_paused = false;
            loop {
                let control = *clock_control_rx.borrow();
                let agent_turns = tokio::select! {
                    turns = reactive_turn_receiver.recv() => {
                        let Some(turns) = turns else {
                            break;
                        };
                        turns
                    }
                    _ = tokio::time::sleep(control.tick_interval), if !control.paused => {
                        let cycle = match ticker.tick_cycle().await {
                            Ok(cycle) => cycle,
                            Err(error) => {
                                eprintln!("world tick failed: {error}");
                                break;
                            }
                        };
                        cycle.agent_turns
                    }
                    changed = clock_control_rx.changed() => {
                        if changed.is_err() {
                            break;
                        }
                        continue;
                    }
                };
                let Some(planner) = &planner else {
                    continue;
                };
                if planner_paused {
                    continue;
                }
                let mut plans = tokio::task::JoinSet::new();
                for turn in agent_turns {
                    let planner = planner.clone();
                    let world = ticker.clone();
                    let trace_id = ticker.next_agent_action_id.fetch_add(1, Ordering::Relaxed);
                    plans.spawn(async move {
                        let mut trace = match planner.new_trace(trace_id, &turn) {
                            Ok(trace) => trace,
                            Err(error) => {
                                return (turn, None, Err(error));
                            }
                        };
                        let actor_id = turn.actor_id;
                        let result = planner
                            .plan(&turn, &mut trace, move |command| {
                                let world = world.clone();
                                async move { world.query_agent(actor_id, command).await }
                            })
                            .await;
                        (turn, Some(trace), result)
                    });
                }
                while let Some(result) = plans.join_next().await {
                    let Ok((turn, trace, plan)) = result else {
                        eprintln!("world resident planning task stopped unexpectedly");
                        continue;
                    };
                    let Some(mut trace) = trace else {
                        eprintln!("{} could not construct an action trace", turn.name);
                        continue;
                    };
                    match plan {
                        Ok(plan) => {
                            eprintln!(
                                "{} chose `{}` via {}",
                                turn.name, plan.command_text, plan.response_id
                            );
                            match ticker
                                .execute_agent_plan(turn.actor_id, plan.command, plan.intention)
                                .await
                            {
                                Ok(output) => {
                                    trace.status = AgentActionStatus::Completed;
                                    trace.execution_output = output.lines.clone();
                                    trace.steps.push(AgentActionStep {
                                        kind: AgentActionStepKind::Execution,
                                        label: "Action accepted by world".to_string(),
                                        rationale: None,
                                        command: trace.final_command.clone(),
                                        result: Some(output.lines.join("\n")),
                                        response_id: trace.response_id.clone(),
                                        input: None,
                                    });
                                }
                                Err(error) => {
                                    eprintln!(
                                        "{}'s planned action was rejected: {error}",
                                        turn.name
                                    );
                                    trace.status = AgentActionStatus::Rejected;
                                    trace.error = Some(error.clone());
                                    trace.steps.push(AgentActionStep {
                                        kind: AgentActionStepKind::Execution,
                                        label: "Action rejected by world".to_string(),
                                        rationale: None,
                                        command: trace.final_command.clone(),
                                        result: Some(error),
                                        response_id: trace.response_id.clone(),
                                        input: None,
                                    });
                                }
                            }
                        }
                        Err(error) => {
                            let message = format!("{error:#}");
                            trace.status = AgentActionStatus::Failed;
                            trace.error = Some(message.clone());
                            if message.contains("insufficient_quota") {
                                if !planner_paused {
                                    planner_paused = true;
                                    eprintln!(
                                        "live resident planning paused because the model account has insufficient quota"
                                    );
                                }
                            } else {
                                eprintln!("{} could not choose an action: {message}", turn.name);
                            }
                        }
                    }
                    trace.completed_at_unix_ms = Some(unix_ms());
                    ticker.record_agent_action(trace);
                }
            }
        });

        handle
    }

    pub fn subscribe(&self) -> broadcast::Receiver<WorldEvent> {
        self.events.subscribe()
    }

    pub fn subscribe_changes(&self) -> broadcast::Receiver<()> {
        self.changes.subscribe()
    }

    pub fn clock_control(&self) -> WorldClockControl {
        *self.clock_control.borrow()
    }

    pub fn set_clock_control(
        &self,
        paused: Option<bool>,
        tick_interval: Option<Duration>,
    ) -> WorldClockControl {
        self.clock_control.send_modify(|control| {
            if let Some(paused) = paused {
                control.paused = paused;
            }
            if let Some(tick_interval) = tick_interval {
                control.tick_interval = tick_interval;
            }
        });
        let control = self.clock_control();
        let _ = self.changes.send(());
        control
    }

    pub async fn ensure_human(
        &self,
        name: impl Into<String>,
        fingerprint: Option<String>,
    ) -> Result<ActorState, String> {
        let (reply, receive) = oneshot::channel();
        self.tx
            .send(Request::EnsureHuman {
                name: name.into(),
                fingerprint,
                reply,
            })
            .await
            .map_err(|_| self.content.text("error.service_stopped").to_string())?;
        receive
            .await
            .map_err(|_| self.content.text("error.service_no_answer").to_string())?
    }

    pub async fn execute(
        &self,
        actor_id: ActorId,
        command: Command,
    ) -> Result<WorldOutput, String> {
        let (reply, receive) = oneshot::channel();
        self.tx
            .send(Request::Execute {
                actor_id,
                command,
                reply,
            })
            .await
            .map_err(|_| self.content.text("error.service_stopped").to_string())?;
        receive
            .await
            .map_err(|_| self.content.text("error.service_no_answer").to_string())?
    }

    pub async fn tick_now(&self) -> Result<Vec<WorldEvent>, String> {
        Ok(self.tick_cycle().await?.events)
    }

    pub async fn debug_snapshot(&self) -> Result<DebugSnapshot, String> {
        let (reply, receive) = oneshot::channel();
        self.tx
            .send(Request::DebugSnapshot { reply })
            .await
            .map_err(|_| self.content.text("error.service_stopped").to_string())?;
        let mut snapshot = receive
            .await
            .map_err(|_| self.content.text("error.service_no_answer").to_string())?;
        snapshot.agent_actions = self
            .agent_actions
            .lock()
            .expect("agent action trace lock must not be poisoned")
            .iter()
            .rev()
            .cloned()
            .collect();
        Ok(snapshot)
    }

    async fn tick_cycle(&self) -> Result<TickCycle, String> {
        let (reply, receive) = oneshot::channel();
        self.tx
            .send(Request::Tick { reply })
            .await
            .map_err(|_| self.content.text("error.service_stopped").to_string())?;
        receive
            .await
            .map_err(|_| self.content.text("error.service_no_answer").to_string())?
    }

    async fn execute_agent_plan(
        &self,
        actor_id: ActorId,
        command: Command,
        intention: String,
    ) -> Result<WorldOutput, String> {
        let (reply, receive) = oneshot::channel();
        self.tx
            .send(Request::ExecuteAgentPlan {
                actor_id,
                command,
                intention,
                reply,
            })
            .await
            .map_err(|_| self.content.text("error.service_stopped").to_string())?;
        receive
            .await
            .map_err(|_| self.content.text("error.service_no_answer").to_string())?
    }

    async fn query_agent(
        &self,
        actor_id: ActorId,
        command_text: String,
    ) -> Result<Vec<String>, String> {
        let command = crate::commands::parse_with_content(command_text.trim(), &self.content)?;
        if !command.is_world_query() {
            return Err(
                "use look, garden, gardens, inspect, inventory, weather, bog, survey, or who"
                    .to_string(),
            );
        }
        let (reply, receive) = oneshot::channel();
        self.tx
            .send(Request::QueryAgent {
                actor_id,
                command,
                reply,
            })
            .await
            .map_err(|_| self.content.text("error.service_stopped").to_string())?;
        let output = receive
            .await
            .map_err(|_| self.content.text("error.service_no_answer").to_string())??;
        Ok(output.lines)
    }

    fn record_agent_action(&self, trace: AgentActionTrace) {
        let mut actions = self
            .agent_actions
            .lock()
            .expect("agent action trace lock must not be poisoned");
        actions.push_back(trace);
        while actions.len() > 32 {
            actions.pop_front();
        }
        drop(actions);
        let _ = self.changes.send(());
    }

    pub async fn shutdown(&self) {
        let (reply, receive) = oneshot::channel();
        if self.tx.send(Request::Shutdown { reply }).await.is_ok() {
            let _ = receive.await;
        }
    }
}

fn unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use tempfile::tempdir;

    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn serializes_one_hundred_concurrent_profiles() {
        let dir = tempdir().unwrap();
        let world = WorldHandle::start(dir.path().join("world"), Duration::from_secs(3600));
        let mut tasks = Vec::new();
        for index in 0..100 {
            let world = world.clone();
            tasks.push(tokio::spawn(async move {
                world
                    .ensure_human(
                        format!("gardener-{index}"),
                        Some(format!("SHA256:test-{index}")),
                    )
                    .await
                    .unwrap()
            }));
        }
        let mut actor_ids = BTreeSet::new();
        let mut garden_ids = BTreeSet::new();
        for task in tasks {
            let actor = task.await.unwrap();
            actor_ids.insert(actor.id);
            garden_ids.insert(actor.home_garden_id);
        }
        assert_eq!(actor_ids.len(), 100);
        assert_eq!(garden_ids.len(), 100);

        let first = world
            .ensure_human("original-name", Some("SHA256:stable-key".to_string()))
            .await
            .unwrap();
        let reconnected = world
            .ensure_human("original-name", Some("SHA256:stable-key".to_string()))
            .await
            .unwrap();
        assert_eq!(first.id, reconnected.id);
        assert_eq!(first.home_garden_id, reconnected.home_garden_id);

        let second_username = world
            .ensure_human("new-name", Some("SHA256:stable-key".to_string()))
            .await
            .unwrap();
        assert_ne!(first.id, second_username.id);
        assert_ne!(first.home_garden_id, second_username.home_garden_id);

        let wrong_key = world
            .ensure_human("original-name", Some("SHA256:different-key".to_string()))
            .await;
        assert!(wrong_key.is_err());
        world.shutdown().await;
    }

    #[tokio::test]
    async fn publishes_world_change_notifications() {
        let dir = tempdir().unwrap();
        let world = WorldHandle::start(dir.path().join("world"), Duration::from_secs(3600));
        let mut changes = world.subscribe_changes();

        world.ensure_human("observer", None).await.unwrap();

        tokio::time::timeout(Duration::from_secs(1), changes.recv())
            .await
            .expect("world change notification timed out")
            .expect("world change channel closed");
        world.shutdown().await;
    }

    #[tokio::test]
    async fn clock_can_pause_and_step_without_waiting_for_the_ticker() {
        let dir = tempdir().unwrap();
        let world = WorldHandle::start(dir.path().join("world"), Duration::from_millis(100));
        let control = world.set_clock_control(Some(true), Some(Duration::from_millis(25)));
        assert!(control.paused);
        assert_eq!(control.tick_interval, Duration::from_millis(25));

        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(world.debug_snapshot().await.unwrap().clock.now, 0);

        world.tick_now().await.unwrap();
        assert_eq!(world.debug_snapshot().await.unwrap().clock.now, 1);
        world.shutdown().await;
    }
}
