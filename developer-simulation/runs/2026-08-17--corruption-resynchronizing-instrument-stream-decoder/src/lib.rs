use std::collections::HashMap;

#[must_use]
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (0_u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
}

/// Builds a complete wire frame for deterministic fixtures and workloads.
///
/// # Panics
///
/// Panics when `payload` is longer than the wire format's unsigned 16-bit
/// payload-length field. The decoder never calls this generator on input.
#[must_use]
pub fn encode_frame(sequence: u32, payload: &[u8]) -> Vec<u8> {
    let payload_len = u16::try_from(payload.len()).expect("wire payload length fits u16");
    let sequence_bytes = sequence.to_be_bytes();
    let mut crc_input = Vec::with_capacity(4 + payload.len());
    crc_input.extend_from_slice(&sequence_bytes);
    crc_input.extend_from_slice(payload);
    let crc = crc32(&crc_input);

    let mut wire = Vec::with_capacity(payload.len() + 12);
    wire.push(0x02);
    wire.extend_from_slice(&payload_len.to_be_bytes());
    wire.extend_from_slice(&sequence_bytes);
    wire.extend_from_slice(payload);
    wire.extend_from_slice(&crc.to_be_bytes());
    wire.push(0x03);
    wire
}

pub const MAX_WIRE_LEN: usize = 8_192;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameRecord {
    pub connection: u64,
    pub sequence: u32,
    pub payload_len: u16,
    pub payload_digest: u64,
    pub end_offset: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FeedResult {
    pub frames: Vec<FrameRecord>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorClass {
    Noise,
    OversizeLength,
    Marker,
    Crc,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    pub connection: u64,
    pub byte_offset: u64,
    pub declared_payload_len: Option<u16>,
    pub sequence: Option<u32>,
    pub class: ErrorClass,
}

#[derive(Default)]
struct ConnectionState {
    bytes: Vec<u8>,
    base_offset: u64,
    noise_start: Option<u64>,
}

#[derive(Default)]
pub struct Decoder {
    connections: HashMap<u64, ConnectionState>,
    max_retained: usize,
    peak_buffered: usize,
}

#[derive(Default)]
pub struct AppendSearchBaseline {
    connections: HashMap<u64, ConnectionState>,
    resets: u64,
    max_retained: usize,
}

impl AppendSearchBaseline {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, connection: u64, chunk: &[u8]) -> FeedResult {
        let state = self.connections.entry(connection).or_default();
        state.bytes.extend_from_slice(chunk);
        self.max_retained = self.max_retained.max(state.bytes.len());
        let mut result = FeedResult::default();

        loop {
            if state.bytes.len() < 3 {
                break;
            }
            if state.bytes[0] != 0x02 {
                if let Some(start) = state.bytes.iter().position(|byte| *byte == 0x02) {
                    state.bytes.drain(..start);
                    state.base_offset = state.base_offset.saturating_add(usize_to_u64(start));
                    continue;
                }
                break;
            }
            let payload_len = u16::from_be_bytes([state.bytes[1], state.bytes[2]]);
            let wire_len = usize::from(payload_len) + 12;
            let terminator = state
                .bytes
                .iter()
                .enumerate()
                .filter(|(_, byte)| **byte == 0x03)
                .map(|(index, _)| index)
                .find(|index| index + 1 >= wire_len);
            let Some(terminator) = terminator else {
                break;
            };
            if terminator + 1 != wire_len {
                result.diagnostics.push(Diagnostic {
                    connection,
                    byte_offset: state.base_offset,
                    declared_payload_len: Some(payload_len),
                    sequence: read_sequence(&state.bytes),
                    class: ErrorClass::Marker,
                });
                state.base_offset = state
                    .base_offset
                    .saturating_add(usize_to_u64(state.bytes.len()));
                state.bytes.clear();
                self.resets = self.resets.saturating_add(1);
                break;
            }
            let sequence = read_sequence(&state.bytes).unwrap_or(0);
            let crc_at = wire_len - 5;
            let expected_crc = u32::from_be_bytes([
                state.bytes[crc_at],
                state.bytes[crc_at + 1],
                state.bytes[crc_at + 2],
                state.bytes[crc_at + 3],
            ]);
            if crc32(&state.bytes[3..crc_at]) != expected_crc {
                result.diagnostics.push(Diagnostic {
                    connection,
                    byte_offset: state.base_offset,
                    declared_payload_len: Some(payload_len),
                    sequence: Some(sequence),
                    class: ErrorClass::Crc,
                });
                state.base_offset = state
                    .base_offset
                    .saturating_add(usize_to_u64(state.bytes.len()));
                state.bytes.clear();
                self.resets = self.resets.saturating_add(1);
                break;
            }
            result.frames.push(FrameRecord {
                connection,
                sequence,
                payload_len,
                payload_digest: fnv1a64(&state.bytes[7..crc_at]),
                end_offset: state.base_offset.saturating_add(usize_to_u64(wire_len - 1)),
            });
            state.bytes.drain(..wire_len);
            state.base_offset = state.base_offset.saturating_add(usize_to_u64(wire_len));
        }
        result
    }

    #[must_use]
    pub fn reset_count(&self) -> u64 {
        self.resets
    }

    #[must_use]
    pub fn max_retained_bytes(&self) -> usize {
        self.max_retained
    }
}

impl Decoder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, connection: u64, chunk: &[u8]) -> FeedResult {
        let state = self.connections.entry(connection).or_default();
        let mut result = FeedResult::default();
        let mut unread = chunk;
        loop {
            let consumed = process_available(state, connection, &mut result);
            if consumed != 0 {
                state.bytes.drain(..consumed);
                state.base_offset = state.base_offset.saturating_add(usize_to_u64(consumed));
            }
            if unread.is_empty() {
                break;
            }
            let room = MAX_WIRE_LEN.saturating_sub(state.bytes.len());
            if room == 0 {
                state.bytes.remove(0);
                state.base_offset = state.base_offset.saturating_add(1);
                continue;
            }
            let take = room.min(unread.len());
            state.bytes.extend_from_slice(&unread[..take]);
            self.peak_buffered = self.peak_buffered.max(state.bytes.len());
            unread = &unread[take..];
        }
        self.max_retained = self.max_retained.max(state.bytes.len());
        result
    }

    #[must_use]
    pub fn max_retained_per_stream(&self) -> usize {
        self.max_retained
    }

    #[must_use]
    pub fn retained_for(&self, connection: u64) -> usize {
        self.connections
            .get(&connection)
            .map_or(0, |state| state.bytes.len())
    }

    #[must_use]
    pub fn peak_buffered_bytes(&self) -> usize {
        self.peak_buffered
    }
}

fn process_available(
    state: &mut ConnectionState,
    connection: u64,
    result: &mut FeedResult,
) -> usize {
    let mut consumed = 0;
    loop {
        if state.bytes.len().saturating_sub(consumed) < 3 {
            break;
        }
        if state.bytes[consumed] != 0x02 {
            if state.noise_start.is_none() {
                state.noise_start = Some(state.base_offset.saturating_add(usize_to_u64(consumed)));
            }
            consumed += 1;
            continue;
        }
        if let Some(byte_offset) = state.noise_start.take() {
            result.diagnostics.push(Diagnostic {
                connection,
                byte_offset,
                declared_payload_len: None,
                sequence: None,
                class: ErrorClass::Noise,
            });
        }
        let remaining = &state.bytes[consumed..];
        let payload_len = u16::from_be_bytes([remaining[1], remaining[2]]);
        let wire_len = usize::from(payload_len) + 12;
        if wire_len > MAX_WIRE_LEN {
            result.diagnostics.push(Diagnostic {
                connection,
                byte_offset: state.base_offset.saturating_add(usize_to_u64(consumed)),
                declared_payload_len: Some(payload_len),
                sequence: None,
                class: ErrorClass::OversizeLength,
            });
            consumed += 1;
            continue;
        }
        if remaining.len() < wire_len {
            break;
        }
        let candidate = &remaining[..wire_len];
        let sequence = u32::from_be_bytes([candidate[3], candidate[4], candidate[5], candidate[6]]);
        let crc_at = wire_len - 5;
        let expected_crc = u32::from_be_bytes([
            candidate[crc_at],
            candidate[crc_at + 1],
            candidate[crc_at + 2],
            candidate[crc_at + 3],
        ]);
        let marker_valid = candidate[wire_len - 1] == 0x03;
        let crc_valid = crc32(&candidate[3..crc_at]) == expected_crc;
        if marker_valid && crc_valid {
            result.frames.push(FrameRecord {
                connection,
                sequence,
                payload_len,
                payload_digest: fnv1a64(&candidate[7..crc_at]),
                end_offset: state
                    .base_offset
                    .saturating_add(usize_to_u64(consumed + wire_len - 1)),
            });
        } else if let Some(later_start) = find_later_stx(&remaining[..wire_len]) {
            result.diagnostics.push(Diagnostic {
                connection,
                byte_offset: state.base_offset.saturating_add(usize_to_u64(consumed)),
                declared_payload_len: None,
                sequence: None,
                class: ErrorClass::Noise,
            });
            consumed += later_start;
            continue;
        } else if !marker_valid {
            result.diagnostics.push(Diagnostic {
                connection,
                byte_offset: state.base_offset.saturating_add(usize_to_u64(consumed)),
                declared_payload_len: Some(payload_len),
                sequence: Some(sequence),
                class: ErrorClass::Marker,
            });
        } else {
            result.diagnostics.push(Diagnostic {
                connection,
                byte_offset: state.base_offset.saturating_add(usize_to_u64(consumed)),
                declared_payload_len: Some(payload_len),
                sequence: Some(sequence),
                class: ErrorClass::Crc,
            });
        }
        consumed += wire_len;
    }
    consumed
}

fn find_later_stx(bytes: &[u8]) -> Option<usize> {
    bytes[1..]
        .iter()
        .position(|byte| *byte == 0x02)
        .map(|offset| offset + 1)
}

#[must_use]
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut digest = 0xCBF2_9CE4_8422_2325;
    for &byte in bytes {
        digest ^= u64::from(byte);
        digest = digest.wrapping_mul(0x0000_0100_0000_01B3);
    }
    digest
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn read_sequence(bytes: &[u8]) -> Option<u32> {
    let sequence = bytes.get(3..7)?;
    Some(u32::from_be_bytes([
        sequence[0],
        sequence[1],
        sequence[2],
        sequence[3],
    ]))
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PropertySummary {
    pub cases: usize,
    pub largest_input: usize,
    pub peak_buffered: usize,
    pub max_retained: usize,
    pub output_digest: u64,
}

#[must_use]
pub fn run_property_cases(cases: usize, max_len: usize, seed: u64) -> PropertySummary {
    let mut rng = DeterministicRng::new(seed);
    let mut summary = PropertySummary {
        cases,
        ..PropertySummary::default()
    };
    for case_index in 0..cases {
        let modulus = max_len.saturating_add(1);
        let length = if modulus == 0 {
            usize::try_from(rng.next()).unwrap_or(usize::MAX)
        } else {
            usize::try_from(rng.next()).unwrap_or(usize::MAX) % modulus
        };
        let mut bytes = vec![0_u8; length];
        for chunk in bytes.chunks_mut(8) {
            let random = rng.next().to_le_bytes();
            chunk.copy_from_slice(&random[..chunk.len()]);
        }
        let mut decoder = Decoder::new();
        let result = decoder.feed(0, &bytes);
        summary.largest_input = summary.largest_input.max(length);
        summary.peak_buffered = summary.peak_buffered.max(decoder.peak_buffered_bytes());
        summary.max_retained = summary.max_retained.max(decoder.retained_for(0));
        summary.output_digest = summary.output_digest.wrapping_mul(0x0000_0100_0000_01B3)
            ^ usize_to_u64(case_index)
            ^ usize_to_u64(result.frames.len()).rotate_left(17)
            ^ usize_to_u64(result.diagnostics.len()).rotate_left(33);
    }
    summary
}

#[must_use]
pub fn oracle_frame(connection: u64, wire: &[u8]) -> Option<FrameRecord> {
    if wire.first().copied()? != 0x02 || wire.last().copied()? != 0x03 {
        return None;
    }
    let length_bytes: [u8; 2] = wire.get(1..3)?.try_into().ok()?;
    let payload_len = u16::from_be_bytes(length_bytes);
    if wire.len() != usize::from(payload_len) + 12 || wire.len() > MAX_WIRE_LEN {
        return None;
    }
    let sequence_bytes: [u8; 4] = wire.get(3..7)?.try_into().ok()?;
    let sequence = u32::from_be_bytes(sequence_bytes);
    let crc_at = wire.len() - 5;
    let crc_bytes: [u8; 4] = wire.get(crc_at..crc_at + 4)?.try_into().ok()?;
    if oracle_crc32(&wire[3..crc_at]) != u32::from_be_bytes(crc_bytes) {
        return None;
    }
    Some(FrameRecord {
        connection,
        sequence,
        payload_len,
        payload_digest: fnv1a64(&wire[7..crc_at]),
        end_offset: usize_to_u64(wire.len() - 1),
    })
}

fn oracle_crc32(bytes: &[u8]) -> u32 {
    let mut value = 0xFFFF_FFFF;
    for byte in bytes {
        value ^= u32::from(*byte);
        for _ in 0..8 {
            if value & 1 == 1 {
                value = (value >> 1) ^ 0xEDB8_8320;
            } else {
                value >>= 1;
            }
        }
    }
    value ^ 0xFFFF_FFFF
}

#[derive(Clone, Copy, Debug)]
struct DeterministicRng(u64);

impl DeterministicRng {
    fn new(seed: u64) -> Self {
        Self(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }

    fn next(&mut self) -> u64 {
        let mut value = self.0;
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        self.0 = value;
        value
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChunkSchedule {
    WholeFrame,
    OneByte,
    Irregular(u64),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CleanSummary {
    pub frames_expected: usize,
    pub frames_emitted: usize,
    pub wire_bytes: u64,
    pub oracle_digest: u64,
    pub candidate_digest: u64,
    pub trace_digest: u64,
    pub diagnostics: usize,
    pub max_retained: usize,
    pub peak_buffered: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BaselineSummary {
    pub frames_emitted: usize,
    pub wire_bytes: u64,
    pub candidate_digest: u64,
    pub diagnostics: usize,
    pub resets: u64,
    pub max_retained: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DamageSummary {
    pub events: usize,
    pub crc_cases: usize,
    pub oversize_cases: usize,
    pub marker_cases: usize,
    pub garbage_cases: usize,
    pub plausible_header_cases: usize,
    pub damaged_emitted: usize,
    pub followers_expected: usize,
    pub followers_emitted: usize,
    pub followers_immediate: usize,
    pub diagnostics: usize,
    pub max_retained: usize,
    pub peak_buffered: usize,
    pub baseline_resets: u64,
    pub baseline_max_retained: usize,
    pub candidate_followers_by_class: [usize; 4],
    pub baseline_followers_by_class: [usize; 4],
    pub diagnostics_by_error: [usize; 4],
    pub candidate_trace_digest: u64,
    pub candidate_diagnostic_digest: u64,
}

#[must_use]
pub fn run_clean_workload(frames: usize, schedule: ChunkSchedule) -> CleanSummary {
    let mut summary = CleanSummary {
        frames_expected: frames,
        oracle_digest: 0xCBF2_9CE4_8422_2325,
        candidate_digest: 0xCBF2_9CE4_8422_2325,
        trace_digest: 0xCBF2_9CE4_8422_2325,
        ..CleanSummary::default()
    };
    let mut decoder = Decoder::new();
    let mut schedule = ScheduleState::new(schedule);
    let mut sequences = [u32::MAX - 5; 64];

    for frame_index in 0..frames {
        let connection_index = frame_index % 64;
        let connection = usize_to_u64(connection_index);
        let sequence = sequences[connection_index];
        sequences[connection_index] = sequence.wrapping_add(1);
        let payload_len = if frame_index % 20 == 0 { 4_084 } else { 180 };
        let payload = make_payload(frame_index, payload_len);
        let wire = encode_frame(sequence, &payload);
        summary.wire_bytes = summary.wire_bytes.saturating_add(usize_to_u64(wire.len()));
        if let Some(expected) = oracle_frame(connection, &wire) {
            update_tuple_digest(&mut summary.oracle_digest, &expected);
        }
        feed_candidate_component(
            &mut decoder,
            connection,
            &wire,
            &mut schedule,
            |result, _is_last| {
                summary.frames_emitted += result.frames.len();
                summary.diagnostics += result.diagnostics.len();
                for frame in &result.frames {
                    update_tuple_digest(&mut summary.candidate_digest, frame);
                    update_trace_digest(&mut summary.trace_digest, frame);
                }
                for diagnostic in &result.diagnostics {
                    update_diagnostic_digest(&mut summary.trace_digest, diagnostic);
                }
            },
        );
    }
    summary.max_retained = decoder.max_retained_per_stream();
    summary.peak_buffered = decoder.peak_buffered_bytes();
    summary
}

#[must_use]
pub fn run_baseline_clean_workload(frames: usize, schedule: ChunkSchedule) -> BaselineSummary {
    let mut summary = BaselineSummary {
        candidate_digest: 0xCBF2_9CE4_8422_2325,
        ..BaselineSummary::default()
    };
    let mut baseline = AppendSearchBaseline::new();
    let mut schedule = ScheduleState::new(schedule);
    let mut sequences = [u32::MAX - 5; 64];
    for frame_index in 0..frames {
        let connection_index = frame_index % 64;
        let connection = usize_to_u64(connection_index);
        let sequence = sequences[connection_index];
        sequences[connection_index] = sequence.wrapping_add(1);
        let payload_len = if frame_index % 20 == 0 { 4_084 } else { 180 };
        let payload = make_payload(frame_index, payload_len);
        let wire = encode_frame(sequence, &payload);
        summary.wire_bytes = summary.wire_bytes.saturating_add(usize_to_u64(wire.len()));
        let mut offset = 0;
        while offset < wire.len() {
            let chunk_len = schedule.next_size(wire.len() - offset);
            let end = offset.saturating_add(chunk_len).min(wire.len());
            let result = baseline.feed(connection, &wire[offset..end]);
            summary.frames_emitted += result.frames.len();
            summary.diagnostics += result.diagnostics.len();
            for frame in &result.frames {
                update_tuple_digest(&mut summary.candidate_digest, frame);
            }
            offset = end;
        }
    }
    summary.resets = baseline.reset_count();
    summary.max_retained = baseline.max_retained_bytes();
    summary
}

#[must_use]
pub fn run_candidate_benchmark_workload(frames: usize, schedule: ChunkSchedule) -> BaselineSummary {
    let mut summary = BaselineSummary {
        candidate_digest: 0xCBF2_9CE4_8422_2325,
        ..BaselineSummary::default()
    };
    let mut decoder = Decoder::new();
    let mut schedule = ScheduleState::new(schedule);
    let mut sequences = [u32::MAX - 5; 64];
    for frame_index in 0..frames {
        let connection_index = frame_index % 64;
        let connection = usize_to_u64(connection_index);
        let sequence = sequences[connection_index];
        sequences[connection_index] = sequence.wrapping_add(1);
        let payload_len = if frame_index % 20 == 0 { 4_084 } else { 180 };
        let payload = make_payload(frame_index, payload_len);
        let wire = encode_frame(sequence, &payload);
        summary.wire_bytes = summary.wire_bytes.saturating_add(usize_to_u64(wire.len()));
        feed_candidate_component(
            &mut decoder,
            connection,
            &wire,
            &mut schedule,
            |result, _is_last| {
                summary.frames_emitted += result.frames.len();
                summary.diagnostics += result.diagnostics.len();
                for frame in &result.frames {
                    update_tuple_digest(&mut summary.candidate_digest, frame);
                }
            },
        );
    }
    summary.max_retained = decoder.max_retained_per_stream();
    summary
}

pub struct BenchmarkCorpus {
    streams: Vec<Vec<u8>>,
    bytes_per_pass: u64,
}

impl BenchmarkCorpus {
    #[must_use]
    pub fn build(frames: usize) -> Self {
        let mut streams = vec![Vec::new(); 64];
        let mut sequences = [u32::MAX - 5; 64];
        let mut bytes_per_pass = 0_u64;
        for frame_index in 0..frames {
            let connection = frame_index % 64;
            let sequence = sequences[connection];
            sequences[connection] = sequence.wrapping_add(1);
            let payload_len = if frame_index % 20 == 0 { 4_084 } else { 180 };
            let payload = make_payload(frame_index, payload_len);
            let wire = encode_frame(sequence, &payload);
            bytes_per_pass = bytes_per_pass.saturating_add(usize_to_u64(wire.len()));
            streams[connection].extend_from_slice(&wire);
        }
        Self {
            streams,
            bytes_per_pass,
        }
    }

    #[must_use]
    pub fn run_baseline(&self, passes: usize) -> BaselineSummary {
        let mut summary = BaselineSummary {
            wire_bytes: self.bytes_per_pass.saturating_mul(usize_to_u64(passes)),
            candidate_digest: 0xCBF2_9CE4_8422_2325,
            ..BaselineSummary::default()
        };
        let mut baseline = AppendSearchBaseline::new();
        for _ in 0..passes {
            for (connection, stream) in self.streams.iter().enumerate() {
                for chunk in stream.chunks(4_096) {
                    let result = baseline.feed(usize_to_u64(connection), chunk);
                    summary.frames_emitted += result.frames.len();
                    summary.diagnostics += result.diagnostics.len();
                    for frame in &result.frames {
                        update_tuple_digest(&mut summary.candidate_digest, frame);
                    }
                }
            }
        }
        summary.resets = baseline.reset_count();
        summary.max_retained = baseline.max_retained_bytes();
        summary
    }

    #[must_use]
    pub fn run_candidate(&self, passes: usize) -> BaselineSummary {
        let mut summary = BaselineSummary {
            wire_bytes: self.bytes_per_pass.saturating_mul(usize_to_u64(passes)),
            candidate_digest: 0xCBF2_9CE4_8422_2325,
            ..BaselineSummary::default()
        };
        let mut decoder = Decoder::new();
        for _ in 0..passes {
            for (connection, stream) in self.streams.iter().enumerate() {
                for chunk in stream.chunks(4_096) {
                    let result = decoder.feed(usize_to_u64(connection), chunk);
                    summary.frames_emitted += result.frames.len();
                    summary.diagnostics += result.diagnostics.len();
                    for frame in &result.frames {
                        update_tuple_digest(&mut summary.candidate_digest, frame);
                    }
                }
            }
        }
        summary.max_retained = decoder.max_retained_per_stream();
        summary
    }
}

#[must_use]
pub fn run_damaged_workload(events: usize, schedule: ChunkSchedule) -> DamageSummary {
    let mut summary = DamageSummary {
        events,
        followers_expected: events.saturating_mul(2),
        candidate_trace_digest: 0xCBF2_9CE4_8422_2325,
        candidate_diagnostic_digest: 0xCBF2_9CE4_8422_2325,
        ..DamageSummary::default()
    };
    let mut decoder = Decoder::new();
    let mut baseline = AppendSearchBaseline::new();
    let mut schedule = ScheduleState::new(schedule);
    let mut sequences = [10_000_u32; 64];

    for event_index in 0..events {
        let connection_index = event_index % 64;
        let connection = usize_to_u64(connection_index);
        let damaged_sequence = sequences[connection_index];
        sequences[connection_index] = damaged_sequence.wrapping_add(1);
        let (class_index, damage) = make_damage(event_index, damaged_sequence, &mut summary);
        feed_both_component(
            &mut decoder,
            &mut baseline,
            connection,
            &damage,
            &mut schedule,
            |result, _baseline_result, _is_last| {
                summary.damaged_emitted += result.frames.len();
                summary.diagnostics += result.diagnostics.len();
                count_diagnostics(&mut summary.diagnostics_by_error, result);
                update_damage_digests(
                    &mut summary.candidate_trace_digest,
                    &mut summary.candidate_diagnostic_digest,
                    result,
                );
            },
        );

        for follower_index in 0..2 {
            let sequence = sequences[connection_index];
            sequences[connection_index] = sequence.wrapping_add(1);
            let payload = make_payload(
                event_index.saturating_mul(2).saturating_add(follower_index),
                180,
            );
            let follower = encode_frame(sequence, &payload);
            feed_both_component(
                &mut decoder,
                &mut baseline,
                connection,
                &follower,
                &mut schedule,
                |result, baseline_result, is_last| {
                    let emitted = result
                        .frames
                        .iter()
                        .filter(|frame| frame.sequence == sequence)
                        .count();
                    summary.followers_emitted += emitted;
                    summary.candidate_followers_by_class[class_index] += emitted;
                    summary.baseline_followers_by_class[class_index] += baseline_result
                        .frames
                        .iter()
                        .filter(|frame| frame.sequence == sequence)
                        .count();
                    if is_last {
                        summary.followers_immediate += emitted;
                    }
                    summary.diagnostics += result.diagnostics.len();
                    count_diagnostics(&mut summary.diagnostics_by_error, result);
                    update_damage_digests(
                        &mut summary.candidate_trace_digest,
                        &mut summary.candidate_diagnostic_digest,
                        result,
                    );
                },
            );
        }
    }
    summary.max_retained = decoder.max_retained_per_stream();
    summary.peak_buffered = decoder.peak_buffered_bytes();
    summary.baseline_resets = baseline.reset_count();
    summary.baseline_max_retained = baseline.max_retained_bytes();
    summary
}

fn make_damage(
    event_index: usize,
    damaged_sequence: u32,
    summary: &mut DamageSummary,
) -> (usize, Vec<u8>) {
    match event_index % 10 {
        0..=3 => {
            summary.crc_cases += 1;
            let mut wire = encode_frame(damaged_sequence, b"crc damage");
            let crc_at = wire.len() - 5;
            wire[crc_at] ^= 0x01;
            (0, wire)
        }
        4..=5 => {
            summary.oversize_cases += 1;
            (1, vec![0x02, 0x1F, 0xF5])
        }
        6..=7 => {
            summary.marker_cases += 1;
            let mut wire = encode_frame(damaged_sequence, b"marker damage");
            if let Some(marker) = wire.last_mut() {
                *marker = 0x7E;
            }
            (2, wire)
        }
        _ => {
            summary.garbage_cases += 1;
            summary.plausible_header_cases += 1;
            (3, vec![0x02, 0x00, 0x13])
        }
    }
}

struct ScheduleState {
    schedule: ChunkSchedule,
    rng: DeterministicRng,
}

impl ScheduleState {
    fn new(schedule: ChunkSchedule) -> Self {
        let seed = match schedule {
            ChunkSchedule::Irregular(seed) => seed,
            ChunkSchedule::WholeFrame | ChunkSchedule::OneByte => 1,
        };
        Self {
            schedule,
            rng: DeterministicRng::new(seed),
        }
    }

    fn next_size(&mut self, remaining: usize) -> usize {
        match self.schedule {
            ChunkSchedule::WholeFrame => remaining,
            ChunkSchedule::OneByte => 1,
            ChunkSchedule::Irregular(_) => {
                usize::try_from(self.rng.next() % 4_096 + 1).unwrap_or(1)
            }
        }
    }
}

fn feed_candidate_component(
    decoder: &mut Decoder,
    connection: u64,
    bytes: &[u8],
    schedule: &mut ScheduleState,
    mut observe: impl FnMut(&FeedResult, bool),
) {
    let mut offset = 0;
    while offset < bytes.len() {
        let chunk_len = schedule.next_size(bytes.len() - offset);
        let end = offset.saturating_add(chunk_len).min(bytes.len());
        let result = decoder.feed(connection, &bytes[offset..end]);
        observe(&result, end == bytes.len());
        offset = end;
    }
}

fn feed_both_component(
    decoder: &mut Decoder,
    baseline: &mut AppendSearchBaseline,
    connection: u64,
    bytes: &[u8],
    schedule: &mut ScheduleState,
    mut observe: impl FnMut(&FeedResult, &FeedResult, bool),
) {
    let mut offset = 0;
    while offset < bytes.len() {
        let chunk_len = schedule.next_size(bytes.len() - offset);
        let end = offset.saturating_add(chunk_len).min(bytes.len());
        let result = decoder.feed(connection, &bytes[offset..end]);
        let baseline_result = baseline.feed(connection, &bytes[offset..end]);
        observe(&result, &baseline_result, end == bytes.len());
        offset = end;
    }
}

fn count_diagnostics(counts: &mut [usize; 4], result: &FeedResult) {
    for diagnostic in &result.diagnostics {
        let index = match diagnostic.class {
            ErrorClass::Crc => 0,
            ErrorClass::OversizeLength => 1,
            ErrorClass::Marker => 2,
            ErrorClass::Noise => 3,
        };
        counts[index] += 1;
    }
}

fn update_damage_digests(frame_digest: &mut u64, diagnostic_digest: &mut u64, result: &FeedResult) {
    for frame in &result.frames {
        update_trace_digest(frame_digest, frame);
    }
    for diagnostic in &result.diagnostics {
        update_diagnostic_digest(diagnostic_digest, diagnostic);
    }
}

fn make_payload(frame_index: usize, payload_len: usize) -> Vec<u8> {
    let mut payload = Vec::with_capacity(payload_len);
    for byte_index in 0..payload_len {
        let value = frame_index
            .wrapping_mul(31)
            .wrapping_add(byte_index.wrapping_mul(17))
            & 0xFF;
        payload.push(u8::try_from(value).unwrap_or(0));
    }
    if let Some(first) = payload.first_mut() {
        *first = 0x02;
    }
    if let Some(second) = payload.get_mut(1) {
        *second = 0x03;
    }
    payload
}

fn update_tuple_digest(digest: &mut u64, frame: &FrameRecord) {
    digest_mix(digest, frame.connection);
    digest_mix(digest, u64::from(frame.sequence));
    digest_mix(digest, frame.payload_digest);
}

fn update_trace_digest(digest: &mut u64, frame: &FrameRecord) {
    update_tuple_digest(digest, frame);
    digest_mix(digest, u64::from(frame.payload_len));
    digest_mix(digest, frame.end_offset);
}

fn update_diagnostic_digest(digest: &mut u64, diagnostic: &Diagnostic) {
    digest_mix(digest, diagnostic.connection);
    digest_mix(digest, diagnostic.byte_offset);
    digest_mix(
        digest,
        diagnostic.declared_payload_len.map_or(u64::MAX, u64::from),
    );
    digest_mix(digest, diagnostic.sequence.map_or(u64::MAX, u64::from));
    let class = match diagnostic.class {
        ErrorClass::Noise => 1,
        ErrorClass::OversizeLength => 2,
        ErrorClass::Marker => 3,
        ErrorClass::Crc => 4,
    };
    digest_mix(digest, class);
}

fn digest_mix(digest: &mut u64, value: u64) {
    *digest ^= value;
    *digest = digest.wrapping_mul(0x0000_0100_0000_01B3);
}

#[cfg(test)]
mod tests {
    fn decode_with_sizes(bytes: &[u8], mut next_size: impl FnMut() -> usize) -> crate::FeedResult {
        let mut decoder = crate::Decoder::new();
        let mut aggregate = crate::FeedResult::default();
        let mut offset = 0;
        while offset < bytes.len() {
            let end = (offset + next_size().max(1)).min(bytes.len());
            let result = decoder.feed(41, &bytes[offset..end]);
            aggregate.frames.extend(result.frames);
            aggregate.diagnostics.extend(result.diagnostics);
            offset = end;
        }
        aggregate
    }

    #[test]
    fn crc_matches_iso_hdlc_check_vector() {
        // Would fail if reflection, polynomial, initial value, or final XOR changed.
        assert_eq!(crate::crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn empty_frame_has_exact_wire_layout() {
        // Would fail on wrong endianness, field order, CRC coverage, or wire length.
        assert_eq!(
            crate::encode_frame(0x0102_0304, &[]),
            [
                0x02, 0x00, 0x00, 0x01, 0x02, 0x03, 0x04, 0xB6, 0x3C, 0xFB, 0xCD, 0x03,
            ]
        );
    }

    #[test]
    fn candidate_decodes_valid_frame_across_every_single_split() {
        // Would fail if any header/payload/CRC/marker boundary loses state.
        let wire = crate::encode_frame(7, b"a\x02b\x03c");
        for split in 0..=wire.len() {
            let mut decoder = crate::Decoder::new();
            let first = decoder.feed(19, &wire[..split]);
            let second = decoder.feed(19, &wire[split..]);
            if split < wire.len() {
                assert!(first.frames.is_empty(), "split {split} emitted early");
            }
            let frames: Vec<_> = first.frames.iter().chain(&second.frames).copied().collect();
            assert_eq!(
                frames,
                [crate::FrameRecord {
                    connection: 19,
                    sequence: 7,
                    payload_len: 5,
                    payload_digest: 0x8767_670E_576E_C006,
                    end_offset: 16,
                }],
                "split {split}"
            );
            assert!(first.diagnostics.is_empty());
            assert!(second.diagnostics.is_empty());
            assert!(decoder.max_retained_per_stream() <= crate::MAX_WIRE_LEN);
        }
    }

    #[test]
    fn candidate_never_emits_valid_nested_payload_before_outer_is_proved() {
        // Would fail if a valid-looking sequence inside opaque payload could preempt its outer frame.
        let mut payload = crate::encode_frame(777, b"aa");
        payload.extend_from_slice(&[0xA5; 32]);
        let wire = crate::encode_frame(42, &payload);

        let single = decode_with_sizes(&wire, || wire.len());
        assert_eq!(
            single
                .frames
                .iter()
                .map(|frame| frame.sequence)
                .collect::<Vec<_>>(),
            [42]
        );
        assert!(single.diagnostics.is_empty());

        let one_byte = decode_with_sizes(&wire, || 1);
        assert_eq!(one_byte.frames, single.frames);
        assert_eq!(one_byte.diagnostics, single.diagnostics);
    }

    #[test]
    fn candidate_rejects_oversize_header_then_recovers_at_next_stx() {
        // Would fail if the decoder waited for 8,192 payload bytes or skipped the follower.
        let mut bytes = vec![0x02, 0x20, 0x00];
        bytes.extend_from_slice(&crate::encode_frame(91, b"ok"));
        let mut decoder = crate::Decoder::new();
        let result = decoder.feed(4, &bytes);
        assert_eq!(result.frames.len(), 1);
        assert_eq!(result.frames[0].sequence, 91);
        assert_eq!(result.diagnostics.len(), 2);
        assert_eq!(
            result.diagnostics[0],
            crate::Diagnostic {
                connection: 4,
                byte_offset: 0,
                declared_payload_len: Some(8_192),
                sequence: None,
                class: crate::ErrorClass::OversizeLength,
            }
        );
        assert_eq!(result.diagnostics[1].class, crate::ErrorClass::Noise);
        assert_eq!(result.diagnostics[1].byte_offset, 1);
        assert_eq!(decoder.retained_for(4), 0);
    }

    #[test]
    fn candidate_defers_nested_followers_until_plausible_header_fails() {
        // Would fail if ambiguous nested bytes were emitted before the plausible outer candidate ended.
        let mut bytes = vec![0x02, 0x00, 0x13];
        bytes.extend_from_slice(&crate::encode_frame(101, b"aa"));
        bytes.extend_from_slice(&crate::encode_frame(102, b"bb"));
        assert_eq!(bytes.len(), 31);

        let mut single_decoder = crate::Decoder::new();
        let single = single_decoder.feed(77, &bytes);
        assert_eq!(
            single
                .frames
                .iter()
                .map(|frame| (frame.sequence, frame.end_offset))
                .collect::<Vec<_>>(),
            [(101, 16), (102, 30)]
        );
        assert_eq!(
            single.diagnostics,
            [crate::Diagnostic {
                connection: 77,
                byte_offset: 0,
                declared_payload_len: None,
                sequence: None,
                class: crate::ErrorClass::Noise,
            }]
        );

        let mut byte_decoder = crate::Decoder::new();
        let mut byte_frames = Vec::new();
        let mut byte_diagnostics = Vec::new();
        let mut emission_calls = Vec::new();
        for (index, byte) in bytes.iter().enumerate() {
            let result = byte_decoder.feed(77, std::slice::from_ref(byte));
            if !result.frames.is_empty() {
                emission_calls.push((
                    index,
                    result
                        .frames
                        .iter()
                        .map(|frame| frame.sequence)
                        .collect::<Vec<_>>(),
                ));
            }
            byte_frames.extend(result.frames);
            byte_diagnostics.extend(result.diagnostics);
        }
        assert_eq!(emission_calls, [(30, vec![101, 102])]);
        assert_eq!(byte_frames, single.frames);
        assert_eq!(byte_diagnostics, single.diagnostics);
    }

    #[test]
    fn candidate_preserves_nested_start_after_plausible_header_fails() {
        // Would fail if rejecting the false candidate consumed an incomplete nested frame.
        let mut bytes = vec![0x02, 0x00, 0x13];
        bytes.extend_from_slice(&crate::encode_frame(201, &[0xA5; 180]));
        bytes.extend_from_slice(&crate::encode_frame(202, &[0x5A; 180]));

        let single = decode_with_sizes(&bytes, || bytes.len());
        assert_eq!(
            single
                .frames
                .iter()
                .map(|frame| frame.sequence)
                .collect::<Vec<_>>(),
            [201, 202]
        );
        let one_byte = decode_with_sizes(&bytes, || 1);
        assert_eq!(one_byte.frames, single.frames);
        assert_eq!(one_byte.diagnostics, single.diagnostics);
    }

    #[test]
    fn candidate_reports_bad_end_marker_and_emits_immediate_follower() {
        // Would fail if marker recovery cleared the whole buffered connection.
        let mut damaged = crate::encode_frame(11, b"bad");
        *damaged.last_mut().expect("test frame is non-empty") = 0x7E;
        damaged.extend_from_slice(&crate::encode_frame(12, b"good"));
        let mut decoder = crate::Decoder::new();
        let result = decoder.feed(8, &damaged);
        assert_eq!(
            result
                .frames
                .iter()
                .map(|frame| frame.sequence)
                .collect::<Vec<_>>(),
            [12]
        );
        assert_eq!(
            result.diagnostics,
            [crate::Diagnostic {
                connection: 8,
                byte_offset: 0,
                declared_payload_len: Some(3),
                sequence: Some(11),
                class: crate::ErrorClass::Marker,
            }]
        );
    }

    #[test]
    fn candidate_reports_crc_flip_and_never_emits_damaged_frame() {
        // Would fail if CRC covered only payload, or if rejection lost the follower.
        let mut damaged = crate::encode_frame(21, b"crc");
        damaged[7] ^= 0x01;
        damaged.extend_from_slice(&crate::encode_frame(22, b"next"));
        let mut decoder = crate::Decoder::new();
        let result = decoder.feed(9, &damaged);
        assert_eq!(
            result
                .frames
                .iter()
                .map(|frame| frame.sequence)
                .collect::<Vec<_>>(),
            [22]
        );
        assert_eq!(
            result.diagnostics,
            [crate::Diagnostic {
                connection: 9,
                byte_offset: 0,
                declared_payload_len: Some(3),
                sequence: Some(21),
                class: crate::ErrorClass::Crc,
            }]
        );
    }

    #[test]
    fn candidate_never_buffers_more_than_max_wire_from_large_chunk() {
        // Would fail if feed appended an arbitrary read wholesale before parsing it.
        let mut decoder = crate::Decoder::new();
        let bytes = vec![0x55; 32 * 1_024];
        let result = decoder.feed(3, &bytes);
        assert!(result.frames.is_empty());
        assert!(decoder.peak_buffered_bytes() <= crate::MAX_WIRE_LEN);
        assert!(decoder.retained_for(3) < 3);
    }

    #[test]
    fn append_search_baseline_resets_and_loses_follower_after_missing_marker() {
        // Would fail if the modeled baseline recovered without its connection-wide reset.
        let mut bytes = crate::encode_frame(31, b"damaged");
        *bytes.last_mut().expect("test frame is non-empty") = 0x7E;
        bytes.extend_from_slice(&crate::encode_frame(32, b"follower"));
        let mut baseline = crate::AppendSearchBaseline::new();
        let result = baseline.feed(1, &bytes);
        assert!(result.frames.is_empty());
        assert_eq!(baseline.reset_count(), 1);

        let mut candidate = crate::Decoder::new();
        let recovered = candidate.feed(1, &bytes);
        assert_eq!(
            recovered
                .frames
                .iter()
                .map(|frame| frame.sequence)
                .collect::<Vec<_>>(),
            [32]
        );
    }

    #[test]
    fn arbitrary_byte_runner_keeps_every_case_bounded() {
        // Would fail on retained-buffer growth or if the requested case count were truncated.
        let summary = crate::run_property_cases(1_000, 32 * 1_024, 0xA17E_2026);
        assert_eq!(summary.cases, 1_000);
        assert!(summary.largest_input <= 32 * 1_024);
        assert!(summary.peak_buffered <= crate::MAX_WIRE_LEN);
        assert!(summary.max_retained <= crate::MAX_WIRE_LEN);
    }

    #[test]
    fn fifty_chunk_schedules_match_single_chunk_trace() {
        // Would fail if buffering or diagnostic boundaries depended on read chunking.
        let mut bytes = crate::encode_frame(u32::MAX, &[]);
        bytes.extend_from_slice(&crate::encode_frame(0, &[0x02, 0x03, 0x02, 0x03]));
        let mut crc_bad = crate::encode_frame(7, b"crc-bad");
        crc_bad[8] ^= 0x80;
        bytes.extend_from_slice(&crc_bad);
        bytes.extend_from_slice(&crate::encode_frame(6, b"decreasing"));
        bytes.extend_from_slice(&[0x02, 0xFF, 0xFF]);
        bytes.extend_from_slice(&crate::encode_frame(6, b"duplicate"));
        let mut marker_bad = crate::encode_frame(8, b"marker-bad");
        *marker_bad.last_mut().expect("test frame is non-empty") = 0x7E;
        bytes.extend_from_slice(&marker_bad);
        bytes.extend_from_slice(&crate::encode_frame(9, &vec![0x03; 8_180]));

        let oracle = decode_with_sizes(&bytes, || bytes.len());
        let one_byte = decode_with_sizes(&bytes, || 1);
        assert_eq!(one_byte, oracle);

        for seed in 1_u64..=50 {
            let mut state = seed;
            let trace = decode_with_sizes(&bytes, || {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                usize::try_from(state % 4_096 + 1).expect("bounded chunk size fits usize")
            });
            assert_eq!(trace, oracle, "seed {seed}");
        }
    }

    #[test]
    fn whole_frame_oracle_requires_exact_length_markers_and_crc() {
        // Would fail if the oracle shared incremental recovery assumptions.
        let valid = crate::encode_frame(44, b"oracle");
        assert_eq!(
            crate::oracle_frame(5, &valid).map(|frame| frame.sequence),
            Some(44)
        );
        let mut trailing = valid.clone();
        trailing.push(0);
        assert!(crate::oracle_frame(5, &trailing).is_none());
        let mut crc_bad = valid.clone();
        crc_bad[7] ^= 1;
        assert!(crate::oracle_frame(5, &crc_bad).is_none());
        let mut marker_bad = valid;
        *marker_bad.last_mut().expect("test frame is non-empty") = 0;
        assert!(crate::oracle_frame(5, &marker_bad).is_none());
    }

    #[test]
    fn candidate_coalesces_noise_diagnostic_across_chunks() {
        // Would fail if diagnostic boundaries followed read calls rather than wire bytes.
        let mut decoder = crate::Decoder::new();
        let first = decoder.feed(6, &[0x55]);
        let mut tail = vec![0x66];
        tail.extend_from_slice(&crate::encode_frame(3, b"ok"));
        let second = decoder.feed(6, &tail);
        assert!(first.diagnostics.is_empty());
        assert_eq!(
            second.diagnostics,
            [crate::Diagnostic {
                connection: 6,
                byte_offset: 0,
                declared_payload_len: None,
                sequence: None,
                class: crate::ErrorClass::Noise,
            }]
        );
        assert_eq!(second.frames.len(), 1);
    }

    #[test]
    fn scaled_workloads_preserve_exact_shape_and_recovery_counts() {
        // Would fail if the bounded harness silently changed the 95/5 or damage ratios.
        let clean = crate::run_clean_workload(2_000, crate::ChunkSchedule::Irregular(42));
        assert_eq!(clean.frames_expected, 2_000);
        assert_eq!(clean.frames_emitted, 2_000);
        assert_eq!(clean.wire_bytes, 774_400);
        assert_eq!(clean.oracle_digest, clean.candidate_digest);
        assert_eq!(clean.diagnostics, 0);
        assert!(clean.max_retained <= crate::MAX_WIRE_LEN);

        let baseline =
            crate::run_baseline_clean_workload(2_000, crate::ChunkSchedule::Irregular(42));
        assert_eq!(baseline.frames_emitted, 2_000);
        assert_eq!(baseline.wire_bytes, 774_400);
        assert_eq!(baseline.candidate_digest, clean.oracle_digest);
        assert_eq!(baseline.diagnostics, 0);
        assert_eq!(baseline.resets, 0);

        let damaged = crate::run_damaged_workload(50, crate::ChunkSchedule::Irregular(91));
        assert_eq!(damaged.crc_cases, 20);
        assert_eq!(damaged.oversize_cases, 10);
        assert_eq!(damaged.marker_cases, 10);
        assert_eq!(damaged.garbage_cases, 10);
        assert_eq!(damaged.plausible_header_cases, 10);
        assert_eq!(damaged.diagnostics_by_error[1], 10);
        assert_eq!(damaged.damaged_emitted, 0);
        assert_eq!(damaged.followers_expected, 100);
        assert_eq!(damaged.followers_emitted, 100);
        assert_eq!(damaged.followers_immediate, 100);
        assert!(damaged.max_retained <= crate::MAX_WIRE_LEN);

        let damaged_whole = crate::run_damaged_workload(50, crate::ChunkSchedule::WholeFrame);
        let damaged_bytes = crate::run_damaged_workload(50, crate::ChunkSchedule::OneByte);
        assert_ne!(damaged.candidate_trace_digest, 0);
        assert_eq!(
            damaged_whole.candidate_trace_digest,
            damaged.candidate_trace_digest
        );
        assert_eq!(
            damaged_bytes.candidate_trace_digest,
            damaged.candidate_trace_digest
        );
        assert_eq!(
            damaged_whole.candidate_diagnostic_digest,
            damaged.candidate_diagnostic_digest
        );
        assert_eq!(
            damaged_bytes.candidate_diagnostic_digest,
            damaged.candidate_diagnostic_digest
        );
        assert_eq!(damaged_whole.followers_immediate, 100);
        assert_eq!(damaged_bytes.followers_immediate, 100);
    }

    #[test]
    fn declared_length_edges_accept_8180_and_reject_8181_and_65535() {
        // Would fail if the 8,192-byte wire limit were applied to payload length.
        for payload_len in [0, 1, 8_180] {
            let payload = vec![0xA5; payload_len];
            let wire = crate::encode_frame(70, &payload);
            let mut decoder = crate::Decoder::new();
            let result = decoder.feed(2, &wire);
            assert_eq!(result.frames.len(), 1, "payload length {payload_len}");
            assert!(result.diagnostics.is_empty());
        }
        for declared in [8_181_u16, u16::MAX] {
            let mut bytes = vec![0x02];
            bytes.extend_from_slice(&declared.to_be_bytes());
            bytes.extend_from_slice(&crate::encode_frame(71, b"recovered"));
            let mut decoder = crate::Decoder::new();
            let result = decoder.feed(2, &bytes);
            assert_eq!(result.frames.len(), 1, "declared length {declared}");
            assert_eq!(result.frames[0].sequence, 71);
            assert_eq!(
                result.diagnostics[0].class,
                crate::ErrorClass::OversizeLength
            );
        }
    }

    #[test]
    fn partial_malicious_connection_does_not_affect_other_63_streams() {
        // Would fail if per-connection framing state or bounds were shared.
        let max_wire = crate::encode_frame(90, &vec![0x03; 8_180]);
        let mut decoder = crate::Decoder::new();
        assert!(decoder.feed(0, &max_wire[..1]).frames.is_empty());
        for connection in 1..64 {
            let result = decoder.feed(connection, &max_wire);
            assert_eq!(result.frames.len(), 1, "connection {connection}");
        }
        let mut malicious_emitted = 0;
        for byte in &max_wire[1..] {
            let result = decoder.feed(0, std::slice::from_ref(byte));
            malicious_emitted += result.frames.len();
        }
        assert_eq!(malicious_emitted, 1);
        assert!(decoder.max_retained_per_stream() <= crate::MAX_WIRE_LEN);
        assert!(decoder.peak_buffered_bytes() <= crate::MAX_WIRE_LEN);
    }

    #[test]
    fn prebuilt_benchmark_corpus_is_identical_for_both_parsers() {
        // Would fail if timed parsers received different bytes or chunk boundaries.
        let corpus = crate::BenchmarkCorpus::build(2_000);
        let baseline = corpus.run_baseline(2);
        let candidate = corpus.run_candidate(2);
        assert_eq!(baseline.frames_emitted, 4_000);
        assert_eq!(candidate.frames_emitted, 4_000);
        assert_eq!(baseline.wire_bytes, 1_548_800);
        assert_eq!(candidate.wire_bytes, baseline.wire_bytes);
        assert_eq!(candidate.candidate_digest, baseline.candidate_digest);
        assert_eq!(baseline.diagnostics, 0);
        assert_eq!(candidate.diagnostics, 0);
    }
}
