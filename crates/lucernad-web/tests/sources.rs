//! As fontes: a LRCLIB (o `get` e a busca conferida), o Vagalume (só com a
//! chave) e o Open-Meteo (a previsão e a busca de cidade), sobre o
//! `FakeHttp`: nada sai para a internet.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lucernad_web::endpoints::Endpoints;
use lucernad_web::http::{HttpError, Request, Response, RetryPolicy};
use lucernad_web::lrc::Line;
use lucernad_web::lyrics::{self, Found, Lyrics, Source};
use lucernad_web::matching::Track;
use lucernad_web::testing::{
    FakeHttp, cities_body, forecast_body, forecast_body_days, json, lrclib_entry, vagalume_body,
};
use lucernad_web::weather::{self, HttpOrFormat, Location};
use serde_json::{Value, json};

const BASE: &str = "http://127.0.0.1:1";
const SYNCED: &str = "[00:12.50]Todos os dias quando acordo\n[00:17.00]";
const PLAIN: &str = "Todos os dias quando acordo\nNão tenho mais";

fn ep() -> Endpoints {
    Endpoints::under(BASE)
}

fn want() -> Track {
    Track {
        artist: "Legião Urbana".to_string(),
        title: "Tempo Perdido".to_string(),
        album: "Dois".to_string(),
        duration: Some(201.0),
    }
}

/// Uma política rápida: 1 nova tentativa com 1 ms de espera.
fn quick() -> RetryPolicy {
    RetryPolicy {
        retries: 1,
        base: Duration::from_millis(1),
        retry_after_max: Duration::from_millis(10),
    }
}

fn get_url() -> String {
    format!("{BASE}/lrclib/api/get")
}

fn search_url() -> String {
    format!("{BASE}/lrclib/api/search")
}

fn vagalume_url() -> String {
    format!("{BASE}/vagalume/search.php")
}

fn answer(status: u16, v: Value) -> impl Fn(&Request) -> Result<Response, HttpError> {
    move |_| Ok(json(status, v.clone()))
}

fn raw(status: u16, body: &str) -> impl Fn(&Request) -> Result<Response, HttpError> {
    let body = body.as_bytes().to_vec();
    move |_| {
        Ok(Response {
            status,
            retry_after: None,
            body: body.clone(),
        })
    }
}

fn not_found() -> Value {
    json!({"statusCode": 404, "name": "TrackNotFound", "message": "Failed to find specified track"})
}

fn param<'a>(req: &'a Request, k: &str) -> Option<&'a str> {
    req.query
        .iter()
        .find(|(key, _)| key == k)
        .map(|(_, v)| v.as_str())
}

fn urls(http: &FakeHttp) -> Vec<String> {
    http.requests().into_iter().map(|r| r.url).collect()
}

async fn find(http: &FakeHttp, t: &Track, key: Option<&str>) -> Result<Found, String> {
    lyrics::find(http, &ep(), &quick(), t, key).await
}

fn synced_lines() -> Vec<Line> {
    vec![
        Line {
            time: Some(12.5),
            text: "Todos os dias quando acordo".to_string(),
        },
        Line {
            time: Some(17.0),
            text: String::new(),
        },
    ]
}

#[tokio::test]
async fn lrclib_get_first_with_all_fields() {
    let http = FakeHttp::new();
    http.route(
        &get_url(),
        answer(
            200,
            lrclib_entry(
                "Legião Urbana",
                "Tempo Perdido",
                201.0,
                Some(PLAIN),
                Some(SYNCED),
            ),
        ),
    );
    let found = find(&http, &want(), None).await.unwrap();
    assert_eq!(
        found,
        Found::Lyrics(Lyrics {
            synced: true,
            lines: synced_lines(),
            source: Source::Lrclib,
            url: None,
        })
    );
    let reqs = http.requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].url, get_url());
    assert_eq!(param(&reqs[0], "track_name"), Some("Tempo Perdido"));
    assert_eq!(param(&reqs[0], "artist_name"), Some("Legião Urbana"));
    assert_eq!(param(&reqs[0], "album_name"), Some("Dois"));
    assert_eq!(param(&reqs[0], "duration"), Some("201"));
}

#[test]
fn album_and_duration_are_optional() {
    let t = Track {
        album: String::new(),
        duration: None,
        ..want()
    };
    let req = lyrics::lrclib_get(&ep(), &t);
    assert_eq!(req.url, get_url());
    assert_eq!(param(&req, "track_name"), Some("Tempo Perdido"));
    assert_eq!(param(&req, "artist_name"), Some("Legião Urbana"));
    assert_eq!(param(&req, "album_name"), None);
    assert_eq!(param(&req, "duration"), None);
    let search = lyrics::lrclib_search(&ep(), &t);
    assert_eq!(search.url, search_url());
    assert_eq!(search.query.len(), 2);
}

#[tokio::test]
async fn get_miss_falls_back_to_a_checked_search() {
    let http = FakeHttp::new();
    http.route(&get_url(), answer(404, not_found()));
    http.route(
        &search_url(),
        answer(
            200,
            json!([
                lrclib_entry(
                    "Outra Banda",
                    "Tempo Perdido",
                    201.0,
                    None,
                    Some("[00:01.00]errada")
                ),
                lrclib_entry("Legião Urbana", "Tempo Perdido", 201.0, Some(PLAIN), None),
                lrclib_entry(
                    "Legião Urbana",
                    "Tempo Perdido",
                    202.5,
                    Some(PLAIN),
                    Some(SYNCED)
                ),
            ]),
        ),
    );
    let found = find(&http, &want(), None).await.unwrap();
    assert_eq!(
        found,
        Found::Lyrics(Lyrics {
            synced: true,
            lines: synced_lines(),
            source: Source::Lrclib,
            url: None,
        })
    );
    assert_eq!(urls(&http), vec![get_url(), search_url()]);
    let search = &http.requests()[1];
    assert_eq!(param(search, "track_name"), Some("Tempo Perdido"));
    assert_eq!(param(search, "artist_name"), Some("Legião Urbana"));
}

#[tokio::test]
async fn search_refuses_the_wrong_song() {
    let http = FakeHttp::new();
    http.route(&get_url(), answer(404, not_found()));
    http.route(
        &search_url(),
        answer(
            200,
            json!([
                lrclib_entry(
                    "Capital Inicial",
                    "Tempo Perdido",
                    201.0,
                    Some(PLAIN),
                    Some(SYNCED)
                ),
                lrclib_entry(
                    "Legião Urbana",
                    "Tempo Perdido",
                    206.0,
                    Some(PLAIN),
                    Some(SYNCED)
                ),
            ]),
        ),
    );
    assert_eq!(
        find(&http, &want(), None).await.unwrap(),
        Found::None { vagalume: false }
    );
    assert_eq!(http.count(), 2);
}

#[tokio::test]
async fn instrumental_or_empty_is_none() {
    // O `get` confirmou que é instrumental: encerra, sem busca nem Vagalume.
    for key in [None, Some("CHAVE")] {
        let http = FakeHttp::new();
        let mut entry = lrclib_entry("Legião Urbana", "Tempo Perdido", 201.0, None, None);
        entry["instrumental"] = json!(true);
        http.route(&get_url(), answer(200, entry));
        http.route(&search_url(), |_| panic!("instrumental não busca"));
        http.route(&vagalume_url(), |_| {
            panic!("instrumental não vai ao Vagalume")
        });
        assert_eq!(
            find(&http, &want(), key).await.unwrap(),
            Found::None { vagalume: false }
        );
        assert_eq!(urls(&http), vec![get_url()]);
    }

    // Sem letra nenhuma no `get` e a busca vazia.
    let http = FakeHttp::new();
    http.route(
        &get_url(),
        answer(
            200,
            lrclib_entry("Legião Urbana", "Tempo Perdido", 201.0, None, None),
        ),
    );
    http.route(&search_url(), answer(200, json!([])));
    assert_eq!(
        find(&http, &want(), None).await.unwrap(),
        Found::None { vagalume: false }
    );
    assert_eq!(http.count(), 2);
}

fn vagalume_lyrics(url: &str) -> Found {
    Found::Lyrics(Lyrics {
        synced: false,
        lines: vec![
            Line {
                time: None,
                text: "a".to_string(),
            },
            Line {
                time: None,
                text: "b".to_string(),
            },
        ],
        source: Source::Vagalume,
        url: Some(url.to_string()),
    })
}

/// A LRCLIB sem nada: `get` 404 e a busca vazia.
fn lrclib_misses(http: &FakeHttp) {
    http.route(&get_url(), answer(404, not_found()));
    http.route(&search_url(), answer(200, json!([])));
}

const PAGE: &str = "https://www.vagalume.com.br/legiao-urbana/tempo-perdido.html";

#[tokio::test]
async fn vagalume_only_with_a_key() {
    let http = FakeHttp::new();
    lrclib_misses(&http);
    http.route(
        &vagalume_url(),
        answer(
            200,
            json!({"type":"exact","art":{"name":"Legião Urbana"},"mus":[{"name":"Tempo Perdido","url":PAGE,"text":"a\nb"}]}),
        ),
    );
    assert_eq!(
        find(&http, &want(), Some("CHAVE")).await.unwrap(),
        vagalume_lyrics(PAGE)
    );
    let reqs = http.requests();
    assert_eq!(reqs.len(), 3);
    assert_eq!(reqs[2].url, vagalume_url());
    assert_eq!(param(&reqs[2], "art"), Some("Legião Urbana"));
    assert_eq!(param(&reqs[2], "mus"), Some("Tempo Perdido"));
    assert_eq!(param(&reqs[2], "apikey"), Some("CHAVE"));

    let http = FakeHttp::new();
    lrclib_misses(&http);
    http.route(&vagalume_url(), |_| panic!("sem chave, sem Vagalume"));
    assert_eq!(
        find(&http, &want(), None).await.unwrap(),
        Found::None { vagalume: false }
    );
    assert_eq!(urls(&http), vec![get_url(), search_url()]);
}

async fn with_vagalume(body: Value) -> Result<Found, String> {
    let http = FakeHttp::new();
    lrclib_misses(&http);
    http.route(&vagalume_url(), answer(200, body));
    find(&http, &want(), Some("CHAVE")).await
}

#[tokio::test]
async fn vagalume_approximate_must_still_match() {
    let live = vagalume_body(
        "aprox",
        "Legião Urbana",
        "Tempo Perdido (Ao Vivo)",
        PAGE,
        "a\nb",
    );
    assert_eq!(with_vagalume(live).await.unwrap(), vagalume_lyrics(PAGE));

    let other = vagalume_body("aprox", "Legião Urbana", "Outra Música", PAGE, "a\nb");
    assert_eq!(
        with_vagalume(other).await.unwrap(),
        Found::None { vagalume: true }
    );
    for kind in ["song_notfound", "notfound"] {
        assert_eq!(
            with_vagalume(json!({ "type": kind })).await.unwrap(),
            Found::None { vagalume: true },
            "{kind}"
        );
    }
    let relative = vagalume_body(
        "exact",
        "Legião Urbana",
        "Tempo Perdido",
        "/legiao-urbana/x.html",
        "a\nb",
    );
    assert_eq!(
        with_vagalume(relative).await.unwrap(),
        vagalume_lyrics("https://www.vagalume.com.br/legiao-urbana/x.html")
    );
    let evil = vagalume_body(
        "exact",
        "Legião Urbana",
        "Tempo Perdido",
        "https://evil.example/x",
        "a\nb",
    );
    assert_eq!(
        with_vagalume(evil).await.unwrap(),
        vagalume_lyrics("https://www.vagalume.com.br/")
    );
}

#[tokio::test]
async fn network_error_is_an_error_not_none() {
    let http = FakeHttp::new();
    http.route(&get_url(), |_| Err(HttpError::Connect));
    let err = find(&http, &want(), None).await.unwrap_err();
    assert!(err.starts_with("Não consegui falar com a LRCLIB"), "{err}");
    assert_eq!(
        err,
        "Não consegui falar com a LRCLIB: não consegui conectar."
    );

    let http = FakeHttp::new();
    http.route(&get_url(), answer(404, not_found()));
    http.route(&search_url(), answer(500, json!({})));
    let err = find(&http, &want(), None).await.unwrap_err();
    assert_eq!(
        err,
        "Não consegui falar com a LRCLIB: o servidor respondeu 500."
    );

    // Só 404 e 400 são "não achou"; outro status é erro, nunca "sem letra".
    let http = FakeHttp::new();
    http.route(&get_url(), answer(403, json!({})));
    assert_eq!(
        find(&http, &want(), None).await,
        Err("Não consegui falar com a LRCLIB: o servidor respondeu 403.".to_string())
    );
    assert_eq!(http.count(), 1);

    let http = FakeHttp::new();
    http.route(&get_url(), answer(404, not_found()));
    http.route(&search_url(), answer(403, json!({})));
    assert_eq!(
        find(&http, &want(), None).await,
        Err("Não consegui falar com a LRCLIB: o servidor respondeu 403.".to_string())
    );

    let http = FakeHttp::new();
    http.route(&get_url(), answer(400, json!({})));
    http.route(&search_url(), answer(400, json!({})));
    assert_eq!(
        find(&http, &want(), None).await,
        Ok(Found::None { vagalume: false })
    );

    let http = FakeHttp::new();
    lrclib_misses(&http);
    http.route(&vagalume_url(), |_| Err(HttpError::Timeout));
    let err = find(&http, &want(), Some("CHAVE")).await.unwrap_err();
    assert!(err.contains("Vagalume"), "{err}");
    assert!(!err.contains("CHAVE"), "{err}");
    assert!(!err.contains("http"), "{err}");
}

/// A LRCLIB sobrecarregada no `get`: 503 com `Retry-After: 1` (dentro da
/// política rápida, o `Retry-After` de 1 ms).
fn overloaded() -> impl Fn(&Request) -> Result<Response, HttpError> {
    |_| {
        Ok(Response {
            status: 503,
            retry_after: Some(Duration::from_millis(1)),
            body: br#"{"name":"ServerOverloaded"}"#.to_vec(),
        })
    }
}

fn get_count(http: &FakeHttp) -> usize {
    urls(http).iter().filter(|u| **u == get_url()).count()
}

const OVERLOADED: &str = "Não consegui falar com a LRCLIB: o servidor respondeu 503.";

#[tokio::test]
async fn overloaded_get_falls_back_to_the_search() {
    let http = FakeHttp::new();
    http.route(&get_url(), overloaded());
    http.route(
        &search_url(),
        answer(
            200,
            json!([
                lrclib_entry("Outro", "Outra", 201.0, Some(PLAIN), Some(SYNCED)),
                lrclib_entry(
                    "Legião Urbana",
                    "Tempo Perdido",
                    201.0,
                    Some(PLAIN),
                    Some(SYNCED)
                ),
            ]),
        ),
    );
    assert_eq!(
        find(&http, &want(), None).await.unwrap(),
        Found::Lyrics(Lyrics {
            synced: true,
            lines: synced_lines(),
            source: Source::Lrclib,
            url: None,
        })
    );
    assert_eq!(get_count(&http), 2);
}

#[tokio::test]
async fn overloaded_get_without_a_match_is_an_error_not_none() {
    let http = FakeHttp::new();
    http.route(&get_url(), overloaded());
    http.route(
        &search_url(),
        answer(
            200,
            json!([lrclib_entry(
                "Outro",
                "Outra",
                201.0,
                Some(PLAIN),
                Some(SYNCED)
            )]),
        ),
    );
    assert_eq!(
        find(&http, &want(), None).await,
        Err(OVERLOADED.to_string())
    );
    assert_eq!(get_count(&http), 2);

    // A busca que dá 404 também não confirma nada.
    let http = FakeHttp::new();
    http.route(&get_url(), overloaded());
    http.route(&search_url(), answer(404, not_found()));
    assert_eq!(
        find(&http, &want(), None).await,
        Err(OVERLOADED.to_string())
    );

    // Com a chave, o Vagalume sem a letra também fica no erro do `get`.
    let http = FakeHttp::new();
    http.route(&get_url(), overloaded());
    http.route(&search_url(), answer(200, json!([])));
    http.route(&vagalume_url(), answer(200, json!({"type": "notfound"})));
    assert_eq!(
        find(&http, &want(), Some("CHAVE")).await,
        Err(OVERLOADED.to_string())
    );
    assert!(urls(&http).contains(&vagalume_url()));
}

#[tokio::test]
async fn overloaded_get_and_failed_search_is_the_get_error() {
    let http = FakeHttp::new();
    http.route(&get_url(), overloaded());
    http.route(&search_url(), answer(500, json!({})));
    assert_eq!(
        find(&http, &want(), None).await,
        Err(OVERLOADED.to_string())
    );
}

#[tokio::test]
async fn forbidden_get_does_not_search() {
    let http = FakeHttp::new();
    http.route(&get_url(), answer(403, json!({})));
    http.route(&search_url(), answer(200, json!([])));
    assert_eq!(
        find(&http, &want(), None).await,
        Err("Não consegui falar com a LRCLIB: o servidor respondeu 403.".to_string())
    );
    assert_eq!(urls(&http), vec![get_url()]);
}

#[tokio::test]
async fn captive_portal_html_is_an_error() {
    let http = FakeHttp::new();
    http.route(
        &get_url(),
        raw(200, "<html><body>Entre na rede</body></html>"),
    );
    assert_eq!(
        find(&http, &want(), None).await,
        Err("A LRCLIB respondeu fora do formato.".to_string())
    );

    let http = FakeHttp::new();
    lrclib_misses(&http);
    http.route(&vagalume_url(), raw(200, "<html>portal</html>"));
    assert_eq!(
        find(&http, &want(), Some("CHAVE")).await,
        Err("O Vagalume respondeu fora do formato.".to_string())
    );
}

#[test]
fn none_expires_and_a_new_key_retries() {
    let t = UNIX_EPOCH + Duration::from_secs(1_790_000_000);
    let day = Duration::from_secs(86_400);
    let none = Found::None { vagalume: false };
    assert!(lyrics::fresh(&none, t, t + day * 6, false));
    assert!(!lyrics::fresh(&none, t, t + day * 8, false));
    assert!(!lyrics::fresh(&none, t, t, true));
    assert!(lyrics::fresh(&Found::None { vagalume: true }, t, t, true));
    let found = Found::Lyrics(Lyrics {
        synced: false,
        lines: vec![Line {
            time: None,
            text: "a".to_string(),
        }],
        source: Source::Vagalume,
        url: Some(PAGE.to_string()),
    });
    assert!(lyrics::fresh(&found, t, t + day * 400, true));
    assert_eq!(lyrics::NONE_TTL, day * 7);
    for f in [
        none,
        Found::None { vagalume: true },
        found,
        Found::Lyrics(Lyrics {
            synced: true,
            lines: synced_lines(),
            source: Source::Lrclib,
            url: None,
        }),
    ] {
        assert_eq!(Found::from_json(&f.to_json()), Some(f));
    }
    assert_eq!(Found::from_json(&json!("lixo")), None);
}

fn cacau() -> Location {
    Location {
        name: "Cacaulândia, Rondônia, BR".to_string(),
        latitude: -10.3375,
        longitude: -62.8986,
    }
}

fn bytes(v: &Value) -> Vec<u8> {
    v.to_string().into_bytes()
}

const FORMAT: &str = "O Open-Meteo respondeu fora do formato.";

#[tokio::test]
async fn forecast_request_and_parse() {
    let req = weather::forecast_request(&ep(), &cacau());
    assert_eq!(req.url, format!("{BASE}/forecast/v1/forecast"));
    assert_eq!(param(&req, "latitude"), Some("-10.3375"));
    assert_eq!(param(&req, "longitude"), Some("-62.8986"));
    assert_eq!(
        param(&req, "current"),
        Some(
            "temperature_2m,apparent_temperature,relative_humidity_2m,weather_code,wind_speed_10m,is_day"
        )
    );
    assert_eq!(
        param(&req, "daily"),
        Some(
            "weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max,sunrise,sunset"
        )
    );
    assert_eq!(param(&req, "timezone"), Some("auto"));
    assert_eq!(param(&req, "forecast_days"), Some("7"));
    assert_eq!(cacau().place_key(), "-10.34,-62.90");

    let f = weather::parse_forecast(&bytes(&forecast_body())).unwrap();
    assert_eq!(f.daily.len(), 7);
    assert_eq!(f.daily[0].date, "2026-10-24");
    assert_eq!(f.daily[0].sunrise.as_deref(), Some("2026-10-24T05:41"));
    assert_eq!(f.utc_offset, -14400);
    assert_eq!(f.current.code, 2);
    assert!(f.current.is_day);
    assert_eq!(weather::Forecast::from_json(&f.to_json()), Some(f.clone()));

    let http = FakeHttp::new();
    http.route(
        &format!("{BASE}/forecast"),
        answer(200, forecast_body_days(10)),
    );
    let f = weather::forecast(&http, &ep(), &quick(), &cacau())
        .await
        .unwrap();
    assert_eq!(f.daily.len(), 7);

    let mut body = forecast_body();
    body["daily"]["sunrise"] = json!([null, null, null, null, null, null, null]);
    body["daily"]["sunset"][0] = Value::Null;
    let f = weather::parse_forecast(&bytes(&body)).unwrap();
    assert_eq!(f.daily[0].sunrise, None);
    assert_eq!(f.daily[0].sunset, None);
    assert_eq!(f.daily[1].sunset.as_deref(), Some("2026-10-25T18:03"));

    let mut body = forecast_body();
    body["daily"]["temperature_2m_max"]
        .as_array_mut()
        .unwrap()
        .pop();
    assert_eq!(weather::parse_forecast(&bytes(&body)), Err(FORMAT.into()));

    let mut body = forecast_body();
    body["daily"]["time"][2] = json!("26/10/2026");
    assert_eq!(weather::parse_forecast(&bytes(&body)), Err(FORMAT.into()));

    let mut body = forecast_body();
    body["daily"]["sunrise"][2] = json!("2026-10-26 05:41");
    assert_eq!(weather::parse_forecast(&bytes(&body)), Err(FORMAT.into()));

    assert_eq!(
        weather::parse_forecast(b"<html>portal</html>"),
        Err(FORMAT.into())
    );

    let http = FakeHttp::new();
    http.route(&format!("{BASE}/forecast"), raw(200, "<html>portal</html>"));
    assert_eq!(
        weather::forecast(&http, &ep(), &quick(), &cacau()).await,
        Err(FORMAT.into())
    );
    http.route(&format!("{BASE}/forecast"), |_| Err(HttpError::Timeout));
    assert_eq!(
        weather::forecast(&http, &ep(), &quick(), &cacau()).await,
        Err(
            "Não consegui falar com o Open-Meteo: o servidor demorou demais para responder.".into()
        )
    );
}

/// `AAAA-MM-DDTHH:MM:SSZ` em segundos Unix (só para os testes).
fn at(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(secs)
}

#[test]
fn past_days_are_dropped_by_the_city_date() {
    let f = weather::parse_forecast(&bytes(&forecast_body())).unwrap();
    // 2026-10-26T02:00Z: ainda 25 na cidade (−4 h).
    let v = f.view(at(1_792_980_000));
    let dates: Vec<&str> = v["daily"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["date"].as_str().unwrap())
        .collect();
    assert_eq!(
        dates,
        [
            "2026-10-25",
            "2026-10-26",
            "2026-10-27",
            "2026-10-28",
            "2026-10-29",
            "2026-10-30"
        ]
    );
    assert_eq!(v["sunrise"], json!("2026-10-25T05:41"));
    assert_eq!(v["sunset"], json!("2026-10-25T18:03"));
    assert!(v["daily"][0].get("sunrise").is_none());
    assert_eq!(v["current"]["isDay"], json!(true));
    assert_eq!(v["daily"][0]["precipitation"], json!(70.0));

    // 2026-10-31T12:00Z: tudo no passado.
    let v = f.view(at(1_793_448_000));
    assert_eq!(v["daily"], json!([]));
    assert_eq!(v["sunrise"], Value::Null);
    assert_eq!(v["sunset"], Value::Null);
}

#[tokio::test]
async fn plain_get_still_looks_for_synced() {
    let plain_get = || {
        answer(
            200,
            lrclib_entry("Legião Urbana", "Tempo Perdido", 201.0, Some(PLAIN), None),
        )
    };
    let synced = Found::Lyrics(Lyrics {
        synced: true,
        lines: synced_lines(),
        source: Source::Lrclib,
        url: None,
    });
    let http = FakeHttp::new();
    http.route(&get_url(), plain_get());
    http.route(
        &search_url(),
        answer(
            200,
            json!([lrclib_entry(
                "Legião Urbana",
                "Tempo Perdido (Remastered)",
                200.0,
                None,
                Some(SYNCED)
            )]),
        ),
    );
    assert_eq!(find(&http, &want(), Some("CHAVE")).await.unwrap(), synced);
    assert_eq!(http.count(), 2);

    let plain = Found::Lyrics(Lyrics {
        synced: false,
        lines: vec![
            Line {
                time: None,
                text: "Todos os dias quando acordo".to_string(),
            },
            Line {
                time: None,
                text: "Não tenho mais".to_string(),
            },
        ],
        source: Source::Lrclib,
        url: None,
    });
    let http = FakeHttp::new();
    http.route(&get_url(), plain_get());
    http.route(
        &search_url(),
        answer(
            200,
            json!([lrclib_entry(
                "Outra Banda",
                "Tempo Perdido",
                201.0,
                None,
                Some(SYNCED)
            )]),
        ),
    );
    http.route(&vagalume_url(), |_| panic!("a LRCLIB já casou"));
    assert_eq!(find(&http, &want(), Some("CHAVE")).await.unwrap(), plain);

    let http = FakeHttp::new();
    http.route(&get_url(), plain_get());
    http.route(&search_url(), answer(500, json!({})));
    assert_eq!(find(&http, &want(), None).await.unwrap(), plain);
}

#[tokio::test]
async fn cities_search() {
    let req = weather::geocoding_request(&ep(), "Cacau");
    assert_eq!(req.url, format!("{BASE}/geocoding/v1/search"));
    assert_eq!(param(&req, "name"), Some("Cacau"));
    assert_eq!(param(&req, "count"), Some("5"));
    assert_eq!(param(&req, "language"), Some("pt"));
    assert_eq!(param(&req, "format"), Some("json"));

    let http = FakeHttp::new();
    http.route(&format!("{BASE}/geocoding"), answer(200, cities_body(6)));
    let cities = weather::search_cities(&http, &ep(), &quick(), "Cacau")
        .await
        .unwrap();
    assert_eq!(cities.len(), 5);
    assert_eq!(cities[0].name, "Cidade 1");
    assert_eq!(cities[0].state, "Rondônia");
    assert_eq!(cities[0].country, "Brasil");
    assert_eq!(cities[0].country_code, "BR");
    assert_eq!(
        cities[0].to_json(),
        json!({"name":"Cidade 1","state":"Rondônia","country":"Brasil","countryCode":"BR",
               "latitude":cities[0].latitude,"longitude":cities[0].longitude})
    );

    assert_eq!(
        weather::parse_cities(&bytes(&json!({"generationtime_ms": 0.5}))),
        Ok(vec![])
    );
    let mut body = cities_body(1);
    body["results"][0].as_object_mut().unwrap().remove("admin1");
    body["results"][0]["name"] = json!("x".repeat(300));
    let cities = weather::parse_cities(&bytes(&body)).unwrap();
    assert_eq!(cities[0].state, "");
    assert_eq!(cities[0].name.chars().count(), 100);

    assert!(weather::parse_cities(b"<html>").is_err());
    http.route(&format!("{BASE}/geocoding"), raw(200, "<html>"));
    assert_eq!(
        weather::search_cities(&http, &ep(), &quick(), "Cacau").await,
        Err(HttpOrFormat::Format)
    );
    http.route(&format!("{BASE}/geocoding"), |_| Err(HttpError::Connect));
    assert_eq!(
        weather::search_cities(&http, &ep(), &quick(), "Cacau").await,
        Err(HttpOrFormat::Http(HttpError::Connect))
    );
}
