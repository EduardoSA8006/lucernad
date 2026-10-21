use lucernad_clipboard::search::{file_names, find_matches, normalize, preview_for};

#[test]
fn normalize_drops_case_and_accents() {
    assert_eq!(normalize("Ação CAFÉ São"), "acao cafe sao");
    assert_eq!(normalize("Ñandú"), "nandu");
}

#[test]
fn matches_ignore_case_and_accents() {
    assert_eq!(
        find_matches("Uma Ação, outra ação", "acao"),
        vec![4..8, 16..20]
    );
    assert!(find_matches("abc", "").is_empty());
}

#[test]
fn matches_are_utf16_ranges() {
    assert_eq!(
        preview_for("😀 Ação", Some("acao")),
        ("😀 Ação".to_string(), vec![[3, 7]])
    );
}

#[test]
fn preview_collapses_and_cuts() {
    assert_eq!(preview_for("a\n\n  b", None).0, "a b");
    assert_eq!(preview_for(&"x".repeat(1000), None).0.chars().count(), 400);
    let mut text = "x".repeat(900);
    text.push('y');
    text.push_str(&"x".repeat(99));
    let (preview, matches) = preview_for(&text, Some("y"));
    assert!(preview.starts_with('…'));
    assert_eq!(matches, vec![[41, 42]]);
}

#[test]
fn short_query_uses_the_same_rule() {
    assert_eq!(find_matches("Pão", "ao"), vec![1..3]);
}

#[test]
fn file_names_decode() {
    let uris = vec![
        "file:///home/u/relat%C3%B3rio.pdf".to_string(),
        "file:///tmp/a%20b".to_string(),
    ];
    assert_eq!(file_names(&uris), "relatório.pdf a b");
}
