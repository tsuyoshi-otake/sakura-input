use crate::hash::sha256_hex;
use crate::types::{Assignment, Side};

pub fn opaque_id(seed: u64, case_id: &str) -> String {
    let digest = sha256_hex(format!("opaque:{seed}:{case_id}").as_bytes());
    format!("c-{}", &digest[..12])
}

pub fn assignment(seed: u64, case_id: &str) -> Assignment {
    let digest = sha256_hex(format!("ab:{seed}:{case_id}").as_bytes());
    let system_a = if digest.as_bytes()[0].is_multiple_of(2) {
        Side::Baseline
    } else {
        Side::Candidate
    };
    Assignment { system_a }
}

pub fn swap_is_consistent(
    first: crate::types::Verdict,
    first_assignment: Assignment,
    second: crate::types::Verdict,
    second_assignment: Assignment,
) -> bool {
    first_assignment.side_for(first) == second_assignment.side_for(second)
}
