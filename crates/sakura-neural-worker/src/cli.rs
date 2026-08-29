use std::path::PathBuf;
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Stdio(PathBuf),
    Probe(PathBuf),
    SelfTest,
}
pub fn parse(a: &[String]) -> Result<Command, &'static str> {
    let mut m = None;
    let mut p = None;
    let mut i = 1;
    while i < a.len() {
        match a[i].as_str() {
            "--stdio" | "--probe" | "--self-test" => {
                if m.replace(a[i].as_str()).is_some() {
                    return Err("duplicate mode");
                }
            }
            "--model-dir" => {
                i += 1;
                if p.is_some() || i >= a.len() || a[i].is_empty() {
                    return Err("bad model dir");
                }
                p = Some(PathBuf::from(&a[i]))
            }
            _ => return Err("unknown argument"),
        }
        i += 1
    }
    match (m, p) {
        (Some("--stdio"), Some(x)) => Ok(Command::Stdio(x)),
        (Some("--probe"), Some(x)) => Ok(Command::Probe(x)),
        (Some("--self-test"), None) => Ok(Command::SelfTest),
        _ => Err("invalid command"),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn p(x: &[&str]) -> Result<Command, &'static str> {
        parse(&x.iter().map(|v| v.to_string()).collect::<Vec<_>>())
    }
    #[test]
    fn ok() {
        assert!(p(&["x", "--stdio", "--model-dir", "m"]).is_ok());
        assert!(p(&["x", "--probe", "--model-dir", "m"]).is_ok());
        assert!(p(&["x", "--self-test"]).is_ok())
    }
    #[test]
    fn no() {
        for x in [
            &["x"][..],
            &["x", "--stdio"][..],
            &["x", "--force-tier", "avx"][..],
            &["x", "--stdio", "--probe", "--model-dir", "m"][..],
            &["x", "--self-test", "--model-dir", "m"][..],
            &["x", "--probe", "--model-dir", ""][..],
        ] {
            assert!(p(x).is_err())
        }
    }

    #[test]
    fn removed_force_tier_is_rejected_instead_of_claiming_an_execution_tier() {
        assert_eq!(
            p(&["x", "--self-test", "--force-tier", "avx2"]),
            Err("unknown argument")
        );
    }
}
