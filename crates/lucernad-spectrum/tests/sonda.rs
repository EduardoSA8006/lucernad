//! A sonda do backend de verdade, num PipeWire isolado: rode pelo
//! `tests/isolado.sh`, que sobe PipeWire, WirePlumber e D-Bus próprios num
//! container, com um sink nulo (`lucerna-nulo`) e um microfone falso
//! (`lucerna-micro`). Fora dele, os testes ficam ignorados.

use std::collections::BTreeSet;
use std::process::Command;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use lucernad_spectrum::backend::{Backend, Event, OutputNode, Session};
use lucernad_spectrum::pipewire::{NODE_NAME, PipeWire};
use lucernad_spectrum::testing::sine;
use pipewire as pw;
use pw::properties::properties;
use pw::spa;
use pw::spa::pod::Pod;

/// O `node.name` do player de teste.
const PLAYER: &str = "sonda-player";
/// O sink nulo do PipeWire isolado.
const SINK: &str = "lucerna-nulo";
/// A amplitude da senoide do player.
const AMP: f32 = 0.25;

/// Um "player" de teste: um fluxo de saída deste processo, tocando
/// `sine(440, taxa, AMP)` em estéreo, numa thread do PipeWire própria.
struct Player {
    quit: pw::channel::Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl Player {
    fn start() -> Player {
        let (quit, quit_rx) = pw::channel::channel::<()>();
        let (ready, ready_rx) = mpsc::channel::<()>();
        let thread = std::thread::spawn(move || play(quit_rx, ready));
        ready_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("o player de teste subir");
        Player {
            quit,
            thread: Some(thread),
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.quit.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn play(quit: pw::channel::Receiver<()>, ready: mpsc::Sender<()>) {
    const RATE: u32 = 48000;
    const CHANNELS: usize = 2;

    let mainloop = pw::main_loop::MainLoopRc::new(None).expect("o laço do player");
    let context = pw::context::ContextRc::new(&mainloop, None).expect("o contexto do player");
    let core = context.connect_rc(None).expect("o player conectar");
    let stream = pw::stream::StreamRc::new(
        core,
        PLAYER,
        properties! {
            "media.type" => "Audio",
            "media.category" => "Playback",
            "media.role" => "Music",
            "node.name" => PLAYER,
            "audio.channels" => "2",
        },
    )
    .expect("o fluxo do player");

    // Um período inteiro de 440 Hz não cabe em 48000 amostras de forma exata,
    // mas um segundo de senoide repetido não tem emenda (440 ciclos).
    let wave = sine(440.0, RATE, AMP, RATE as usize);
    let _listener = stream
        .add_local_listener_with_user_data(0usize)
        .process(move |stream, pos| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let data = &mut buffer.datas_mut()[0];
            let stride = CHANNELS * size_of::<f32>();
            let frames = match data.data() {
                Some(bytes) => {
                    let frames = bytes.len() / stride;
                    for frame in bytes.chunks_exact_mut(stride).take(frames) {
                        let value = wave[*pos].to_le_bytes();
                        *pos = (*pos + 1) % wave.len();
                        for channel in frame.chunks_exact_mut(size_of::<f32>()) {
                            channel.copy_from_slice(&value);
                        }
                    }
                    frames
                }
                None => 0,
            };
            let chunk = data.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = stride as i32;
            *chunk.size_mut() = (stride * frames) as u32;
        })
        .register()
        .expect("o ouvinte do player");

    let mut info = spa::param::audio::AudioInfoRaw::new();
    info.set_format(spa::param::audio::AudioFormat::F32LE);
    info.set_rate(RATE);
    info.set_channels(CHANNELS as u32);
    let mut position = [0; spa::param::audio::MAX_CHANNELS];
    position[0] = spa::sys::SPA_AUDIO_CHANNEL_FL;
    position[1] = spa::sys::SPA_AUDIO_CHANNEL_FR;
    info.set_position(position);
    let bytes = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(spa::pod::Object {
            type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
            id: spa::param::ParamType::EnumFormat.as_raw(),
            properties: info.into(),
        }),
    )
    .expect("o formato do player")
    .0
    .into_inner();
    let mut params = [Pod::from_bytes(&bytes).expect("o pod do formato")];
    stream
        .connect(
            spa::utils::Direction::Output,
            None,
            pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
            &mut params,
        )
        .expect("o player ligar");

    let quit_loop = mainloop.clone();
    let _quit = quit.attach(mainloop.loop_(), move |()| quit_loop.quit());
    let _ = ready.send(());
    mainloop.run();
}

/// Os links do grafo como pares (nó de saída, nó de entrada), pelo
/// `pw-link -l`.
fn links() -> BTreeSet<(String, String)> {
    let out = Command::new("pw-link")
        .arg("-l")
        .output()
        .expect("rodar o pw-link");
    assert!(out.status.success(), "pw-link -l falhou");
    let text = String::from_utf8_lossy(&out.stdout);
    let node = |port: &str| {
        port.trim()
            .rsplit_once(':')
            .map_or(port.trim(), |(node, _)| node)
            .to_string()
    };
    let mut pairs = BTreeSet::new();
    let mut current = String::new();
    for line in text.lines() {
        if let Some(target) = line.trim_start().strip_prefix("|->") {
            pairs.insert((node(&current), node(target)));
        } else if !line.starts_with(char::is_whitespace) {
            current = line.to_string();
        }
    }
    pairs
}

/// Repete `check` até dar verdadeiro ou o prazo acabar.
fn eventually(within: Duration, mut check: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + within;
    loop {
        if check() {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn involves_capture(pairs: &BTreeSet<(String, String)>) -> bool {
    pairs.iter().any(|(a, b)| a == NODE_NAME || b == NODE_NAME)
}

/// Espera o nó de saída deste processo (o player de teste) no `Graph`.
async fn player_node(session: &mut Session) -> OutputNode {
    let pid = std::process::id();
    let wait = async {
        loop {
            match session.events.recv().await {
                Some(Event::Graph(nodes)) => {
                    if let Some(node) = nodes.into_iter().find(|n| n.client_pid == Some(pid)) {
                        return node;
                    }
                }
                Some(_) => {}
                None => panic!("a sessão fechou esperando o player"),
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(3), wait)
        .await
        .expect("o nó do player aparecer no Graph")
}

/// Espera amostras do `serial` com o RMS da senoide do player.
async fn expect_player_samples(session: &mut Session, serial: u64) {
    let expected = AMP / 2f32.sqrt();
    let wait = async {
        let mut window: Vec<f32> = Vec::new();
        loop {
            match session.events.recv().await {
                Some(Event::Samples {
                    serial: s, mono, ..
                }) if s == serial => {
                    window.extend(mono);
                    // Meio segundo de áudio a partir do primeiro buffer cheio
                    // (o começo do fluxo pode vir com silêncio).
                    if window.len() >= 24000 {
                        let tail = &window[window.len() - 4800..];
                        let rms =
                            (tail.iter().map(|x| x * x).sum::<f32>() / tail.len() as f32).sqrt();
                        if (rms - expected).abs() <= expected * 0.1 {
                            return rms;
                        }
                    }
                }
                Some(_) => {}
                None => panic!("a sessão fechou esperando amostras"),
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(3), wait)
        .await
        .expect("amostras do player com o RMS da senoide em 3 s");
}

#[tokio::test]
#[ignore = "só no PipeWire isolado (tests/isolado.sh)"]
async fn capture_follows_a_player_stream_without_moving_it() {
    let backend = PipeWire::system();
    let player = Player::start();
    let mut session = backend.open().expect("abrir a sessão");
    let node = player_node(&mut session).await;

    assert!(
        eventually(Duration::from_secs(3), || links()
            .contains(&(PLAYER.into(), SINK.into()))),
        "o WirePlumber ligar o player ao sink nulo"
    );
    let before = links();

    session.control.capture(&[node.serial]);
    expect_player_samples(&mut session, node.serial).await;

    let after = links();
    let mut expected = before.clone();
    expected.insert((PLAYER.into(), NODE_NAME.into()));
    assert_eq!(after, expected, "antes: {before:?}");
    drop(player);
}

#[tokio::test]
#[ignore = "só no PipeWire isolado (tests/isolado.sh)"]
async fn capture_right_when_the_node_appears() {
    let backend = PipeWire::system();
    let mut session = backend.open().expect("abrir a sessão");
    let player = Player::start();
    let node = player_node(&mut session).await;
    // Sem esperar o WirePlumber ligar o player ao sink: se o primeiro fluxo
    // for destruído, o backend o reabre.
    session.control.capture(&[node.serial]);
    expect_player_samples(&mut session, node.serial).await;
    drop(player);
}

#[tokio::test]
#[ignore = "só no PipeWire isolado (tests/isolado.sh)"]
async fn capture_never_falls_back_to_the_microphone() {
    let backend = PipeWire::system();
    let player = Player::start();
    let mut session = backend.open().expect("abrir a sessão");
    let node = player_node(&mut session).await;
    session.control.capture(&[node.serial]);
    expect_player_samples(&mut session, node.serial).await;
    assert!(involves_capture(&links()));

    // Durante 2 s, a captura nunca recebe de outro nó (o microfone falso é o
    // padrão do WirePlumber), e no fim saiu do grafo.
    drop(player);
    let end = Instant::now() + Duration::from_secs(2);
    let mut pairs = links();
    while Instant::now() < end {
        pairs = links();
        assert!(
            !pairs.iter().any(|(a, b)| b == NODE_NAME && a != PLAYER),
            "a captura ligou em outro nó: {pairs:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!involves_capture(&pairs), "a captura ficou: {pairs:?}");

    session.control.capture(&[999_999]);
    let end = Instant::now() + Duration::from_secs(2);
    while Instant::now() < end {
        let pairs = links();
        assert!(!involves_capture(&pairs), "a captura ligou em: {pairs:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
}
