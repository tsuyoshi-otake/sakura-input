//! Isolated dynamic ONNX Runtime session for Sakura-Rerank-Tiny-v1.

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

pub const CONTEXT_LENGTH: usize = 64;
pub const READING_LENGTH: usize = 32;
pub const TOP_K: usize = 6;
pub const SURFACE_LENGTH: usize = 32;
pub const FEATURE_DIM: usize = 6;

static ORT_INITIALIZED: OnceLock<Result<(), ()>> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    CurrentExecutable,
    MissingExecutableParent,
    OrtInitialize,
    SessionBuild,
    SessionContract,
    SessionLock,
    InputContract,
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
            Self::SessionContract => "exported ONNX model does not match the Sakura contract",
            Self::SessionLock => "ONNX Runtime session lock is unavailable",
            Self::InputContract => "Sakura model input does not match the bounded contract",
            Self::DeadlineExceeded => "model inference deadline expired",
            Self::Inference => "ONNX Runtime model inference failed",
            Self::OutputContract => "ONNX Runtime score output does not match the Sakura contract",
            Self::OutputNonFinite => "ONNX Runtime score output contains a non-finite value",
        })
    }
}

impl std::error::Error for Error {}

#[derive(Debug, Clone, PartialEq)]
pub struct Inputs {
    pub context_ids: Vec<i64>,
    pub context_lengths: Vec<i64>,
    pub reading_ids: Vec<i64>,
    pub reading_lengths: Vec<i64>,
    pub candidate_ids: Vec<i64>,
    pub candidate_lengths: Vec<i64>,
    pub features: Vec<f32>,
    pub candidate_mask: Vec<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ElementType {
    I64,
    F32,
    Bool,
    Other,
}

#[derive(Debug, Clone, Copy)]
struct TensorMetadata<'a> {
    name: &'a str,
    element: ElementType,
    shape: &'a [i64],
}

/// One process-lifetime ORT session. The worker remains the only owner of ORT.
pub struct ModelRuntime {
    session: Mutex<Session>,
}

impl fmt::Debug for ModelRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ModelRuntime")
            .finish_non_exhaustive()
    }
}

impl ModelRuntime {
    pub fn load(model_directory: &Path) -> Result<Self, Error> {
        let executable = env::current_exe().map_err(|_| Error::CurrentExecutable)?;
        Self::load_from(model_directory, &executable)
    }

    fn load_from(model_directory: &Path, executable: &Path) -> Result<Self, Error> {
        initialize_ort(executable)?;
        let session = Session::builder()
            .map_err(|_| Error::SessionBuild)?
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
        validate_ort_contract(&session)?;
        Ok(Self {
            session: Mutex::new(session),
        })
    }

    pub fn scores(&self, inputs: &Inputs, deadline: Instant) -> Result<Vec<f32>, Error> {
        check_deadline(deadline)?;
        validate_inputs(inputs)?;
        let mut session = self.session.lock().map_err(|_| Error::SessionLock)?;
        check_deadline(deadline)?;

        let context_ids = Tensor::from_array(([1, CONTEXT_LENGTH], inputs.context_ids.clone()))
            .map_err(|_| Error::Inference)?;
        let context_lengths = Tensor::from_array(([1], inputs.context_lengths.clone()))
            .map_err(|_| Error::Inference)?;
        let reading_ids = Tensor::from_array(([1, READING_LENGTH], inputs.reading_ids.clone()))
            .map_err(|_| Error::Inference)?;
        let reading_lengths = Tensor::from_array(([1], inputs.reading_lengths.clone()))
            .map_err(|_| Error::Inference)?;
        let candidate_ids =
            Tensor::from_array(([1, TOP_K, SURFACE_LENGTH], inputs.candidate_ids.clone()))
                .map_err(|_| Error::Inference)?;
        let candidate_lengths = Tensor::from_array(([1, TOP_K], inputs.candidate_lengths.clone()))
            .map_err(|_| Error::Inference)?;
        let features = Tensor::from_array(([1, TOP_K, FEATURE_DIM], inputs.features.clone()))
            .map_err(|_| Error::Inference)?;
        let candidate_mask = Tensor::from_array(([1, TOP_K], inputs.candidate_mask.clone()))
            .map_err(|_| Error::Inference)?;

        let outputs = session
            .run(ort::inputs![
                "context_ids" => context_ids,
                "context_lengths" => context_lengths,
                "reading_ids" => reading_ids,
                "reading_lengths" => reading_lengths,
                "candidate_ids" => candidate_ids,
                "candidate_lengths" => candidate_lengths,
                "features" => features,
                "candidate_mask" => candidate_mask,
            ])
            .map_err(|_| Error::Inference)?;
        check_deadline(deadline)?;
        let output = outputs.get("scores").ok_or(Error::OutputContract)?;
        let (shape, values) = output
            .try_extract_tensor::<f32>()
            .map_err(|_| Error::OutputContract)?;
        if shape.as_ref() != [1, TOP_K as i64] || values.len() != TOP_K {
            return Err(Error::OutputContract);
        }
        if values.iter().any(|value| !value.is_finite()) {
            return Err(Error::OutputNonFinite);
        }
        Ok(values.to_vec())
    }
}

pub fn sibling_dll(executable: &Path) -> Result<PathBuf, Error> {
    let parent = executable.parent().ok_or(Error::MissingExecutableParent)?;
    Ok(parent.join("onnxruntime.dll"))
}

fn initialize_ort(executable: &Path) -> Result<(), Error> {
    let result = ORT_INITIALIZED.get_or_init(|| {
        let dll = sibling_dll(executable).map_err(|_| ())?;
        let builder = ort::init_from(dll).map_err(|_| ())?;
        builder.commit().then_some(()).ok_or(())
    });
    result.as_ref().map_err(|_| Error::OrtInitialize).copied()
}

fn validate_ort_contract(session: &Session) -> Result<(), Error> {
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
                TensorElementType::Bool => ElementType::Bool,
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
) -> Result<(), Error> {
    let expected = [
        ("context_ids", ElementType::I64, &[-1, 64][..]),
        ("context_lengths", ElementType::I64, &[-1][..]),
        ("reading_ids", ElementType::I64, &[-1, 32][..]),
        ("reading_lengths", ElementType::I64, &[-1][..]),
        ("candidate_ids", ElementType::I64, &[-1, 6, 32][..]),
        ("candidate_lengths", ElementType::I64, &[-1, 6][..]),
        ("features", ElementType::F32, &[-1, 6, 6][..]),
        ("candidate_mask", ElementType::Bool, &[-1, 6][..]),
    ];
    if inputs.len() != expected.len() {
        return Err(Error::SessionContract);
    }
    for (name, element, shape) in expected {
        let tensor = find_exact(inputs, name).ok_or(Error::SessionContract)?;
        if tensor.element != element || !shape_matches(tensor.shape, shape) {
            return Err(Error::SessionContract);
        }
    }
    let scores = find_exact(outputs, "scores").ok_or(Error::SessionContract)?;
    if outputs.len() != 1
        || scores.element != ElementType::F32
        || !shape_matches(scores.shape, &[-1, 6])
    {
        return Err(Error::SessionContract);
    }
    Ok(())
}

fn shape_matches(actual: &[i64], expected: &[i64]) -> bool {
    actual.len() == expected.len()
        && actual.iter().zip(expected).all(|(actual, expected)| {
            if *expected == -1 {
                *actual == -1 || *actual == 1
            } else {
                actual == expected
            }
        })
}

fn find_exact<'a>(tensors: &'a [TensorMetadata<'_>], name: &str) -> Option<&'a TensorMetadata<'a>> {
    let mut matches = tensors.iter().filter(|tensor| tensor.name == name);
    let tensor = matches.next()?;
    matches.next().is_none().then_some(tensor)
}

fn validate_inputs(inputs: &Inputs) -> Result<(), Error> {
    let active_candidates = inputs
        .candidate_mask
        .iter()
        .take_while(|active| **active)
        .count();
    if inputs.context_ids.len() != CONTEXT_LENGTH
        || inputs.context_lengths.len() != 1
        || inputs.reading_ids.len() != READING_LENGTH
        || inputs.reading_lengths.len() != 1
        || inputs.candidate_ids.len() != TOP_K * SURFACE_LENGTH
        || inputs.candidate_lengths.len() != TOP_K
        || inputs.features.len() != TOP_K * FEATURE_DIM
        || inputs.candidate_mask.len() != TOP_K
        || inputs.context_lengths[0] != 0
        || inputs.reading_lengths[0] != 0
        || inputs.context_ids.iter().any(|value| *value != 0)
        || inputs.reading_ids.iter().any(|value| *value != 0)
        || !(2..=TOP_K).contains(&active_candidates)
        || inputs.candidate_mask[active_candidates..]
            .iter()
            .any(|active| *active)
        || inputs.candidate_lengths[..active_candidates]
            .iter()
            .any(|length| !(1..=SURFACE_LENGTH as i64).contains(length))
        || inputs.candidate_lengths[active_candidates..]
            .iter()
            .any(|length| *length != 0)
        || inputs.features.iter().any(|value| !value.is_finite())
    {
        return Err(Error::InputContract);
    }
    Ok(())
}

fn check_deadline(deadline: Instant) -> Result<(), Error> {
    if Instant::now() >= deadline {
        Err(Error::DeadlineExceeded)
    } else {
        Ok(())
    }
}

pub fn self_test() -> Result<(), Error> {
    let inputs = Inputs {
        context_ids: vec![0; CONTEXT_LENGTH],
        context_lengths: vec![0],
        reading_ids: vec![0; READING_LENGTH],
        reading_lengths: vec![0],
        candidate_ids: vec![0; TOP_K * SURFACE_LENGTH],
        candidate_lengths: vec![1, 1, 0, 0, 0, 0],
        features: vec![0.0; TOP_K * FEATURE_DIM],
        candidate_mask: vec![true, true, false, false, false, false],
    };
    validate_inputs(&inputs)?;
    check_deadline(Instant::now() + std::time::Duration::from_secs(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONTEXT: TensorMetadata<'static> = TensorMetadata {
        name: "context_ids",
        element: ElementType::I64,
        shape: &[-1, 64],
    };

    fn contract_inputs() -> Vec<TensorMetadata<'static>> {
        vec![
            CONTEXT,
            TensorMetadata {
                name: "context_lengths",
                element: ElementType::I64,
                shape: &[-1],
            },
            TensorMetadata {
                name: "reading_ids",
                element: ElementType::I64,
                shape: &[-1, 32],
            },
            TensorMetadata {
                name: "reading_lengths",
                element: ElementType::I64,
                shape: &[-1],
            },
            TensorMetadata {
                name: "candidate_ids",
                element: ElementType::I64,
                shape: &[-1, 6, 32],
            },
            TensorMetadata {
                name: "candidate_lengths",
                element: ElementType::I64,
                shape: &[-1, 6],
            },
            TensorMetadata {
                name: "features",
                element: ElementType::F32,
                shape: &[-1, 6, 6],
            },
            TensorMetadata {
                name: "candidate_mask",
                element: ElementType::Bool,
                shape: &[-1, 6],
            },
        ]
    }

    #[test]
    fn accepts_exact_sakura_contract() {
        let scores = TensorMetadata {
            name: "scores",
            element: ElementType::F32,
            shape: &[-1, 6],
        };
        assert!(validate_model_contract(&contract_inputs(), &[scores]).is_ok());
    }

    #[test]
    fn rejects_wrong_name_type_or_shape() {
        let scores = TensorMetadata {
            name: "scores",
            element: ElementType::F32,
            shape: &[-1, 6],
        };
        let mut inputs = contract_inputs();
        inputs[0] = TensorMetadata {
            name: "input_ids",
            ..CONTEXT
        };
        assert_eq!(
            validate_model_contract(&inputs, &[scores]),
            Err(Error::SessionContract)
        );
        let mut inputs = contract_inputs();
        inputs[7] = TensorMetadata {
            element: ElementType::I64,
            ..inputs[7]
        };
        assert_eq!(
            validate_model_contract(&inputs, &[scores]),
            Err(Error::SessionContract)
        );
        let wrong_scores = TensorMetadata {
            shape: &[-1, 5],
            ..scores
        };
        assert_eq!(
            validate_model_contract(&contract_inputs(), &[wrong_scores]),
            Err(Error::SessionContract)
        );
    }

    #[test]
    fn sibling_path_and_model_free_self_test_pass() {
        assert_eq!(
            sibling_dll(Path::new(r"C:\payload\sakura_neural_worker.exe")).unwrap(),
            PathBuf::from(r"C:\payload\onnxruntime.dll")
        );
        self_test().unwrap();
    }

    #[test]
    fn input_validation_rejects_sparse_masks_and_invalid_lengths() {
        let mut inputs = Inputs {
            context_ids: vec![0; CONTEXT_LENGTH],
            context_lengths: vec![0],
            reading_ids: vec![0; READING_LENGTH],
            reading_lengths: vec![0],
            candidate_ids: vec![0; TOP_K * SURFACE_LENGTH],
            candidate_lengths: vec![1, 1, 0, 0, 0, 0],
            features: vec![0.0; TOP_K * FEATURE_DIM],
            candidate_mask: vec![true, true, false, false, false, false],
        };
        assert_eq!(validate_inputs(&inputs), Ok(()));
        inputs.candidate_mask = vec![true, false, true, false, false, false];
        assert_eq!(validate_inputs(&inputs), Err(Error::InputContract));
        inputs.candidate_mask = vec![true, true, false, false, false, false];
        inputs.candidate_lengths[1] = 0;
        assert_eq!(validate_inputs(&inputs), Err(Error::InputContract));
    }
}
