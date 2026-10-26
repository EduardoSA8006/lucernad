//! As bases das fontes: as de verdade e a de teste, só em loopback.

use std::ffi::OsStr;

use lucernad_web::endpoints::{Endpoints, base_from_env};

#[test]
fn real_and_local_bases() {
    let real = Endpoints::real();
    assert_eq!(real.lrclib, "https://lrclib.net");
    assert_eq!(real.vagalume, "https://api.vagalume.com.br");
    assert_eq!(real.forecast, "https://api.open-meteo.com");
    assert_eq!(real.geocoding, "https://geocoding-api.open-meteo.com");

    let local = Endpoints::under("http://127.0.0.1:8080");
    assert_eq!(local.lrclib, "http://127.0.0.1:8080/lrclib");
    assert_eq!(local.vagalume, "http://127.0.0.1:8080/vagalume");
    assert_eq!(local.forecast, "http://127.0.0.1:8080/forecast");
    assert_eq!(local.geocoding, "http://127.0.0.1:8080/geocoding");
}

#[test]
fn test_base_only_on_loopback() {
    assert_eq!(base_from_env(None), Ok(None));
    assert_eq!(base_from_env(Some(OsStr::new(""))), Ok(None));
    for good in ["http://127.0.0.1:8080", "http://[::1]:9"] {
        assert_eq!(
            base_from_env(Some(OsStr::new(good))),
            Ok(Some(good.to_string())),
            "{good}"
        );
    }
    for bad in [
        "http://localhost:8080",
        "https://127.0.0.1:8080",
        "http://127.0.0.1",
        "http://127.0.0.1:0",
        "http://127.0.0.1:8080/x",
        "http://10.0.0.1:80",
        "http://127.0.0.1.evil.com:80",
        "http://user@127.0.0.1:8",
        "http://127.0.0.1:70000",
    ] {
        let err = base_from_env(Some(OsStr::new(bad))).expect_err(bad);
        assert!(err.contains("LUCERNAD_WEB_BASE inválido"), "{bad}: {err}");
    }
}
