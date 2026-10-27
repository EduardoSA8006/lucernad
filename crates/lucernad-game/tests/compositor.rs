//! O compositor no modo jogo com o Hyprland falso e o diário numa pasta
//! temporária: o plano, o diário antes do `eval`, a restauração exata, o
//! reload no meio e a restauração sem daemon.

use std::collections::BTreeMap;

use lucernad_game::compositor::{apply, apply_after_reload, plan, restore, restore_cli_at};
use lucernad_game::config::GameSettings;
use lucernad_game::journal::{Entry, Store};
use lucernad_hyprland::Hypr;
use lucernad_hyprland::options::OptionValue::{self, Bool, Int};
use lucernad_hyprland::socket::HyprError;
use lucernad_hyprland::testing::{FakeHypr, FakeSockets, default_options};

const BLUR: &str = "decoration:blur:enabled";
const SHADOW: &str = "decoration:shadow:enabled";
const ANIM: &str = "animations:enabled";
const VRR: &str = "misc:vrr";
const TEAR: &str = "general:allow_tearing";

fn current(pairs: &[(&str, OptionValue)]) -> Vec<(String, OptionValue)> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

fn defaults() -> Vec<(String, OptionValue)> {
    default_options().into_iter().collect()
}

fn names(e: &[Entry]) -> Vec<&str> {
    e.iter().map(|e| e.option.as_str()).collect()
}

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path().join("game"));
    (dir, store)
}

#[test]
fn plan_turns_effects_off() {
    let s = GameSettings::default();
    let p = plan(&defaults(), &s);
    for o in [BLUR, SHADOW, ANIM] {
        let e = p.iter().find(|e| e.option == o).unwrap();
        assert_eq!((&e.before, &e.applied), (&Bool(true), &Bool(false)), "{o}");
    }
    let off = GameSettings {
        effects: false,
        vrr: false,
        ..GameSettings::default()
    };
    assert!(plan(&defaults(), &off).is_empty());
    // Já desligado não entra (não há o que devolver).
    let p = plan(
        &current(&[(BLUR, Bool(false)), (SHADOW, Bool(true))]),
        &off_but_effects(),
    );
    assert_eq!(names(&p), vec![SHADOW]);
}

fn off_but_effects() -> GameSettings {
    GameSettings {
        vrr: false,
        ..GameSettings::default()
    }
}

#[test]
fn plan_vrr_only_from_zero() {
    let s = GameSettings {
        effects: false,
        ..GameSettings::default()
    };
    let p = plan(&current(&[(VRR, Int(0))]), &s);
    assert_eq!(p.len(), 1);
    assert_eq!((&p[0].before, &p[0].applied), (&Int(0), &Int(2)));
    for v in [1, 2, 3] {
        assert!(plan(&current(&[(VRR, Int(v))]), &s).is_empty(), "vrr {v}");
    }
}

#[test]
fn plan_tearing_only_when_asked() {
    let s = GameSettings {
        effects: false,
        vrr: false,
        ..GameSettings::default()
    };
    assert!(plan(&defaults(), &s).is_empty());
    let t = GameSettings { tearing: true, ..s };
    let p = plan(&defaults(), &t);
    assert_eq!(names(&p), vec![TEAR]);
    assert_eq!(p[0].applied, Bool(true));
}

#[tokio::test]
async fn journal_written_before_eval() {
    let (_d, store) = store();
    let h = FakeHypr::new();
    h.fail_on("eval", HyprError::TimedOut);
    let e = apply(&h, &store, &GameSettings::default(), &[])
        .await
        .unwrap_err();
    assert_eq!(e, HyprError::TimedOut);
    let j = store.load().expect("o diário já existe antes do eval");
    assert_eq!(j.instance, "fake_1");
    assert_eq!(names(&j.entries), vec![BLUR, SHADOW, ANIM, VRR]);
    // Nada mudou no Hyprland.
    assert_eq!(h.options(), default_options());
}

/// Valores de antes fora do padrão.
fn unusual(h: &FakeHypr) -> BTreeMap<String, OptionValue> {
    h.set_option(BLUR, Bool(true));
    h.set_option(SHADOW, Bool(false));
    h.set_option(ANIM, Bool(true));
    h.set_option(VRR, Int(0));
    h.set_option(TEAR, Bool(false));
    h.options()
}

#[tokio::test]
async fn restore_returns_exact_values() {
    let (_d, store) = store();
    let h = FakeHypr::new();
    let before = unusual(&h);
    let s = GameSettings {
        tearing: true,
        ..GameSettings::default()
    };
    let j = apply(&h, &store, &s, &["0xab".into()])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(h.option(BLUR), Some(Bool(false)));
    assert_eq!(h.option(ANIM), Some(Bool(false)));
    assert_eq!(h.option(VRR), Some(Int(2)));
    assert_eq!(h.option(TEAR), Some(Bool(true)));
    assert_eq!(h.prop("0xab", "immediate").as_deref(), Some("1"));
    // Uma mudança só, num eval só.
    let evals: Vec<String> = h
        .requests()
        .into_iter()
        .filter(|r| r.starts_with("eval hl.config"))
        .collect();
    assert_eq!(evals.len(), 1, "{evals:?}");
    restore(&h, &store, &j).await.unwrap();
    assert_eq!(h.options(), before);
    assert_eq!(h.prop("0xab", "immediate").as_deref(), Some("0"));
    assert_eq!(store.load(), None);
}

#[tokio::test]
async fn user_change_during_game_is_kept() {
    let (_d, store) = store();
    let h = FakeHypr::new();
    let j = apply(&h, &store, &GameSettings::default(), &[])
        .await
        .unwrap()
        .unwrap();
    // O usuário liga as animações na mão durante o jogo.
    h.set_option(ANIM, Bool(true));
    h.set_option(VRR, Int(1));
    restore(&h, &store, &j).await.unwrap();
    assert_eq!(h.option(ANIM), Some(Bool(true)));
    assert_eq!(h.option(VRR), Some(Int(1)), "a escolha dele fica");
    assert_eq!(h.option(BLUR), Some(Bool(true)));
    assert_eq!(h.option(SHADOW), Some(Bool(true)));
}

#[tokio::test]
async fn reload_mid_game_resnapshots_and_reapplies() {
    let (_d, store) = store();
    let h = FakeHypr::new();
    let s = GameSettings::default();
    apply(&h, &store, &s, &[]).await.unwrap();
    // O usuário salva o arquivo: o reload volta tudo ao arquivo, que agora
    // tem o desfoque desligado e o VRR 1.
    h.reload(&[
        (BLUR, Bool(false)),
        (SHADOW, Bool(true)),
        (ANIM, Bool(true)),
        (VRR, Int(1)),
        (TEAR, Bool(false)),
    ]);
    let j = apply_after_reload(&h, &store, &s, &[])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(store.load().as_ref(), Some(&j), "um diário só");
    assert_eq!(
        names(&j.entries),
        vec![SHADOW, ANIM],
        "o antes é o do arquivo"
    );
    assert_eq!(h.option(ANIM), Some(Bool(false)), "reaplicado");
    assert_eq!(h.option(SHADOW), Some(Bool(false)));
    restore(&h, &store, &j).await.unwrap();
    assert_eq!(h.option(BLUR), Some(Bool(false)));
    assert_eq!(h.option(SHADOW), Some(Bool(true)));
    assert_eq!(h.option(ANIM), Some(Bool(true)));
    assert_eq!(h.option(VRR), Some(Int(1)));
}

#[tokio::test]
async fn reapply_keeps_the_first_before() {
    // Um configure no meio (sem reload): o "antes" continua o de antes do
    // primeiro apply, e o item desligado volta na hora.
    let (_d, store) = store();
    let h = FakeHypr::new();
    apply(&h, &store, &GameSettings::default(), &[])
        .await
        .unwrap();
    let no_effects = GameSettings {
        effects: false,
        ..GameSettings::default()
    };
    let j = apply(&h, &store, &no_effects, &[]).await.unwrap().unwrap();
    assert_eq!(names(&j.entries), vec![VRR]);
    assert_eq!(j.entries[0].before, Int(0));
    assert_eq!(h.option(ANIM), Some(Bool(true)), "desfeito na hora");
    assert_eq!(h.option(VRR), Some(Int(2)));
    let nothing = GameSettings {
        effects: false,
        vrr: false,
        ..GameSettings::default()
    };
    assert_eq!(apply(&h, &store, &nothing, &[]).await.unwrap(), None);
    assert_eq!(h.options(), default_options());
    assert_eq!(store.load(), None);
}

#[tokio::test]
async fn journal_of_other_instance_is_dropped() {
    let (_d, store) = store();
    let h = FakeHypr::new();
    let j = apply(&h, &store, &GameSettings::default(), &[])
        .await
        .unwrap()
        .unwrap();
    // O Hyprland reiniciou: outra instância, já com a config do arquivo.
    h.set_instance(Some("fake_2"));
    for (k, v) in default_options() {
        h.set_option(&k, v);
    }
    h.set_option(ANIM, Bool(false)); // o arquivo novo desliga as animações
    restore(&h, &store, &j).await.unwrap();
    assert_eq!(store.load(), None);
    assert_eq!(h.option(ANIM), Some(Bool(false)), "nada foi devolvido");
}

#[tokio::test]
async fn failed_restore_keeps_journal_and_retries() {
    let (_d, store) = store();
    let h = FakeHypr::new();
    let j = apply(&h, &store, &GameSettings::default(), &[])
        .await
        .unwrap()
        .unwrap();
    h.fail_on("eval", HyprError::TimedOut);
    assert_eq!(restore(&h, &store, &j).await, Err(HyprError::TimedOut));
    assert_eq!(store.load(), Some(j.clone()));
    restore(&h, &store, &j).await.unwrap();
    assert_eq!(store.load(), None);
    assert_eq!(h.options(), default_options());
}

#[tokio::test]
async fn restore_cli_without_daemon() {
    let tmp = tempfile::tempdir().unwrap();
    let run = tmp.path().join("run");
    let inst = run.join("hypr/sig_7");
    std::fs::create_dir_all(&inst).unwrap();
    let fake = FakeSockets::start(&inst);
    let store = Store::new(run.join("lucerna/game"));
    // Sem diário: 0 e nenhum pedido.
    assert_eq!(restore_cli_at(&store, &run).await, 0);
    assert!(fake.requests().is_empty());
    // Um modo jogo deixado no meio (o daemon morto com kill -9).
    let h = lucernad_hyprland::SocketHypr::at(&run, Some("sig_7"));
    let j = apply(&h, &store, &GameSettings::default(), &[])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(j.instance, "sig_7");
    assert_eq!(h.instance().as_deref(), Some("sig_7"));
    drop(h);
    assert_eq!(
        fake.with_model(|m| m.options.get(ANIM).cloned()),
        Some(Bool(false))
    );
    assert_eq!(restore_cli_at(&store, &run).await, 0);
    assert_eq!(fake.with_model(|m| m.options.clone()), default_options());
    assert_eq!(store.load(), None);
    // Diário de uma instância que não existe mais: apagado, 0.
    store.save(&j).unwrap();
    std::fs::remove_file(inst.join(".socket.sock")).unwrap();
    assert_eq!(restore_cli_at(&store, &run).await, 0);
    assert_eq!(store.load(), None);
}
