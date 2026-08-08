use std::process::ExitCode;

fn main() -> ExitCode {
    ocr_redaction_remap::entry(std::env::args().skip(1).collect())
}
