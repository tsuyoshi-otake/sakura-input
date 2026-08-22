use crate::types::{err, Certainty, Error, JudgeResult, Verdict, REASON_CODES};

pub fn parse_result(text: &str) -> Result<JudgeResult, Error> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|error| err(format!("judge JSON: {error}")))?;
    let object = value
        .as_object()
        .ok_or_else(|| err("judge result is not an object"))?;
    for required in [
        "case_id",
        "verdict",
        "severity",
        "certainty",
        "reason_codes",
        "short_reason",
    ] {
        if !object.contains_key(required) {
            return Err(err(format!("judge result missing {required}")));
        }
    }
    if object.keys().any(|key| {
        !matches!(
            key.as_str(),
            "case_id" | "verdict" | "severity" | "certainty" | "reason_codes" | "short_reason"
        )
    }) {
        return Err(err("judge result has additional properties"));
    }
    let case_id = object["case_id"]
        .as_str()
        .ok_or_else(|| err("case_id must be a string"))?
        .to_owned();
    let verdict = Verdict::parse(
        object["verdict"]
            .as_str()
            .ok_or_else(|| err("verdict must be a string"))?,
    )?;
    let severity = object["severity"]
        .as_u64()
        .ok_or_else(|| err("severity must be an integer"))?;
    if severity > 4 {
        return Err(err("severity out of range"));
    }
    let certainty = match object["certainty"].as_str() {
        Some("low") => Certainty::Low,
        Some("medium") => Certainty::Medium,
        Some("high") => Certainty::High,
        _ => return Err(err("certainty must be low, medium, or high")),
    };
    let codes = object["reason_codes"]
        .as_array()
        .ok_or_else(|| err("reason_codes must be an array"))?;
    let mut reason_codes = Vec::new();
    for code in codes {
        let code = code
            .as_str()
            .ok_or_else(|| err("reason_code must be a string"))?;
        if !REASON_CODES.contains(&code) {
            return Err(err(format!("unknown reason_code {code}")));
        }
        reason_codes.push(code.to_owned());
    }
    let short_reason = object["short_reason"]
        .as_str()
        .ok_or_else(|| err("short_reason must be a string"))?
        .to_owned();
    if short_reason.chars().count() > 240 {
        return Err(err("short_reason longer than 240 characters"));
    }
    if matches!(verdict, Verdict::Tie | Verdict::Ungradable) && severity != 0 {
        return Err(err("tie/ungradable results must use severity 0"));
    }
    Ok(JudgeResult {
        case_id,
        verdict,
        severity: u8::try_from(severity).map_err(|_| err("severity out of range"))?,
        certainty,
        reason_codes,
        short_reason,
    })
}
