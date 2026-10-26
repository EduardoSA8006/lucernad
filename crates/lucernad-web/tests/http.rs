//! O `ReqwestHttp` de verdade, em `Network::Loopback`, contra o
//! `testing::LocalServer` (só 127.0.0.1), e a nova tentativa do `fetch`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lucernad_web::http::{
    BODY_MAX, Http, HttpError, Network, Request, ReqwestHttp, RetryPolicy, USER_AGENT, fetch,
    retry_after, same_host_redirect,
};
use lucernad_web::testing::{LocalServer, Reply, closed_port};

fn fast() -> RetryPolicy {
    RetryPolicy {
        retries: 3,
        base: Duration::from_millis(10),
        retry_after_max: Duration::from_secs(2),
    }
}

fn client() -> ReqwestHttp {
    ReqwestHttp::new(Duration::from_secs(5), Network::Loopback).expect("cliente HTTP")
}

/// Um servidor que responde, em ordem, as respostas dadas (a última se
/// repete).
async fn sequence(replies: Vec<Reply>) -> LocalServer {
    let next = AtomicUsize::new(0);
    LocalServer::start(move |_| {
        let i = next.fetch_add(1, Ordering::SeqCst).min(replies.len() - 1);
        replies[i].clone()
    })
    .await
}

#[tokio::test]
async fn sends_the_user_agent_and_query() {
    let server = LocalServer::start(|_| Reply::text(200, "olá")).await;
    let req = Request::new(format!("{}/x", server.base())).param("a", "b c");
    let resp = client().get(&req).await.expect("resposta");
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, "olá".as_bytes());
    assert_eq!(resp.retry_after, None);
    let seen = server.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].path, "/x");
    assert_eq!(seen[0].query, vec![("a".to_string(), "b c".to_string())]);
    assert_eq!(seen[0].user_agent, USER_AGENT);
}

#[tokio::test]
async fn timeout_is_its_own_error() {
    let server =
        LocalServer::start(|_| Reply::text(200, "tarde").after(Duration::from_secs(2))).await;
    let http = ReqwestHttp::new(Duration::from_millis(300), Network::Loopback).unwrap();
    let start = Instant::now();
    let result = http
        .get(&Request::new(format!("{}/x", server.base())))
        .await;
    assert_eq!(result, Err(HttpError::Timeout));
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "{:?}",
        start.elapsed()
    );
}

#[tokio::test]
async fn connection_refused() {
    let result = client()
        .get(&Request::new(format!("{}/x", closed_port())))
        .await;
    assert_eq!(result, Err(HttpError::Connect));
}

#[tokio::test]
async fn body_over_the_limit_is_refused() {
    let server = LocalServer::start(|seen| {
        let size = if seen.path == "/grande" {
            BODY_MAX + 1
        } else {
            BODY_MAX
        };
        Reply::bytes(200, vec![b'a'; size])
    })
    .await;
    let http = client();
    let big = http
        .get(&Request::new(format!("{}/grande", server.base())))
        .await;
    assert_eq!(big, Err(HttpError::TooLarge));
    let ok = http
        .get(&Request::new(format!("{}/certo", server.base())))
        .await
        .expect("no limite");
    assert_eq!(ok.body.len(), BODY_MAX);
}

#[tokio::test]
async fn retries_transient_errors_then_gives_up() {
    let http = client();

    let server = sequence(vec![
        Reply::text(503, "fora"),
        Reply::text(503, "fora"),
        Reply::text(200, "pronto"),
    ])
    .await;
    let req = Request::new(format!("{}/x", server.base()));
    let resp = fetch(&http, &req, &fast()).await.expect("na terceira");
    assert_eq!(resp.status, 200);
    assert_eq!(server.seen().len(), 3);

    let server = LocalServer::start(|_| Reply::text(500, "quebrado")).await;
    let req = Request::new(format!("{}/x", server.base()));
    assert_eq!(
        fetch(&http, &req, &fast()).await,
        Err(HttpError::Status(500))
    );
    assert_eq!(server.seen().len(), 4);

    let server = LocalServer::start(|_| Reply::text(404, "nada")).await;
    let req = Request::new(format!("{}/x", server.base()));
    let resp = fetch(&http, &req, &fast()).await.expect("404 é resposta");
    assert_eq!(resp.status, 404);
    assert_eq!(server.seen().len(), 1);

    let server = LocalServer::start(|_| Reply::dropped()).await;
    let req = Request::new(format!("{}/x", server.base()));
    assert_eq!(fetch(&http, &req, &fast()).await, Err(HttpError::Connect));
    assert_eq!(server.seen().len(), 4);
}

#[tokio::test]
async fn retry_after_is_respected() {
    let http = client();
    let times = Arc::new(Mutex::new(Vec::new()));
    let next = AtomicUsize::new(0);
    let log = times.clone();
    let server = LocalServer::start(move |_| {
        log.lock().unwrap().push(Instant::now());
        if next.fetch_add(1, Ordering::SeqCst) == 0 {
            Reply::text(429, "devagar").header("Retry-After", "1")
        } else {
            Reply::text(200, "pronto")
        }
    })
    .await;
    let req = Request::new(format!("{}/x", server.base()));
    let resp = fetch(&http, &req, &fast()).await.expect("depois da espera");
    assert_eq!(resp.status, 200);
    let times = times.lock().unwrap().clone();
    assert_eq!(times.len(), 2);
    assert!(times[1] - times[0] >= Duration::from_secs(1));

    let server =
        LocalServer::start(|_| Reply::text(429, "muito").header("Retry-After", "120")).await;
    let req = Request::new(format!("{}/x", server.base()));
    assert_eq!(
        fetch(&http, &req, &fast()).await,
        Err(HttpError::WaitTooLong(120))
    );
    assert_eq!(server.seen().len(), 1);
}

#[tokio::test]
async fn errors_never_carry_the_url() {
    let http = client();
    let dropped = LocalServer::start(|_| Reply::dropped()).await;
    for base in [closed_port(), dropped.base()] {
        let req = Request::new(format!("{base}/x")).param("apikey", "SEGREDO123");
        let err = http.get(&req).await.expect_err("falha");
        for text in [format!("{err:?}"), err.message()] {
            assert!(!text.contains("SEGREDO123"), "{text}");
            assert!(!text.contains("127.0.0.1"), "{text}");
        }
    }
    // O corpo mais curto que o anunciado cai no `Other`, o caminho em que o
    // texto do `reqwest` vai para o erro.
    let short =
        LocalServer::start(|_| Reply::text(200, "curto").header("Content-Length", "100")).await;
    let req = Request::new(format!("{}/x", short.base())).param("apikey", "SEGREDO123");
    let err = http.get(&req).await.expect_err("corpo cortado");
    assert!(matches!(err, HttpError::Other(_)), "{err:?}");
    for text in [format!("{err:?}"), err.message()] {
        assert!(!text.contains("SEGREDO123"), "{text}");
        assert!(!text.contains("127.0.0.1"), "{text}");
    }
    let req = Request::new("http://127.0.0.1:9/x")
        .param("q", "a")
        .param("apikey", "SEGREDO123");
    let debug = format!("{req:?}");
    assert!(debug.contains("apikey=***"), "{debug}");
    assert!(!debug.contains("SEGREDO123"), "{debug}");
}

#[tokio::test]
async fn loopback_does_not_follow_redirects() {
    let server = LocalServer::start(|_| Reply::text(302, "").header("Location", "/y")).await;
    let resp = client()
        .get(&Request::new(format!("{}/x", server.base())))
        .await
        .expect("o 302 volta");
    assert_eq!(resp.status, 302);
    let seen = server.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].path, "/x");
}

#[test]
fn redirects_only_to_the_same_host() {
    let from = "https://lrclib.net/a";
    assert!(same_host_redirect(from, "https://lrclib.net/b", 0));
    assert!(same_host_redirect(from, "https://lrclib.net/b", 2));
    assert!(!same_host_redirect(from, "https://outro.example/b", 0));
    assert!(!same_host_redirect(from, "http://lrclib.net/b", 0));
    assert!(!same_host_redirect(from, "https://lrclib.net/b", 3));
}

#[test]
fn retry_after_parsing() {
    assert_eq!(retry_after("5"), Some(Duration::from_secs(5)));
    assert_eq!(retry_after(" 0 "), Some(Duration::ZERO));
    assert_eq!(retry_after("Wed, 21 Oct 2015 07:28:00 GMT"), None);
    assert_eq!(retry_after("-1"), None);
    assert_eq!(retry_after(""), None);
}
