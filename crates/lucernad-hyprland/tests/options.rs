//! O `j/getoption` (as formas da sonda do Hyprland 0.56.2) e o Lua do
//! `hl.config` que muda as opções.

use lucernad_hyprland::options::{OptionValue, config_lua, parse_getoption};

#[test]
fn parses_getoption_kinds() {
    use OptionValue::*;
    let cases = [
        (
            r#"{"option": "decoration:blur:enabled", "bool": true, "set": false }"#,
            Bool(true),
        ),
        (
            r#"{"option": "general:allow_tearing", "bool": false, "set": true }"#,
            Bool(false),
        ),
        (r#"{"option": "misc:vrr", "int": 0, "set": false }"#, Int(0)),
        (
            r#"{"option": "decoration:active_opacity", "float": 1.000000, "set": false }"#,
            Float(1.0),
        ),
        (
            r#"{"option": "general:layout", "str": "dwindle", "set": false }"#,
            Str("dwindle".into()),
        ),
    ];
    for (json, want) in cases {
        assert_eq!(parse_getoption(json).unwrap(), want, "{json}");
    }
    assert!(parse_getoption("no such option").is_err());
    assert!(
        parse_getoption(
            r#"{"option": "general:col.active_border", "gradient": "ff88c0d0 0deg", "set": true }"#
        )
        .is_err()
    );
}

#[test]
fn option_value_round_trips_as_json() {
    let v = OptionValue::Int(2);
    let json = serde_json::to_string(&v).unwrap();
    assert_eq!(json, r#"{"int":2}"#);
    assert_eq!(serde_json::from_str::<OptionValue>(&json).unwrap(), v);
}

#[test]
fn config_lua_nests_and_joins() {
    use OptionValue::*;
    assert_eq!(
        config_lua(&[
            ("decoration:blur:enabled", Bool(false)),
            ("decoration:shadow:enabled", Bool(false)),
        ]),
        "hl.config({ decoration = { blur = { enabled = false }, shadow = { enabled = false } } })"
    );
    assert_eq!(
        config_lua(&[
            ("animations:enabled", Bool(true)),
            ("misc:vrr", Int(2)),
            ("decoration:active_opacity", Float(0.5)),
        ]),
        "hl.config({ animations = { enabled = true }, misc = { vrr = 2 }, decoration = { active_opacity = 0.5 } })"
    );
    // Float inteiro continua com ponto (o Lua não confunde com inteiro).
    assert_eq!(
        config_lua(&[("decoration:active_opacity", Float(1.0))]),
        "hl.config({ decoration = { active_opacity = 1.0 } })"
    );
}

#[test]
fn config_lua_escapes_strings() {
    assert_eq!(
        config_lua(&[("general:layout", OptionValue::Str("a\"b\\c\nd]]".into()))]),
        r#"hl.config({ general = { layout = "a\"b\\c\nd]]" } })"#
    );
    // Um pedaço do nome que não é identificador do Lua vai entre colchetes.
    assert_eq!(
        config_lua(&[(
            "general:col.active_border",
            OptionValue::Str("ff0000ff".into())
        )]),
        r#"hl.config({ general = { ["col.active_border"] = "ff0000ff" } })"#
    );
}

#[test]
fn fake_reads_back_the_generated_lua() {
    use lucernad_hyprland::testing::{Model, parse_config_lua};
    use lucernad_hyprland::window_prop_lua;
    let changes = [
        ("decoration:blur:enabled", OptionValue::Bool(false)),
        ("misc:vrr", OptionValue::Int(2)),
        ("decoration:active_opacity", OptionValue::Float(0.5)),
        (
            "general:col.active_border",
            OptionValue::Str("a\"b\\c\nd".into()),
        ),
    ];
    let mut back = parse_config_lua(&config_lua(&changes)).unwrap();
    back.sort_by(|a, b| a.0.cmp(&b.0));
    let mut want: Vec<(String, OptionValue)> = changes
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();
    want.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(back, want);
    let mut m = Model::default();
    assert_eq!(
        m.handle(&format!(
            "eval {}",
            window_prop_lua("0xab", "immediate", "1")
        )),
        "ok"
    );
    assert_eq!(m.prop("0xab", "immediate").as_deref(), Some("1"));
    assert_eq!(m.handle("eval hl.config({ misc = { vrr = 2 } })"), "ok");
    assert_eq!(
        parse_getoption(&m.handle("j/getoption misc:vrr")).unwrap(),
        OptionValue::Int(2)
    );
}
