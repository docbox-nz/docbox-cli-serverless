use aws_sdk_lambda::{error::SdkError, operation::invoke::InvokeError, primitives::Blob};
use docbox_management_interface::{
    DocboxManagementCommand, DocboxServiceError, ManagementError, RemoteDocboxManagementTransport,
    async_trait, error::DynServiceError,
};
use serde::{
    Deserialize,
    de::{DeserializeOwned, Error},
};
use thiserror::Error;

pub struct LambdaManagementTransport {
    pub client: aws_sdk_lambda::Client,
    pub config: FunctionConfig,
}

pub struct FunctionConfig {
    pub name: String,
    pub qualifier: Option<String>,
    pub tenant_id: Option<String>,
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Error)]
enum LambdaManagementError {
    #[error("failed to serialize request")]
    SerializeRequest(serde_json::Error),

    #[error("failed to invoke lambda: {0}")]
    InvokeLambda(SdkError<InvokeError>),

    #[error("failed to parse response from server")]
    ParseResponse(serde_json::Error),

    #[error("unknown function error ({0}): {1}")]
    UnknownFunctionError(String, String),

    #[error("service error: {0}")]
    Service(String),

    #[error("unknown error type")]
    Unknown(String),

    #[error("missing response payload")]
    MissingResponse,
}

impl From<LambdaManagementError> for ManagementError {
    fn from(value: LambdaManagementError) -> Self {
        ManagementError::Service(DynServiceError::from(value))
    }
}

impl DocboxServiceError for LambdaManagementError {}

#[async_trait]
impl RemoteDocboxManagementTransport for LambdaManagementTransport {
    async fn execute_command<T>(
        &self,
        command: DocboxManagementCommand,
    ) -> Result<T, ManagementError>
    where
        T: DeserializeOwned,
    {
        let message =
            serde_json::to_string(&command).map_err(LambdaManagementError::SerializeRequest)?;

        let output = self
            .client
            .invoke()
            .payload(Blob::new(message))
            .function_name(&self.config.name)
            .set_qualifier(self.config.qualifier.clone())
            .set_tenant_id(self.config.tenant_id.clone())
            .send()
            .await
            .map_err(LambdaManagementError::InvokeLambda)?;

        if let Some(function_error) = output.function_error {
            let payload = output
                .payload
                .ok_or(LambdaManagementError::MissingResponse)?;

            if function_error != "Handled" {
                let payload_utf8 = String::from_utf8_lossy(payload.as_ref()).to_string();
                return Err(LambdaManagementError::UnknownFunctionError(
                    function_error,
                    payload_utf8,
                )
                .into());
            }

            let diagnostic: Diagnostic = serde_json::from_slice(payload.as_ref())
                .map_err(LambdaManagementError::ParseResponse)?;

            return Err(match diagnostic.error_type.as_str() {
                "SERVICE_ERROR" => ManagementError::Service(
                    LambdaManagementError::Service(diagnostic.error_message).into(),
                ),
                "UNSUPPORTED_OPERATION" => ManagementError::UnsupportedOperation,
                "SERIALIZE_RESPONSE" => ManagementError::SerializeResponse(
                    serde_json::Error::custom(diagnostic.error_message),
                ),
                _ => ManagementError::Service(
                    LambdaManagementError::Unknown(diagnostic.error_message).into(),
                ),
            });
        }

        let payload = output
            .payload()
            .ok_or(LambdaManagementError::MissingResponse)?;
        let result: T = serde_json::from_slice(payload.as_ref())
            .map_err(LambdaManagementError::ParseResponse)?;
        Ok(result)
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Diagnostic {
    error_type: String,
    error_message: String,
}
