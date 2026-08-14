//! Bounded, dependency-free protocol between the TSF frontend and the isolated
//! OpenAI text worker. User text never appears on a command line.

use std::io::{self, Read, Write};

const REQUEST_MAGIC: &[u8; 4] = b"SAIR";
const RESPONSE_MAGIC: &[u8; 4] = b"SAIS";
const VERSION: u16 = 1;
pub const MODEL: &str = "gpt-5.6-luna";
pub const MAX_TEXT_BYTES: usize = 4 * 1024;
pub const MAX_METADATA_BYTES: usize = 128;
pub const MAX_ENDPOINT_BYTES: usize = 2 * 1024;
pub const MAX_API_KEY_BYTES: usize = 2 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Operation {
    Transform = 1,
    Proofread = 2,
}

impl Operation {
    fn decode(value: u8) -> io::Result<Self> {
        match value {
            1 => Ok(Self::Transform),
            2 => Ok(Self::Proofread),
            _ => Err(invalid("unknown AI text operation")),
        }
    }
}

macro_rules! wire_enum {
    ($name:ident { $($variant:ident = $value:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #[repr(u8)]
        pub enum $name { $($variant = $value),+ }
        impl $name {
            fn decode(value: u8) -> io::Result<Self> {
                match value {
                    $($value => Ok(Self::$variant),)+
                    _ => Err(invalid(concat!("unknown ", stringify!($name)))),
                }
            }
        }
    };
}

wire_enum!(Provider {
    OpenAi = 1,
    AzureOpenAi = 2,
    AwsBedrock = 3,
    Cloudflare = 4,
    Custom = 5,
    ChatGptCodex = 6,
});
wire_enum!(Auth { Bearer = 1, ApiKey = 2, None = 3 });
wire_enum!(Style {
    Spoken = 1,
    Polite = 2,
    Business = 3,
    Government = 4,
    Technical = 5,
    Academic = 6,
    Contract = 7,
    Novel = 8,
    Social = 9,
});
wire_enum!(Effort {
    ProviderDefault = 1,
    None = 2,
    Low = 3,
    Medium = 4,
    High = 5,
    XHigh = 6,
    Max = 7,
});
wire_enum!(ServiceTier { ProviderDefault = 1, Priority = 2 });

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Status {
    Applied = 1,
    MissingKey = 2,
    TooLarge = 3,
    HttpError = 4,
    ApiError = 5,
    MalformedResponse = 6,
    Timeout = 7,
    WorkerError = 8,
}

impl Status {
    fn decode(value: u8) -> io::Result<Self> {
        match value {
            1 => Ok(Self::Applied),
            2 => Ok(Self::MissingKey),
            3 => Ok(Self::TooLarge),
            4 => Ok(Self::HttpError),
            5 => Ok(Self::ApiError),
            6 => Ok(Self::MalformedResponse),
            7 => Ok(Self::Timeout),
            8 => Ok(Self::WorkerError),
            _ => Err(invalid("unknown AI text status")),
        }
    }

    pub const fn succeeded(self) -> bool {
        matches!(self, Self::Applied)
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Request {
    pub id: u64,
    pub operation: Operation,
    pub provider: Provider,
    pub endpoint: String,
    pub auth: Auth,
    pub api_key: String,
    pub style: Style,
    pub effort: Effort,
    pub service_tier: ServiceTier,
    pub text: String,
}

impl core::fmt::Debug for Request {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("Request")
            .field("id", &self.id)
            .field("operation", &self.operation)
            .field("provider", &self.provider)
            .field("auth", &self.auth)
            .field("style", &self.style)
            .field("effort", &self.effort)
            .field("service_tier", &self.service_tier)
            .field("endpoint_present", &!self.endpoint.is_empty())
            .field("api_key_present", &!self.api_key.is_empty())
            .field("text_bytes", &self.text.len())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub id: u64,
    pub status: Status,
    pub result: String,
    pub model: String,
    pub error_code: String,
    pub latency_ms: u64,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cached_tokens: u32,
    pub attempts: u32,
}

pub fn write_request(mut writer: impl Write, request: &Request) -> io::Result<()> {
    validate_text(&request.text)?;
    writer.write_all(REQUEST_MAGIC)?;
    writer.write_all(&VERSION.to_le_bytes())?;
    writer.write_all(&request.id.to_le_bytes())?;
    writer.write_all(&[request.operation as u8])?;
    writer.write_all(&[request.provider as u8])?;
    writer.write_all(&[request.auth as u8])?;
    writer.write_all(&[request.style as u8])?;
    writer.write_all(&[request.effort as u8])?;
    writer.write_all(&[request.service_tier as u8])?;
    write_string(&mut writer, &request.endpoint, MAX_ENDPOINT_BYTES)?;
    write_string(&mut writer, &request.api_key, MAX_API_KEY_BYTES)?;
    write_string(&mut writer, &request.text, MAX_TEXT_BYTES)
}

pub fn read_request(mut reader: impl Read) -> io::Result<Request> {
    read_header(&mut reader, REQUEST_MAGIC)?;
    let id = read_u64(&mut reader)?;
    let operation = Operation::decode(read_u8(&mut reader)?)?;
    let provider = Provider::decode(read_u8(&mut reader)?)?;
    let auth = Auth::decode(read_u8(&mut reader)?)?;
    let style = Style::decode(read_u8(&mut reader)?)?;
    let effort = Effort::decode(read_u8(&mut reader)?)?;
    let service_tier = ServiceTier::decode(read_u8(&mut reader)?)?;
    let endpoint = read_string(&mut reader, MAX_ENDPOINT_BYTES)?;
    let api_key = read_string(&mut reader, MAX_API_KEY_BYTES)?;
    let text = read_string(&mut reader, MAX_TEXT_BYTES)?;
    ensure_eof(&mut reader)?;
    Ok(Request {
        id,
        operation,
        provider,
        endpoint,
        auth,
        api_key,
        style,
        effort,
        service_tier,
        text,
    })
}

pub fn write_response(mut writer: impl Write, response: &Response) -> io::Result<()> {
    if response.status.succeeded() {
        validate_text(&response.result)?;
        if response.result.is_empty() {
            return Err(invalid("successful AI result is empty"));
        }
    } else if !response.result.is_empty() {
        return Err(invalid("failed AI result contains text"));
    }
    writer.write_all(RESPONSE_MAGIC)?;
    writer.write_all(&VERSION.to_le_bytes())?;
    writer.write_all(&response.id.to_le_bytes())?;
    writer.write_all(&[response.status as u8])?;
    writer.write_all(&response.latency_ms.to_le_bytes())?;
    writer.write_all(&response.input_tokens.to_le_bytes())?;
    writer.write_all(&response.output_tokens.to_le_bytes())?;
    writer.write_all(&response.cached_tokens.to_le_bytes())?;
    writer.write_all(&response.attempts.to_le_bytes())?;
    write_string(&mut writer, &response.model, MAX_METADATA_BYTES)?;
    write_string(&mut writer, &response.error_code, MAX_METADATA_BYTES)?;
    write_string(&mut writer, &response.result, MAX_TEXT_BYTES)
}

pub fn read_response(mut reader: impl Read) -> io::Result<Response> {
    read_header(&mut reader, RESPONSE_MAGIC)?;
    let response = Response {
        id: read_u64(&mut reader)?,
        status: Status::decode(read_u8(&mut reader)?)?,
        latency_ms: read_u64(&mut reader)?,
        input_tokens: read_u32(&mut reader)?,
        output_tokens: read_u32(&mut reader)?,
        cached_tokens: read_u32(&mut reader)?,
        attempts: read_u32(&mut reader)?,
        model: read_string(&mut reader, MAX_METADATA_BYTES)?,
        error_code: read_string(&mut reader, MAX_METADATA_BYTES)?,
        result: read_string(&mut reader, MAX_TEXT_BYTES)?,
    };
    ensure_eof(&mut reader)?;
    if response.status.succeeded() && response.result.is_empty() {
        return Err(invalid("successful AI result is empty"));
    }
    if !response.status.succeeded() && !response.result.is_empty() {
        return Err(invalid("failed AI result contains text"));
    }
    Ok(response)
}

fn read_header(reader: &mut impl Read, magic: &[u8; 4]) -> io::Result<()> {
    let mut actual = [0; 4];
    reader.read_exact(&mut actual)?;
    if &actual != magic {
        return Err(invalid("bad AI protocol magic"));
    }
    let mut version = [0; 2];
    reader.read_exact(&mut version)?;
    if u16::from_le_bytes(version) != VERSION {
        return Err(invalid("unsupported AI protocol version"));
    }
    Ok(())
}

fn write_string(writer: &mut impl Write, text: &str, limit: usize) -> io::Result<()> {
    if text.len() > limit {
        return Err(invalid("AI protocol string too large"));
    }
    let len = u32::try_from(text.len()).map_err(|_| invalid("AI protocol string too large"))?;
    writer.write_all(&len.to_le_bytes())?;
    writer.write_all(text.as_bytes())
}

fn read_string(reader: &mut impl Read, limit: usize) -> io::Result<String> {
    let len = usize::try_from(read_u32(reader)?).map_err(|_| invalid("invalid string length"))?;
    if len > limit {
        return Err(invalid("AI protocol string too large"));
    }
    let mut bytes = vec![0; len];
    reader.read_exact(&mut bytes)?;
    String::from_utf8(bytes).map_err(|_| invalid("AI protocol string is not UTF-8"))
}

fn validate_text(text: &str) -> io::Result<()> {
    if text.is_empty() || text.len() > MAX_TEXT_BYTES {
        Err(invalid("AI text is empty or too large"))
    } else {
        Ok(())
    }
}

fn read_u8(reader: &mut impl Read) -> io::Result<u8> {
    let mut bytes = [0; 1];
    reader.read_exact(&mut bytes)?;
    Ok(bytes[0])
}

fn read_u32(reader: &mut impl Read) -> io::Result<u32> {
    let mut bytes = [0; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64(reader: &mut impl Read) -> io::Result<u64> {
    let mut bytes = [0; 8];
    reader.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

fn ensure_eof(reader: &mut impl Read) -> io::Result<()> {
    let mut trailing = [0; 1];
    if reader.read(&mut trailing)? == 0 {
        Ok(())
    } else {
        Err(invalid("trailing AI protocol bytes"))
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(text: impl Into<String>) -> Request {
        Request {
            id: 9,
            operation: Operation::Proofread,
            provider: Provider::OpenAi,
            endpoint: "https://api.openai.com/v1".to_owned(),
            auth: Auth::Bearer,
            api_key: "secret-key".to_owned(),
            style: Style::Polite,
            effort: Effort::Low,
            service_tier: ServiceTier::Priority,
            text: text.into(),
        }
    }

    #[test]
    fn request_and_response_roundtrip_unicode_and_metrics() {
        let request = request("文章😀");
        let mut wire = Vec::new();
        write_request(&mut wire, &request).expect("encode request");
        assert_eq!(read_request(&wire[..]).expect("decode request"), request);

        let response = Response {
            id: 9,
            status: Status::Applied,
            result: "文章。😀".into(),
            model: MODEL.into(),
            error_code: String::new(),
            latency_ms: 321,
            input_tokens: 8,
            output_tokens: 4,
            cached_tokens: 2,
            attempts: 1,
        };
        wire.clear();
        write_response(&mut wire, &response).expect("encode response");
        assert_eq!(read_response(&wire[..]).expect("decode response"), response);
    }

    #[test]
    fn failures_cannot_smuggle_result_text() {
        let response = Response {
            id: 1,
            status: Status::ApiError,
            result: "secret".into(),
            model: MODEL.into(),
            error_code: "api_error".into(),
            latency_ms: 1,
            input_tokens: 0,
            output_tokens: 0,
            cached_tokens: 0,
            attempts: 2,
        };
        assert!(write_response(Vec::new(), &response).is_err());
    }

    #[test]
    fn bounds_and_trailing_bytes_fail_closed() {
        let mut request = request("x".repeat(MAX_TEXT_BYTES + 1));
        request.id = 1;
        request.operation = Operation::Transform;
        assert!(write_request(Vec::new(), &request).is_err());
        let mut wire = Vec::new();
        request.text = "x".to_owned();
        write_request(&mut wire, &request).expect("encode");
        wire.push(0);
        assert!(read_request(&wire[..]).is_err());
    }

    #[test]
    fn debug_output_redacts_key_endpoint_and_text() {
        let populated = request("private source");
        let debug = format!("{populated:?}");
        assert!(!debug.contains("secret-key"));
        assert!(!debug.contains("api.openai.com"));
        assert!(!debug.contains("private source"));
        assert!(debug.contains("api_key_present: true"));
        assert!(debug.contains("endpoint_present: true"));
        assert!(debug.contains("text_bytes: 14"));

        let mut absent = request("x");
        absent.endpoint.clear();
        absent.api_key.clear();
        let debug = format!("{absent:?}");
        assert!(debug.contains("endpoint_present: false"));
        assert!(debug.contains("api_key_present: false"));
    }

    #[test]
    fn independent_wire_oracle_covers_every_declared_enum_value() {
        assert_eq!(MAX_TEXT_BYTES, 4096);
        assert_eq!(MAX_ENDPOINT_BYTES, 2048);
        assert_eq!(MAX_API_KEY_BYTES, 2048);
        assert_eq!(MAX_METADATA_BYTES, 128);
        assert_eq!(
            Operation::decode(1).expect("transform"),
            Operation::Transform
        );
        assert_eq!(
            Operation::decode(2).expect("proofread"),
            Operation::Proofread
        );
        assert!(Operation::decode(0).is_err());
        assert!(Operation::decode(3).is_err());

        let statuses = [
            Status::Applied,
            Status::MissingKey,
            Status::TooLarge,
            Status::HttpError,
            Status::ApiError,
            Status::MalformedResponse,
            Status::Timeout,
            Status::WorkerError,
        ];
        for (index, status) in statuses.into_iter().enumerate() {
            assert_eq!(Status::decode((index + 1) as u8).expect("status"), status);
            assert_eq!(status.succeeded(), status == Status::Applied);
        }
        assert!(Status::decode(0).is_err());
        assert!(Status::decode(9).is_err());

        let providers = [
            Provider::OpenAi,
            Provider::AzureOpenAi,
            Provider::AwsBedrock,
            Provider::Cloudflare,
            Provider::Custom,
            Provider::ChatGptCodex,
        ];
        let auths = [Auth::Bearer, Auth::ApiKey, Auth::None];
        let styles = [
            Style::Spoken,
            Style::Polite,
            Style::Business,
            Style::Government,
            Style::Technical,
            Style::Academic,
            Style::Contract,
            Style::Novel,
            Style::Social,
        ];
        let efforts = [
            Effort::ProviderDefault,
            Effort::None,
            Effort::Low,
            Effort::Medium,
            Effort::High,
            Effort::XHigh,
            Effort::Max,
        ];
        let tiers = [ServiceTier::ProviderDefault, ServiceTier::Priority];
        for provider in providers {
            for auth in auths {
                for style in styles {
                    for effort in efforts {
                        for service_tier in tiers {
                            let value = Request {
                                provider,
                                auth,
                                style,
                                effort,
                                service_tier,
                                ..request("property")
                            };
                            let mut wire = Vec::new();
                            write_request(&mut wire, &value).expect("encode combination");
                            assert_eq!(read_request(&wire[..]).expect("decode combination"), value);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn exact_boundaries_magic_version_utf8_and_terminal_status_are_fail_closed() {
        let mut value = request("x".repeat(MAX_TEXT_BYTES));
        value.endpoint = "e".repeat(MAX_ENDPOINT_BYTES);
        value.api_key = "k".repeat(MAX_API_KEY_BYTES);
        let mut wire = Vec::new();
        write_request(&mut wire, &value).expect("exact limits");
        assert_eq!(
            read_request(&wire[..]).expect("exact limit roundtrip"),
            value
        );

        value.endpoint.push('e');
        assert!(write_request(Vec::new(), &value).is_err());
        value.endpoint.pop();
        value.api_key.push('k');
        assert!(write_request(Vec::new(), &value).is_err());
        assert!(write_request(Vec::new(), &request("")).is_err());

        let mut bad_magic = wire.clone();
        bad_magic[0] ^= 1;
        assert!(read_request(&bad_magic[..]).is_err());
        let mut bad_version = wire.clone();
        bad_version[4..6].copy_from_slice(&2u16.to_le_bytes());
        assert!(read_request(&bad_version[..]).is_err());

        let success = Response {
            id: u64::MAX - 7,
            status: Status::Applied,
            result: "r".repeat(MAX_TEXT_BYTES),
            model: "m".repeat(MAX_METADATA_BYTES),
            error_code: "e".repeat(MAX_METADATA_BYTES),
            latency_ms: u64::MAX - 9,
            input_tokens: u32::MAX - 1,
            output_tokens: u32::MAX - 2,
            cached_tokens: u32::MAX - 3,
            attempts: u32::MAX - 4,
        };
        let mut success_wire = Vec::new();
        write_response(&mut success_wire, &success).expect("response limits");
        assert_eq!(
            read_response(&success_wire[..]).expect("response roundtrip"),
            success
        );

        let mut failed_with_text = success_wire.clone();
        failed_with_text[14] = Status::ApiError as u8;
        assert!(read_response(&failed_with_text[..]).is_err());
        let failure = Response {
            status: Status::ApiError,
            result: String::new(),
            ..success
        };
        let mut empty_wire = Vec::new();
        write_response(&mut empty_wire, &failure).expect("empty failure");
        empty_wire[14] = Status::Applied as u8;
        assert!(read_response(&empty_wire[..]).is_err());

        let mut invalid_utf8 = Vec::new();
        write_string(&mut invalid_utf8, "x", 1).expect("string");
        *invalid_utf8.last_mut().expect("payload") = 0xff;
        assert!(read_string(&mut &invalid_utf8[..], 1).is_err());
    }
}
