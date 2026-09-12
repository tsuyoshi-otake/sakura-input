#[allow(dead_code)]
# [ cfg ( test ) ]
# [allow(unused_variables)]
fn escaped_quote<const N: usize = { 1 + 1 }>() {
    let quote = '\'';
    let value = N;
}
fn production_after() {}
