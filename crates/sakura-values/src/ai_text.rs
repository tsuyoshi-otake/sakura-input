#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AiTextOperation {
    Transform = 1,
    Proofread = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AiTextStatus {
    Applied = 1,
    Cancelled = 2,
    Timeout = 3,
    MissingKey = 4,
    WorkerError = 5,
    ApiError = 6,
    Rejected = 7,
}
