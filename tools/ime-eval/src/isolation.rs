use std::fs;
use std::path::{Path, PathBuf};

use crate::types::{err, Error};

#[derive(Debug)]
pub struct IsolationDir {
    pub path: PathBuf,
}

impl IsolationDir {
    pub fn create(parent: &Path, case_json: &str, schema_bytes: &[u8]) -> Result<Self, Error> {
        fs::create_dir_all(parent)
            .map_err(|error| err(format!("create {}: {error}", parent.display())))?;
        let suffix = crate::hash::sha256_hex(case_json.as_bytes());
        let mut path = None;
        for attempt in 0..32u32 {
            let candidate = parent.join(format!(
                "sakura-ime-judge-{}-{}-{}",
                std::process::id(),
                &suffix[..12],
                attempt
            ));
            match fs::create_dir(&candidate) {
                Ok(()) => {
                    path = Some(candidate);
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(err(format!("create {}: {error}", candidate.display())));
                }
            }
        }
        let path =
            path.ok_or_else(|| err("could not allocate a unique Judge isolation directory"))?;
        fs::write(path.join("case.json"), case_json)
            .map_err(|error| err(format!("write case.json: {error}")))?;
        fs::write(path.join("result.schema.json"), schema_bytes)
            .map_err(|error| err(format!("write result.schema.json: {error}")))?;
        let listing = listed_names(&path)?;
        if listing != ["case.json", "result.schema.json"] {
            return Err(err(format!(
                "isolation dir contains unexpected files: {listing:?}"
            )));
        }
        Ok(Self { path })
    }

    pub fn listed_names(&self) -> Result<Vec<String>, Error> {
        listed_names(&self.path)
    }

    pub fn remove(self) -> Result<(), Error> {
        fs::remove_dir_all(&self.path)
            .map_err(|error| err(format!("remove {}: {error}", self.path.display())))
    }
}

fn listed_names(path: &Path) -> Result<Vec<String>, Error> {
    let mut names = Vec::new();
    for entry in
        fs::read_dir(path).map_err(|error| err(format!("list {}: {error}", path.display())))?
    {
        let entry = entry.map_err(|error| err(format!("list {}: {error}", path.display())))?;
        names.push(entry.file_name().to_string_lossy().into_owned());
    }
    names.sort();
    Ok(names)
}
