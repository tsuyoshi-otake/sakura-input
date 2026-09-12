fn production() {
    let text = "#[cfg(test)] mod tests { }";
    let raw = r###"#[cfg(test)] { ; }"###;
    let multiline = "still production
#[cfg(test)] mod not_an_item { }
";
    let character = '}';
    // #[cfg(test)] fn commented() {}
    /* outer #[cfg(test)] {
       /* nested } */
    */
    assert!(!text.is_empty() && !raw.is_empty() && !multiline.is_empty() && character == '}');
}
