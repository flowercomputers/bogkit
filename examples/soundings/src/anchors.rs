//! The anchor question: how many example sentences make an axis?
//!
//! The concrete——abstract axis is a centroid difference — each pole's mean
//! embedding carries signal (the property) plus nuisance (each anchor's
//! topic). Averaging cancels nuisance at 1/n, so more, topic-diverse
//! anchors should yield a better axis. This module measures that claim:
//! three nested configurations (4 / 20 / 50 anchors per pole) scored
//! against a held-out 30-sentence ladder in six human-graded bands.
//!
//! Metric: pairwise ordering accuracy — over every cross-band pair, does
//! the axis order the two sentences the way the bands do? (Concordance is
//! well-defined under ties; Spearman with banded ties is not.) Plus the
//! ladder's t-spread and the axis construction cost.
//!
//! `cargo run -p soundings -- anchors` prints it; /engine displays it live.

use crate::serve::Axis;
use std::sync::OnceLock;

// Nested pole sets: [..4] is the shipped axis, [..20] adds topic breadth,
// [..50] adds bulk. Concrete = physical, sensory, specific; abstract =
// conceptual, definitional, general. Roughly length-matched across poles.
pub const CONCRETE: [&str; 50] = [
    "She wiped the counter and stacked the blue ceramic bowls.",
    "The bus driver counted quarters into a paper cup.",
    "Rain dripped from the fire escape onto the trash bags below.",
    "He tied his boots and zipped the canvas jacket to his chin.",
    "The welder flipped her visor down and struck the first bead.",
    "A gull dropped the clam twice on the parking lot asphalt.",
    "The nurse peeled the backing off the bandage and pressed it flat.",
    "He cracked two eggs against the skillet and stirred them with a fork.",
    "The goalie punched the ball over the crossbar with both fists.",
    "She folded the map along its worn creases and slid it in the glovebox.",
    "The tractor dragged a harrow through the wet black field.",
    "He sharpened the pencil until the shavings curled onto the desk.",
    "The ferry bumped the pilings and the deckhand threw the line.",
    "She pinned the hem and fed the fabric under the needle.",
    "The bartender rattled the shaker and strained the drink over ice.",
    "A raccoon pried the lid off the compost bin at dusk.",
    "The mason tapped each brick level with the butt of his trowel.",
    "He plugged the amp in and the speaker hummed against the floor.",
    "The climber chalked her hands and jammed a fist into the crack.",
    "The printer chewed the envelope and flashed a red light.",
    "She scraped frost off the windshield with a spatula.",
    "The dog shook lake water across the porch boards.",
    "He levered the tire off the rim with two long irons.",
    "The cashier sorted the register and banded the twenties.",
    "A dragonfly landed on the oar and rode it upstream.",
    "The surgeon clamped the vessel and asked for suction.",
    "She split kindling on the stump behind the cabin.",
    "The toddler stacked wooden blocks until the tower toppled.",
    "He soldered the last joint and taped the wires back.",
    "The waitress balanced four plates up one freckled arm.",
    "Steam rose off the manhole covers on Fifth Avenue.",
    "The librarian stamped the card and slid the book across.",
    "He netted the trout and wet his hands before touching it.",
    "The barber snapped the cape and brushed hair from his collar.",
    "She measured the flour by spooning it into the cup.",
    "The lineman cinched his belt and started up the pole.",
    "A crow walked the fence rail with something shiny in its beak.",
    "The mechanic rolled out from under the truck and wiped his palms.",
    "She threaded the projector and the reel began to tick.",
    "The tide left a line of kelp and bottle caps on the sand.",
    "He hammered the stake in with the flat of a rock.",
    "The florist stripped the thorns and cut each stem at an angle.",
    "Snow slid off the tin roof all at once with a whump.",
    "The umpire dusted the plate with a little whisk broom.",
    "She wrung the mop into the bucket and started on the hall.",
    "The beekeeper puffed smoke under the lid and waited.",
    "He backed the trailer down the ramp until the hubs touched water.",
    "The seamstress bit the thread and knotted it close to the cloth.",
    "A forklift beeped backward through the loading dock.",
    "The potter centered the clay and pressed a thumb into the spin.",
];

pub const ABSTRACT: [&str; 50] = [
    "Freedom is the capacity to author one's own life.",
    "Progress depends on institutions that outlive their founders.",
    "Meaning arises when suffering is given a purpose.",
    "Truth survives only where inquiry is unafraid.",
    "Justice is the debt the strong owe to the weak.",
    "Memory is the architecture of identity.",
    "Trust is a currency that inflation cannot touch.",
    "Every tradition began as an act of rebellion.",
    "Knowledge advances by the correction of error.",
    "Power reveals character more reliably than adversity.",
    "Hope is a discipline rather than a mood.",
    "The value of a promise lies in its cost.",
    "Culture is what remains when incentives are forgotten.",
    "Time is the one inheritance divided equally.",
    "Language is the map by which thought travels.",
    "Courage is fear that has accepted a purpose.",
    "Institutions decay when their rituals outlast their reasons.",
    "Beauty is order perceived without effort.",
    "A society is judged by what it refuses to sell.",
    "Wisdom begins where certainty ends.",
    "History is an argument the present has with itself.",
    "Responsibility is the price of every liberty.",
    "Genius is patience wearing the mask of instinct.",
    "The self is a story revised in the telling.",
    "Morality is imagination applied to consequence.",
    "Peace is not the absence of conflict but its governance.",
    "Education is the slow transfer of doubt.",
    "Ambition without humility becomes appetite.",
    "The law is frozen politics slowly thawing.",
    "Grief is love with nowhere to go.",
    "Charity that humiliates is a subtle form of theft.",
    "Every measurement is a confession of ignorance.",
    "Identity hardens fastest under siege.",
    "Forgiveness is memory that has renounced its weapon.",
    "An economy is trust made countable.",
    "Silence can be the most articulate form of dissent.",
    "Habit is character in its everyday clothes.",
    "The future is a debt the present keeps refinancing.",
    "Doubt is the engine of every honest method.",
    "Loyalty untested is merely convenience.",
    "Art is attention made permanent.",
    "Bureaucracy is caution compounded annually.",
    "Solitude is the workshop of the self.",
    "A metaphor is a bridge built from the known.",
    "Fairness is symmetry applied to strangers.",
    "Curiosity is hunger that feeds on its own satisfaction.",
    "Dignity is the one possession that must be given away to be lost.",
    "Consensus is often exhaustion wearing agreement's face.",
    "Innovation is heresy that happened to work.",
    "Patience is confidence with a longer horizon.",
];

/// Held-out validation ladder: six graded bands, five sentences each,
/// deliberately overlapping in topic ACROSS bands (kitchens appear at both
/// ends) so the axis can't score topic instead of register. None of these
/// appear in the anchor sets.
pub const LADDER: [(u8, &str); 30] = [
    (0, "The kettle clicked off and she filled the two chipped mugs."),
    (0, "He knelt in the gravel and patched the inner tube with rubber cement."),
    (0, "The cat batted the bottle cap under the refrigerator."),
    (0, "She snapped the beans into a colander on the porch step."),
    (0, "The train doors chimed and pinched shut on a tote bag."),
    (1, "The kitchen smelled like the mornings of his childhood."),
    (1, "She kept the garden going long after anyone asked her to."),
    (1, "The night shift left him tired in a way sleep did not fix."),
    (1, "They argued quietly so the children would not hear."),
    (1, "The old road flooded every spring and no one complained."),
    (2, "He worked two jobs because the family needed the money."),
    (2, "She practiced daily, and the practicing became its own reward."),
    (2, "The town survived on habits nobody could quite explain."),
    (2, "Raising children changed what the couple argued about."),
    (2, "The business grew slowly, the way trust grows."),
    (3, "Their sacrifice meant something larger than the daily grind."),
    (3, "The meal mattered less than the fact that they cooked it together."),
    (3, "What the flood took was not property but continuity."),
    (3, "Her patience was a kind of quiet argument about the future."),
    (3, "The team's losses taught what its victories concealed."),
    (4, "Dignity is the quiet premise beneath every fair wage."),
    (4, "Home is a claim we make against impermanence."),
    (4, "Work becomes meaningful when effort and identity align."),
    (4, "Community is the compound interest of small kindnesses."),
    (4, "Craft is knowledge that lives in the hands."),
    (5, "Justice, at last, is a promise a society makes to itself."),
    (5, "To exist is to be indebted to what preceded you."),
    (5, "Value is agreement wearing the costume of fact."),
    (5, "The good is what remains desirable under full knowledge."),
    (5, "Being is the horizon against which every question is asked."),
];

#[derive(Debug, Clone, serde::Serialize)]
pub struct AnchorBench {
    pub per_pole: usize,
    pub pairwise_accuracy: f32,
    pub spread: f32,
    pub violations: usize,
    pub pairs: usize,
    pub build_us: u64,
}

fn bench_config(n: usize) -> AnchorBench {
    let t0 = std::time::Instant::now();
    let c: Vec<&str> = CONCRETE[..n].to_vec();
    let a: Vec<&str> = ABSTRACT[..n].to_vec();
    let axis = Axis::from_anchors(&c, &a);
    let build_us = t0.elapsed().as_micros() as u64;

    let scored: Vec<(u8, f32)> = LADDER.iter().map(|(b, s)| (*b, axis.score(s).t)).collect();
    let (mut concordant, mut pairs) = (0usize, 0usize);
    for i in 0..scored.len() {
        for j in 0..scored.len() {
            if scored[i].0 < scored[j].0 {
                pairs += 1;
                if scored[i].1 < scored[j].1 {
                    concordant += 1;
                }
            }
        }
    }
    let ts: Vec<f32> = scored.iter().map(|(_, t)| *t).collect();
    let spread = ts.iter().cloned().fold(f32::MIN, f32::max) - ts.iter().cloned().fold(f32::MAX, f32::min);
    AnchorBench {
        per_pole: n,
        pairwise_accuracy: concordant as f32 / pairs.max(1) as f32,
        spread,
        violations: pairs - concordant,
        pairs,
        build_us,
    }
}

pub fn bench() -> &'static Vec<AnchorBench> {
    static B: OnceLock<Vec<AnchorBench>> = OnceLock::new();
    B.get_or_init(|| [4usize, 20, 50].into_iter().map(bench_config).collect())
}

pub fn print_bench() {
    println!("anchored-axis bench · 30-sentence held-out ladder, 6 graded bands · {} cross-band pairs", bench()[0].pairs);
    for b in bench() {
        println!(
            "  {:>2}/pole  pairwise ordering {:.1}%  ({} violations)  spread {:.2}  built in {}µs",
            b.per_pole,
            b.pairwise_accuracy * 100.0,
            b.violations,
            b.spread,
            b.build_us
        );
    }
}
