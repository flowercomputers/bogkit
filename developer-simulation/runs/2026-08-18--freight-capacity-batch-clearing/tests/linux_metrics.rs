use freight_clearing::parse_linux_peak_rss_bytes;

#[test]
fn linux_vmhwm_is_converted_from_kibibytes_to_bytes() {
    // Catches reporting the Linux high-water mark in KiB while labelling it
    // as bytes in machine-readable benchmark output.
    let status = "Name:\tfreight\nVmRSS:\t 120000 kB\nVmHWM:\t 123456 kB\n";

    assert_eq!(parse_linux_peak_rss_bytes(status), Some(126_418_944));
}
