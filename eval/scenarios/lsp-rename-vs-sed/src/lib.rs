pub fn foo_bar(x: i32) -> i32 {
    x.saturating_mul(2)
}

pub fn caller() -> i32 {
    foo_bar(5)
}
