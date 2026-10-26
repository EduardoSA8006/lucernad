//! O clima, pelo Open-Meteo: a previsão de 7 dias (com o fuso da cidade) e a
//! busca de cidade.
//!
//! A previsão guarda o `utc_offset_seconds` e o nascer/pôr de cada dia; o
//! [`Forecast::view`] calcula a data local da cidade na hora de publicar e
//! descarta os dias que já passaram: a previsão de ontem mostrada offline
//! não chama ontem de hoje.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

use crate::endpoints::Endpoints;
use crate::http::{Http, HttpError, Request, RetryPolicy, fetch};

/// Quantos dias de previsão.
const DAYS: usize = 7;
/// Quantas cidades a busca devolve.
const CITIES_MAX: usize = 5;
/// O maior texto de uma cidade (nome, estado, país), em caracteres.
const CITY_TEXT_MAX: usize = 100;

const FORMAT: &str = "O Open-Meteo respondeu fora do formato.";

const CURRENT_FIELDS: &str =
    "temperature_2m,apparent_temperature,relative_humidity_2m,weather_code,wind_speed_10m,is_day";
const DAILY_FIELDS: &str = "weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max,sunrise,sunset";

/// A cidade escolhida.
#[derive(Clone, Debug, PartialEq)]
pub struct Location {
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
}

impl Location {
    /// A chave do cache: `"{lat:.2},{lon:.2}"`.
    pub fn place_key(&self) -> String {
        format!("{:.2},{:.2}", self.latitude, self.longitude)
    }
}

/// O tempo agora (°C, %, km/h; `code` é o código WMO).
#[derive(Clone, Debug, PartialEq)]
pub struct Current {
    pub temperature: f64,
    pub apparent: f64,
    pub humidity: f64,
    pub wind: f64,
    pub code: u32,
    pub is_day: bool,
}

/// Um dia da previsão; `date` em `AAAA-MM-DD`, nascer e pôr em
/// `AAAA-MM-DDTHH:MM` (hora local da cidade).
#[derive(Clone, Debug, PartialEq)]
pub struct Day {
    pub date: String,
    pub code: u32,
    pub min: f64,
    pub max: f64,
    pub precipitation: Option<f64>,
    pub sunrise: Option<String>,
    pub sunset: Option<String>,
}

/// A previsão inteira, como veio (até 7 dias), com o fuso da cidade.
#[derive(Clone, Debug, PartialEq)]
pub struct Forecast {
    pub current: Current,
    pub daily: Vec<Day>,
    pub utc_offset: i32,
}

impl Forecast {
    /// O formato do cache, com todos os dias.
    pub fn to_json(&self) -> Value {
        json!({
            "current": current_json(&self.current),
            "daily": self.daily.iter().map(|d| json!({
                "date": d.date,
                "code": d.code,
                "min": d.min,
                "max": d.max,
                "precipitation": d.precipitation,
                "sunrise": d.sunrise,
                "sunset": d.sunset,
            })).collect::<Vec<_>>(),
            "utcOffset": self.utc_offset,
        })
    }

    /// Lê o formato do cache; qualquer coisa fora dele → `None`.
    pub fn from_json(v: &Value) -> Option<Forecast> {
        let c = v.get("current")?;
        let current = Current {
            temperature: finite(c.get("temperature")?)?,
            apparent: finite(c.get("apparent")?)?,
            humidity: finite(c.get("humidity")?)?,
            wind: finite(c.get("wind")?)?,
            code: code(c.get("code")?)?,
            is_day: c.get("isDay")?.as_bool()?,
        };
        let daily = v
            .get("daily")?
            .as_array()?
            .iter()
            .map(|d| {
                Some(Day {
                    date: d.get("date")?.as_str().filter(|s| is_date(s))?.to_string(),
                    code: code(d.get("code")?)?,
                    min: finite(d.get("min")?)?,
                    max: finite(d.get("max")?)?,
                    precipitation: optional(d.get("precipitation")?, finite)?,
                    sunrise: optional(d.get("sunrise")?, time_text)?,
                    sunset: optional(d.get("sunset")?, time_text)?,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        let utc_offset = i32::try_from(v.get("utcOffset")?.as_i64()?).ok()?;
        Some(Forecast {
            current,
            daily,
            utc_offset,
        })
    }

    /// A parte do tópico `web.weather` sem `location`, `state` e
    /// `updatedAt`: `daily` só de hoje em diante (a data local da cidade,
    /// `now + utc_offset`), sem o nascer/pôr por dia; `sunrise`/`sunset` do
    /// dia de hoje, ou `null`.
    pub fn view(&self, now: SystemTime) -> Value {
        let secs = now
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let today = civil_date((secs + i64::from(self.utc_offset)).div_euclid(86_400));
        let days: Vec<&Day> = self
            .daily
            .iter()
            .filter(|d| d.date.as_str() >= today.as_str())
            .collect();
        let current_day = days.iter().find(|d| d.date == today);
        json!({
            "current": current_json(&self.current),
            "daily": days.iter().map(|d| json!({
                "date": d.date,
                "code": d.code,
                "min": d.min,
                "max": d.max,
                "precipitation": d.precipitation,
            })).collect::<Vec<_>>(),
            "sunrise": current_day.and_then(|d| d.sunrise.clone()),
            "sunset": current_day.and_then(|d| d.sunset.clone()),
        })
    }
}

fn current_json(c: &Current) -> Value {
    json!({
        "temperature": c.temperature,
        "apparent": c.apparent,
        "humidity": c.humidity,
        "wind": c.wind,
        "code": c.code,
        "isDay": c.is_day,
    })
}

/// O pedido da previsão: o tempo agora e 7 dias, no fuso da cidade.
pub fn forecast_request(ep: &Endpoints, loc: &Location) -> Request {
    Request::new(format!("{}/v1/forecast", ep.forecast))
        .param("latitude", loc.latitude.to_string())
        .param("longitude", loc.longitude.to_string())
        .param("current", CURRENT_FIELDS)
        .param("daily", DAILY_FIELDS)
        .param("timezone", "auto")
        .param("forecast_days", DAYS.to_string())
}

/// Lê a resposta da previsão (até 7 dias). Listas de tamanhos diferentes,
/// número não finito, data ou hora fora do formato → o erro do Formato; os
/// nulos de precipitação, nascer e pôr viram `None`.
pub fn parse_forecast(body: &[u8]) -> Result<Forecast, String> {
    parse_forecast_value(body).ok_or_else(|| FORMAT.to_string())
}

fn parse_forecast_value(body: &[u8]) -> Option<Forecast> {
    let v: Value = serde_json::from_slice(body).ok()?;
    let c = v.get("current")?;
    let current = Current {
        temperature: finite(c.get("temperature_2m")?)?,
        apparent: finite(c.get("apparent_temperature")?)?,
        humidity: finite(c.get("relative_humidity_2m")?)?,
        wind: finite(c.get("wind_speed_10m")?)?,
        code: code(c.get("weather_code")?)?,
        is_day: match c.get("is_day")?.as_u64()? {
            0 => false,
            1 => true,
            _ => return None,
        },
    };
    let utc_offset = i32::try_from(v.get("utc_offset_seconds")?.as_i64()?).ok()?;
    let d = v.get("daily")?.as_object()?;
    let list = |k: &str| d.get(k).and_then(Value::as_array);
    let dates = list("time")?;
    let codes = list("weather_code")?;
    let maxs = list("temperature_2m_max")?;
    let mins = list("temperature_2m_min")?;
    let precs = list("precipitation_probability_max")?;
    let rises = list("sunrise")?;
    let sets = list("sunset")?;
    let n = dates.len();
    if [codes, maxs, mins, precs, rises, sets]
        .iter()
        .any(|l| l.len() != n)
    {
        return None;
    }
    let daily = (0..n.min(DAYS))
        .map(|i| {
            Some(Day {
                date: dates[i].as_str().filter(|s| is_date(s))?.to_string(),
                code: code(&codes[i])?,
                min: finite(&mins[i])?,
                max: finite(&maxs[i])?,
                precipitation: optional(&precs[i], finite)?,
                sunrise: optional(&rises[i], time_text)?,
                sunset: optional(&sets[i], time_text)?,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(Forecast {
        current,
        daily,
        utc_offset,
    })
}

/// Busca a previsão; os `Err` são os textos do Formato.
pub async fn forecast(
    http: &dyn Http,
    ep: &Endpoints,
    policy: &RetryPolicy,
    loc: &Location,
) -> Result<Forecast, String> {
    let err = |e: HttpError| format!("Não consegui falar com o Open-Meteo: {}.", e.message());
    let resp = fetch(http, &forecast_request(ep, loc), policy)
        .await
        .map_err(err)?;
    if resp.status != 200 {
        return Err(err(HttpError::Status(resp.status)));
    }
    parse_forecast(&resp.body)
}

/// Uma cidade achada na busca.
#[derive(Clone, Debug, PartialEq)]
pub struct City {
    pub name: String,
    pub state: String,
    pub country: String,
    pub country_code: String,
    pub latitude: f64,
    pub longitude: f64,
}

impl City {
    /// `{name, state, country, countryCode, latitude, longitude}`.
    pub fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "state": self.state,
            "country": self.country,
            "countryCode": self.country_code,
            "latitude": self.latitude,
            "longitude": self.longitude,
        })
    }
}

/// O pedido da busca de cidade (até 5, em português).
pub fn geocoding_request(ep: &Endpoints, query: &str) -> Request {
    Request::new(format!("{}/v1/search", ep.geocoding))
        .param("name", query)
        .param("count", CITIES_MAX.to_string())
        .param("language", "pt")
        .param("format", "json")
}

/// Lê a resposta da busca: até 5 cidades; sem `results` → `[]`; texto
/// ausente → `""`, cortado em 100 caracteres; uma cidade sem coordenadas
/// válidas fica de fora.
pub fn parse_cities(body: &[u8]) -> Result<Vec<City>, String> {
    let v: Value = serde_json::from_slice(body).map_err(|_| FORMAT.to_string())?;
    let v = v.as_object().ok_or(FORMAT)?;
    let results = match v.get("results") {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(r) => r.as_array().ok_or(FORMAT)?,
    };
    Ok(results
        .iter()
        .filter_map(Value::as_object)
        .filter_map(city)
        .take(CITIES_MAX)
        .collect())
}

fn city(r: &Map<String, Value>) -> Option<City> {
    let latitude = r
        .get("latitude")
        .and_then(Value::as_f64)
        .filter(|l| (-90.0..=90.0).contains(l))?;
    let longitude = r
        .get("longitude")
        .and_then(Value::as_f64)
        .filter(|l| (-180.0..=180.0).contains(l))?;
    let text = |k: &str| -> String {
        r.get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .chars()
            .filter(|c| !c.is_control())
            .take(CITY_TEXT_MAX)
            .collect()
    };
    Some(City {
        name: text("name"),
        state: text("admin1"),
        country: text("country"),
        country_code: text("country_code"),
        latitude,
        longitude,
    })
}

/// Por que a busca de cidade falhou: o HTTP, ou a resposta fora do formato
/// (o módulo transforma em `1002`/`1003`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HttpOrFormat {
    Http(HttpError),
    Format,
}

/// Busca as cidades.
pub async fn search_cities(
    http: &dyn Http,
    ep: &Endpoints,
    policy: &RetryPolicy,
    query: &str,
) -> Result<Vec<City>, HttpOrFormat> {
    let resp = fetch(http, &geocoding_request(ep, query), policy)
        .await
        .map_err(HttpOrFormat::Http)?;
    if resp.status != 200 {
        return Err(HttpOrFormat::Http(HttpError::Status(resp.status)));
    }
    parse_cities(&resp.body).map_err(|_| HttpOrFormat::Format)
}

/// Um número finito.
fn finite(v: &Value) -> Option<f64> {
    v.as_f64().filter(|n| n.is_finite())
}

/// Um código WMO: inteiro de 0 a `u32::MAX`.
fn code(v: &Value) -> Option<u32> {
    v.as_u64()
        .or_else(|| {
            v.as_f64()
                .filter(|n| n.is_finite() && *n >= 0.0 && n.fract() == 0.0)
                .map(|n| n as u64)
        })
        .and_then(|n| u32::try_from(n).ok())
}

/// `null` → `Some(None)`; um valor válido → `Some(Some(_))`; inválido →
/// `None`.
fn optional<T>(v: &Value, f: impl Fn(&Value) -> Option<T>) -> Option<Option<T>> {
    if v.is_null() {
        Some(None)
    } else {
        f(v).map(Some)
    }
}

/// Uma hora `AAAA-MM-DDTHH:MM`.
fn time_text(v: &Value) -> Option<String> {
    let s = v.as_str()?;
    let (date, hm) = s.split_once('T')?;
    let b = hm.as_bytes();
    let ok = is_date(date)
        && b.len() == 5
        && b[2] == b':'
        && digits(&b[..2])
        && digits(&b[3..])
        && hm[..2].parse::<u32>().ok()? < 24
        && hm[3..].parse::<u32>().ok()? < 60;
    ok.then(|| s.to_string())
}

/// Uma data `AAAA-MM-DD` (mês de 1 a 12, dia de 1 a 31).
fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && digits(&b[..4])
        && digits(&b[5..7])
        && digits(&b[8..])
        && s[5..7].parse::<u32>().is_ok_and(|m| (1..=12).contains(&m))
        && s[8..].parse::<u32>().is_ok_and(|d| (1..=31).contains(&d))
}

fn digits(b: &[u8]) -> bool {
    b.iter().all(u8::is_ascii_digit)
}

/// A data `AAAA-MM-DD` do dia `days` contado de 1970-01-01 (o algoritmo
/// `civil_from_days` de Howard Hinnant).
pub(crate) fn civil_date(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}
