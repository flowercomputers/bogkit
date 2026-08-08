use std::path::PathBuf;
use std::time::Instant;

use flash_config_journal::{
    ERASE_BLOCKS, FLASH_BYTES, FileNor, JOURNAL_WORKING_MEMORY_CEILING_BYTES, MAX_CONFIG_BYTES,
    MIN_CONFIG_BYTES, PatternReader, corrupt_payload_byte, scan, verify_pattern, write_config,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let image_path = PathBuf::from("target/flash-config-journal-demo.bin");
    let mut flash = FileNor::create_fresh(&image_path)?;

    let old = write_config(
        &mut flash,
        100,
        MIN_CONFIG_BYTES,
        &mut PatternReader::new(11, MIN_CONFIG_BYTES),
    )?;
    let new = write_config(
        &mut flash,
        101,
        MAX_CONFIG_BYTES,
        &mut PatternReader::new(29, MAX_CONFIG_BYTES),
    )?;
    let update_erase_count = flash.erase_counts().iter().sum::<u32>();
    drop(flash);

    let reopened = FileNor::open(&image_path)?;
    let started = Instant::now();
    let boot = scan(&reopened)?;
    let boot_elapsed = started.elapsed();
    let active = boot.active.ok_or("no valid configuration after reopen")?;
    if active != new || !verify_pattern(&reopened, active, 29)? {
        return Err("reopened configuration is not the complete new value".into());
    }
    println!(
        "reopen: revision {}, {} bytes, {} blocks scanned, {:?}",
        active.revision, active.payload_len, boot.scanned_blocks, boot_elapsed
    );
    drop(reopened);

    let mut flash = FileNor::open(&image_path)?;
    let corrupted_at = corrupt_payload_byte(&mut flash, new, 17)?;
    drop(flash);

    let reopened = FileNor::open(&image_path)?;
    let fallback = scan(&reopened)?
        .active
        .ok_or("no valid fallback after deterministic corruption")?;
    if fallback != old || !verify_pattern(&reopened, fallback, 11)? {
        return Err("corruption did not fall back to the complete old value".into());
    }

    println!(
        "corruption: changed new payload byte {corrupted_at}; boot rejected revision {} and recovered revision {}",
        new.revision, fallback.revision
    );
    println!(
        "model bounds: {FLASH_BYTES} flash bytes, {ERASE_BLOCKS} erase blocks, {}-byte explicit-buffer design budget; whole stack unresolved",
        JOURNAL_WORKING_MEMORY_CEILING_BYTES
    );
    println!(
        "file-backed emulator: {} ({} erases during this process)",
        image_path.display(),
        update_erase_count
    );
    Ok(())
}
