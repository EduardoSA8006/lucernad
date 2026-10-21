use lucernad_clipboard::mime::{
    Choice, GNOME_FILES, IMAGE_MAX, Kind, OWN_MIME, TEXT_MAX, TEXT_MIMES, URI_LIST, choose,
    offers_for,
};

fn m(mimes: &[&str]) -> Vec<String> {
    mimes.iter().map(|s| s.to_string()).collect()
}

fn read(kind: Kind, mime: &str) -> Choice {
    Choice::Read {
        kind,
        mime: mime.to_string(),
    }
}

#[test]
fn secret_wins_over_everything() {
    assert_eq!(
        choose(&m(&["text/plain", "x-kde-passwordManagerHint"])),
        Choice::Secret
    );
    assert_eq!(
        choose(&m(&["image/png", "application/x-kde-passwordManagerHint"])),
        Choice::Secret
    );
}

#[test]
fn own_copy_is_recognized() {
    assert_eq!(choose(&m(&["text/plain", OWN_MIME])), Choice::Own);
}

#[test]
fn image_before_files_before_text() {
    assert_eq!(
        choose(&m(&["text/plain", "image/jpeg", "image/png"])),
        read(Kind::Image, "image/png")
    );
    assert_eq!(
        choose(&m(&["text/plain", "image/webp"])),
        read(Kind::Image, "image/webp")
    );
    assert_eq!(
        choose(&m(&["text/plain;charset=utf-8", "text/uri-list"])),
        read(Kind::Files, "text/uri-list")
    );
    assert_eq!(
        choose(&m(&["STRING", "text/plain", "UTF8_STRING"])),
        read(Kind::Text, "text/plain")
    );
}

#[test]
fn html_alone_is_nothing() {
    assert_eq!(choose(&m(&["text/html"])), Choice::Nothing);
    assert_eq!(choose(&m(&[])), Choice::Nothing);
}

#[test]
fn offers_cover_each_kind() {
    let text = offers_for(Kind::Text, None, "olá".as_bytes(), 7);
    let names: Vec<&str> = text.iter().map(|(mime, _)| mime.as_str()).collect();
    let mut expected: Vec<&str> = TEXT_MIMES.to_vec();
    expected.push(OWN_MIME);
    assert_eq!(names, expected);
    for (_, data) in &text[..TEXT_MIMES.len()] {
        assert_eq!(&data[..], "olá".as_bytes());
    }
    assert_eq!(&text.last().unwrap().1[..], b"7");

    let uris = b"file:///a\r\n# c\r\nfile:///b\r\n";
    let files = offers_for(Kind::Files, Some(URI_LIST), uris, 3);
    let names: Vec<&str> = files.iter().map(|(mime, _)| mime.as_str()).collect();
    assert_eq!(names, [URI_LIST, GNOME_FILES, OWN_MIME]);
    assert_eq!(&files[0].1[..], uris);
    assert_eq!(&files[1].1[..], b"copy\nfile:///a\nfile:///b");
    assert_eq!(&files[2].1[..], b"3");

    let image = offers_for(Kind::Image, Some("image/jpeg"), b"\xff\xd8", 12);
    let names: Vec<&str> = image.iter().map(|(mime, _)| mime.as_str()).collect();
    assert_eq!(names, ["image/jpeg", OWN_MIME]);
    assert_eq!(&image[0].1[..], b"\xff\xd8");
    assert_eq!(&image[1].1[..], b"12");
}

#[test]
fn kind_round_trip() {
    assert_eq!(Kind::parse("files"), Some(Kind::Files));
    assert_eq!(Kind::parse("x"), None);
    for kind in [Kind::Text, Kind::Image, Kind::Files] {
        assert_eq!(Kind::parse(kind.as_str()), Some(kind));
    }
    assert_eq!(Kind::Files.max_size(), TEXT_MAX);
    assert_eq!(Kind::Text.max_size(), TEXT_MAX);
    assert_eq!(Kind::Image.max_size(), IMAGE_MAX);
}
