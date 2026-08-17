use std::io::Cursor;

use reach_rollup_lab::{
    LOAD_RECORDS, LOAD_SHARDS, LOAD_UNIQUE_OCCURRENCES, LoadCorpus, Record, encode_record_file,
    stream_record_file,
};

#[test]
fn load_generator_has_exact_shape_and_shuffled_equal_shards() {
    // Catches count drift, an uneven partition, or accidentally sorted shard input.
    let corpus = LoadCorpus::new(0x243f_6a88_85a3_08d3);
    assert_eq!(LOAD_UNIQUE_OCCURRENCES, 10_800_000);
    assert_eq!(LOAD_RECORDS, 12_000_000);
    assert_eq!(LOAD_SHARDS, 8);
    for shard in 0..LOAD_SHARDS {
        let records = corpus.shard(shard).unwrap();
        assert_eq!(records.len(), 1_500_000);
        let first: Vec<_> = records.take(4).collect();
        assert!(first.windows(2).any(|pair| pair[0] > pair[1]));
    }
}

#[test]
fn load_generator_assigns_required_bucket_cardinalities_and_only_valid_duplicates() {
    // Catches an off-by-one in bucket layout or a "duplicate" that creates a new ID.
    let corpus = LoadCorpus::new(0x243f_6a88_85a3_08d3);
    let first_small: Vec<_> = (0..1_000)
        .map(|index| corpus.logical(index).unwrap())
        .collect();
    assert!(
        first_small
            .iter()
            .all(|record| record.tenant_id == 0 && record.unix_hour == 0)
    );
    assert_eq!(
        first_small
            .iter()
            .map(|record| record.installation_id)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        1_000
    );

    let first_medium = corpus.logical(20_000).unwrap();
    let first_large = corpus.logical(35_000).unwrap();
    assert_eq!((first_medium.tenant_id, first_medium.unix_hour), (0, 20));
    assert_eq!((first_large.tenant_id, first_large.unix_hour), (0, 23));

    let duplicate = corpus.logical(LOAD_UNIQUE_OCCURRENCES).unwrap();
    assert_eq!(duplicate, corpus.logical(0).unwrap());
    assert!(corpus.logical(LOAD_RECORDS).is_none());
}

#[test]
fn streaming_reader_round_trips_and_rejects_malformed_files() {
    // Catches buffered-only parsing, unknown versions, truncation, trailing bytes, or late validation.
    let records = [Record::new(2, 100, [1; 16]), Record::new(3, 123, [2; 16])];
    let bytes = encode_record_file(&records);
    let window = reach_rollup_lab::DayWindow::new(100, 2, 3).unwrap();
    let mut seen = Vec::new();
    stream_record_file(Cursor::new(&bytes), window, |record| {
        seen.push(record);
        Ok(())
    })
    .unwrap();
    assert_eq!(seen, records);

    let mut unknown_version = bytes.clone();
    unknown_version[4] = 2;
    assert!(stream_record_file(Cursor::new(unknown_version), window, |_| Ok(())).is_err());
    assert!(
        stream_record_file(Cursor::new(&bytes[..bytes.len() - 1]), window, |_| Ok(())).is_err()
    );

    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(stream_record_file(Cursor::new(trailing), window, |_| Ok(())).is_err());

    let invalid = encode_record_file(&[Record::new(2, 124, [0; 16])]);
    assert!(stream_record_file(Cursor::new(invalid), window, |_| Ok(())).is_err());
}
