use crate::content::GameContent;
use crate::domain::GardenPosition;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Look(Option<String>),
    Garden,
    Gardens,
    Go(String),
    WalkTo(String),
    Enter,
    Knock,
    LockGarden,
    UnlockGarden,
    Admit(String),
    Home,
    Plant {
        species: String,
        position: GardenPosition,
        name: Option<String>,
    },
    Water(String),
    Prune(String),
    Harvest(String),
    Inspect(String),
    Say(String),
    Inventory,
    Shop,
    Buy(String),
    Place {
        decoration: String,
        position: GardenPosition,
    },
    TakeDecoration(String),
    Offer {
        item: String,
        recipient: String,
    },
    Allow {
        actor: String,
        action: String,
    },
    Forbid {
        actor: String,
        action: String,
    },
    Visit(String),
    ChangeWeather(String),
    Weather,
    Bog,
    Survey(Option<(u16, u16)>),
    Restore(u16, u16),
    Who,
    Changes,
    Help,
    Quit,
}

impl Command {
    pub fn is_world_query(&self) -> bool {
        matches!(
            self,
            Self::Look(_)
                | Self::Garden
                | Self::Gardens
                | Self::Inspect(_)
                | Self::Inventory
                | Self::Shop
                | Self::Weather
                | Self::Bog
                | Self::Survey(_)
                | Self::Who
        )
    }
}

pub fn parse(line: &str) -> Result<Command, String> {
    parse_with_content(line, &GameContent::bundled())
}

pub fn parse_with_content(line: &str, content: &GameContent) -> Result<Command, String> {
    if let Some(body) = raw_speech_body(line) {
        return required(body, content.text("command.say_what")).map(Command::Say);
    }

    let words = shell_words::split(line)
        .map_err(|err| content.render("command.unreadable", &[("error", err.to_string())]))?;
    let Some(verb) = words.first().map(|word| word.to_ascii_lowercase()) else {
        return Ok(Command::Look(None));
    };

    let tail = words[1..].join(" ");
    match verb.as_str() {
        "look" | "l" => Ok(Command::Look((!tail.is_empty()).then_some(tail))),
        "garden" | "board" => Ok(Command::Garden),
        "gardens" | "gates" => Ok(Command::Gardens),
        "enter" | "in" => Ok(Command::Enter),
        "knock" => Ok(Command::Knock),
        "lock" => Ok(Command::LockGarden),
        "unlock" => Ok(Command::UnlockGarden),
        "admit" => required(&tail, content.text("command.admit_whom")).map(Command::Admit),
        "go" | "move" => required(&tail, content.text("command.go_where")).map(Command::Go),
        "walk" => {
            let destination = if tail
                .get(..3)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("to "))
            {
                &tail[3..]
            } else {
                &tail
            };
            required(destination, content.text("command.walk_where")).map(Command::WalkTo)
        }
        "north" | "n" | "south" | "s" | "east" | "e" | "west" | "w" | "out" | "o" => {
            Ok(Command::Go(verb))
        }
        "home" => Ok(Command::Home),
        "plant" => {
            if tail.is_empty() {
                return Err(content.text("command.plant_usage").to_string());
            }
            let (placement, name) = match tail.rsplit_once(" as ") {
                Some((species, name)) => {
                    (species.trim().to_string(), Some(name.trim().to_string()))
                }
                None => (tail, None),
            };
            let (species, position) = placement
                .rsplit_once(" at ")
                .ok_or_else(|| content.text("command.plant_usage").to_string())?;
            let position = position
                .parse::<GardenPosition>()
                .map_err(|_| content.text("command.coordinate_range").to_string())?;
            Ok(Command::Plant {
                species: species.trim().to_string(),
                position,
                name,
            })
        }
        "water" => required(&tail, content.text("command.water_what")).map(Command::Water),
        "prune" => required(&tail, content.text("command.prune_what")).map(Command::Prune),
        "harvest" => required(&tail, content.text("command.harvest_what")).map(Command::Harvest),
        "inspect" | "examine" | "x" => {
            required(&tail, content.text("command.inspect_what")).map(Command::Inspect)
        }
        "inventory" | "inv" | "i" => Ok(Command::Inventory),
        "shop" | "browse" => Ok(Command::Shop),
        "buy" => required(&tail, content.text("command.buy_what")).map(Command::Buy),
        "place" | "decorate" => {
            let (decoration, position) = tail
                .rsplit_once(" at ")
                .ok_or_else(|| content.text("command.place_usage").to_string())?;
            let position = position
                .parse::<GardenPosition>()
                .map_err(|_| content.text("command.coordinate_range").to_string())?;
            Ok(Command::Place {
                decoration: decoration.trim().to_string(),
                position,
            })
        }
        "take" | "pickup" => {
            required(&tail, content.text("command.take_what")).map(Command::TakeDecoration)
        }
        "offer" => {
            let (item, recipient) = tail
                .split_once(" to ")
                .ok_or_else(|| content.text("command.offer_usage").to_string())?;
            Ok(Command::Offer {
                item: item.trim().to_string(),
                recipient: recipient.trim().to_string(),
            })
        }
        "allow" => {
            let (actor, action) = tail
                .split_once(" to ")
                .ok_or_else(|| content.text("command.allow_usage").to_string())?;
            Ok(Command::Allow {
                actor: actor.trim().to_string(),
                action: action.trim().to_string(),
            })
        }
        "forbid" => {
            let (actor, action) = tail
                .split_once(" from ")
                .ok_or_else(|| content.text("command.forbid_usage").to_string())?;
            Ok(Command::Forbid {
                actor: actor.trim().to_string(),
                action: action.trim().to_string(),
            })
        }
        "visit" => required(&tail, content.text("command.visit_whom")).map(Command::Visit),
        "invoke" if tail.to_ascii_lowercase().starts_with("weather ") => required(
            tail["weather ".len()..].trim(),
            content.text("command.invoke_weather"),
        )
        .map(Command::ChangeWeather),
        "weather" => Ok(Command::Weather),
        "bog" | "ecology" => Ok(Command::Bog),
        "survey" => {
            if tail.is_empty() {
                Ok(Command::Survey(None))
            } else if tail.eq_ignore_ascii_case("garden") || tail.eq_ignore_ascii_case("board") {
                Ok(Command::Garden)
            } else {
                parse_bog_coordinate(&tail, content).map(|(x, y)| Command::Survey(Some((x, y))))
            }
        }
        "restore" => parse_bog_coordinate(&tail, content).map(|(x, y)| Command::Restore(x, y)),
        "who" => Ok(Command::Who),
        "changes" | "events" => Ok(Command::Changes),
        "help" | "?" => Ok(Command::Help),
        "quit" | "exit" => Ok(Command::Quit),
        _ => Err(content.render("command.unknown", &[("verb", verb)])),
    }
}

fn raw_speech_body(line: &str) -> Option<&str> {
    let line = line.trim();
    let raw = if let Some(tail) = line.strip_prefix('\'') {
        tail.trim_start()
    } else if let Some(verb_end) = line.find(char::is_whitespace) {
        let (verb, tail) = line.split_at(verb_end);
        verb.eq_ignore_ascii_case("say")
            .then_some(tail.trim_start())?
    } else if line.eq_ignore_ascii_case("say") {
        ""
    } else {
        return None;
    };
    let bytes = raw.as_bytes();
    if bytes.len() >= 2
        && matches!(
            (bytes.first(), bytes.last()),
            (Some(b'"'), Some(b'"')) | (Some(b'\''), Some(b'\''))
        )
    {
        Some(raw[1..raw.len() - 1].trim())
    } else {
        Some(raw.trim())
    }
}

fn parse_bog_coordinate(value: &str, content: &GameContent) -> Result<(u16, u16), String> {
    let normalized = value.replace(',', " ");
    let parts = normalized.split_whitespace().collect::<Vec<_>>();
    if parts.len() != 2 {
        return Err(content.text("command.bog_coordinate_usage").to_string());
    }
    let x = parts[0]
        .parse::<u16>()
        .map_err(|_| content.text("command.bog_coordinate_integer").to_string())?;
    let y = parts[1]
        .parse::<u16>()
        .map_err(|_| content.text("command.bog_coordinate_integer").to_string())?;
    Ok((x, y))
}

fn required(value: &str, message: &str) -> Result<String, String> {
    if value.trim().is_empty() {
        Err(message.to_string())
    } else {
        Ok(value.trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_named_plant() {
        assert_eq!(
            parse("plant scarlet runner bean at c4 as red runner").unwrap(),
            Command::Plant {
                species: "scarlet runner bean".to_string(),
                position: "C4".parse().unwrap(),
                name: Some("red runner".to_string())
            }
        );
    }

    #[test]
    fn rejects_planting_without_a_valid_coordinate() {
        assert!(
            parse("plant scarlet runner bean")
                .unwrap_err()
                .contains("A1-H8")
        );
        assert!(
            parse("plant scarlet runner bean at J2")
                .unwrap_err()
                .contains("A1 to H8")
        );
    }

    #[test]
    fn parses_direction_shortcuts() {
        assert_eq!(parse("n").unwrap(), Command::Go("n".to_string()));
        assert_eq!(parse("out").unwrap(), Command::Go("out".to_string()));
        assert_eq!(
            parse("walk to the wild edge").unwrap(),
            Command::WalkTo("the wild edge".to_string())
        );
        assert_eq!(
            parse("walk compost").unwrap(),
            Command::WalkTo("compost".to_string())
        );
        assert_eq!(
            parse("walk TO the pond").unwrap(),
            Command::WalkTo("the pond".to_string())
        );
        assert_eq!(
            parse("look east").unwrap(),
            Command::Look(Some("east".to_string()))
        );
        assert_eq!(parse("garden").unwrap(), Command::Garden);
        assert_eq!(parse("board").unwrap(), Command::Garden);
        assert_eq!(parse("gardens").unwrap(), Command::Gardens);
        assert_eq!(parse("in").unwrap(), Command::Enter);
        assert_eq!(parse("knock").unwrap(), Command::Knock);
        assert_eq!(parse("lock").unwrap(), Command::LockGarden);
        assert_eq!(parse("unlock").unwrap(), Command::UnlockGarden);
        assert_eq!(
            parse("admit mara").unwrap(),
            Command::Admit("mara".to_string())
        );
    }

    #[test]
    fn parses_permission_commands() {
        assert_eq!(
            parse("allow mara to tend here").unwrap(),
            Command::Allow {
                actor: "mara".to_string(),
                action: "tend here".to_string(),
            }
        );
    }

    #[test]
    fn speech_treats_apostrophes_as_plain_text() {
        assert_eq!(
            parse("say you're repeating yourself").unwrap(),
            Command::Say("you're repeating yourself".to_string())
        );
        assert_eq!(
            parse("say \"you're repeating yourself\"").unwrap(),
            Command::Say("you're repeating yourself".to_string())
        );
        assert_eq!(
            parse("say 'mine died'").unwrap(),
            Command::Say("mine died".to_string())
        );
        assert_eq!(
            parse("' hello there").unwrap(),
            Command::Say("hello there".to_string())
        );
    }

    #[test]
    fn parses_decoration_commands() {
        assert_eq!(parse("shop").unwrap(), Command::Shop);
        assert_eq!(
            parse("buy mossy stone seat").unwrap(),
            Command::Buy("mossy stone seat".to_string())
        );
        assert_eq!(
            parse("place mossy stone seat at d5").unwrap(),
            Command::Place {
                decoration: "mossy stone seat".to_string(),
                position: "D5".parse().unwrap(),
            }
        );
        assert_eq!(
            parse("take D5").unwrap(),
            Command::TakeDecoration("D5".to_string())
        );
    }

    #[test]
    fn parses_bog_survey_and_restoration_coordinates() {
        assert_eq!(parse("bog").unwrap(), Command::Bog);
        assert_eq!(parse("survey").unwrap(), Command::Survey(None));
        assert_eq!(parse("survey garden").unwrap(), Command::Garden);
        assert_eq!(parse("survey BOARD").unwrap(), Command::Garden);
        assert_eq!(
            parse("survey 12, 7").unwrap(),
            Command::Survey(Some((12, 7)))
        );
        assert_eq!(parse("restore 4 9").unwrap(), Command::Restore(4, 9));
    }

    #[test]
    fn agent_world_queries_are_observation_only() {
        assert!(Command::Look(None).is_world_query());
        assert!(Command::Survey(Some((3, 4))).is_world_query());
        assert!(!Command::Go("north".to_string()).is_world_query());
        assert!(!Command::Restore(3, 4).is_world_query());
        assert!(!Command::Say("hello".to_string()).is_world_query());
    }
}
