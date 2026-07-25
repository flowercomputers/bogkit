use crate::domain::WorldOutput;

const RESET: &str = "\x1b[0m";
const BOLD_MOSS: &str = "\x1b[1;38;5;108m";
const MOSS: &str = "\x1b[38;5;108m";
const GOLD: &str = "\x1b[38;5;186m";
const PLUM: &str = "\x1b[38;5;139m";
const CLAY: &str = "\x1b[38;5;173m";
const WATER: &str = "\x1b[38;5;109m";
const MUTED: &str = "\x1b[38;5;245m";
const DANGER: &str = "\x1b[38;5;167m";

pub fn output(output: &WorldOutput, color: bool) -> String {
    let heading = output.lines.iter().position(|line| !line.is_empty());
    let mut text = output
        .lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            if !color {
                line.clone()
            } else if is_board_line(line) {
                board_line(line)
            } else if Some(index) == heading {
                paint(BOLD_MOSS, line)
            } else if line.starts_with("  ")
                && heading
                    .is_some_and(|index| output.lines[index].trim_end_matches(':') == "Commands")
            {
                help_line(line)
            } else {
                inline_commands(line)
            }
        })
        .collect::<Vec<_>>()
        .join("\r\n");
    if !text.is_empty() {
        text.push_str("\r\n");
    }
    text
}

pub fn banner(line: &str, color: bool) -> String {
    if !color {
        return line.to_string();
    }
    if line.contains("roots below") {
        paint(GOLD, line)
    } else if line.contains('#') {
        paint(BOLD_MOSS, line)
    } else {
        paint(MOSS, line)
    }
}

pub fn prompt(prompt: &str, color: bool) -> String {
    if color {
        paint(GOLD, prompt)
    } else {
        prompt.to_string()
    }
}

pub fn accent(text: &str, color: bool) -> String {
    if color {
        paint(MOSS, text)
    } else {
        text.to_string()
    }
}

pub fn hint(text: &str, color: bool) -> String {
    if color {
        paint(MUTED, text)
    } else {
        text.to_string()
    }
}

pub fn error(text: &str, color: bool) -> String {
    if color {
        paint(DANGER, text)
    } else {
        text.to_string()
    }
}

pub fn event(text: &str, color: bool) -> String {
    if color {
        paint(PLUM, text)
    } else {
        text.to_string()
    }
}

fn is_board_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with("+---")
        || trimmed.starts_with("A   B   C")
        || (trimmed.len() > 4 && trimmed.as_bytes()[0].is_ascii_digit() && trimmed.contains('|'))
        || trimmed.starts_with(". empty")
}

fn board_line(line: &str) -> String {
    let trimmed = line.trim();
    if trimmed.starts_with("+---") || trimmed.starts_with("A   B   C") {
        return paint(MUTED, line);
    }
    if trimmed.starts_with(". empty") {
        return inline_commands(&paint(MUTED, line));
    }

    let mut styled = String::with_capacity(line.len() + 64);
    for character in line.chars() {
        let color = match character {
            '.' => Some(MUTED),
            's' => Some(GOLD),
            '+' | 'g' => Some(MOSS),
            '*' => Some(PLUM),
            'o' => Some(CLAY),
            'd' => Some(MUTED),
            '|' => Some(MUTED),
            character if character.is_ascii_alphabetic() => Some(WATER),
            _ => None,
        };
        if let Some(color) = color {
            styled.push_str(color);
            styled.push(character);
            styled.push_str(RESET);
        } else {
            styled.push(character);
        }
    }
    styled
}

fn help_line(line: &str) -> String {
    let Some(description_at) = line
        .char_indices()
        .skip(4)
        .find_map(|(index, _)| line[index..].starts_with("  ").then_some(index))
    else {
        return paint(GOLD, line);
    };
    format!(
        "{}{}",
        paint(GOLD, &line[..description_at]),
        paint(MUTED, &line[description_at..])
    )
}

fn inline_commands(line: &str) -> String {
    let mut styled = String::with_capacity(line.len());
    let mut parts = line.split('`');
    if let Some(first) = parts.next() {
        styled.push_str(first);
    }
    for (index, part) in parts.enumerate() {
        if index % 2 == 0 {
            styled.push_str(&paint(GOLD, part));
        } else {
            styled.push_str(part);
        }
    }
    styled
}

fn paint(color: &str, text: &str) -> String {
    format!("{color}{text}{RESET}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_output_stays_plain() {
        let output = WorldOutput::lines(["", "The Glasshouse", "Use `look garden`."]);
        assert_eq!(
            super::output(&output, false),
            "\r\nThe Glasshouse\r\nUse `look garden`.\r\n"
        );
    }

    #[test]
    fn colored_output_styles_headings_and_commands() {
        let output = WorldOutput::lines(["", "The Glasshouse", "Use `look garden`."]);
        let rendered = super::output(&output, true);

        assert!(rendered.contains("\x1b[1;38;5;108mThe Glasshouse\x1b[0m"));
        assert!(rendered.contains("\x1b[38;5;186mlook garden\x1b[0m"));
    }

    #[test]
    fn garden_board_symbols_receive_distinct_colors() {
        let rendered = board_line(" 8  | s | + | g | * | o | d | b | . |  8");

        assert!(rendered.contains("\x1b[38;5;186ms\x1b[0m"));
        assert!(rendered.contains("\x1b[38;5;139m*\x1b[0m"));
        assert!(rendered.contains("\x1b[38;5;109mb\x1b[0m"));
    }
}
