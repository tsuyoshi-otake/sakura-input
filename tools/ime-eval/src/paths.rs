use std::path::{Path, PathBuf};

use crate::types::{err, Error};

pub fn find_repo_root(start: &Path) -> Result<PathBuf, Error> {
    let mut dir = if start.is_dir() {
        start.to_path_buf()
    } else {
        start
            .parent()
            .ok_or_else(|| err("search root has no parent"))?
            .to_path_buf()
    };
    loop {
        if dir
            .join("eval")
            .join("judge")
            .join("v1")
            .join("manifest.json")
            .is_file()
        {
            return Ok(dir);
        }
        if !dir.pop() {
            break;
        }
    }
    Err(err(
        "could not find eval/judge/v1/manifest.json from the search root",
    ))
}

pub fn find_eval_root(start: &Path) -> Result<PathBuf, Error> {
    Ok(find_repo_root(start)?.join("eval"))
}

pub fn judge_v1_dir(eval_root: &Path) -> PathBuf {
    eval_root.join("judge").join("v1")
}

pub fn semantic_corpus_dir(eval_root: &Path) -> PathBuf {
    eval_root.join("corpus").join("semantic")
}
