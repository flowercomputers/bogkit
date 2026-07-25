use std::time::Duration;

pub fn opening_banner_reveal_duration(line_count: usize, delay_ms: u64) -> Duration {
    Duration::from_millis(delay_ms) * line_count.saturating_sub(1) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reveal_duration_counts_gaps_between_lines() {
        assert_eq!(
            opening_banner_reveal_duration(3, 75),
            Duration::from_millis(150)
        );
        assert_eq!(opening_banner_reveal_duration(0, 75), Duration::ZERO);
    }
}
