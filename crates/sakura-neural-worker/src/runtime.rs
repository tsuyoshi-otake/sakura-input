//! Isolated dynamic ONNX Runtime session for the exported fill-mask model.
//!
//! The exporter invokes Optimum's `fill-mask` task with opset 18/O2. Its
//! admitted interface is the conventional Int64 `[batch, sequence]`
//! `input_ids` and `attention_mask`, optional `token_type_ids`, and Float32
//! `[batch, sequence, vocabulary]` `logits` output. Session metadata is still
//! checked at startup so a different export fails closed.

use std::{
    env, fmt,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::Instant,
};

use ort::{
    session::{builder::GraphOptimizationLevel, Session},
    value::{Tensor, TensorElementType, ValueType},
};

pub const MAX_BATCH: usize = 6;
pub const MAX_SEQUENCE: usize = 128;
pub const MAX_VOCABULARY: usize = 65_536;
pub const MAX_LOGIT_VALUES: usize = MAX_BATCH * MAX_SEQUENCE * MAX_VOCABULARY;

static ORT_INITIALIZED: OnceLock<Result<(), ()>> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    CurrentExecutable,
    MissingExecutableParent,
    OrtInitialize,
    SessionBuild,
    SessionContract,
    SessionLock,
    InvalidBatch,
    InvalidSequence,
    InputLength,
    TokenTypeMismatch,
    DeadlineExceeded,
    Inference,
    OutputContract,
    OutputNonFinite,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::CurrentExecutable => "unable to resolve worker executable",
            Self::MissingExecutableParent => "worker executable has no parent directory",
            Self::OrtInitialize => "unable to initialize ONNX Runtime",
            Self::SessionBuild => "unable to construct ONNX Runtime model session",
            Self::SessionContract => "exported ONNX model does not match the worker contract",
            Self::SessionLock => "ONNX Runtime session lock is unavailable",
            Self::InvalidBatch => "model batch is outside the worker bound",
            Self::InvalidSequence => "model sequence is outside the worker bound",
            Self::InputLength => "model input buffer does not match batch and sequence",
            Self::TokenTypeMismatch => "model token type input does not match the export contract",
            Self::DeadlineExceeded => "model inference deadline expired",
            Self::Inference => "ONNX Runtime model inference failed",
            Self::OutputContract => "ONNX Runtime logits output does not match the worker contract",
            Self::OutputNonFinite => "ONNX Runtime logits output contains a non-finite value",
        })
    }
}

impl std::error::Error for Error {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BatchShape {
    batch: usize,
    sequence: usize,
    elements: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InputContract {
    token_type_ids: bool,
    vocabulary: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ElementType {
    I64,
    F32,
    Other,
}

#[derive(Debug, Clone, Copy)]
struct TensorMetadata<'a> {
    name: &'a str,
    element: ElementType,
    shape: &'a [i64],
}

#[derive(Debug, PartialEq)]
pub struct Logits {
    pub batch: usize,
    pub sequence: usize,
    pub vocabulary: usize,
    pub values: Vec<f32>,
}

/// A single ORT session is built at worker startup and serialized with a mutex.
/// `ort` deliberately makes `Session::run` mutable because ONNX Runtime
/// execution is not safely concurrent; a separate session per request is not
/// permitted by this wrapper.
pub struct ModelRuntime {
    session: Mutex<Session>,
    contract: InputContract,
}

impl fmt::Debug for ModelRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ModelRuntime")
            .field("contract", &self.contract)
            .finish_non_exhaustive()
    }
}

impl ModelRuntime {
    /// Loads the installer-provided sibling DLL and model exactly once per
    /// worker process. This is intentionally not callable from request paths.
    pub fn load(model_directory: &Path) -> Result<Self, Error> {
        let executable = env::current_exe().map_err(|_| Error::CurrentExecutable)?;
        Self::load_from(model_directory, &executable)
    }

    fn load_from(model_directory: &Path, executable: &Path) -> Result<Self, Error> {
        initialize_ort(executable)?;
        let session = Session::builder()
            .map_err(|_| Error::SessionBuild)?
            // This worker processes one bounded batch at a time. Keep ORT
            // parallelism bounded instead of competing with the engine.
            .with_intra_threads(1)
            .map_err(|_| Error::SessionBuild)?
            .with_inter_threads(1)
            .map_err(|_| Error::SessionBuild)?
            .with_parallel_execution(false)
            .map_err(|_| Error::SessionBuild)?
            .with_optimization_level(GraphOptimizationLevel::Level2)
            .map_err(|_| Error::SessionBuild)?
            .commit_from_file(model_directory.join("model.onnx"))
            .map_err(|_| Error::SessionBuild)?;
        let contract = validate_ort_contract(&session)?;
        Ok(Self {
            session: Mutex::new(session),
            contract,
        })
    }

    /// Runs one already-tokenized, bounded MLM batch and owns the copied logits.
    ///
    /// ORT's safe Rust API exposes cooperative RunOptions termination, but it
    /// requires a second thread and its own docs do not promise a reliable hard
    /// cancellation boundary. The engine's 500 ms worker-process kill boundary
    /// remains the hard stop. We check deadlines before and after `run`; a late
    /// result is discarded instead of returned to the engine.
    pub fn logits(
        &self,
        batch: usize,
        sequence: usize,
        input_ids: &[i64],
        attention_mask: &[i64],
        token_type_ids: Option<&[i64]>,
        deadline: Instant,
    ) -> Result<Logits, Error> {
        check_deadline(deadline)?;
        let shape = validate_input_buffers(
            self.contract,
            batch,
            sequence,
            input_ids,
            attention_mask,
            token_type_ids,
        )?;
        let mut session = self.session.lock().map_err(|_| Error::SessionLock)?;
        check_deadline(deadline)?;

        let ids = Tensor::from_array(([shape.batch, shape.sequence], input_ids.to_vec()))
            .map_err(|_| Error::Inference)?;
        let attention =
            Tensor::from_array(([shape.batch, shape.sequence], attention_mask.to_vec()))
                .map_err(|_| Error::Inference)?;
        let outputs = if self.contract.token_type_ids {
            let token_types = Tensor::from_array((
                [shape.batch, shape.sequence],
                token_type_ids.expect("validated token type input").to_vec(),
            ))
            .map_err(|_| Error::Inference)?;
            session.run(ort::inputs![
                "input_ids" => ids,
                "attention_mask" => attention,
                "token_type_ids" => token_types,
            ])
        } else {
            session.run(ort::inputs![
                "input_ids" => ids,
                "attention_mask" => attention,
            ])
        }
        .map_err(|_| Error::Inference)?;
        check_deadline(deadline)?;

        let output = outputs.get("logits").ok_or(Error::OutputContract)?;
        let (output_shape, values) = output
            .try_extract_tensor::<f32>()
            .map_err(|_| Error::OutputContract)?;
        validate_logits(shape, self.contract.vocabulary, output_shape, values)
    }

    pub fn requires_token_type_ids(&self) -> bool {
        self.contract.token_type_ids
    }
}

pub fn sibling_dll(executable: &Path) -> Result<PathBuf, Error> {
    let parent = executable.parent().ok_or(Error::MissingExecutableParent)?;
    Ok(parent.join("onnxruntime.dll"))
}

fn initialize_ort(executable: &Path) -> Result<(), Error> {
    let result = ORT_INITIALIZED.get_or_init(|| {
        let dll = match sibling_dll(executable) {
            Ok(path) => path,
            Err(_) => return Err(()),
        };
        let builder = ort::init_from(dll).map_err(|_| ())?;
        if builder.commit() {
            Ok(())
        } else {
            Err(())
        }
    });
    result.as_ref().map_err(|_| Error::OrtInitialize).copied()
}

fn validate_ort_contract(session: &Session) -> Result<InputContract, Error> {
    let inputs = session
        .inputs()
        .iter()
        .map(outlet_metadata)
        .collect::<Result<Vec<_>, _>>()?;
    let outputs = session
        .outputs()
        .iter()
        .map(outlet_metadata)
        .collect::<Result<Vec<_>, _>>()?;
    validate_model_contract(&inputs, &outputs)
}

fn outlet_metadata(outlet: &ort::value::Outlet) -> Result<TensorMetadata<'_>, Error> {
    match outlet.dtype() {
        ValueType::Tensor { ty, shape, .. } => Ok(TensorMetadata {
            name: outlet.name(),
            element: match ty {
                TensorElementType::Int64 => ElementType::I64,
                TensorElementType::Float32 => ElementType::F32,
                _ => ElementType::Other,
            },
            shape,
        }),
        _ => Err(Error::SessionContract),
    }
}

fn validate_model_contract(
    inputs: &[TensorMetadata<'_>],
    outputs: &[TensorMetadata<'_>],
) -> Result<InputContract, Error> {
    let input_ids = find_exact(inputs, "input_ids").ok_or(Error::SessionContract)?;
    let attention = find_exact(inputs, "attention_mask").ok_or(Error::SessionContract)?;
    let token_types = find_exact(inputs, "token_type_ids");
    if inputs.len() != if token_types.is_some() { 3 } else { 2 }
        || !valid_input_tensor(input_ids)
        || !valid_input_tensor(attention)
        || token_types.is_some_and(|tensor| !valid_input_tensor(tensor))
    {
        return Err(Error::SessionContract);
    }

    let logits = find_exact(outputs, "logits").ok_or(Error::SessionContract)?;
    if outputs.len() != 1 || logits.element != ElementType::F32 || logits.shape.len() != 3 {
        return Err(Error::SessionContract);
    }
    if !valid_dynamic_dimension(logits.shape[0], MAX_BATCH)
        || !valid_dynamic_dimension(logits.shape[1], MAX_SEQUENCE)
    {
        return Err(Error::SessionContract);
    }
    let vocabulary = usize::try_from(logits.shape[2]).map_err(|_| Error::SessionContract)?;
    if !(1..=MAX_VOCABULARY).contains(&vocabulary) {
        return Err(Error::SessionContract);
    }
    Ok(InputContract {
        token_type_ids: token_types.is_some(),
        vocabulary,
    })
}

fn find_exact<'a>(tensors: &'a [TensorMetadata<'_>], name: &str) -> Option<&'a TensorMetadata<'a>> {
    let mut matches = tensors.iter().filter(|tensor| tensor.name == name);
    let tensor = matches.next()?;
    matches.next().is_none().then_some(tensor)
}

fn valid_input_tensor(tensor: &TensorMetadata<'_>) -> bool {
    tensor.element == ElementType::I64
        && tensor.shape.len() == 2
        && valid_dynamic_dimension(tensor.shape[0], MAX_BATCH)
        && valid_dynamic_dimension(tensor.shape[1], MAX_SEQUENCE)
}

fn valid_dynamic_dimension(value: i64, maximum: usize) -> bool {
    value == -1 || usize::try_from(value).is_ok_and(|value| (1..=maximum).contains(&value))
}

fn validate_input_buffers(
    contract: InputContract,
    batch: usize,
    sequence: usize,
    input_ids: &[i64],
    attention_mask: &[i64],
    token_type_ids: Option<&[i64]>,
) -> Result<BatchShape, Error> {
    let shape = validate_batch_shape(batch, sequence)?;
    if input_ids.len() != shape.elements || attention_mask.len() != shape.elements {
        return Err(Error::InputLength);
    }
    match (contract.token_type_ids, token_type_ids) {
        (true, Some(values)) if values.len() == shape.elements => Ok(shape),
        (false, None) => Ok(shape),
        _ => Err(Error::TokenTypeMismatch),
    }
}

fn validate_batch_shape(batch: usize, sequence: usize) -> Result<BatchShape, Error> {
    if !(1..=MAX_BATCH).contains(&batch) {
        return Err(Error::InvalidBatch);
    }
    if !(2..=MAX_SEQUENCE).contains(&sequence) {
        return Err(Error::InvalidSequence);
    }
    let elements = batch.checked_mul(sequence).ok_or(Error::InputLength)?;
    Ok(BatchShape {
        batch,
        sequence,
        elements,
    })
}

fn validate_logits(
    batch: BatchShape,
    vocabulary: usize,
    output_shape: &[i64],
    values: &[f32],
) -> Result<Logits, Error> {
    if output_shape
        != [
            i64::try_from(batch.batch).map_err(|_| Error::OutputContract)?,
            i64::try_from(batch.sequence).map_err(|_| Error::OutputContract)?,
            i64::try_from(vocabulary).map_err(|_| Error::OutputContract)?,
        ]
    {
        return Err(Error::OutputContract);
    }
    let count = batch
        .elements
        .checked_mul(vocabulary)
        .filter(|count| *count <= MAX_LOGIT_VALUES)
        .ok_or(Error::OutputContract)?;
    if values.len() != count {
        return Err(Error::OutputContract);
    }
    if values.iter().any(|value| !value.is_finite()) {
        return Err(Error::OutputNonFinite);
    }
    Ok(Logits {
        batch: batch.batch,
        sequence: batch.sequence,
        vocabulary,
        values: values.to_vec(),
    })
}

fn check_deadline(deadline: Instant) -> Result<(), Error> {
    if Instant::now() >= deadline {
        Err(Error::DeadlineExceeded)
    } else {
        Ok(())
    }
}

/// Validates the B1 no-DLL trust boundaries used by `--self-test`.
pub fn self_test() -> Result<(), Error> {
    let ids = TensorMetadata {
        name: "input_ids",
        element: ElementType::I64,
        shape: &[-1, -1],
    };
    let mask = TensorMetadata {
        name: "attention_mask",
        element: ElementType::I64,
        shape: &[-1, -1],
    };
    let logits = TensorMetadata {
        name: "logits",
        element: ElementType::F32,
        shape: &[-1, -1, 4],
    };
    let contract = validate_model_contract(&[ids, mask], &[logits])?;
    let batch = validate_input_buffers(contract, 1, 2, &[1, 2], &[1, 1], None)?;
    validate_logits(batch, contract.vocabulary, &[1, 2, 4], &[0.0; 8])?;
    check_deadline(Instant::now() + std::time::Duration::from_secs(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const IDS: TensorMetadata<'static> = TensorMetadata {
        name: "input_ids",
        element: ElementType::I64,
        shape: &[-1, -1],
    };
    const MASK: TensorMetadata<'static> = TensorMetadata {
        name: "attention_mask",
        element: ElementType::I64,
        shape: &[-1, -1],
    };
    const TYPES: TensorMetadata<'static> = TensorMetadata {
        name: "token_type_ids",
        element: ElementType::I64,
        shape: &[-1, -1],
    };
    const LOGITS: TensorMetadata<'static> = TensorMetadata {
        name: "logits",
        element: ElementType::F32,
        shape: &[-1, -1, 32_000],
    };

    #[test]
    fn sibling_path_is_cwd_independent() {
        assert_eq!(
            sibling_dll(Path::new(r"C:\payload\sakura_neural_worker.exe")).unwrap(),
            PathBuf::from(r"C:\payload\onnxruntime.dll")
        );
    }

    #[test]
    fn accepts_fill_mask_contract_with_optional_token_types() {
        assert_eq!(
            validate_model_contract(&[IDS, MASK], &[LOGITS]).unwrap(),
            InputContract {
                token_type_ids: false,
                vocabulary: 32_000
            }
        );
        assert!(
            validate_model_contract(&[IDS, MASK, TYPES], &[LOGITS])
                .unwrap()
                .token_type_ids
        );
    }

    #[test]
    fn rejects_wrong_names_types_shapes_and_vocabulary() {
        let wrong_name = TensorMetadata { name: "ids", ..IDS };
        let wrong_type = TensorMetadata {
            element: ElementType::F32,
            ..MASK
        };
        let wrong_rank = TensorMetadata {
            shape: &[-1],
            ..LOGITS
        };
        let too_wide = TensorMetadata {
            shape: &[-1, -1, 65_537],
            ..LOGITS
        };
        assert_eq!(
            validate_model_contract(&[wrong_name, MASK], &[LOGITS]),
            Err(Error::SessionContract)
        );
        assert_eq!(
            validate_model_contract(&[IDS, wrong_type], &[LOGITS]),
            Err(Error::SessionContract)
        );
        assert_eq!(
            validate_model_contract(&[IDS, MASK], &[wrong_rank]),
            Err(Error::SessionContract)
        );
        assert_eq!(
            validate_model_contract(&[IDS, MASK], &[too_wide]),
            Err(Error::SessionContract)
        );
    }

    #[test]
    fn input_and_logit_bounds_are_checked_without_ort() {
        let contract = InputContract {
            token_type_ids: false,
            vocabulary: 4,
        };
        assert_eq!(
            validate_input_buffers(contract, 1, 2, &[1, 2], &[1, 1], None).unwrap(),
            BatchShape {
                batch: 1,
                sequence: 2,
                elements: 2
            }
        );
        assert_eq!(
            validate_batch_shape(MAX_BATCH + 1, 2),
            Err(Error::InvalidBatch)
        );
        assert_eq!(
            validate_batch_shape(1, MAX_SEQUENCE + 1),
            Err(Error::InvalidSequence)
        );
        assert_eq!(
            validate_input_buffers(contract, 1, 2, &[1], &[1, 1], None),
            Err(Error::InputLength)
        );
        assert_eq!(
            validate_input_buffers(contract, 1, 2, &[1, 2], &[1, 1], Some(&[0, 0])),
            Err(Error::TokenTypeMismatch)
        );
        let batch = validate_batch_shape(1, 2).unwrap();
        assert_eq!(
            validate_logits(batch, 4, &[1, 2, 4], &[0.0; 8])
                .unwrap()
                .values
                .len(),
            8
        );
        assert_eq!(
            validate_logits(batch, 4, &[1, 2, 4], &[f32::NAN; 8]),
            Err(Error::OutputNonFinite)
        );
    }

    #[test]
    fn deadline_is_checked_before_and_after_run_boundaries() {
        assert_eq!(
            check_deadline(Instant::now() - Duration::from_millis(1)),
            Err(Error::DeadlineExceeded)
        );
        assert!(check_deadline(Instant::now() + Duration::from_secs(1)).is_ok());
    }

    #[test]
    fn model_free_self_test_passes() {
        self_test().unwrap();
    }
}
