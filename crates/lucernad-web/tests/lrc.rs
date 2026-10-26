use lucernad_web::lrc::{
    LINE_CHARS_MAX, LINES_MAX, Line, TEXT_BYTES_MAX, lines_json, parse_lrc, plain_lines,
};

fn timed(lines: &[Line]) -> Vec<(f64, &str)> {
    lines
        .iter()
        .map(|l| (l.time.expect("com tempo"), l.text.as_str()))
        .collect()
}

#[test]
fn several_stamps_on_one_line() {
    let lines = parse_lrc("[00:12.00][01:30.50]refrão\n[00:20]outra");
    assert_eq!(
        timed(&lines),
        [(12.0, "refrão"), (20.0, "outra"), (90.5, "refrão")]
    );
}

#[test]
fn offset_shifts_every_line() {
    let t = |lrc: &str| parse_lrc(lrc)[0].time.unwrap();
    assert_eq!(t("[offset:+500]\n[00:10.00]a"), 9.5);
    assert_eq!(t("[offset:-1000]\n[00:10.00]a"), 11.0);
    assert_eq!(t("[offset:+20000]\n[00:10.00]a"), 0.0);
}

#[test]
fn empty_lines_are_pauses() {
    let lines = parse_lrc("[00:01.00]a\n[00:05.00]\n[00:09.00]b");
    assert_eq!(timed(&lines), [(1.0, "a"), (5.0, ""), (9.0, "b")]);
}

#[test]
fn metadata_and_garbage_are_ignored() {
    let lines = parse_lrc("[ar:Artista]\n[ti:x]\nsem tempo\n[99:aa]x\n[00:02.5]ok");
    assert_eq!(timed(&lines), [(2.5, "ok")]);
    assert_eq!(parse_lrc("[100:00.00]a")[0].time, Some(6000.0));
    assert_eq!(parse_lrc("[00:01.234]a")[0].time, Some(1.234));
}

#[test]
fn limits_hold() {
    let many: String = (0..5000)
        .map(|i| format!("[00:{:02}]x\n", i % 60))
        .collect();
    assert_eq!(parse_lrc(&many).len(), LINES_MAX);

    let long = format!("[00:01]{}", "é".repeat(10_000));
    assert_eq!(parse_lrc(&long)[0].text.chars().count(), LINE_CHARS_MAX);

    let euros = "€".repeat(200);
    let heavy: String = (0..1000).map(|_| format!("[00:01]{euros}\n")).collect();
    let lines = parse_lrc(&heavy);
    assert_eq!(lines.len(), 218);
    assert!(lines.iter().map(|l| l.text.len()).sum::<usize>() <= TEXT_BYTES_MAX);

    let plain: String = (0..1000).map(|_| format!("{euros}\n")).collect();
    let lines = plain_lines(&plain);
    assert_eq!(lines.len(), 218);
    assert!(lines.iter().map(|l| l.text.len()).sum::<usize>() <= TEXT_BYTES_MAX);

    let many_plain = "x\n".repeat(5000);
    assert_eq!(plain_lines(&many_plain).len(), LINES_MAX);
    let long_plain = "é".repeat(10_000);
    assert_eq!(
        plain_lines(&long_plain)[0].text.chars().count(),
        LINE_CHARS_MAX
    );
}

#[test]
fn control_characters_are_removed() {
    assert_eq!(parse_lrc("[00:01]a\u{1b}[2Jb")[0].text, "a[2Jb");
    assert_eq!(plain_lines("a\u{1b}[2Jb")[0].text, "a[2Jb");
}

#[test]
fn plain_lyrics() {
    let lines = plain_lines("a\r\n\r\nb\n\n");
    let line = |t: &str| Line {
        time: None,
        text: t.to_string(),
    };
    assert_eq!(lines, [line("a"), line(""), line("b")]);
    let json = lines_json(&lines);
    assert_eq!(
        json.to_string(),
        r#"[{"time":null,"text":"a"},{"time":null,"text":""},{"time":null,"text":"b"}]"#
    );
    let synced = lines_json(&parse_lrc("[00:01.5]a"));
    assert_eq!(synced.to_string(), r#"[{"time":1.5,"text":"a"}]"#);
}

#[test]
fn huge_stamp_lines_stay_bounded() {
    let stamps = "[00:01.00]".repeat(200_000);
    let lrc = format!("{stamps}{}", "x".repeat(1_000_000));
    let start = std::time::Instant::now();
    let lines = parse_lrc(&lrc);
    assert!(start.elapsed() < std::time::Duration::from_secs(5));
    assert!(!lines.is_empty() && lines.len() <= LINES_MAX);
    assert!(
        lines
            .iter()
            .all(|l| l.text.chars().count() <= LINE_CHARS_MAX)
    );
}
