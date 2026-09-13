//! Dormant, std-only wire contract for Sakura Context Intelligence.
//!
//! This crate deliberately has no production callers yet. It defines the
//! versioned snapshot and residual-score boundary shared by Context Prediction
//! and Sakura-Rerank without activating either task or changing the existing
//! engine/TSF protocol.

#![forbid(unsafe_code)]

use std::fmt;

pub const CONTRACT_VERSION: u16 = 1;
pub const WIRE_VERSION: u16 = 2;
pub const FRAME_HEADER_LEN: usize = 4;
pub const MAX_FRAME: usize = 32 * 1024;
pub const MAX_PAYLOAD: usize = MAX_FRAME - FRAME_HEADER_LEN;
pub const MAX_CONTEXT_BYTES: usize = 512;
pub const MAX_READING_BYTES: usize = 512;
pub const MAX_CANDIDATE_SURFACE_BYTES: usize = 512;
pub const MAX_PREDICTION_CANDIDATES: usize = 32;
pub const MAX_RERANK_CANDIDATES: usize = 6;
pub const MAX_PREDICTION_DEADLINE_MS: u16 = 10;
pub const MAX_RERANK_DEADLINE_MS: u16 = 500;
pub const MAX_RESIDUAL: i16 = 1_024;
pub const FINGERPRINT_BYTES: usize = 32;

const MAGIC: [u8; 4] = *b"SCV1";
const REQUEST_KIND: u8 = 1;
const RESPONSE_KIND: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Truncated,
    InvalidFrameLength,
    InvalidMagic,
    UnsupportedVersion,
    InvalidMessageKind,
    InvalidEnum,
    InvalidUtf8,
    InvalidBounds,
    DuplicateCandidate,
    MissingCandidate,
    UnexpectedCandidate,
    TrailingBytes,
    InvariantViolation,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Truncated => "truncated frame",
            Self::InvalidFrameLength => "invalid frame length",
            Self::InvalidMagic => "invalid frame magic",
            Self::UnsupportedVersion => "unsupported wire version",
            Self::InvalidMessageKind => "invalid message kind",
            Self::InvalidEnum => "invalid enum value",
            Self::InvalidUtf8 => "invalid UTF-8",
            Self::InvalidBounds => "value exceeds contract bounds",
            Self::DuplicateCandidate => "duplicate candidate id",
            Self::MissingCandidate => "missing candidate score",
            Self::UnexpectedCandidate => "unexpected candidate score",
            Self::TrailingBytes => "trailing frame bytes",
            Self::InvariantViolation => "contract invariant violation",
        };
        f.write_str(message)
    }
}

impl std::error::Error for Error {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TaskKind {
    Prediction = 1,
    Rerank = 2,
}

impl TaskKind {
    fn decode(value: u8) -> Result<Self, Error> {
        match value {
            1 => Ok(Self::Prediction),
            2 => Ok(Self::Rerank),
            _ => Err(Error::InvalidEnum),
        }
    }

    const fn max_candidates(self) -> usize {
        match self {
            Self::Prediction => MAX_PREDICTION_CANDIDATES,
            Self::Rerank => MAX_RERANK_CANDIDATES,
        }
    }

    const fn max_deadline_ms(self) -> u16 {
        match self {
            Self::Prediction => MAX_PREDICTION_DEADLINE_MS,
            Self::Rerank => MAX_RERANK_DEADLINE_MS,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ScopeClass {
    Normal = 1,
    Sensitive = 2,
    Unclassified = 3,
}

impl ScopeClass {
    fn decode(value: u8) -> Result<Self, Error> {
        match value {
            1 => Ok(Self::Normal),
            2 => Ok(Self::Sensitive),
            3 => Ok(Self::Unclassified),
            _ => Err(Error::InvalidEnum),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CandidateAuthority {
    Ordinary = 0,
    ExactLearning = 1,
    UserDictionary = 2,
}

impl CandidateAuthority {
    fn decode(value: u8) -> Result<Self, Error> {
        match value {
            0 => Ok(Self::Ordinary),
            1 => Ok(Self::ExactLearning),
            2 => Ok(Self::UserDictionary),
            _ => Err(Error::InvalidEnum),
        }
    }

    pub const fn protected(self) -> bool {
        !matches!(self, Self::Ordinary)
    }
}

pub type Fingerprint = [u8; FINGERPRINT_BYTES];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateInput {
    pub candidate_id: u64,
    pub base_cost: i32,
    pub authority: CandidateAuthority,
    pub surface: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextSnapshot {
    pub task: TaskKind,
    pub request_id: u64,
    pub owner_id: u64,
    pub session_id: u64,
    pub context_generation: u64,
    pub composition_generation: u64,
    pub candidate_set_fingerprint: Fingerprint,
    pub model_fingerprint: Fingerprint,
    pub tokenizer_fingerprint: Fingerprint,
    pub scope: ScopeClass,
    pub test_only: bool,
    pub deadline_ms: u16,
    pub committed_context: String,
    pub reading: String,
    pub candidates: Vec<CandidateInput>,
}

impl ContextSnapshot {
    pub fn validate(&self) -> Result<(), Error> {
        if self.scope != ScopeClass::Normal || self.test_only {
            return Err(Error::InvariantViolation);
        }
        if self.request_id == 0 || self.owner_id == 0 || self.session_id == 0 {
            return Err(Error::InvariantViolation);
        }
        if self.deadline_ms == 0 || self.deadline_ms > self.task.max_deadline_ms() {
            return Err(Error::InvalidBounds);
        }
        if self.committed_context.len() > MAX_CONTEXT_BYTES
            || self.reading.is_empty()
            || self.reading.len() > MAX_READING_BYTES
            || self.candidates.is_empty()
            || self.candidates.len() > self.task.max_candidates()
        {
            return Err(Error::InvalidBounds);
        }
        if is_zero_fingerprint(&self.candidate_set_fingerprint)
            || is_zero_fingerprint(&self.model_fingerprint)
            || is_zero_fingerprint(&self.tokenizer_fingerprint)
        {
            return Err(Error::InvariantViolation);
        }

        for (index, candidate) in self.candidates.iter().enumerate() {
            if candidate.candidate_id == 0
                || candidate.surface.is_empty()
                || candidate.surface.len() > MAX_CANDIDATE_SURFACE_BYTES
            {
                return Err(Error::InvalidBounds);
            }
            if self
                .candidates
                .iter()
                .take(index)
                .any(|previous| previous.candidate_id == candidate.candidate_id)
            {
                return Err(Error::DuplicateCandidate);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ResponseStatus {
    Ready = 1,
    Unavailable = 2,
}

impl ResponseStatus {
    fn decode(value: u8) -> Result<Self, Error> {
        match value {
            1 => Ok(Self::Ready),
            2 => Ok(Self::Unavailable),
            _ => Err(Error::InvalidEnum),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResidualScore {
    pub candidate_id: u64,
    pub residual: i16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScoreResponse {
    pub task: TaskKind,
    pub request_id: u64,
    pub owner_id: u64,
    pub session_id: u64,
    pub context_generation: u64,
    pub composition_generation: u64,
    pub candidate_set_fingerprint: Fingerprint,
    pub model_fingerprint: Fingerprint,
    pub tokenizer_fingerprint: Fingerprint,
    pub status: ResponseStatus,
    pub scores: Vec<ResidualScore>,
}

impl ScoreResponse {
    pub fn validate_against(&self, snapshot: &ContextSnapshot) -> Result<(), Error> {
        snapshot.validate()?;
        if self.task != snapshot.task
            || self.request_id != snapshot.request_id
            || self.owner_id != snapshot.owner_id
            || self.session_id != snapshot.session_id
            || self.context_generation != snapshot.context_generation
            || self.composition_generation != snapshot.composition_generation
            || self.candidate_set_fingerprint != snapshot.candidate_set_fingerprint
            || self.model_fingerprint != snapshot.model_fingerprint
            || self.tokenizer_fingerprint != snapshot.tokenizer_fingerprint
        {
            return Err(Error::InvariantViolation);
        }
        if self.scores.len() > self.task.max_candidates() {
            return Err(Error::InvalidBounds);
        }
        match self.status {
            ResponseStatus::Unavailable if self.scores.is_empty() => return Ok(()),
            ResponseStatus::Unavailable => return Err(Error::InvariantViolation),
            ResponseStatus::Ready => {}
        }
        if self.scores.len() != snapshot.candidates.len() {
            return Err(if self.scores.len() < snapshot.candidates.len() {
                Error::MissingCandidate
            } else {
                Error::UnexpectedCandidate
            });
        }

        for score in &self.scores {
            if score.residual.unsigned_abs() > MAX_RESIDUAL.unsigned_abs() {
                return Err(Error::InvalidBounds);
            }
            let Some(candidate) = snapshot
                .candidates
                .iter()
                .find(|candidate| candidate.candidate_id == score.candidate_id)
            else {
                return Err(Error::UnexpectedCandidate);
            };
            if candidate.authority.protected() && score.residual != 0 {
                return Err(Error::InvariantViolation);
            }
            if self
                .scores
                .iter()
                .filter(|other| other.candidate_id == score.candidate_id)
                .count()
                != 1
            {
                return Err(Error::DuplicateCandidate);
            }
        }
        Ok(())
    }
}

pub fn encode_request(snapshot: &ContextSnapshot) -> Result<Vec<u8>, Error> {
    snapshot.validate()?;
    let mut payload = Vec::with_capacity(256);
    write_header(&mut payload, REQUEST_KIND, snapshot.task);
    write_correlation(&mut payload, snapshot);
    payload.push(snapshot.scope as u8);
    payload.push(u8::from(snapshot.test_only));
    put_u16(&mut payload, snapshot.deadline_ms);
    put_u16(&mut payload, checked_len(snapshot.committed_context.len())?);
    put_u16(&mut payload, checked_len(snapshot.reading.len())?);
    put_u16(&mut payload, checked_len(snapshot.candidates.len())?);
    payload.extend_from_slice(snapshot.committed_context.as_bytes());
    payload.extend_from_slice(snapshot.reading.as_bytes());
    for candidate in &snapshot.candidates {
        put_u64(&mut payload, candidate.candidate_id);
        put_i32(&mut payload, candidate.base_cost);
        payload.push(candidate.authority as u8);
        payload.push(0);
        put_u16(&mut payload, checked_len(candidate.surface.len())?);
        payload.extend_from_slice(candidate.surface.as_bytes());
    }
    finish_frame(payload)
}

pub fn decode_request(frame: &[u8]) -> Result<ContextSnapshot, Error> {
    let payload = open_frame(frame)?;
    let mut reader = Reader::new(payload);
    read_header(&mut reader, REQUEST_KIND)?;
    let task = TaskKind::decode(reader.u8()?)?;
    let correlation = reader.correlation()?;
    let scope = ScopeClass::decode(reader.u8()?)?;
    let test_only = match reader.u8()? {
        0 => false,
        1 => true,
        _ => return Err(Error::InvalidEnum),
    };
    let deadline_ms = reader.u16()?;
    let context_len = usize::from(reader.u16()?);
    let reading_len = usize::from(reader.u16()?);
    let count = usize::from(reader.u16()?);
    if context_len > MAX_CONTEXT_BYTES
        || reading_len == 0
        || reading_len > MAX_READING_BYTES
        || count == 0
        || count > task.max_candidates()
    {
        return Err(Error::InvalidBounds);
    }
    let committed_context = reader.utf8(context_len)?;
    let reading = reader.utf8(reading_len)?;
    let mut candidates = Vec::with_capacity(count);
    for _ in 0..count {
        let candidate_id = reader.u64()?;
        let base_cost = reader.i32()?;
        let authority = CandidateAuthority::decode(reader.u8()?)?;
        if reader.u8()? != 0 {
            return Err(Error::InvalidEnum);
        }
        let surface_len = usize::from(reader.u16()?);
        if surface_len == 0 || surface_len > MAX_CANDIDATE_SURFACE_BYTES {
            return Err(Error::InvalidBounds);
        }
        candidates.push(CandidateInput {
            candidate_id,
            base_cost,
            authority,
            surface: reader.utf8(surface_len)?,
        });
    }
    reader.finish()?;
    let snapshot = ContextSnapshot {
        task,
        request_id: correlation.request_id,
        owner_id: correlation.owner_id,
        session_id: correlation.session_id,
        context_generation: correlation.context_generation,
        composition_generation: correlation.composition_generation,
        candidate_set_fingerprint: correlation.candidate_set_fingerprint,
        model_fingerprint: correlation.model_fingerprint,
        tokenizer_fingerprint: correlation.tokenizer_fingerprint,
        scope,
        test_only,
        deadline_ms,
        committed_context,
        reading,
        candidates,
    };
    snapshot.validate()?;
    Ok(snapshot)
}

pub fn encode_response(response: &ScoreResponse) -> Result<Vec<u8>, Error> {
    if response.scores.len() > response.task.max_candidates() {
        return Err(Error::InvalidBounds);
    }
    if response.request_id == 0 || response.owner_id == 0 || response.session_id == 0 {
        return Err(Error::InvariantViolation);
    }
    if is_zero_fingerprint(&response.candidate_set_fingerprint)
        || is_zero_fingerprint(&response.model_fingerprint)
        || is_zero_fingerprint(&response.tokenizer_fingerprint)
    {
        return Err(Error::InvariantViolation);
    }
    if response
        .scores
        .iter()
        .any(|score| score.residual.unsigned_abs() > MAX_RESIDUAL.unsigned_abs())
    {
        return Err(Error::InvalidBounds);
    }
    if response.status == ResponseStatus::Unavailable && !response.scores.is_empty() {
        return Err(Error::InvariantViolation);
    }
    let mut payload = Vec::with_capacity(192);
    write_header(&mut payload, RESPONSE_KIND, response.task);
    write_correlation(&mut payload, response);
    payload.push(response.status as u8);
    payload.extend_from_slice(&[0, 0, 0]);
    put_u16(&mut payload, checked_len(response.scores.len())?);
    for score in &response.scores {
        put_u64(&mut payload, score.candidate_id);
        put_i16(&mut payload, score.residual);
    }
    finish_frame(payload)
}

pub fn decode_response(frame: &[u8]) -> Result<ScoreResponse, Error> {
    let payload = open_frame(frame)?;
    let mut reader = Reader::new(payload);
    read_header(&mut reader, RESPONSE_KIND)?;
    let task = TaskKind::decode(reader.u8()?)?;
    let correlation = reader.correlation()?;
    let status = ResponseStatus::decode(reader.u8()?)?;
    if reader.bytes(3)? != [0, 0, 0] {
        return Err(Error::InvalidEnum);
    }
    let count = usize::from(reader.u16()?);
    if count > task.max_candidates() {
        return Err(Error::InvalidBounds);
    }
    let mut scores = Vec::with_capacity(count);
    for _ in 0..count {
        scores.push(ResidualScore {
            candidate_id: reader.u64()?,
            residual: reader.i16()?,
        });
    }
    if status == ResponseStatus::Unavailable && !scores.is_empty() {
        return Err(Error::InvariantViolation);
    }
    if scores
        .iter()
        .any(|score| score.residual.unsigned_abs() > MAX_RESIDUAL.unsigned_abs())
    {
        return Err(Error::InvalidBounds);
    }
    reader.finish()?;
    Ok(ScoreResponse {
        task,
        request_id: correlation.request_id,
        owner_id: correlation.owner_id,
        session_id: correlation.session_id,
        context_generation: correlation.context_generation,
        composition_generation: correlation.composition_generation,
        candidate_set_fingerprint: correlation.candidate_set_fingerprint,
        model_fingerprint: correlation.model_fingerprint,
        tokenizer_fingerprint: correlation.tokenizer_fingerprint,
        status,
        scores,
    })
}

fn is_zero_fingerprint(fingerprint: &Fingerprint) -> bool {
    fingerprint.iter().all(|byte| *byte == 0)
}

fn checked_len(length: usize) -> Result<u16, Error> {
    u16::try_from(length).map_err(|_| Error::InvalidBounds)
}

fn put_u16(output: &mut Vec<u8>, value: u16) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn put_i16(output: &mut Vec<u8>, value: i16) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn put_i32(output: &mut Vec<u8>, value: i32) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn put_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn write_header(output: &mut Vec<u8>, kind: u8, task: TaskKind) {
    output.extend_from_slice(&MAGIC);
    put_u16(output, WIRE_VERSION);
    output.push(kind);
    output.push(task as u8);
}

#[derive(Debug, Clone, Copy)]
struct Correlation {
    request_id: u64,
    owner_id: u64,
    session_id: u64,
    context_generation: u64,
    composition_generation: u64,
    candidate_set_fingerprint: Fingerprint,
    model_fingerprint: Fingerprint,
    tokenizer_fingerprint: Fingerprint,
}

fn write_correlation(output: &mut Vec<u8>, source: &impl CorrelationFields) {
    put_u64(output, source.request_id());
    put_u64(output, source.owner_id());
    put_u64(output, source.session_id());
    put_u64(output, source.context_generation());
    put_u64(output, source.composition_generation());
    output.extend_from_slice(source.candidate_set_fingerprint());
    output.extend_from_slice(source.model_fingerprint());
    output.extend_from_slice(source.tokenizer_fingerprint());
}

trait CorrelationFields {
    fn request_id(&self) -> u64;
    fn owner_id(&self) -> u64;
    fn session_id(&self) -> u64;
    fn context_generation(&self) -> u64;
    fn composition_generation(&self) -> u64;
    fn candidate_set_fingerprint(&self) -> &Fingerprint;
    fn model_fingerprint(&self) -> &Fingerprint;
    fn tokenizer_fingerprint(&self) -> &Fingerprint;
}

impl CorrelationFields for ContextSnapshot {
    fn request_id(&self) -> u64 {
        self.request_id
    }
    fn owner_id(&self) -> u64 {
        self.owner_id
    }
    fn session_id(&self) -> u64 {
        self.session_id
    }
    fn context_generation(&self) -> u64 {
        self.context_generation
    }
    fn composition_generation(&self) -> u64 {
        self.composition_generation
    }
    fn candidate_set_fingerprint(&self) -> &Fingerprint {
        &self.candidate_set_fingerprint
    }
    fn model_fingerprint(&self) -> &Fingerprint {
        &self.model_fingerprint
    }
    fn tokenizer_fingerprint(&self) -> &Fingerprint {
        &self.tokenizer_fingerprint
    }
}

impl CorrelationFields for ScoreResponse {
    fn request_id(&self) -> u64 {
        self.request_id
    }
    fn owner_id(&self) -> u64 {
        self.owner_id
    }
    fn session_id(&self) -> u64 {
        self.session_id
    }
    fn context_generation(&self) -> u64 {
        self.context_generation
    }
    fn composition_generation(&self) -> u64 {
        self.composition_generation
    }
    fn candidate_set_fingerprint(&self) -> &Fingerprint {
        &self.candidate_set_fingerprint
    }
    fn model_fingerprint(&self) -> &Fingerprint {
        &self.model_fingerprint
    }
    fn tokenizer_fingerprint(&self) -> &Fingerprint {
        &self.tokenizer_fingerprint
    }
}

fn finish_frame(payload: Vec<u8>) -> Result<Vec<u8>, Error> {
    if payload.is_empty() || payload.len() > MAX_PAYLOAD {
        return Err(Error::InvalidBounds);
    }
    let length = u32::try_from(payload.len()).map_err(|_| Error::InvalidBounds)?;
    let mut frame = Vec::with_capacity(FRAME_HEADER_LEN + payload.len());
    frame.extend_from_slice(&length.to_le_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

fn open_frame(frame: &[u8]) -> Result<&[u8], Error> {
    if frame.len() < FRAME_HEADER_LEN {
        return Err(Error::Truncated);
    }
    let declared = u32::from_le_bytes(
        frame[..FRAME_HEADER_LEN]
            .try_into()
            .map_err(|_| Error::Truncated)?,
    ) as usize;
    if declared == 0 || declared > MAX_PAYLOAD || declared != frame.len() - FRAME_HEADER_LEN {
        return Err(Error::InvalidFrameLength);
    }
    Ok(&frame[FRAME_HEADER_LEN..])
}

fn read_header(reader: &mut Reader<'_>, expected_kind: u8) -> Result<(), Error> {
    if reader.bytes(4)? != MAGIC {
        return Err(Error::InvalidMagic);
    }
    if reader.u16()? != WIRE_VERSION {
        return Err(Error::UnsupportedVersion);
    }
    if reader.u8()? != expected_kind {
        return Err(Error::InvalidMessageKind);
    }
    Ok(())
}

struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn bytes(&mut self, length: usize) -> Result<&'a [u8], Error> {
        let end = self.position.checked_add(length).ok_or(Error::Truncated)?;
        let result = self.bytes.get(self.position..end).ok_or(Error::Truncated)?;
        self.position = end;
        Ok(result)
    }

    fn u8(&mut self) -> Result<u8, Error> {
        Ok(*self.bytes(1)?.first().ok_or(Error::Truncated)?)
    }

    fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_le_bytes(
            self.bytes(2)?.try_into().map_err(|_| Error::Truncated)?,
        ))
    }

    fn i16(&mut self) -> Result<i16, Error> {
        Ok(i16::from_le_bytes(
            self.bytes(2)?.try_into().map_err(|_| Error::Truncated)?,
        ))
    }

    fn i32(&mut self) -> Result<i32, Error> {
        Ok(i32::from_le_bytes(
            self.bytes(4)?.try_into().map_err(|_| Error::Truncated)?,
        ))
    }

    fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(
            self.bytes(8)?.try_into().map_err(|_| Error::Truncated)?,
        ))
    }

    fn utf8(&mut self, length: usize) -> Result<String, Error> {
        String::from_utf8(self.bytes(length)?.to_vec()).map_err(|_| Error::InvalidUtf8)
    }

    fn correlation(&mut self) -> Result<Correlation, Error> {
        Ok(Correlation {
            request_id: self.u64()?,
            owner_id: self.u64()?,
            session_id: self.u64()?,
            context_generation: self.u64()?,
            composition_generation: self.u64()?,
            candidate_set_fingerprint: self.bytes(32)?.try_into().map_err(|_| Error::Truncated)?,
            model_fingerprint: self.bytes(32)?.try_into().map_err(|_| Error::Truncated)?,
            tokenizer_fingerprint: self.bytes(32)?.try_into().map_err(|_| Error::Truncated)?,
        })
    }

    fn finish(self) -> Result<(), Error> {
        if self.position == self.bytes.len() {
            Ok(())
        } else {
            Err(Error::TrailingBytes)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> ContextSnapshot {
        ContextSnapshot {
            task: TaskKind::Prediction,
            request_id: 7,
            owner_id: 8,
            session_id: 9,
            context_generation: 10,
            composition_generation: 11,
            candidate_set_fingerprint: [1; 32],
            model_fingerprint: [2; 32],
            tokenizer_fingerprint: [3; 32],
            scope: ScopeClass::Normal,
            test_only: false,
            deadline_ms: 10,
            committed_context: "abc".to_owned(),
            reading: "pre".to_owned(),
            candidates: vec![
                CandidateInput {
                    candidate_id: 42,
                    base_cost: -7,
                    authority: CandidateAuthority::Ordinary,
                    surface: "A".to_owned(),
                },
                CandidateInput {
                    candidate_id: 43,
                    base_cost: 13,
                    authority: CandidateAuthority::UserDictionary,
                    surface: "B".to_owned(),
                },
            ],
        }
    }

    fn response(snapshot: &ContextSnapshot) -> ScoreResponse {
        ScoreResponse {
            task: snapshot.task,
            request_id: snapshot.request_id,
            owner_id: snapshot.owner_id,
            session_id: snapshot.session_id,
            context_generation: snapshot.context_generation,
            composition_generation: snapshot.composition_generation,
            candidate_set_fingerprint: snapshot.candidate_set_fingerprint,
            model_fingerprint: snapshot.model_fingerprint,
            tokenizer_fingerprint: snapshot.tokenizer_fingerprint,
            status: ResponseStatus::Ready,
            scores: vec![
                ResidualScore {
                    candidate_id: 42,
                    residual: -11,
                },
                ResidualScore {
                    candidate_id: 43,
                    residual: 0,
                },
            ],
        }
    }

    #[test]
    fn request_round_trip_and_golden_header() {
        let original = snapshot();
        let frame = encode_request(&original).expect("encode");
        assert_eq!(u32::from_le_bytes(frame[..4].try_into().unwrap()), 194);
        assert_eq!(&frame[4..12], b"SCV1\x02\x00\x01\x01");
        assert_eq!(decode_request(&frame).expect("decode"), original);
    }

    #[test]
    fn response_round_trip_and_authority_validation() {
        let original = snapshot();
        let response = response(&original);
        response
            .validate_against(&original)
            .expect("valid response");
        let frame = encode_response(&response).expect("encode");
        assert_eq!(decode_response(&frame).expect("decode"), response);
    }

    #[test]
    fn protected_candidates_cannot_receive_a_residual() {
        let original = snapshot();
        let mut invalid = response(&original);
        invalid.scores[1].residual = 1;
        assert_eq!(
            invalid.validate_against(&original),
            Err(Error::InvariantViolation)
        );
    }

    #[test]
    fn stale_response_and_malformed_scores_fail_closed() {
        let original = snapshot();
        let mut stale = response(&original);
        stale.context_generation += 1;
        assert_eq!(
            stale.validate_against(&original),
            Err(Error::InvariantViolation)
        );

        let mut out_of_bounds = response(&original);
        out_of_bounds.scores[0].residual = i16::MAX;
        assert_eq!(encode_response(&out_of_bounds), Err(Error::InvalidBounds));

        let mut unavailable_with_scores = response(&original);
        unavailable_with_scores.status = ResponseStatus::Unavailable;
        assert_eq!(
            encode_response(&unavailable_with_scores),
            Err(Error::InvariantViolation)
        );
    }

    #[test]
    fn privacy_and_bound_checks_fail_closed() {
        let mut invalid = snapshot();
        invalid.scope = ScopeClass::Unclassified;
        assert_eq!(invalid.validate(), Err(Error::InvariantViolation));
        invalid.scope = ScopeClass::Normal;
        invalid.test_only = true;
        assert_eq!(invalid.validate(), Err(Error::InvariantViolation));
        invalid.test_only = false;
        invalid.committed_context = "x".repeat(MAX_CONTEXT_BYTES + 1);
        assert_eq!(invalid.validate(), Err(Error::InvalidBounds));
        invalid.committed_context.clear();
        invalid.candidates[1].candidate_id = invalid.candidates[0].candidate_id;
        assert_eq!(invalid.validate(), Err(Error::DuplicateCandidate));
    }

    #[test]
    fn malformed_frames_are_rejected_without_panicking() {
        let frame = encode_request(&snapshot()).expect("encode");
        for end in 0..frame.len() {
            let _ = decode_request(&frame[..end]);
        }
        let mut wrong_version = frame.clone();
        wrong_version[8] = 0;
        wrong_version[9] = 0;
        assert_eq!(
            decode_request(&wrong_version),
            Err(Error::UnsupportedVersion)
        );
        let mut trailing = frame.clone();
        trailing[..4].copy_from_slice(&(195u32).to_le_bytes());
        trailing.push(0);
        assert_eq!(decode_request(&trailing), Err(Error::TrailingBytes));
    }

    #[test]
    fn generated_bytes_never_escape_the_decoder_bounds() {
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        for length in 0..=512usize {
            for _ in 0..8 {
                let mut bytes = vec![0u8; length];
                for byte in &mut bytes {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    *byte = state as u8;
                }
                let _ = decode_request(&bytes);
                let _ = decode_response(&bytes);
            }
        }
    }
}
