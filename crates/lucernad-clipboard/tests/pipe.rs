use std::io::{Read, Write, pipe};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use lucernad_clipboard::mime::TEXT_MAX;
use lucernad_clipboard::pipe::{ReadError, read_pipe, socket_path};

#[test]
fn reads_until_eof() {
    let (reader, mut writer) = pipe().unwrap();
    let data: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    let sent = data.clone();
    let handle = thread::spawn(move || {
        writer.write_all(&sent).unwrap();
    });
    let got = read_pipe(reader, TEXT_MAX, Instant::now() + Duration::from_secs(5));
    handle.join().unwrap();
    assert_eq!(got, Ok(data));
}

#[test]
fn over_limit_is_refused() {
    let (reader, mut writer) = pipe().unwrap();
    let handle = thread::spawn(move || {
        // Quem lê desiste antes do fim: o pipe quebrado aqui é o esperado.
        let _ = writer.write_all(&vec![b'a'; TEXT_MAX + 1]);
    });
    let start = Instant::now();
    let got = read_pipe(reader, TEXT_MAX, start + Duration::from_secs(5));
    assert_eq!(got, Err(ReadError::TooBig));
    assert!(start.elapsed() < Duration::from_secs(1));
    handle.join().unwrap();
}

#[test]
fn reads_at_most_one_byte_past_the_limit() {
    let (reader, mut writer) = pipe().unwrap();
    writer.write_all(&[b'z'; 100]).unwrap();
    drop(writer);
    let mut rest = reader.try_clone().unwrap();
    let got = read_pipe(reader, 10, Instant::now() + Duration::from_secs(5));
    assert_eq!(got, Err(ReadError::TooBig));
    let mut left = Vec::new();
    rest.read_to_end(&mut left).unwrap();
    assert_eq!(left.len(), 100 - 11);
}

#[test]
fn stuck_writer_times_out() {
    let (reader, mut writer) = pipe().unwrap();
    let (done, wait) = mpsc::channel::<()>();
    let handle = thread::spawn(move || {
        writer.write_all(&[b'x'; 10]).unwrap();
        // Segura o pipe aberto até o teste acabar.
        let _ = wait.recv();
        drop(writer);
    });
    let start = Instant::now();
    let got = read_pipe(reader, TEXT_MAX, start + Duration::from_millis(200));
    let elapsed = start.elapsed();
    done.send(()).unwrap();
    handle.join().unwrap();
    assert_eq!(got, Err(ReadError::TimedOut));
    assert!(elapsed >= Duration::from_millis(150), "{elapsed:?}");
    assert!(elapsed < Duration::from_millis(1000), "{elapsed:?}");
}

#[test]
fn socket_paths() {
    assert_eq!(
        socket_path("wayland-1", Some(Path::new("/run/user/1000"))),
        Ok(PathBuf::from("/run/user/1000/wayland-1"))
    );
    assert_eq!(
        socket_path("/tmp/x/wayland-9", None),
        Ok(PathBuf::from("/tmp/x/wayland-9"))
    );
    let err = socket_path("wayland-1", None).unwrap_err();
    assert!(err.contains("XDG_RUNTIME_DIR"), "{err}");
    let err = socket_path("", Some(Path::new("/run/user/1000"))).unwrap_err();
    assert!(err.contains("WAYLAND_DISPLAY"), "{err}");
}
