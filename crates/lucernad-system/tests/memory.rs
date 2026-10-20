use lucernad_system::memory::{Memory, parse_meminfo};
use lucernad_system::testing::testdata;

#[test]
fn memory_and_swap() {
    let m = parse_meminfo(&std::fs::read_to_string(testdata("machine/proc/meminfo")).unwrap());
    assert_eq!(
        (m.mem_total, m.mem_used, m.swap_total, m.swap_used),
        (16384000, 10240000, 2048000, 1536000)
    );
    assert_eq!(
        parse_meminfo(""),
        Memory::default(),
        "chave que falta conta 0"
    );
}
