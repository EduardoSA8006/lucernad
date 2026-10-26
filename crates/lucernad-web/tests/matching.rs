use lucernad_web::matching::{Track, first_artist, normalize, same_track, title_key};

fn want(artist: &str, title: &str, duration: Option<f64>) -> Track {
    Track {
        artist: artist.to_string(),
        title: title.to_string(),
        album: String::new(),
        duration,
    }
}

#[test]
fn accents_case_and_punctuation() {
    assert_eq!(normalize("Ação, É Já!"), "acao e ja");
    assert_eq!(normalize("  AC/DC "), "ac dc");
}

#[test]
fn known_suffixes_are_dropped() {
    for t in [
        "Faroeste Caboclo - Remastered 2011",
        "Faroeste Caboclo (Ao Vivo)",
        "Faroeste Caboclo [Live]",
        "Faroeste Caboclo - Ao Vivo",
        "Faroeste Caboclo (feat. Fulano)",
        "Faroeste Caboclo ft. Fulano",
    ] {
        assert_eq!(title_key(t), "faroeste caboclo", "{t}");
    }
    assert_eq!(
        title_key("Tempo Perdido (Instrumental)"),
        "tempo perdido instrumental"
    );
    assert_eq!(title_key("Música (Parte 2)"), "musica parte 2");
}

#[test]
fn suffix_words_are_whole_words() {
    assert_eq!(title_key("Recomeço (Começo)"), "recomeco comeco");
    assert_eq!(title_key("Música - Parte Final"), "musica parte final");
    assert_eq!(title_key("Música (Liveira)"), "musica liveira");
    assert_eq!(
        title_key("Música (Edição Especial)"),
        "musica edicao especial"
    );
}

#[test]
fn first_artist_splits_guests() {
    assert_eq!(first_artist("Anitta, Cardi B"), "anitta");
    assert_eq!(first_artist("Chitãozinho & Xororó"), "chitaozinho");
    assert_eq!(first_artist("Skank feat. Fulano"), "skank");
}

#[test]
fn same_track_accepts_and_refuses() {
    let w = want("Legião Urbana", "Tempo Perdido - Remastered", Some(201.0));
    assert!(same_track(
        &w,
        "Tempo Perdido",
        "Legião Urbana",
        Some(202.5)
    ));
    assert!(same_track(&w, "tempo perdido", "LEGIAO URBANA", None));
    assert!(!same_track(&w, "Tempo Perdido", "Outra Banda", None));
    assert!(!same_track(
        &w,
        "Tempo Perdido",
        "Legião Urbana",
        Some(204.0)
    ));
    assert!(!same_track(&w, "Tempo", "Legião Urbana", None));
}

#[test]
fn guests_match_the_main_artist() {
    let w = want("Anitta, Cardi B", "Me Gusta", None);
    assert!(same_track(&w, "Me Gusta", "Anitta", None));
    assert!(!same_track(&w, "Me Gusta", "Cardi B", None));
    let w = want("Chitãozinho & Xororó", "Evidências", None);
    assert!(same_track(&w, "Evidências", "Chitãozinho & Xororó", None));
}

#[test]
fn key_is_normalized_and_rounded() {
    let a = want("Legião Urbana", "Tempo Perdido", Some(201.4)).key();
    let b = want("LEGIAO URBANA", "Tempo Perdido (Remastered)", Some(200.6)).key();
    assert_eq!(a, b);
    assert!(want("A", "B", None).key().ends_with("\u{1f}-"));
}
