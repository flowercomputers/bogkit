//! Concurrency hammer: many writers and readers at once, plus a /watch
//! subscriber. Asserts the three properties the RwLock design promises:
//! every write gets a unique monotonic seq, no reader ever observes a torn
//! snapshot, and the watch feed is nondecreasing.

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, header};
use bog_serve::{App, NoParams};
use fold::pipeline::terminal;
use http_body_util::BodyExt;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_stream::StreamExt;
use tower::ServiceExt;

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
struct Item {
    n: u64,
}

const WRITERS: u64 = 8;
const WRITES_EACH: u64 = 25;
const TOTAL: u64 = WRITERS * WRITES_EACH;

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn writers_readers_and_watchers_agree() {
    let dir = tempfile::tempdir().unwrap().keep();
    let router = App::stream(
        dir,
        (
            terminal::Count::new("total"),
            terminal::Bag::<Item>::new("items"),
        ),
    )
    // the torn-snapshot detector: two sinks read in ONE rtx must agree.
    // If a reader could ever interleave with a half-applied write, the
    // count and the bag would diverge here.
    .get("/invariant", |(count, items), _: NoParams| {
        let count = count.get();
        let bag_total: i64 = items.iter().map(|(_, mult)| mult).sum();
        if count == bag_total {
            Ok(json!({ "consistent": true, "count": count }))
        } else {
            Err((500, format!("torn snapshot: count={count} bag={bag_total}")))
        }
    })
    .into_router();

    // subscribe to /watch before any writes so the feed spans the whole run
    let watch_resp = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/watch")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let mut watch_events = watch_resp.into_body().into_data_stream();

    // writers: each inserts its own range, collecting the returned seqs
    let mut tasks = Vec::new();
    for w in 0..WRITERS {
        let router = router.clone();
        tasks.push(tokio::spawn(async move {
            let mut seqs = Vec::new();
            for i in 0..WRITES_EACH {
                let body = json!({ "n": w * WRITES_EACH + i }).to_string();
                let req = Request::builder()
                    .method("POST")
                    .uri("/insert")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .unwrap();
                let resp = router.clone().oneshot(req).await.unwrap();
                assert!(resp.status().is_success());
                let bytes = resp.into_body().collect().await.unwrap().to_bytes();
                let v: Value = serde_json::from_slice(&bytes).unwrap();
                seqs.push(v["seq"].as_u64().unwrap());
            }
            seqs
        }));
    }

    // readers: hammer the invariant route the whole time
    let mut readers = Vec::new();
    for _ in 0..8 {
        let router = router.clone();
        readers.push(tokio::spawn(async move {
            loop {
                let req = Request::builder()
                    .uri("/invariant")
                    .body(Body::empty())
                    .unwrap();
                let resp = router.clone().oneshot(req).await.unwrap();
                assert!(
                    resp.status().is_success(),
                    "invariant route reported a torn snapshot"
                );
                let bytes = resp.into_body().collect().await.unwrap().to_bytes();
                let v: Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(v["data"]["consistent"], true);
                if v["data"]["count"].as_i64().unwrap() as u64 >= TOTAL {
                    return;
                }
                tokio::task::yield_now().await;
            }
        }));
    }

    // collect writer seqs: every one unique, together exactly 1..=TOTAL
    let mut all_seqs = Vec::new();
    for t in tasks {
        all_seqs.extend(t.await.unwrap());
    }
    all_seqs.sort_unstable();
    assert_eq!(
        all_seqs,
        (1..=TOTAL).collect::<Vec<u64>>(),
        "seqs must be unique and gapless"
    );

    for r in readers {
        tokio::time::timeout(Duration::from_secs(30), r)
            .await
            .expect("readers finish")
            .unwrap();
    }

    // watch feed: nondecreasing seqs, reaching the final one
    let mut last = 0;
    loop {
        let chunk = tokio::time::timeout(Duration::from_secs(10), watch_events.next())
            .await
            .expect("watch event")
            .unwrap()
            .unwrap();
        for line in String::from_utf8_lossy(&chunk).lines() {
            if let Some(data) = line.strip_prefix("data: ") {
                let v: Value = serde_json::from_str(data).unwrap();
                let seq = v["seq"].as_u64().unwrap();
                assert!(seq >= last, "watch went backwards: {last} -> {seq}");
                last = seq;
            }
        }
        if last >= TOTAL {
            break;
        }
    }
}
