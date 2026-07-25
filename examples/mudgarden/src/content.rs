use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::domain::{ActorKind, AgentStrategy, RoomKind};

const BUNDLED_CONTENT: &str = include_str!("../content.json");

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GameContent {
    pub game: GameIdentity,
    pub world: WorldContent,
    pub dialogue: DialoguePolicy,
    pub npcs: BTreeMap<String, NpcDefinition>,
    pub merchant: MerchantDefinition,
    pub command_help: Vec<String>,
    pub resident_commands: Vec<String>,
    pub text: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MerchantDefinition {
    pub name: String,
    pub room: RoomKind,
    pub greeting: String,
    pub catalog: Vec<DecorationDefinition>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DecorationDefinition {
    pub name: String,
    pub description: String,
    pub symbol: char,
    pub fruit_cost: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GameIdentity {
    pub title: String,
    pub tagline: String,
    pub local_intro: String,
    pub opening_banner: Vec<String>,
    pub opening_banner_delay_ms: u64,
    pub opening_banner_pause_ms: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorldContent {
    pub rooms: Vec<RoomDefinition>,
    pub gardens: Vec<GardenDefinition>,
    pub species: Vec<String>,
    pub starter_seeds: Vec<String>,
    pub starter_fruit: Vec<String>,
    pub board_header: String,
    pub board_border: String,
    pub board_legend: String,
    pub home_descriptions: HomeDescriptions,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RoomDefinition {
    pub kind: RoomKind,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GardenDefinition {
    pub room: RoomKind,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HomeDescriptions {
    pub human: String,
    pub gardener: String,
    pub helper: String,
    pub spirit: String,
    pub god: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DialoguePolicy {
    pub instruction_template: String,
    pub shared_rules: Vec<String>,
    pub tool_description: String,
    pub command_description: String,
    pub intention_description: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NpcDefinition {
    pub name: String,
    pub kind: ActorKind,
    pub strategy: AgentStrategy,
    pub goal: String,
    pub wake_interval: u64,
    pub action_budget: u16,
    pub dialogue: DialogueProfile,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DialogueProfile {
    pub persona: String,
    pub voice: String,
    pub interests: Vec<String>,
    pub boundaries: Vec<String>,
    pub example_lines: Vec<String>,
}

impl GameContent {
    pub fn bundled() -> Arc<Self> {
        Arc::new(
            Self::parse(BUNDLED_CONTENT)
                .expect("the bundled MUDGarden content configuration must be valid"),
        )
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Arc<Self>> {
        let path = path.as_ref();
        let source = std::fs::read_to_string(path)
            .with_context(|| format!("could not read content config {}", path.display()))?;
        let mut merged: serde_json::Value =
            serde_json::from_str(BUNDLED_CONTENT).context("bundled content is not valid JSON")?;
        let overrides: serde_json::Value =
            serde_json::from_str(&source).context("content overrides are not valid JSON")?;
        merge_json(&mut merged, overrides);
        Self::parse(&serde_json::to_string(&merged)?)
            .with_context(|| format!("invalid content config {}", path.display()))
            .map(Arc::new)
    }

    pub fn load_from_env() -> Result<Arc<Self>> {
        match std::env::var_os("MUDGARDEN_CONTENT") {
            Some(path) => Self::load(path),
            None => Ok(Self::bundled()),
        }
    }

    pub fn parse(source: &str) -> Result<Self> {
        let content: Self =
            serde_json::from_str(source).context("content config is not valid JSON")?;
        content.validate()?;
        Ok(content)
    }

    pub fn text(&self, key: &str) -> &str {
        self.text
            .get(key)
            .unwrap_or_else(|| panic!("content config is missing text key `{key}`"))
    }

    pub fn render(&self, key: &str, values: &[(&str, String)]) -> String {
        let mut rendered = self.text(key).to_string();
        for (name, value) in values {
            rendered = rendered.replace(&format!("{{{{{name}}}}}"), value);
        }
        assert!(
            !rendered.contains("{{"),
            "content template `{key}` has an unresolved placeholder after rendering"
        );
        rendered
    }

    pub fn npc(&self, id: &str) -> Option<&NpcDefinition> {
        self.npcs.get(id)
    }

    pub fn npc_for_actor(&self, npc_id: &str, actor_name: &str) -> Option<&NpcDefinition> {
        self.npc(npc_id)
            .or_else(|| self.npcs.values().find(|npc| npc.name == actor_name))
    }

    pub fn room(&self, kind: &RoomKind) -> &RoomDefinition {
        self.world
            .rooms
            .iter()
            .find(|room| &room.kind == kind)
            .unwrap_or_else(|| panic!("content config is missing room {kind:?}"))
    }

    fn validate(&self) -> Result<()> {
        if self.game.title.trim().is_empty() {
            bail!("game.title cannot be empty");
        }
        if self.world.species.is_empty() {
            bail!("world.species must contain at least one species");
        }
        for seed in &self.world.starter_seeds {
            if !self.world.species.contains(seed) {
                bail!("starter seed `{seed}` is not listed in world.species");
            }
        }
        for fruit in &self.world.starter_fruit {
            if !self.world.species.contains(fruit) {
                bail!("starter fruit `{fruit}` is not listed in world.species");
            }
        }

        let mut room_kinds = BTreeSet::new();
        for room in &self.world.rooms {
            let key = format!("{:?}", room.kind);
            if !room_kinds.insert(key) {
                bail!("room kind {:?} is configured more than once", room.kind);
            }
        }
        for required in [
            RoomKind::Gate,
            RoomKind::CommonPath,
            RoomKind::Glasshouse,
            RoomKind::MoonBed,
            RoomKind::Pond,
            RoomKind::Compost,
            RoomKind::WildEdge,
        ] {
            if !self.world.rooms.iter().any(|room| room.kind == required) {
                bail!("world.rooms is missing {required:?}");
            }
        }
        for required in [
            RoomKind::Glasshouse,
            RoomKind::MoonBed,
            RoomKind::Pond,
            RoomKind::Compost,
            RoomKind::WildEdge,
        ] {
            if !self
                .world
                .gardens
                .iter()
                .any(|garden| garden.room == required)
            {
                bail!("world.gardens is missing {required:?}");
            }
        }

        let mut npc_names = BTreeSet::new();
        for (npc_id, npc) in &self.npcs {
            if npc_id.trim().is_empty() || npc.name.trim().is_empty() {
                bail!("NPC IDs and names cannot be empty");
            }
            if !npc_names.insert(npc.name.to_ascii_lowercase()) {
                bail!("NPC name `{}` is configured more than once", npc.name);
            }
            if npc.wake_interval == 0 {
                bail!("NPC `{npc_id}` must have a positive wake_interval");
            }
            if npc.dialogue.persona.trim().is_empty() || npc.dialogue.voice.trim().is_empty() {
                bail!("NPC `{npc_id}` needs both a dialogue persona and voice");
            }
        }
        if self.merchant.name.trim().is_empty() || self.merchant.greeting.trim().is_empty() {
            bail!("merchant name and greeting cannot be empty");
        }
        if !self
            .world
            .rooms
            .iter()
            .any(|room| room.kind == self.merchant.room)
        {
            bail!("merchant room must be one of the configured shared rooms");
        }
        if self.merchant.catalog.is_empty() {
            bail!("merchant catalog must contain at least one decoration");
        }
        let mut decoration_names = BTreeSet::new();
        for decoration in &self.merchant.catalog {
            if decoration.name.trim().is_empty()
                || decoration.description.trim().is_empty()
                || decoration.symbol.is_whitespace()
                || decoration.fruit_cost == 0
            {
                bail!(
                    "merchant decorations need a name, description, visible symbol, and positive fruit cost"
                );
            }
            if !decoration_names.insert(decoration.name.to_ascii_lowercase()) {
                bail!(
                    "merchant decoration `{}` is configured more than once",
                    decoration.name
                );
            }
        }
        for (key, value) in &self.text {
            if value.trim().is_empty() {
                bail!("text entry `{key}` cannot be empty");
            }
            if value.matches("{{").count() != value.matches("}}").count() {
                bail!("text entry `{key}` has an unbalanced template placeholder");
            }
        }
        Ok(())
    }

    pub fn dialogue_instructions(&self, npc: &NpcDefinition, goal: &str) -> String {
        let list = |items: &[String]| {
            items
                .iter()
                .map(|item| format!("- {item}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let rendered = [
            ("persona", npc.dialogue.persona.clone()),
            ("goal", goal.to_string()),
            ("voice", npc.dialogue.voice.clone()),
            ("interests", list(&npc.dialogue.interests)),
            ("boundaries", list(&npc.dialogue.boundaries)),
            ("examples", list(&npc.dialogue.example_lines)),
            ("shared_rules", list(&self.dialogue.shared_rules)),
        ]
        .into_iter()
        .fold(
            self.dialogue.instruction_template.clone(),
            |text, (key, value)| text.replace(&format!("{{{{{key}}}}}"), &value),
        );
        assert!(
            !rendered.contains("{{"),
            "dialogue.instruction_template has an unresolved placeholder"
        );
        rendered
    }
}

fn merge_json(target: &mut serde_json::Value, overrides: serde_json::Value) {
    match (target, overrides) {
        (serde_json::Value::Object(target), serde_json::Value::Object(overrides)) => {
            for (key, value) in overrides {
                match target.get_mut(&key) {
                    Some(target_value) => merge_json(target_value, value),
                    None => {
                        target.insert(key, value);
                    }
                }
            }
        }
        (target, overrides) => *target = overrides,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_content_is_valid_and_dialogue_is_addressable_by_stable_id() {
        let content = GameContent::bundled();
        assert_eq!(content.game.opening_banner_delay_ms, 75);
        assert_eq!(content.game.opening_banner_pause_ms, 2_000);
        assert_eq!(content.world.starter_fruit.len(), 2);
        let npc = content.npc("mosswife").unwrap();
        assert_eq!(npc.name, "Mosswife");
        assert!(
            content
                .dialogue_instructions(npc, &npc.goal)
                .contains("Voice:")
        );
    }

    #[test]
    fn opening_banner_can_have_any_number_of_lines() {
        let mut source: serde_json::Value = serde_json::from_str(BUNDLED_CONTENT).unwrap();
        source["game"]["opening_banner"] = serde_json::json!(["A shorter banner"]);
        source["game"]["opening_banner_delay_ms"] = serde_json::json!(0);

        let content = GameContent::parse(&serde_json::to_string(&source).unwrap()).unwrap();

        assert_eq!(content.game.opening_banner, ["A shorter banner"]);
    }

    #[test]
    fn templates_replace_named_values() {
        let content = GameContent::bundled();
        assert_eq!(
            content.render("event.first_arrival", &[("name", "Mara".to_string())]),
            "Mara enters the garden for the first time."
        );
    }

    #[test]
    fn json_overrides_merge_recursively() {
        let mut target = serde_json::json!({
            "game": { "title": "MUDGarden", "tagline": "original" }
        });
        merge_json(
            &mut target,
            serde_json::json!({ "game": { "tagline": "custom" } }),
        );
        assert_eq!(target["game"]["title"], "MUDGarden");
        assert_eq!(target["game"]["tagline"], "custom");
    }

    #[test]
    fn one_npc_dialogue_field_can_be_overridden_without_replacing_other_npcs() {
        let mut target: serde_json::Value = serde_json::from_str(BUNDLED_CONTENT).unwrap();
        merge_json(
            &mut target,
            serde_json::json!({
                "npcs": {
                    "mosswife": {
                        "dialogue": { "voice": "Only speaks in questions." }
                    }
                }
            }),
        );
        let content = GameContent::parse(&target.to_string()).unwrap();
        assert_eq!(
            content.npc("mosswife").unwrap().dialogue.voice,
            "Only speaks in questions."
        );
        assert_eq!(content.npc("ivo").unwrap().name, "Ivo");
    }
}
