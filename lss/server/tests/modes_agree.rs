//! The two read modes must be indistinguishable: the same writes, sent to a
//! sqlite-mode server and a memory-mode server, must make every GET answer
//! byte-for-byte the same.

mod common;

use common::*;

/// Tiny deterministic generator, so both servers get the identical sequence and
/// a failure is reproducible.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self, n: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) % n
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_get_is_byte_identical() {
    let sql = TestServer::start(ReadSource::Sqlite);
    let mem = TestServer::start(ReadSource::Memory);
    let c = client();
    let mut rng = Lcg(42);

    // The same writes, in the same order, to both. Their responses must match too.
    let mut writes: Vec<(String, serde_json::Value)> = vec![];
    for i in 0..120 {
        let cat = format!("cag{}", rng.next(4));
        // Few distinct prices so the id tie-break is exercised heavily.
        let price = (rng.next(5) * 100) as i64;
        let stock = rng.next(4) as i64; // includes 0 (sold out from the start)
        let id = format!("pag{}", rng.next(1000) * 1000 + i);
        writes.push((
            "/products".into(),
            product(&id, &format!("Name \"{i}\" ✓"), &cat, price, stock),
        ));
    }
    // Duplicate creates (409) mixed in.
    writes.push(("/products".into(), writes[3].1.clone()));
    let ids: Vec<String> = writes
        .iter()
        .map(|(_, p)| p["id"].as_str().unwrap().to_owned())
        .collect();
    for _ in 0..400 {
        let user = format!("uag{}", rng.next(40));
        let product = if rng.next(20) == 0 {
            "pmissing".to_owned()
        } else {
            ids[rng.next(ids.len() as u64) as usize].clone()
        };
        writes.push(("/orders".into(), order(&user, &product)));
    }
    for (path, body) in &writes {
        let a = post(&c, &sql.url(path), body).await;
        let b = post(&c, &mem.url(path), body).await;
        assert_eq!(
            (a.status, &a.text),
            (b.status, &b.text),
            "POST {path} {body}"
        );
    }

    let mut reads: Vec<String> = vec![];
    for id in ids.iter().chain(["pmissing".to_owned()].iter()) {
        reads.push(format!("/products/{id}"));
    }
    for cat in ["cag0", "cag1", "cag2", "cag3", "cagnone"] {
        for limit in [1, 7, 20, 50] {
            for page in 1..=8 {
                reads.push(format!(
                    "/products?category={cat}&page={page}&limit={limit}"
                ));
            }
        }
        reads.push(format!("/products?category={cat}"));
    }
    for u in 0..45 {
        reads.push(format!("/users/uag{u}/orders"));
    }
    for path in &reads {
        let a = get(&c, &sql.url(path)).await;
        let b = get(&c, &mem.url(path)).await;
        assert_eq!((a.status, &a.text), (b.status, &b.text), "GET {path}");
    }
    assert!(reads.len() > 250);

    drop(c);
    sql.stop();
    mem.stop();
}
