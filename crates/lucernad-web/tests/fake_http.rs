//! O `testing::FakeHttp`: as rotas, a contagem e o `hold`/`release`.

use std::time::Duration;

use lucernad_web::http::{Http, HttpError, Request};
use lucernad_web::testing::{FakeHttp, json};
use serde_json::json;

/// Espera (até 1 s) o `FakeHttp` ter `n` pedidos em voo.
async fn until_in_flight(http: &FakeHttp, n: usize) {
    for _ in 0..100 {
        if http.in_flight() == n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("em voo: {} (esperava {n})", http.in_flight());
}

fn status(code: u16) -> impl Fn(&Request) -> Result<lucernad_web::http::Response, HttpError> {
    move |_| Ok(json(code, json!({})))
}

#[tokio::test]
async fn routes_by_the_longest_prefix() {
    let http = FakeHttp::new();
    assert_eq!(
        http.get(&Request::new("http://a/x")).await,
        Err(HttpError::Connect)
    );
    http.route("http://a", status(200));
    http.route("http://a/x", status(201));
    assert_eq!(
        http.get(&Request::new("http://a/x/y"))
            .await
            .unwrap()
            .status,
        201
    );
    assert_eq!(
        http.get(&Request::new("http://a/z")).await.unwrap().status,
        200
    );
    assert_eq!(
        http.get(&Request::new("http://b/x")).await,
        Err(HttpError::Connect)
    );
}

#[tokio::test]
async fn the_same_prefix_replaces_the_route() {
    let http = FakeHttp::new();
    http.route("http://a/x", status(201));
    http.route("http://a/x", status(202));
    assert_eq!(
        http.get(&Request::new("http://a/x")).await.unwrap().status,
        202
    );
}

#[tokio::test]
async fn records_the_requests() {
    let http = FakeHttp::new();
    http.route("http://a", status(200));
    let first = Request::new("http://a/1").param("q", "um");
    let second = Request::new("http://sem-rota/2");
    let _ = http.get(&first).await;
    let _ = http.get(&second).await;
    assert_eq!(http.count(), 2);
    assert_eq!(http.requests(), vec![first, second]);
}

#[tokio::test]
async fn hold_counts_in_flight_and_abort_leaves_the_count() {
    let http = FakeHttp::new();
    http.route("http://a", status(200));
    http.hold();
    let one = http.clone();
    let first = tokio::spawn(async move { one.get(&Request::new("http://a/1")).await });
    let two = http.clone();
    let second = tokio::spawn(async move { two.get(&Request::new("http://a/2")).await });
    until_in_flight(&http, 2).await;
    assert_eq!(http.count(), 2);

    second.abort();
    let _ = second.await;
    assert_eq!(http.in_flight(), 1);

    http.release();
    assert_eq!(first.await.unwrap().unwrap().status, 200);
    assert_eq!(http.in_flight(), 0);
    assert_eq!(http.max_in_flight(), 2);

    // Solto, os próximos passam direto.
    assert_eq!(
        http.get(&Request::new("http://a/3")).await.unwrap().status,
        200
    );
    assert_eq!(http.max_in_flight(), 2);
}

#[tokio::test]
async fn release_then_hold_still_frees_the_waiting_request() {
    let http = FakeHttp::new();
    http.route("http://a", status(200));
    http.hold();
    let one = http.clone();
    let waiting = tokio::spawn(async move { one.get(&Request::new("http://a/1")).await });
    until_in_flight(&http, 1).await;

    // O `hold` logo depois do `release` não prende o que já esperava…
    http.release();
    http.hold();
    let resp = tokio::time::timeout(Duration::from_secs(1), waiting)
        .await
        .expect("o pedido parado saiu")
        .unwrap();
    assert_eq!(resp.unwrap().status, 200);

    // …mas prende o próximo.
    let two = http.clone();
    let next = tokio::spawn(async move { two.get(&Request::new("http://a/2")).await });
    until_in_flight(&http, 1).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!next.is_finished());
    http.release();
    assert_eq!(next.await.unwrap().unwrap().status, 200);
}

#[tokio::test]
async fn panicking_records_then_panics() {
    let http = FakeHttp::panicking();
    http.route("http://", status(200));
    let one = http.clone();
    let result = tokio::spawn(async move { one.get(&Request::new("http://a")).await }).await;
    assert!(result.unwrap_err().is_panic());
    assert_eq!(http.count(), 1);
    assert_eq!(http.in_flight(), 0);
}
