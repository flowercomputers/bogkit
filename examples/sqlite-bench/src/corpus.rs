//! Deterministic synthetic corpus: topic-clustered English documents.
//!
//! Every document blends two of forty topic pools — ~45% of its words from
//! a primary topic, ~25% from a secondary, ~30% from a general pool — so
//! the ese embeddings have real cluster structure for the vector indexes
//! to exploit without collapsing into a few dense blobs of near-duplicate
//! vectors, and BM25 has meaningful term statistics. Everything is
//! lowercase ASCII on purpose: FTS5's unicode61 tokenizer and fold's
//! ASCII-alnum BM25 tokenizer agree on this corpus, so neither side gains
//! a tokenization edge.
//!
//! All randomness flows from one seeded SplitMix64, so a given
//! `--seed`/`--scale` pair produces the identical corpus, queries, and
//! churn schedule on every run and for both systems.

/// SplitMix64: tiny, seedable, deterministic. No `rand` dependency.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }

    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n`.
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    pub fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
}

pub const TOPICS: &[&[&str]] = &[
    &["database", "index", "btree", "query", "transaction", "commit", "rollback", "schema", "table", "row", "cursor", "checkpoint", "vacuum", "journal", "replication", "shard", "latency", "throughput", "cache", "durability", "snapshot", "compaction", "postings", "tokenizer"],
    &["network", "packet", "router", "socket", "protocol", "bandwidth", "gateway", "loadbalancer", "subnet", "ethernet", "wireless", "handshake", "congestion", "broadcast", "endpoint", "tunnel", "proxy", "switch", "datagram", "backbone", "peering", "multicast", "jitter", "telemetry"],
    &["recipe", "onion", "garlic", "butter", "simmer", "roast", "saute", "kitchen", "flour", "yeast", "dough", "oven", "seasoning", "broth", "skillet", "marinade", "vinegar", "caramel", "whisk", "braise", "pepper", "cinnamon", "ginger", "saffron"],
    &["telescope", "galaxy", "nebula", "orbit", "comet", "asteroid", "supernova", "quasar", "eclipse", "constellation", "photon", "gravity", "spectrum", "parallax", "magnitude", "occultation", "zenith", "perihelion", "equinox", "cosmology", "pulsar", "redshift", "luminosity", "observatory"],
    &["garden", "compost", "seedling", "mulch", "pruning", "perennial", "trellis", "loam", "pollinator", "greenhouse", "irrigation", "fertilizer", "rootstock", "graft", "canopy", "hedge", "orchard", "bulb", "germination", "cultivar", "nitrogen", "topsoil", "weeding", "harvest"],
    &["melody", "harmony", "rhythm", "chord", "rubato", "cadence", "sonata", "orchestra", "conductor", "violin", "clarinet", "percussion", "crescendo", "octave", "arpeggio", "counterpoint", "timbre", "ensemble", "recital", "concerto", "libretto", "overture", "acoustics", "resonance"],
    &["midfielder", "penalty", "offside", "tackle", "dribble", "corner", "keeper", "striker", "defender", "fixture", "league", "relegation", "transfer", "stadium", "referee", "formation", "counterattack", "header", "volley", "clearance", "playmaker", "touchline", "extratime", "derby"],
    &["diagnosis", "symptom", "therapy", "dosage", "antibody", "vaccine", "pathogen", "chronic", "clinical", "prescription", "cardiology", "anesthesia", "surgery", "recovery", "immune", "protein", "enzyme", "receptor", "biopsy", "remission", "triage", "prognosis", "syndrome", "placebo"],
    &["statute", "verdict", "plaintiff", "defendant", "appeal", "precedent", "jurisdiction", "contract", "tort", "liability", "injunction", "subpoena", "testimony", "arbitration", "clause", "indemnity", "negligence", "damages", "counsel", "docket", "acquittal", "deposition", "covenant", "estoppel"],
    &["portfolio", "dividend", "equity", "shortsell", "liquidity", "arbitrage", "futures", "yield", "inflation", "treasury", "brokerage", "collateral", "leverage", "derivative", "volatility", "custody", "settlement", "margin", "solvency", "audit", "ledger", "valuation", "coupon", "underwriting"],
    &["mainsail", "rudder", "keel", "tack", "jib", "mooring", "regatta", "windward", "leeward", "hull", "spinnaker", "halyard", "buoy", "anchorage", "helm", "bowline", "capsize", "gybe", "waterline", "tiller", "ballast", "berth", "knot", "harbor"],
    &["forecast", "cyclone", "humidity", "barometer", "monsoon", "drizzle", "thunder", "frost", "blizzard", "drought", "isobar", "cumulus", "stratus", "hail", "gale", "microclimate", "precipitation", "dewpoint", "windchill", "jetstream", "anemometer", "squall", "overcast", "heatwave"],
    &["runway", "tailor", "fabric", "silhouette", "couture", "hemline", "textile", "pattern", "embroidery", "stitch", "garment", "atelier", "collection", "silk", "linen", "denim", "palette", "accessory", "retro", "bespoke", "mannequin", "drape", "lookbook", "trend"],
    &["gambit", "endgame", "castling", "checkmate", "sacrifice", "opening", "blunder", "zugzwang", "stalemate", "grandmaster", "tactic", "pawn", "bishop", "knight", "rook", "tempo", "fianchetto", "pin", "fork", "skewer", "notation", "clock", "rating", "variation"],
    &["director", "screenplay", "cinematography", "montage", "premiere", "casting", "soundtrack", "trailer", "editing", "framing", "closeup", "dialogue", "storyboard", "celluloid", "screening", "boxoffice", "sequel", "documentary", "animation", "subtitle", "genre", "auteur", "scene", "footage"],
    &["molecule", "catalyst", "titration", "solvent", "polymer", "isotope", "reaction", "compound", "electron", "valence", "oxidation", "reagent", "distillation", "chromatography", "crystalline", "solution", "precipitate", "buffer", "acidity", "synthesis", "monomer", "lattice", "bond", "equilibrium"],
    &["itinerary", "passport", "hostel", "layover", "excursion", "backpack", "visa", "customs", "souvenir", "guidebook", "trailhead", "ferry", "stopover", "expedition", "lodging", "voyage", "pilgrimage", "checkin", "terminal", "roundtrip", "postcard", "landmark", "detour", "wanderlust"],
    &["facade", "cantilever", "blueprint", "masonry", "atrium", "buttress", "cornice", "girder", "vault", "pillar", "mezzanine", "cladding", "truss", "foundation", "scaffold", "courtyard", "archway", "parapet", "beam", "renovation", "zoning", "skylight", "pavilion", "colonnade"],
    &["encryption", "firewall", "malware", "phishing", "credential", "kerberos", "certificate", "cipher", "entropy", "exploit", "patch", "sandbox", "forensics", "intrusion", "keypair", "signature", "nonce", "hashing", "token", "breach", "hardening", "rotation", "keystore", "zeroday"],
    &["silicon", "transistor", "wafer", "soldering", "capacitor", "resistor", "voltage", "amperage", "oscillator", "firmware", "heatsink", "chipset", "breadboard", "multimeter", "diode", "inductor", "microcontroller", "pcb", "clockspeed", "register", "sram", "pipeline", "interconnect", "substrate"],
    &["shutter", "aperture", "exposure", "viewfinder", "tripod", "bokeh", "negative", "darkroom", "lens", "focal", "iso", "histogram", "portrait", "landscape", "flash", "filter", "composition", "contrast", "saturation", "timelapse", "macro", "panorama", "vignette", "crop"],
    &["cockpit", "altimeter", "fuselage", "aileron", "throttle", "taxiway", "turbulence", "airspeed", "flaps", "rudderpedal", "takeoff", "approach", "holding", "mayday", "transponder", "glideslope", "crosswind", "nosewheel", "hangar", "preflight", "stall", "yoke", "avionics", "airfoil"],
    &["stratum", "sediment", "basalt", "granite", "erosion", "tectonic", "magma", "fossil", "mineral", "quartz", "seismic", "fault", "glacier", "bedrock", "limestone", "volcanic", "crust", "mantle", "outcrop", "moraine", "aquifer", "shale", "geode", "subduction"],
    &["espresso", "roastery", "arabica", "robusta", "grinder", "portafilter", "crema", "tamping", "pourover", "barista", "decaf", "brew", "extraction", "bitterness", "aroma", "cupping", "latte", "cappuccino", "macchiato", "beans", "filtercoffee", "kettle", "bloom", "dose"],
    &["peloton", "derailleur", "sprocket", "pedaling", "handlebar", "saddle", "crankset", "puncture", "sprint", "breakaway", "domestique", "timetrial", "velodrome", "gravel", "downhill", "switchback", "drafting", "chainring", "spokes", "pannier", "criterium", "cleats", "bidon", "gruppetto"],
    &["hive", "queen", "drone", "brood", "nectar", "pollen", "honeycomb", "swarm", "apiary", "beeswax", "propolis", "forager", "smoker", "frame", "colony", "royal", "sting", "waggle", "nucleus", "varroa", "extractor", "super", "cluster", "beekeeper"],
    &["kiln", "glaze", "wheel", "clay", "bisque", "stoneware", "porcelain", "slip", "trimming", "wedging", "earthenware", "firing", "crackle", "underglaze", "throwing", "handbuilding", "coil", "slab", "greenware", "oxide", "raku", "burnish", "grog", "vitrify"],
    &["vineyard", "tannin", "vintage", "terroir", "fermentation", "oak", "barrel", "bouquet", "decant", "sommelier", "varietal", "pressing", "crush", "must", "lees", "malolactic", "appellation", "corked", "finish", "nose", "cellar", "blend", "sweetness", "bottling"],
    &["sawdust", "chisel", "dovetail", "mortise", "tenon", "plane", "lathe", "grain", "hardwood", "veneer", "joinery", "clamp", "sanding", "varnish", "workbench", "jigsaw", "bandsaw", "mallet", "kerf", "miter", "rabbet", "spokeshave", "burl", "lumber"],
    &["plumage", "migration", "warbler", "raptor", "binoculars", "nesting", "songbird", "roost", "fledgling", "beak", "talon", "wingspan", "heron", "kestrel", "flock", "molt", "perch", "birdsong", "sparrow", "falcon", "wader", "crest", "aviary", "sighting"],
    &["angler", "lure", "baitbox", "reel", "trolling", "bait", "trout", "salmon", "flyfishing", "hook", "sinker", "bobber", "creel", "waders", "spawn", "catchrelease", "line", "leader", "spool", "netting", "pike", "minnow", "riverbank", "drift"],
    &["summit", "belay", "crampon", "icefall", "carabiner", "rappel", "crevasse", "ascent", "basecamp", "altitude", "acclimatize", "piton", "ridge", "couloir", "serac", "avalanche", "bivouac", "traverse", "scree", "icefield", "fixed", "rope", "oxygen", "descent"],
    &["typeface", "kerning", "serif", "ligature", "glyph", "baseline", "leading", "italic", "typesetting", "letterpress", "font", "ascender", "descender", "tracking", "xheight", "smallcaps", "grotesque", "foundry", "specimen", "hyphenation", "justification", "widow", "orphan", "em"],
    &["actuator", "servo", "gripper", "kinematics", "sensor", "gyroscope", "autonomy", "manipulator", "locomotion", "gait", "teleoperation", "payload", "encoder", "torque", "gearbox", "chassis", "lidar", "odometry", "waypoint", "calibration", "endeffector", "joint", "linkage", "fleet"],
    &["locomotive", "signal", "junction", "platform", "gauge", "timetable", "carriage", "freight", "siding", "trackbed", "sleeper", "viaduct", "shunting", "coupling", "pantograph", "electrification", "branchline", "terminus", "level", "crossing", "boxcar", "tender", "turntable", "railyard"],
    &["cartographer", "projection", "contour", "meridian", "latitude", "longitude", "scale", "legend", "topographic", "atlas", "surveying", "elevation", "gazetteer", "hachure", "graticule", "datum", "isoline", "basemap", "terrain", "plat", "compass", "bearing", "quadrant", "annotation"],
    &["proscenium", "understudy", "matinee", "monologue", "blocking", "stagecraft", "greenroom", "curtain", "audition", "playwright", "cast", "prop", "wings", "downstage", "upstage", "soliloquy", "intermission", "callback", "dramaturg", "footlights", "backstage", "cue", "troupe", "rehearsal"],
    &["southpaw", "jab", "uppercut", "clinch", "footwork", "sparring", "heavyweight", "ringcraft", "canvas", "knockout", "counterpunch", "bout", "scorecard", "mouthguard", "welterweight", "haymaker", "bobbing", "weaving", "ringside", "cutman", "combination", "guard", "feint", "rounds"],
    &["silage", "paddock", "tractor", "plough", "furrow", "heifer", "pasture", "barley", "fallow", "drainage", "combine", "haybale", "livestock", "dairy", "shearing", "fencing", "manure", "cropland", "sowing", "threshing", "granary", "cattle", "lamb", "acreage"],
    &["blowpipe", "molten", "annealing", "gaffer", "furnace", "cullet", "marver", "punty", "gather", "flamework", "borosilicate", "murrine", "cane", "frit", "kilnwork", "lampwork", "glory", "hole", "shears", "jacks", "optic", "mold", "incalmo", "sculpting"],
];

pub const GENERAL: &[&str] = &[
    "the", "a", "of", "and", "to", "in", "for", "with", "on", "that", "this", "was", "were", "been", "being", "have", "has", "had", "not", "but", "from", "they", "their", "them", "then", "than", "when", "where", "which", "while", "after", "before", "under", "over", "between", "through", "during", "without", "within", "against", "about", "because", "however", "therefore", "although", "meanwhile", "notably", "roughly", "nearly", "almost", "often", "rarely", "usually", "sometimes", "quickly", "slowly", "carefully", "eventually", "recently", "early", "late", "new", "old", "small", "large", "good", "poor", "high", "low", "long", "short", "first", "last", "next", "several", "many", "few", "most", "least", "again", "still", "once", "twice", "together", "apart",
];

/// One document: 40–90 words blending two topics — ~45% from a primary
/// pool, ~25% from a secondary, ~30% general. Blending keeps the corpus
/// clustered enough for semantic search to mean something while avoiding
/// dense blobs of near-duplicate vectors.
pub fn gen_doc(rng: &mut Rng) -> String {
    let primary = rng.below(TOPICS.len());
    let mut secondary = rng.below(TOPICS.len());
    while secondary == primary {
        secondary = rng.below(TOPICS.len());
    }
    let len = 40 + rng.below(51);
    let mut words = Vec::with_capacity(len);
    for _ in 0..len {
        let roll = rng.below(100);
        if roll < 45 {
            words.push(*rng.pick(TOPICS[primary]));
        } else if roll < 70 {
            words.push(*rng.pick(TOPICS[secondary]));
        } else {
            words.push(*rng.pick(GENERAL));
        }
    }
    words.join(" ")
}

/// A 2–3 word query drawn from one topic pool — the realistic shape for
/// both keyword and semantic retrieval over this corpus.
pub fn gen_query(rng: &mut Rng) -> String {
    let topic = TOPICS[rng.below(TOPICS.len())];
    let n = 2 + rng.below(2);
    let mut terms: Vec<&str> = Vec::new();
    while terms.len() < n {
        let w = *rng.pick(topic);
        if !terms.contains(&w) {
            terms.push(w);
        }
    }
    terms.join(" ")
}
