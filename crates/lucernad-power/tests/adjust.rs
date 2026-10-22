//! O juntar de ajustes: no máximo uma escrita em voo, o pedido do meio só
//! troca o alvo, o `change` soma ao alvo pendente e só a última releitura é
//! publicada. `set`/`change` em percentual; o resto em valor bruto.

use lucernad_power::adjust::{Adjuster, Step, percent_of, raw_of};

#[test]
fn percent_and_raw() {
    assert_eq!(raw_of(50, 19200), 9600);
    assert_eq!(raw_of(1, 100), 1);
    assert_eq!(raw_of(1, 50), 1, "o mínimo é 1, nunca 0");
    assert_eq!(percent_of(9600, 19200), 50);
    assert_eq!(percent_of(19199, 19200), 100);
}

#[test]
fn set_while_writing_keeps_only_the_last() {
    let mut a = Adjuster::new(50, 100);
    assert_eq!(a.set(60), Step::Write(60));
    assert_eq!(a.set(70), Step::Idle);
    assert_eq!(a.set(80), Step::Idle);
    assert_eq!(a.written(Ok(60)), Step::Write(80));
    assert_eq!(a.written(Ok(80)), Step::Publish(80));
    assert!(!a.busy());
    assert_eq!(a.current(), 80);
}

#[test]
fn change_accumulates_over_the_pending_target() {
    let mut a = Adjuster::new(50, 100);
    assert_eq!(a.change(5), Step::Write(55));
    for _ in 0..19 {
        assert_eq!(a.change(5), Step::Idle);
    }
    assert_eq!(a.written(Ok(55)), Step::Write(100), "limitado a 100");
    assert_eq!(a.written(Ok(100)), Step::Publish(100));
    assert_eq!(a.change(-200), Step::Write(1), "o mínimo é 1%");
}

#[test]
fn units_with_a_real_max() {
    let mut a = Adjuster::new(9600, 19200);
    assert_eq!(a.set(30), Step::Write(5760));
    assert_eq!(a.written(Ok(5760)), Step::Publish(5760));
    assert_eq!(a.change(5), Step::Write(6720), "35%");
    assert_eq!(a.written(Ok(6720)), Step::Publish(6720));
    assert_eq!(a.change(-50), Step::Write(192), "1%");
}

#[test]
fn failure_reports_and_frees() {
    let mut a = Adjuster::new(50, 100);
    assert_eq!(a.set(60), Step::Write(60));
    assert_eq!(a.written(Err("x".into())), Step::Failed("x".into()));
    assert!(!a.busy());
    assert_eq!(a.set(60), Step::Write(60));
}

#[test]
fn external_change_publishes_only_when_idle() {
    let mut a = Adjuster::new(50, 100);
    assert!(a.external(30));
    assert_eq!(a.current(), 30);
    assert_eq!(a.set(60), Step::Write(60));
    assert!(!a.external(31));
    assert_eq!(a.current(), 31);
}

#[test]
fn floor_holds_with_a_real_max() {
    let mut a = Adjuster::new(192, 19200);
    assert_eq!(a.change(-1), Step::Write(192), "no piso de 1%, fica nele");
    assert_eq!(a.written(Ok(192)), Step::Publish(192));
    assert_eq!(a.change(-5), Step::Write(192));
    assert_eq!(a.written(Ok(192)), Step::Publish(192));
    // Uma base abaixo de 1% (mudança de fora) com delta negativo vai ao piso.
    assert!(a.external(100));
    assert_eq!(a.change(-5), Step::Write(192));
}

#[test]
fn small_max_steps_by_one() {
    let mut a = Adjuster::new(5, 10);
    // 51% de 10 arredonda para 5: anda um.
    assert_eq!(a.change(1), Step::Write(6));
    assert_eq!(a.written(Ok(6)), Step::Publish(6));
    assert_eq!(a.change(-1), Step::Write(5));
    assert_eq!(a.written(Ok(5)), Step::Publish(5));
    // No limite, fica nele.
    let mut top = Adjuster::new(10, 10);
    assert_eq!(top.change(5), Step::Write(10));
    let mut bottom = Adjuster::new(1, 10);
    assert_eq!(bottom.change(-5), Step::Write(1));
}

#[test]
fn failure_with_a_pending_target_tries_the_target() {
    let mut a = Adjuster::new(50, 100);
    assert_eq!(a.set(60), Step::Write(60));
    assert_eq!(a.set(70), Step::Idle);
    assert_eq!(a.written(Err("x".into())), Step::Write(70));
    assert_eq!(a.written(Err("y".into())), Step::Failed("y".into()));
    assert!(!a.busy());
}

#[test]
fn external_during_a_write_is_kept_and_the_reread_published() {
    let mut a = Adjuster::new(50, 100);
    assert_eq!(a.set(60), Step::Write(60));
    assert!(!a.external(40), "com escrita em voo, não publica");
    assert_eq!(a.written(Ok(60)), Step::Publish(60));
    assert_eq!(a.current(), 60);
    assert!(a.external(45));
}

#[test]
fn new_max_drops_the_raw_targets() {
    let mut a = Adjuster::new(50, 100);
    assert_eq!(a.set(60), Step::Write(60));
    assert_eq!(a.set(70), Step::Idle);
    a.set_max(200);
    assert!(!a.busy(), "os alvos eram do máximo antigo");
    assert_eq!(a.written(Ok(120)), Step::Publish(120));
    assert_eq!(a.set(70), Step::Write(140));
}
