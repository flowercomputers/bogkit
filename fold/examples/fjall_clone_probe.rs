//! Empirical probe: is clone-then-open of a quiesced fjall dir valid?
//! 1. Open db A, write, persist(SyncAll).
//! 2. While A is still open, `cp -c` (APFS clonefile) the dir.
//! 3. Open the clone as db B in the same process; verify contents.
//! 4. Diverge writes in A and B; verify isolation.
//! 5. Try to re-open A's dir while A is open; expect Locked.

use fjall::{Database, KeyspaceCreateOptions, PersistMode};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let base = std::env::temp_dir().join(format!("fjall_clone_probe_{}", std::process::id()));
    let orig = base.join("orig");
    let clone = base.join("clone");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base)?;

    // 1. create + fill original
    let db_a = Database::builder(&orig).open()?;
    let ks_a = db_a.keyspace("items", KeyspaceCreateOptions::default)?;
    for i in 0..10_000u32 {
        ks_a.insert(i.to_be_bytes(), format!("value-{i}"))?;
    }
    db_a.persist(PersistMode::SyncAll)?;

    // 2. clonefile the dir while db_a is OPEN
    let st = std::process::Command::new("cp")
        .args(["-cR"])
        .arg(&orig)
        .arg(&clone)
        .status()?;
    assert!(st.success(), "cp -cR failed");
    println!("cloned dir while original open: OK");

    // 3. open clone in same process while original still open
    let db_b = Database::builder(&clone).open()?;
    let ks_b = db_b.keyspace("items", KeyspaceCreateOptions::default)?;
    assert_eq!(ks_b.len()?, 10_000, "clone should see all 10k items");
    assert_eq!(
        ks_b.get(5_000u32.to_be_bytes())?.as_deref(),
        Some(b"value-5000".as_ref())
    );
    println!("clone opened + verified 10k items: OK");

    // 4. diverge
    ks_a.insert(99_999u32.to_be_bytes(), "only-in-a")?;
    ks_b.insert(88_888u32.to_be_bytes(), "only-in-b")?;
    db_a.persist(PersistMode::SyncAll)?;
    db_b.persist(PersistMode::SyncAll)?;
    assert!(ks_a.get(88_888u32.to_be_bytes())?.is_none());
    assert!(ks_b.get(99_999u32.to_be_bytes())?.is_none());
    println!("divergent writes isolated: OK");

    // 5. double-open of same dir must fail with Locked
    match Database::builder(&orig).open() {
        Err(fjall::Error::Locked) => println!("double-open rejected with Error::Locked: OK"),
        Err(e) => println!("double-open rejected with unexpected error: {e:?}"),
        Ok(_) => panic!("double-open of locked dir unexpectedly succeeded"),
    }

    // 6. reopen clone after both dropped (simulates checkpoint reuse)
    drop((ks_a, db_a, ks_b, db_b));
    let db_b2 = Database::builder(&clone).open()?;
    let ks_b2 = db_b2.keyspace("items", KeyspaceCreateOptions::default)?;
    assert_eq!(ks_b2.len()?, 10_001);
    println!("clone reopened after drop, 10_001 items: OK");

    let _ = std::fs::remove_dir_all(&base);
    println!("ALL PROBES PASSED");
    Ok(())
}
