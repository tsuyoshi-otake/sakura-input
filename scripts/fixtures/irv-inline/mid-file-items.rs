fn production_before() {}

#[cfg(test)]
fn test_helper() {
    let brace = '{';
}

fn production_middle() {}

#[allow(dead_code)]
#[cfg(test)]
static TEST_DATA: &str = r##"
#[cfg(test)] mod fake {
}
"##;

struct Worker;
impl Worker {
    #[cfg(test)]
    fn test_method(&self) {
        let value = 1;
        assert_eq!(value, 1);
    }

    fn production_method(&self) {}
}
fn production_after() {}
