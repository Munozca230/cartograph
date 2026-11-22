use crate::store;

pub fn run_all() {
    chain1();
    a1();
    a2();
    a3();
    a4();
    a5();
    a6();
    a7();
    a8();
    a9();
    a10();
}

pub fn chain1() {
    chain2();
}
fn chain2() {
    chain3();
}
fn chain3() {
    chain4();
}
fn chain4() {
    chain5();
}
fn chain5() {}

pub fn countdown(n: u32) {
    if n > 0 {
        countdown(n - 1);
    }
}

pub fn a1() {
    store::flush();
}
pub fn a2() {}
pub fn a3() {}
pub fn a4() {}
pub fn a5() {}
pub fn a6() {}
pub fn a7() {}
pub fn a8() {}
pub fn a9() {}
pub fn a10() {}
